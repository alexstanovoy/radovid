//! The `/control` connection.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use radovid_proto::*;
use tungstenite::Message;

use crate::url::{Ws, connect, socket};
use crate::{ClientError, TlsConfig};

const IO_TIMEOUT: Duration = Duration::from_secs(60);
/// Events kept while a request waits for its reply.
const MAX_QUEUED_EVENTS: usize = 1024;
const LABEL: &str = concat!("radovid_client/", env!("CARGO_PKG_VERSION"));

/// What a login tells.
#[derive(Clone, Debug)]
pub struct LoggedIn {
    pub me: AccountInfo,
    /// Presented to `resume` instead of the password.
    pub session: SessionToken,
    /// Where the account is: `InMatch` means ask for the handoff and connect.
    pub presence: Presence,
}

/// A control connection.
pub struct Client {
    ws: Ws,
    seq: u32,
    inbox: VecDeque<Event>,
    server_bota_wire: u64,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Client")
    }
}

impl Client {
    /// Connects to `url` (`wss://host:port`, with or without `/control`) and
    /// says Hello. The server's game wire is kept: see [`Client::server_bota_wire`].
    pub fn connect(url: &str, tls: &TlsConfig) -> Result<Client, ClientError> {
        Self::connect_as(url, tls, PROTO_VERSION, bota_proto::WIRE_FINGERPRINT)
    }

    /// As [`Client::connect`], claiming other versions: for tools and tests of
    /// the refusals.
    pub fn connect_as(
        url: &str,
        tls: &TlsConfig,
        proto: u16,
        bota_wire: u64,
    ) -> Result<Client, ClientError> {
        let url = if url.trim_end_matches('/').ends_with("/control") {
            url.to_owned()
        } else {
            format!("{}/control", url.trim_end_matches('/'))
        };
        let ws = connect(&url, tls, MAX_CONTROL_PAYLOAD_LEN + LEN_PREFIX, IO_TIMEOUT)?;
        let mut client = Client {
            ws,
            seq: 0,
            inbox: VecDeque::new(),
            server_bota_wire: 0,
        };
        let reply = client.request(ClientMsg::Hello {
            proto,
            bota_wire,
            client: LABEL.to_owned(),
        })?;
        match reply {
            Reply::Welcome { bota_wire, .. } => client.server_bota_wire = bota_wire,
            other => {
                return Err(ClientError::Protocol(format!(
                    "expected Welcome, got {other:?}"
                )));
            }
        }
        Ok(client)
    }

    /// Sends bytes as one message, as they are: for tests of how the service
    /// meets hostile input.
    #[doc(hidden)]
    pub fn send_raw(&mut self, bytes: Vec<u8>) -> Result<(), ClientError> {
        self.ws.send(Message::Binary(bytes.into()))?;
        Ok(())
    }

    /// Reads one message of any kind, for the same tests.
    #[doc(hidden)]
    pub fn read_raw(&mut self, wait: Duration) -> Result<Option<ServerMsg>, ClientError> {
        self.read(wait)
    }

    /// The fingerprint of the game wire the server's game servers speak.
    pub fn server_bota_wire(&self) -> u64 {
        self.server_bota_wire
    }

    pub fn register(
        &mut self,
        login: &str,
        password: &str,
        kind: AccountKind,
        info: ClientInfo,
    ) -> Result<LoggedIn, ClientError> {
        let reply = self.request(ClientMsg::Register {
            login: login.to_owned(),
            password: password.to_owned(),
            kind,
            client: info,
        })?;
        logged_in(reply)
    }

    pub fn login(
        &mut self,
        login: &str,
        password: &str,
        info: ClientInfo,
    ) -> Result<LoggedIn, ClientError> {
        let reply = self.request(ClientMsg::Login {
            login: login.to_owned(),
            password: password.to_owned(),
            client: info,
        })?;
        logged_in(reply)
    }

    pub fn resume(
        &mut self,
        token: SessionToken,
        info: ClientInfo,
    ) -> Result<LoggedIn, ClientError> {
        let reply = self.request(ClientMsg::Resume {
            session: token,
            client: info,
        })?;
        logged_in(reply)
    }

    /// Sends one request and waits for its reply; events that arrive meanwhile
    /// are kept for [`Client::next_event`].
    pub fn request(&mut self, msg: ClientMsg) -> Result<Reply, ClientError> {
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        let bytes = encode_frame_to_vec(&ClientFrame { seq, msg })
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        self.ws.send(Message::Binary(bytes.into()))?;
        let deadline = Instant::now() + IO_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.read(left)? {
                None => return Err(ClientError::Timeout),
                Some(ServerMsg::Reply { seq: got, reply }) if got == seq => return Ok(reply),
                Some(ServerMsg::Refused { seq: got, error }) if got == seq => {
                    return Err(ClientError::Refused(error));
                }
                Some(ServerMsg::Event(event)) => self.keep(event),
                Some(_) => {}
            }
        }
    }

    fn keep(&mut self, event: Event) {
        if self.inbox.len() >= MAX_QUEUED_EVENTS {
            self.inbox.pop_front();
        }
        self.inbox.push_back(event);
    }

    /// The next event, waiting up to `timeout` for one.
    pub fn next_event(&mut self, timeout: Duration) -> Result<Option<Event>, ClientError> {
        if let Some(event) = self.inbox.pop_front() {
            return Ok(Some(event));
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.read(left)? {
                None => return Ok(None),
                Some(ServerMsg::Event(event)) => return Ok(Some(event)),
                Some(_) => {}
            }
        }
    }

    /// Joins the queue and waits for the match to be found.
    pub fn queue_and_wait(&mut self, ruleset: &RulesetId) -> Result<Handoff, ClientError> {
        self.request(ClientMsg::QueueJoin {
            ruleset: ruleset.clone(),
        })?;
        loop {
            match self.next_event(Duration::from_secs(3600))? {
                Some(Event::MatchFound { handoff }) => return Ok(handoff),
                Some(Event::Bye { error }) => return Err(ClientError::Refused(error)),
                Some(_) => {}
                None => return Err(ClientError::Timeout),
            }
        }
    }

    /// Reads one message, or none when `wait` runs out first.
    fn read(&mut self, wait: Duration) -> Result<Option<ServerMsg>, ClientError> {
        let wait = wait.max(Duration::from_millis(1));
        if let Some(socket) = socket(&self.ws) {
            socket
                .set_read_timeout(Some(wait))
                .map_err(|e| ClientError::Io(e.to_string()))?;
        }
        loop {
            match self.ws.read() {
                Ok(Message::Binary(bytes)) => return decode(&bytes).map(Some),
                Ok(Message::Close(_)) => return Err(ClientError::Closed),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None);
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

fn decode(bytes: &[u8]) -> Result<ServerMsg, ClientError> {
    let payload = whole_frame(bytes, MAX_CONTROL_PAYLOAD_LEN)
        .ok_or_else(|| ClientError::Protocol("a message that is no frame".to_owned()))?;
    decode_payload(payload).map_err(|e| ClientError::Protocol(e.to_string()))
}

fn logged_in(reply: Reply) -> Result<LoggedIn, ClientError> {
    match reply {
        Reply::LoggedIn {
            me,
            session,
            presence,
        } => Ok(LoggedIn {
            me,
            session,
            presence,
        }),
        other => Err(ClientError::Protocol(format!(
            "expected LoggedIn, got {other:?}"
        ))),
    }
}
