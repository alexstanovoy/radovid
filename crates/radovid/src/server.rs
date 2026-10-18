//! The listener: TLS, the WebSocket upgrade, and the choice of path.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use radovid_proto::*;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, watch};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::config::Config;
use crate::db::Db;
use crate::hub::HubHandle;
use crate::password::Passwords;
use crate::throttle::{LoginThrottle, Windowed, source};

/// Seconds a peer has for the TLS and WebSocket handshakes.
const HANDSHAKE_SECS: u64 = 10;
/// Registrations per source address per hour at most.
const REGISTRATIONS_PER_HOUR: u32 = 20;

pub type Socket = WebSocketStream<TlsStream<TcpStream>>;

/// What every connection shares.
pub struct Shared {
    pub config: Arc<Config>,
    pub db: Db,
    pub hub: HubHandle,
    pub passwords: Passwords,
    throttle: Mutex<LoginThrottle>,
    /// Registrations seen per address in the last hour.
    registrations: Mutex<Windowed<IpAddr>>,
    unauthenticated: Mutex<HashMap<IpAddr, usize>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

fn throttled(wait: Duration) -> Error {
    Error::Throttled {
        retry_after_ms: wait.as_millis().min(u128::from(u32::MAX)) as u32,
    }
}

impl Shared {
    pub fn new(config: Arc<Config>, db: Db, hub: HubHandle, passwords: Passwords) -> Shared {
        Shared {
            config,
            db,
            hub,
            passwords,
            throttle: Mutex::new(LoginThrottle::new()),
            registrations: Mutex::new(Windowed::new()),
            unauthenticated: Mutex::new(HashMap::new()),
        }
    }
}

impl Shared {
    /// The refusal for a login attempt that falls inside the delay its address
    /// or its login has earned.
    pub fn login_delay(&self, ip: IpAddr, login: &str) -> Option<Error> {
        let wait = lock(&self.throttle).wait(ip, login, Instant::now());
        (!wait.is_zero()).then(|| throttled(wait))
    }

    pub fn login_failed(&self, ip: IpAddr, login: &str) {
        lock(&self.throttle).failed(ip, login, Instant::now());
    }

    pub fn login_succeeded(&self, login: &str) {
        lock(&self.throttle).succeeded(login);
    }

    /// Counts a registration from `ip`, or refuses it past the hourly limit.
    pub fn register_allowed(&self, ip: IpAddr) -> Result<(), Error> {
        lock(&self.registrations)
            .take(
                &ip,
                Instant::now(),
                REGISTRATIONS_PER_HOUR,
                Duration::from_secs(3600),
            )
            .map_err(throttled)
    }
}

/// Holds one of an address's places for unauthenticated connections.
pub struct Unauthenticated {
    shared: Arc<Shared>,
    ip: IpAddr,
    held: bool,
}

impl Unauthenticated {
    fn take(shared: &Arc<Shared>, ip: IpAddr) -> Option<Unauthenticated> {
        let mut map = lock(&shared.unauthenticated);
        let count = map.entry(ip).or_insert(0);
        if *count >= MAX_UNAUTHENTICATED_PER_IP {
            return None;
        }
        *count += 1;
        Some(Unauthenticated {
            shared: Arc::clone(shared),
            ip,
            held: true,
        })
    }

    /// The connection authenticated: its place is free for the next one.
    pub fn release(&mut self) {
        if self.held {
            self.held = false;
            let mut map = lock(&self.shared.unauthenticated);
            if let Some(count) = map.get_mut(&self.ip) {
                *count -= 1;
                if *count == 0 {
                    map.remove(&self.ip);
                }
            }
        }
    }
}

impl Drop for Unauthenticated {
    fn drop(&mut self) {
        self.release();
    }
}

/// Accepts connections until `stop` says so.
pub async fn serve(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    shared: Arc<Shared>,
    mut stop: watch::Receiver<bool>,
) {
    let held = Arc::new(Semaphore::new(shared.config.max_connections));
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = stop.changed() => return,
        };
        let Ok((stream, peer)) = accepted else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let Ok(place) = Arc::clone(&held).try_acquire_owned() else {
            continue;
        };
        let source = source(peer.ip());
        let Some(guard) = Unauthenticated::take(&shared, source) else {
            continue;
        };
        let acceptor = acceptor.clone();
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            handle(stream, source, acceptor, shared, guard).await;
            drop(place);
        });
    }
}

async fn handle(
    stream: TcpStream,
    source: IpAddr,
    acceptor: TlsAcceptor,
    shared: Arc<Shared>,
    guard: Unauthenticated,
) {
    let _ = stream.set_nodelay(true);
    let deadline = Duration::from_secs(HANDSHAKE_SECS);
    let Ok(Ok(tls)) = tokio::time::timeout(deadline, acceptor.accept(stream)).await else {
        return;
    };
    let mut path = String::new();
    #[allow(clippy::result_large_err)] // the type tungstenite's callback fixes
    let callback = |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
        path = request.uri().path().to_owned();
        if path == "/control" || path == "/game" {
            Ok(response)
        } else {
            let mut refusal = ErrorResponse::new(None);
            *refusal.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::NOT_FOUND;
            Err(refusal)
        }
    };
    // What a client sends on either path (requests, a Hello, orders) is small:
    // a larger message is refused as its header comes in, not buffered first.
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_CONTROL_PAYLOAD_LEN + LEN_PREFIX))
        .max_frame_size(Some(MAX_CONTROL_PAYLOAD_LEN + LEN_PREFIX));
    let upgraded = tokio::time::timeout(
        deadline,
        tokio_tungstenite::accept_hdr_async_with_config(tls, callback, Some(config)),
    )
    .await;
    let Ok(Ok(ws)) = upgraded else {
        return;
    };
    match path.as_str() {
        "/control" => crate::control::run(ws, source, shared, guard).await,
        "/game" => crate::game::run(ws, shared, guard).await,
        _ => {}
    }
}
