//! Why a request failed.

use serde::{Deserialize, Serialize};

use crate::{LobbyId, UnixMs};

/// Everything the service refuses with. Variant 0 is frozen: it must decode in
/// every version, so that a client of any age can read why it was turned away.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Error {
    /// The `Hello` named another protocol version; closes the connection.
    ProtoMismatch {
        server: u16,
    },
    /// The bytes were no message; closes the connection.
    Malformed,
    NotLoggedIn,
    AlreadyLoggedIn,
    TooManySessions,
    /// Too many messages; the frame was dropped.
    RateLimited {
        retry_after_ms: u32,
    },
    /// Too many failed attempts (logins, lobby passwords), or too many
    /// abandoned matches; the request was not evaluated.
    Throttled {
        retry_after_ms: u32,
    },
    BadCredentials,
    BadSession,
    Banned {
        reason: String,
        until: Option<UnixMs>,
    },
    BadLogin,
    LoginTaken,
    BadNickname,
    WeakPassword,
    BadBotVersion,
    BadLobbyName,
    Forbidden,
    UnknownAccount,
    UnknownRuleset,
    /// The ruleset is not rated: it has no queue and no ladder.
    NoQueue,
    /// The client's `Hello` named another game wire than the ruleset's game
    /// servers speak.
    GameVersionMismatch {
        server: u64,
    },
    NotInQueue,
    NotInLobby {
        lobby: LobbyId,
    },
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
    /// Only in `Event::Bye`: the service is shutting down.
    ServerStopping,
    Internal,
    /// The account has this many games in flight (queue entries, lobbies, join
    /// requests and unfinished matches) already.
    TooManyGames {
        limit: u8,
    },
    /// The accounts of this network have this many games in flight already.
    TooManyGamesFromSource {
        limit: u8,
    },
    /// The queues hold as many entries as the service takes.
    QueueFull {
        retry_after_ms: u32,
    },
    /// A declined or unanswered proposal keeps the account out of the queues
    /// this long.
    QueueCooldown {
        retry_after_ms: u32,
    },
    UnknownProposal,
    /// The proposal's time to answer ran out.
    ProposalExpired,
    BadLobbyPassword,
    LobbyNameTaken,
    /// The match has no artifact: not finished, none made, already pruned, or not the caller's.
    NoArtifact,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}
