//! The listener: TLS, the WebSocket upgrade, and the choice of path.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use radovid_proto::*;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::config::Config;
use crate::db::Db;
use crate::hub::HubHandle;
use crate::password::Passwords;
use crate::throttle::{LoginThrottle, Windowed};

/// Seconds a peer has for the TLS and WebSocket handshakes.
const HANDSHAKE_SECS: u64 = 10;
/// Registrations per source address per hour at most.
pub const REGISTRATIONS_PER_HOUR: u32 = 20;

pub type Socket = WebSocketStream<TlsStream<TcpStream>>;

/// What every connection shares.
pub struct Shared {
    pub config: Arc<Config>,
    pub db: Db,
    pub hub: HubHandle,
    pub passwords: Passwords,
    pub throttle: Mutex<LoginThrottle>,
    /// Registrations seen per address in the last hour.
    pub registrations: Mutex<Windowed<IpAddr>>,
    unauthenticated: Mutex<HashMap<IpAddr, usize>>,
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

/// Holds one of an address's places for unauthenticated connections.
pub struct Unauthenticated {
    shared: Arc<Shared>,
    ip: IpAddr,
    held: bool,
}

impl Unauthenticated {
    fn take(shared: &Arc<Shared>, ip: IpAddr) -> Option<Unauthenticated> {
        let mut map = shared
            .unauthenticated
            .lock()
            .unwrap_or_else(|p| p.into_inner());
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
            self.shared.release_unauthenticated(self.ip);
        }
    }
}

impl Drop for Unauthenticated {
    fn drop(&mut self) {
        self.release();
    }
}

impl Shared {
    fn release_unauthenticated(&self, ip: IpAddr) {
        let mut map = self
            .unauthenticated
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(count) = map.get_mut(&ip) {
            *count -= 1;
            if *count == 0 {
                map.remove(&ip);
            }
        }
    }
}

/// Accepts connections until `stop` says so.
pub async fn serve(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    shared: Arc<Shared>,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = stop.changed() => return,
        };
        let Ok((stream, peer)) = accepted else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let Some(guard) = Unauthenticated::take(&shared, peer.ip()) else {
            continue;
        };
        let acceptor = acceptor.clone();
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            handle(stream, peer, acceptor, shared, guard).await;
        });
    }
}

async fn handle(
    stream: TcpStream,
    peer: SocketAddr,
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
    // Frames up to bota's largest are legal on the game path; the control path
    // holds its own, smaller limit.
    let config = WebSocketConfig::default()
        .max_message_size(Some(bota_proto::MAX_PAYLOAD_LEN + bota_proto::LEN_PREFIX))
        .max_frame_size(Some(bota_proto::MAX_PAYLOAD_LEN + bota_proto::LEN_PREFIX));
    let upgraded = tokio::time::timeout(
        deadline,
        tokio_tungstenite::accept_hdr_async_with_config(tls, callback, Some(config)),
    )
    .await;
    let Ok(Ok(ws)) = upgraded else {
        return;
    };
    match path.as_str() {
        "/control" => crate::control::run(ws, peer.ip(), shared, guard).await,
        "/game" => crate::game::run(ws, peer.ip(), shared, guard).await,
        _ => {}
    }
}
