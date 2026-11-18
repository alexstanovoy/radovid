//! Servers that answer a client's first contact the way something else than a service would, for
//! the tests of clients that check a service before they sign in to it. Everything is bounded: a
//! server takes a few connections, each lives on a thread that ends with its client, and no
//! server sends more than [`SEND_CAP`] bytes.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use radovid_proto::{
    ClientFrame, Error, ServerMsg, decode_payload, encode_frame_to_vec, whole_frame,
};
use rustls::{ServerConnection, StreamOwned};
use tokio_tungstenite::tungstenite::{self, Message};

/// Connections a server takes in all; it takes no more after.
const MAX_CONNECTIONS: usize = 32;
/// Bytes a server sends in all, over every connection.
pub const SEND_CAP: u64 = 64 * 1024 * 1024;
/// How long a connection waits for its client before it ends: a hang guard, not a behaviour.
const HANG_GUARD: Duration = Duration::from_secs(120);

/// What the server is.
#[derive(Clone, Debug)]
pub enum Behaviour {
    /// A TLS WebSocket server that sends every binary message back: anything that echoes.
    Echo,
    /// A web server without TLS: every connection gets an HTTP error and is closed.
    PlainHttp,
    /// Takes the connection and says nothing, until the client ends it.
    Silent,
    /// Takes the connection and closes it at once.
    HangUp,
    /// A TLS WebSocket server whose first message is this many bytes of zeros.
    Oversized(usize),
    /// A TLS WebSocket server whose first message never ends: fragment after fragment, until the
    /// client cuts the connection off or [`SEND_CAP`] is spent.
    Endless,
    /// A TLS WebSocket server that answers the `Hello` with this refusal.
    Refuses(Error),
    /// A TLS WebSocket server that answers with a message of the protocol that is no answer to
    /// the `Hello`: a `Done`.
    Mistaken,
}

/// The binary messages a server has read from its clients, in the order they came.
type Received = Arc<Mutex<Vec<Vec<u8>>>>;

/// A running server on a loopback port, stopped when dropped.
pub struct Impostor {
    /// `wss://127.0.0.1:PORT`.
    pub url: String,
    /// The certificate it shows (for 127.0.0.1 and localhost): what a client that trusts it needs.
    pub pem: String,
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    sent: Arc<AtomicU64>,
    received: Received,
    accepted: Mutex<Receiver<()>>,
    ended: Mutex<Receiver<()>>,
}

impl Impostor {
    pub fn start(behaviour: Behaviour) -> Impostor {
        let identity =
            crate::tls::self_signed(vec!["127.0.0.1".to_owned(), "localhost".to_owned()])
                .expect("a certificate");
        let tls = crate::tls::server_config(&identity).expect("a TLS configuration");
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let address = listener.local_addr().expect("bound");
        let stop = Arc::new(AtomicBool::new(false));
        let sent = Arc::new(AtomicU64::new(0));
        let received = Received::default();
        let (ended_tx, ended) = std::sync::mpsc::channel();
        let (accepted_tx, accepted) = std::sync::mpsc::channel();
        let shared = (Arc::clone(&stop), Arc::clone(&sent), Arc::clone(&received));
        std::thread::Builder::new()
            .name("impostor".to_owned())
            .spawn(move || {
                let (stop, sent, received) = shared;
                for stream in listener.incoming().take(MAX_CONNECTIONS) {
                    if stop.load(Ordering::Acquire) {
                        return;
                    }
                    let Ok(stream) = stream else { return };
                    let _ = accepted_tx.send(());
                    let (behaviour, tls) = (behaviour.clone(), Arc::clone(&tls));
                    let (sent, ended) = (Arc::clone(&sent), ended_tx.clone());
                    let received = Arc::clone(&received);
                    let spawned = std::thread::Builder::new()
                        .name("impostor-connection".to_owned())
                        .spawn(move || {
                            serve(&behaviour, stream, tls, &sent, &received);
                            let _ = ended.send(());
                        });
                    if spawned.is_err() {
                        return;
                    }
                }
            })
            .expect("the server thread");
        Impostor {
            url: format!("wss://127.0.0.1:{}", address.port()),
            pem: identity.cert_pem,
            address,
            stop,
            sent,
            received,
            accepted: Mutex::new(accepted),
            ended: Mutex::new(ended),
        }
    }

    /// Bytes the server has sent so far.
    pub fn sent(&self) -> u64 {
        self.sent.load(Ordering::Acquire)
    }

    /// The binary messages the clients have sent so far, as they came: what a client says to a
    /// server it has not signed in to, for a test that it says nothing it should not.
    pub fn received(&self) -> Vec<Vec<u8>> {
        self.received.lock().expect("the received messages").clone()
    }

    /// Waits until the server has taken one more connection.
    pub fn wait_for_a_connection(&self) {
        let accepted = self.accepted.lock().expect("the accept channel");
        accepted
            .recv_timeout(HANG_GUARD)
            .expect("a connection came within the hang guard");
    }

    /// Waits until one connection has ended (its client closed it, or the server did): for a
    /// test that must see a client let go, without a sleep.
    pub fn wait_for_a_connection_to_end(&self) {
        let ended = self.ended.lock().expect("the end channel");
        ended
            .recv_timeout(HANG_GUARD)
            .expect("a connection ended within the hang guard");
    }
}

impl Drop for Impostor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wakes the thread blocked in accept.
        let _ = TcpStream::connect(self.address);
    }
}

fn serve(
    behaviour: &Behaviour,
    mut tcp: TcpStream,
    tls: Arc<rustls::ServerConfig>,
    sent: &AtomicU64,
    received: &Received,
) {
    let _ = tcp.set_read_timeout(Some(HANG_GUARD));
    let _ = tcp.set_write_timeout(Some(HANG_GUARD));
    match behaviour {
        Behaviour::HangUp => {}
        Behaviour::Silent => {
            let mut sink = [0u8; 1024];
            while matches!(tcp.read(&mut sink), Ok(read) if read > 0) {}
        }
        Behaviour::PlainHttp => {
            let mut sink = [0u8; 1024];
            let _ = tcp.read(&mut sink);
            let page =
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = tcp.write_all(page);
            sent.fetch_add(page.len() as u64, Ordering::AcqRel);
        }
        other => {
            let Ok(connection) = ServerConnection::new(tls) else {
                return;
            };
            let Ok(mut ws) = tungstenite::accept(StreamOwned::new(connection, tcp)) else {
                return;
            };
            speak(other, &mut ws, sent, received);
        }
    }
}

type Ws = tungstenite::WebSocket<StreamOwned<ServerConnection, TcpStream>>;

/// What a WebSocket server of this behaviour says once the handshake is done.
fn speak(behaviour: &Behaviour, ws: &mut Ws, sent: &AtomicU64, received: &Received) {
    match behaviour {
        Behaviour::Echo => {
            while let Ok(message) = ws.read() {
                let Message::Binary(bytes) = message else {
                    continue;
                };
                keep(received, &bytes);
                if ws.send(Message::Binary(bytes)).is_err() {
                    return;
                }
            }
        }
        Behaviour::Oversized(len) => {
            let len = (*len as u64).min(SEND_CAP) as usize;
            if ws.send(Message::Binary(vec![0; len].into())).is_ok() {
                sent.fetch_add(len as u64, Ordering::AcqRel);
            }
            drain(ws, received);
        }
        Behaviour::Endless => endless(ws, sent),
        Behaviour::Refuses(error) => {
            if let Some(seq) = hello_seq(ws, received) {
                reply(
                    ws,
                    &ServerMsg::Refused {
                        seq,
                        error: error.clone(),
                    },
                    sent,
                );
            }
            drain(ws, received);
        }
        Behaviour::Mistaken => {
            if let Some(seq) = hello_seq(ws, received) {
                let done = ServerMsg::Reply {
                    seq,
                    reply: radovid_proto::Reply::Done,
                };
                reply(ws, &done, sent);
            }
            drain(ws, received);
        }
        Behaviour::PlainHttp | Behaviour::Silent | Behaviour::HangUp => {}
    }
}

/// The sequence number of the client's first message.
fn hello_seq(ws: &mut Ws, received: &Received) -> Option<u32> {
    loop {
        let Message::Binary(bytes) = ws.read().ok()? else {
            continue;
        };
        keep(received, &bytes);
        let payload = whole_frame(&bytes, radovid_proto::MAX_CONTROL_PAYLOAD_LEN)?;
        return decode_payload::<ClientFrame>(payload)
            .ok()
            .map(|frame| frame.seq);
    }
}

fn reply(ws: &mut Ws, message: &ServerMsg, sent: &AtomicU64) {
    let Ok(frame) = encode_frame_to_vec(message) else {
        return;
    };
    let len = frame.len() as u64;
    if ws.send(Message::Binary(frame.into())).is_ok() {
        sent.fetch_add(len, Ordering::AcqRel);
    }
}

/// Reads until the client ends the connection.
fn drain(ws: &mut Ws, received: &Received) {
    while let Ok(message) = ws.read() {
        if let Message::Binary(bytes) = message {
            keep(received, &bytes);
        }
    }
}

/// Remembers a message of a client, up to a bound: a test reads a few.
fn keep(received: &Received, bytes: &[u8]) {
    const MAX_KEPT: usize = 64;
    let mut kept = received.lock().expect("the received messages");
    if kept.len() < MAX_KEPT {
        kept.push(bytes.to_vec());
    }
}

/// Fragments of one binary message, written raw, until the connection breaks or the cap is spent.
fn endless(ws: &mut Ws, sent: &AtomicU64) {
    const FRAGMENT: usize = 16 * 1024;
    let mut fragment = vec![0u8; 4 + FRAGMENT];
    // Server frames are not masked: FIN clear, then 16-bit length.
    fragment[1] = 126;
    fragment[2..4].copy_from_slice(&(FRAGMENT as u16).to_be_bytes());
    let mut opcode = 0x02;
    while sent.load(Ordering::Acquire) < SEND_CAP {
        fragment[0] = opcode;
        opcode = 0x00;
        let stream = ws.get_mut();
        if stream.write_all(&fragment).is_err() {
            return;
        }
        sent.fetch_add(fragment.len() as u64, Ordering::AcqRel);
    }
}
