//! The native [`ServiceCheck`]: a thread that resolves, connects, greets and judges within a
//! deadline, while its owner polls. Every step is bounded: the address list, each connect, each
//! read and write (in short slices, so that the deadline and a cancel are looked at between them),
//! the headers and the one message read.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use radovid_proto::LEN_PREFIX;
use rustls::pki_types::ServerName;
use rustls::{ClientConnection, StreamOwned};
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{HandshakeError, Message, WebSocket};

use crate::check::{
    MAX_ANSWER_BYTES, MAX_CHECKS_AT_ONCE, ServiceFault, ServiceFound, judge_message,
};
use crate::{Control, TlsConfig, control_url};

/// How long a read or write waits before the deadline and the cancel are looked at again.
const SLICE: Duration = Duration::from_millis(100);
/// How long one connection attempt takes at most, so that a host with several addresses is not
/// spent on the first.
const CONNECT_ATTEMPT: Duration = Duration::from_secs(3);
/// Addresses of a host tried at most.
const MAX_ADDRESSES: usize = 8;
/// Messages that are no answer (pings, texts) put up with before the service is no service.
const MAX_NOISE: usize = 16;

/// Threads of checks that run now, over every check of the process.
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// One of the [`MAX_CHECKS_AT_ONCE`] places, given back when dropped.
struct Place;

impl Place {
    fn take() -> Option<Place> {
        let before = RUNNING.fetch_add(1, Ordering::AcqRel);
        if before >= MAX_CHECKS_AT_ONCE {
            RUNNING.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Place)
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        RUNNING.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A check of a service under way. Dropping it ends the check at the next step of its thread: the
/// thread never outlives its deadline, except inside the system's name lookup.
pub struct ServiceCheck {
    outcome: Receiver<Result<ServiceFound, ServiceFault>>,
    cancel: Arc<AtomicBool>,
    reported: bool,
}

impl ServiceCheck {
    /// Starts checking `url` (`wss://host[:port][/path]`, `/control` added as it is for a
    /// connection) for a client of the game wire `game_fingerprint`, giving up after `patience`.
    pub fn start(
        url: &str,
        tls: &TlsConfig,
        game_fingerprint: u64,
        patience: Duration,
    ) -> ServiceCheck {
        let (report, outcome) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let check = ServiceCheck {
            outcome,
            cancel: Arc::clone(&cancel),
            reported: false,
        };
        let Some(place) = Place::take() else {
            let _ = report.send(Err(ServiceFault::TooManyChecks));
            return check;
        };
        let (url, tls) = (control_url(url), tls.clone());
        let deadline = Instant::now() + patience;
        let spawned = std::thread::Builder::new()
            .name("radovid-check".to_owned())
            .spawn({
                let report = report.clone();
                move || {
                    let clock = Clock { deadline, cancel };
                    let _ = report.send(run(&url, &tls, game_fingerprint, &clock));
                    drop(place);
                }
            });
        if spawned.is_err() {
            let _ = report.send(Err(ServiceFault::TooManyChecks));
        }
        check
    }

    /// The outcome, once, when it is there; nothing while the check runs and nothing after it
    /// was returned.
    pub fn poll(&mut self) -> Option<Result<ServiceFound, ServiceFault>> {
        if self.reported {
            return None;
        }
        let outcome = match self.outcome.try_recv() {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(ServiceFault::Hangup),
        };
        self.reported = true;
        Some(outcome)
    }
}

impl Drop for ServiceCheck {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

/// The deadline and the cancel, looked at between the steps.
struct Clock {
    deadline: Instant,
    cancel: Arc<AtomicBool>,
}

impl Clock {
    /// The time left, or why the check ends now.
    fn left(&self) -> Result<Duration, ServiceFault> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(ServiceFault::Hangup);
        }
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            Err(ServiceFault::TimedOut)
        } else {
            Ok(left)
        }
    }
}

/// A socket whose timeouts read as `WouldBlock` on every platform, as the handshake and the
/// reads resume on that.
struct Sliced(TcpStream);

fn patient(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::TimedOut {
        io::ErrorKind::WouldBlock.into()
    } else {
        error
    }
}

impl Read for Sliced {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer).map_err(patient)
    }
}

impl Write for Sliced {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.write(buffer).map_err(patient)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush().map_err(patient)
    }
}

type Ws = WebSocket<StreamOwned<ClientConnection, Sliced>>;

fn run(
    url: &str,
    tls: &TlsConfig,
    game_fingerprint: u64,
    clock: &Clock,
) -> Result<ServiceFound, ServiceFault> {
    let request = url
        .into_client_request()
        .map_err(|_| ServiceFault::BadAddress)?;
    let uri = request.uri();
    if uri.scheme_str() != Some("wss") {
        return Err(ServiceFault::BadAddress);
    }
    let host = uri
        .host()
        .ok_or(ServiceFault::BadAddress)?
        .trim_matches(['[', ']'])
        .to_owned();
    let port = uri.port_u16().unwrap_or(443);
    let name = ServerName::try_from(host.clone()).map_err(|_| ServiceFault::BadAddress)?;
    clock.left()?;
    let stream = open_tcp(&host, port, clock)?;
    stream
        .set_read_timeout(Some(SLICE))
        .and_then(|()| stream.set_write_timeout(Some(SLICE)))
        .and_then(|()| stream.set_nodelay(true))
        .map_err(|_| ServiceFault::Hangup)?;
    let session =
        ClientConnection::new(tls.config.clone(), name).map_err(|_| ServiceFault::Insecure)?;
    let stream = StreamOwned::new(session, Sliced(stream));
    let max = MAX_ANSWER_BYTES + LEN_PREFIX;
    let config = WebSocketConfig::default()
        .max_message_size(Some(max))
        .max_frame_size(Some(max));
    let mut attempt = tungstenite::client::client_with_config(request, stream, Some(config));
    let mut ws = loop {
        match attempt {
            Ok((ws, _)) => break ws,
            Err(HandshakeError::Interrupted(middle)) => {
                clock.left()?;
                attempt = middle.handshake();
            }
            Err(HandshakeError::Failure(error)) => return Err(classify(&error)),
        }
    };
    let mut control = Control::new();
    let hello = control
        .hello(game_fingerprint)
        .map_err(|_| ServiceFault::BadAddress)?;
    send(&mut ws, hello.bytes, clock)?;
    let judged = read_answer(&mut ws, clock).and_then(|message| judge_message(hello.seq, &message));
    let _ = ws.close(None);
    judged
}

/// The first address of `host` that takes a connection within the time left.
fn open_tcp(host: &str, port: u16, clock: &Clock) -> Result<TcpStream, ServiceFault> {
    let addresses: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|_| ServiceFault::HostNotFound)?
        .take(MAX_ADDRESSES)
        .collect();
    if addresses.is_empty() {
        return Err(ServiceFault::HostNotFound);
    }
    let mut fault = ServiceFault::TimedOut;
    for address in addresses {
        let attempt = clock.left()?.min(CONNECT_ATTEMPT);
        match TcpStream::connect_timeout(&address, attempt) {
            Ok(stream) => return Ok(stream),
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                fault = ServiceFault::Refused;
            }
            Err(_) => fault = ServiceFault::TimedOut,
        }
    }
    Err(fault)
}

fn would_block(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

fn send(ws: &mut Ws, bytes: Vec<u8>, clock: &Clock) -> Result<(), ServiceFault> {
    match ws.send(Message::Binary(bytes.into())) {
        Ok(()) => return Ok(()),
        Err(tungstenite::Error::Io(error)) if would_block(&error) => {}
        Err(error) => return Err(classify(&error)),
    }
    // Queued: written once the socket takes it.
    loop {
        clock.left()?;
        match ws.flush() {
            Ok(()) => return Ok(()),
            Err(tungstenite::Error::Io(error)) if would_block(&error) => {}
            Err(error) => return Err(classify(&error)),
        }
    }
}

/// The first binary message of the service.
fn read_answer(ws: &mut Ws, clock: &Clock) -> Result<Vec<u8>, ServiceFault> {
    let mut noise = 0;
    loop {
        clock.left()?;
        match ws.read() {
            Ok(Message::Binary(bytes)) => return Ok(bytes.into()),
            Ok(Message::Close(_)) => return Err(ServiceFault::Hangup),
            Ok(_) => {
                noise += 1;
                if noise > MAX_NOISE {
                    return Err(ServiceFault::NotService);
                }
            }
            Err(tungstenite::Error::Io(error)) if would_block(&error) => {}
            Err(error) => return Err(classify(&error)),
        }
    }
}

/// What a failed handshake or read says of the service.
fn classify(error: &tungstenite::Error) -> ServiceFault {
    match error {
        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => {
            ServiceFault::Hangup
        }
        tungstenite::Error::Io(error) => classify_io(error),
        tungstenite::Error::Url(_) => ServiceFault::BadAddress,
        // An HTTP answer that is no upgrade, headers that are no HTTP, frames that are no
        // WebSocket, a message past the bound: whatever answers is no radovid.
        _ => ServiceFault::NotService,
    }
}

fn classify_io(error: &io::Error) -> ServiceFault {
    use io::ErrorKind::{BrokenPipe, ConnectionAborted, ConnectionReset, UnexpectedEof};
    let tls = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>());
    match tls {
        Some(
            rustls::Error::InvalidCertificate(_)
            | rustls::Error::NoCertificatesPresented
            | rustls::Error::PeerIncompatible(_)
            | rustls::Error::AlertReceived(_),
        ) => ServiceFault::Insecure,
        // Bytes that are no TLS at all: another kind of service on this port.
        Some(_) => ServiceFault::NotService,
        None => match error.kind() {
            UnexpectedEof | ConnectionReset | ConnectionAborted | BrokenPipe => {
                ServiceFault::Hangup
            }
            _ => ServiceFault::NotService,
        },
    }
}
