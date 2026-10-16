//! Why a request failed.

use serde::{Deserialize, Serialize};

use crate::{Presence, UnixMs};

/// Everything the service refuses with. Variant 0 is frozen: it must decode in
/// every version, so that a client of any age can read why it was turned away.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Error {
    /// The `Hello` named another protocol version; closes the connection.
    ProtoMismatch { server: u16 },
    /// The bytes were no message; closes the connection.
    Malformed,
    NotLoggedIn,
    AlreadyLoggedIn,
    TooManySessions,
    /// Too many messages; the frame was dropped.
    RateLimited { retry_after_ms: u32 },
    /// Too many failed logins; the attempt was not evaluated.
    Throttled { retry_after_ms: u32 },
    BadCredentials,
    BadSession,
    Banned { reason: String, until: Option<UnixMs> },
    BadLogin,
    LoginTaken,
    BadNickname,
    WeakPassword,
    BadBotVersion,
    BadLobbyName,
    Forbidden,
    UnknownAccount,
    UnknownRuleset,
    NoQueue,
    /// The game servers run another wire than the client was built with.
    BotaWireMismatch { server: u64 },
    Busy { presence: Presence },
    NotInQueue,
    NotInLobby,
    NotInMatch,
    UnknownLobby,
    UnknownMatch,
    LobbyFull,
    LobbyNotFull,
    NotAllReady,
    AudienceMismatch,
    NotLobbyOwner,
    NoSuchMember,
    NoSuchRequest,
    NoPendingRequest,
    TooManyLobbies,
    /// The lobby is counting down; nothing changes now.
    Starting,
    /// Only in `Event::Bye`: the connection was idle too long.
    Idle,
    /// Only in `Event::Bye`: the service is shutting down.
    ServerStopping,
    Internal,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}
