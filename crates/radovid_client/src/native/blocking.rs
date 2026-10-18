//! A blocking control connection, for bots and tools.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use radovid_proto::*;

use super::socket::{Ws, connect, read_binary, send_binary, set_read_timeout};
use crate::{ClientError, Control, Incoming, TlsConfig, control_url};

/// How long a request waits for its answer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Events kept while a request waits for its reply.
const MAX_QUEUED_EVENTS: usize = 1024;

/// What a login tells.
#[derive(Clone, Debug)]
pub struct LoggedIn {
    pub me: AccountInfo,
    /// Presented to `resume` instead of the password.
    pub session: SessionToken,
    /// What the account has in flight: running matches to rejoin, queue
    /// entries, open proposals, lobbies.
    pub activities: Activities,
}

/// A control connection whose every call blocks until it is answered.
pub struct Client {
    ws: Ws,
    control: Control,
    inbox: VecDeque<Event>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Client")
    }
}

impl Client {
    /// Connects to `url` (`wss://host:port`, with or without `/control`) and
    /// says Hello for a client of the game wire `game_fingerprint`.
    pub fn connect(
        url: &str,
        tls: &TlsConfig,
        game_fingerprint: u64,
    ) -> Result<Client, ClientError> {
        Self::connect_as(url, tls, PROTO_VERSION, game_fingerprint)
    }

    /// As [`Client::connect`], claiming another protocol version: for tools
    /// and tests of the refusals.
    pub fn connect_as(
        url: &str,
        tls: &TlsConfig,
        proto: u16,
        game_fingerprint: u64,
    ) -> Result<Client, ClientError> {
        let ws = connect(&control_url(url), tls, REQUEST_TIMEOUT)?;
        let mut client = Client {
            ws,
            control: Control::new(),
            inbox: VecDeque::new(),
        };
        let reply = client.request(ClientMsg::Hello {
            proto,
            game_fingerprint,
            client: crate::control::LABEL.to_owned(),
        })?;
        match reply {
            Reply::Welcome { .. } => Ok(client),
            other => Err(ClientError::Protocol(format!(
                "expected Welcome, got {other:?}"
            ))),
        }
    }

    /// What the service knows about itself and the account.
    pub fn control(&self) -> &Control {
        &self.control
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
            password: Secret::new(password),
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
            password: Secret::new(password),
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

    /// Sends one request and waits for its answer; events that arrive
    /// meanwhile are kept for [`Client::next_event`].
    pub fn request(&mut self, msg: ClientMsg) -> Result<Reply, ClientError> {
        let out = self.control.request(msg)?;
        send_binary(&mut self.ws, out.bytes)?;
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(ClientError::Timeout);
            }
            match self.read(left)? {
                None => {}
                Some(Incoming::Reply { seq, result }) if seq == out.seq => {
                    return result.map_err(ClientError::Refused);
                }
                Some(Incoming::Reply { .. }) => {}
                Some(Incoming::Event(event)) => self.keep(event),
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
            if left.is_zero() {
                return Ok(None);
            }
            if let Some(Incoming::Event(event)) = self.read(left)? {
                return Ok(Some(event));
            }
        }
    }

    /// Sends bytes as one message, as they are: for tests of how the service
    /// meets hostile input.
    #[doc(hidden)]
    pub fn send_raw(&mut self, bytes: Vec<u8>) -> Result<(), ClientError> {
        send_binary(&mut self.ws, bytes)
    }

    /// Reads one message of any kind, for the same tests.
    #[doc(hidden)]
    pub fn read_raw(&mut self, wait: Duration) -> Result<Option<Incoming>, ClientError> {
        self.read(wait)
    }

    /// Reads one message, or none when `wait` runs out first.
    fn read(&mut self, wait: Duration) -> Result<Option<Incoming>, ClientError> {
        set_read_timeout(&self.ws, wait)?;
        match read_binary(&mut self.ws)? {
            Some(bytes) => self.control.receive(&bytes).map(Some),
            None => Ok(None),
        }
    }
}

fn logged_in(reply: Reply) -> Result<LoggedIn, ClientError> {
    match reply {
        Reply::LoggedIn {
            me,
            session,
            activities,
        } => Ok(LoggedIn {
            me,
            session,
            activities,
        }),
        other => Err(ClientError::Protocol(format!(
            "expected LoggedIn, got {other:?}"
        ))),
    }
}
