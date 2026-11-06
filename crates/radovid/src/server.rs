//! The listener: TLS (its certificate swapped on reload), the WebSocket
//! upgrade, and `/control`.

use std::net::IpAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use radovid_proto::*;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, watch};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::server::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::config::Config;
use crate::db::Db;
use crate::hub::HubHandle;
use crate::password::Passwords;
use crate::throttle::{LoginThrottle, Place, Places, Windowed, source};
use crate::web;

/// Seconds a peer has for the TLS and WebSocket handshakes.
const HANDSHAKE_SECS: u64 = 10;
/// Registrations per source address per hour at most.
const REGISTRATIONS_PER_HOUR: u32 = 20;

/// What a connection speaks over: the TLS stream, with the bytes of the request head that were
/// read first (to tell a page from the control socket) given back before the rest.
pub struct Replayed {
    head: Vec<u8>,
    at: usize,
    inner: TlsStream<TcpStream>,
}

impl AsyncRead for Replayed {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = &mut *self;
        if this.at < this.head.len() {
            let n = (this.head.len() - this.at).min(buf.remaining());
            buf.put_slice(&this.head[this.at..this.at + n]);
            this.at += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Replayed {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, data)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

pub type Socket = WebSocketStream<Replayed>;

/// The TLS configuration new connections get; a reload replaces it, and
/// connections already up keep theirs.
pub type Certificates = Arc<RwLock<Arc<ServerConfig>>>;

/// What every connection shares.
pub struct Shared {
    pub config: Arc<Config>,
    pub db: Db,
    pub hub: HubHandle,
    pub passwords: Passwords,
    throttle: Mutex<LoginThrottle>,
    /// Registrations seen per address in the last hour.
    registrations: Mutex<Windowed<IpAddr>>,
    unauthenticated: Places,
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
            unauthenticated: Places::default(),
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

    pub fn login_succeeded(&self, ip: IpAddr, login: &str) {
        lock(&self.throttle).succeeded(ip, login);
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

/// Holds one of an address's places for unauthenticated connections, in every group it counts
/// in ([`groups`](crate::throttle::groups)).
pub struct Unauthenticated(Option<Place>);

impl Unauthenticated {
    fn take(shared: &Shared, ip: IpAddr) -> Option<Unauthenticated> {
        shared
            .unauthenticated
            .take(ip)
            .map(|place| Unauthenticated(Some(place)))
    }

    /// The connection authenticated: its place is free for the next one.
    pub fn release(&mut self) {
        self.0 = None;
    }
}

/// Accepts connections until `stop` says so.
pub async fn serve(
    listener: TcpListener,
    certificates: Certificates,
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
        let Some(guard) = Unauthenticated::take(&shared, peer.ip()) else {
            continue;
        };
        let current = certificates
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let acceptor = TlsAcceptor::from(current);
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
    let Ok(Ok(mut tls)) = tokio::time::timeout(deadline, acceptor.accept(stream)).await else {
        return;
    };
    // With a directory of files to serve, the request head says whether this is the control socket
    // or a page; without one every request is the socket's.
    let mut head = Vec::new();
    if let Some(web) = &shared.config.web {
        let Ok(Ok(())) = tokio::time::timeout(deadline, read_head(&mut tls, &mut head)).await
        else {
            return;
        };
        match web::parse(&head) {
            Some(request) if !request.upgrade => {
                // The page keeps its source's place until it is sent, within its deadline: a peer
                // that reads it at a crawl would otherwise hold one of the service's connections
                // for every request it makes, all of them in the end.
                let sent = async {
                    let _ = web::serve(&mut tls, &web.static_dir, &request).await;
                    let _ = tls.shutdown().await;
                };
                let deadline = Duration::from_secs(web.send_deadline_secs);
                let _ = tokio::time::timeout(deadline, sent).await;
                return;
            }
            Some(_) => {}
            None => return,
        }
    }
    let tls = Replayed {
        head,
        at: 0,
        inner: tls,
    };
    #[allow(clippy::result_large_err)] // the type tungstenite's callback fixes
    let callback = |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
        if request.uri().path() == "/control" {
            Ok(response)
        } else {
            let mut refusal = ErrorResponse::new(None);
            *refusal.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::NOT_FOUND;
            Err(refusal)
        }
    };
    // What a client sends is small: a larger message is refused as its header
    // comes in, not buffered first.
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
    crate::control::run(ws, source, shared, guard).await;
}

/// Reads until the request head is complete (its blank line) or too long.
async fn read_head(tls: &mut TlsStream<TcpStream>, head: &mut Vec<u8>) -> std::io::Result<()> {
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        if head.len() >= web::MAX_HEAD_LEN {
            return Err(std::io::Error::other("a request head too long"));
        }
        let n = tls.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        head.extend_from_slice(&chunk[..n]);
    }
    Ok(())
}
