//! The protocol of `/control` without I/O.

use radovid_proto::*;

use crate::ClientError;

/// What a client says in its `Hello` besides the versions.
pub const LABEL: &str = concat!("radovid_client/", env!("CARGO_PKG_VERSION"));

/// One request, numbered and encoded: send `bytes` as one binary WebSocket
/// message; the answer carries `seq`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outgoing {
    pub seq: u32,
    pub bytes: Vec<u8>,
}

/// One message from the service.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    /// The answer to the request numbered `seq`.
    Reply {
        seq: u32,
        result: Result<Reply, Error>,
    },
    Event(Event),
}

/// The client's side of a control connection, without the connection: it
/// numbers and encodes requests, decodes what arrives, and keeps what the
/// service said about itself and the account.
#[derive(Debug, Default)]
pub struct Control {
    seq: u32,
    server: Option<String>,
    rulesets: Vec<RulesetInfo>,
    me: Option<AccountInfo>,
}

impl Control {
    pub fn new() -> Control {
        Control::default()
    }

    /// The first request of every connection.
    pub fn hello(&mut self, game_fingerprint: u64) -> Result<Outgoing, ClientError> {
        self.request(ClientMsg::Hello {
            proto: PROTO_VERSION,
            game_fingerprint,
            client: LABEL.to_owned(),
        })
    }

    /// Numbers and encodes a request.
    pub fn request(&mut self, msg: ClientMsg) -> Result<Outgoing, ClientError> {
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        let bytes = encode_frame_to_vec(&ClientFrame { seq, msg })
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        if bytes.len() > LEN_PREFIX + MAX_CONTROL_PAYLOAD_LEN {
            return Err(ClientError::Protocol("the request is too large".to_owned()));
        }
        Ok(Outgoing { seq, bytes })
    }

    /// Decodes one binary message from the service.
    pub fn receive(&mut self, message: &[u8]) -> Result<Incoming, ClientError> {
        let payload = whole_frame(message, MAX_SERVER_PAYLOAD_LEN)
            .ok_or_else(|| ClientError::Protocol("a message that is no frame".to_owned()))?;
        let msg: ServerMsg =
            decode_payload(payload).map_err(|e| ClientError::Protocol(e.to_string()))?;
        Ok(match msg {
            ServerMsg::Reply { seq, reply } => {
                self.note(&reply);
                Incoming::Reply {
                    seq,
                    result: Ok(reply),
                }
            }
            ServerMsg::Refused { seq, error } => Incoming::Reply {
                seq,
                result: Err(error),
            },
            ServerMsg::Event(event) => Incoming::Event(event),
        })
    }

    fn note(&mut self, reply: &Reply) {
        match reply {
            Reply::Welcome {
                server, rulesets, ..
            } => {
                self.server = Some(server.clone());
                self.rulesets = rulesets.clone();
            }
            Reply::Rulesets { list } => self.rulesets = list.clone(),
            Reply::LoggedIn { me, .. } => self.me = Some(me.clone()),
            _ => {}
        }
    }

    /// What the service calls itself, once it welcomed the client.
    pub fn server(&self) -> Option<&str> {
        self.server.as_deref()
    }

    /// The catalog, as the service last sent it.
    pub fn rulesets(&self) -> &[RulesetInfo] {
        &self.rulesets
    }

    /// The account, once logged in.
    pub fn me(&self) -> Option<&AccountInfo> {
        self.me.as_ref()
    }
}

/// The control URL of a service: `wss://host[:port]` with `/control`
/// appended unless it is there already.
pub fn control_url(url: &str) -> String {
    let url = url.trim_end_matches('/');
    if url.ends_with("/control") {
        url.to_owned()
    } else {
        format!("{url}/control")
    }
}
