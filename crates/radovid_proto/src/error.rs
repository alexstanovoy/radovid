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
    /// Too many failed attempts (logins, lobby passwords); the request was not evaluated.
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
    /// The requested queue or ladder is not configured for this ruleset.
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
    /// Retired: no such join request. Never sent.
    #[doc(hidden)]
    RetiredNoSuchRequest,
    /// Retired: the caller had no join request there. Never sent.
    #[doc(hidden)]
    RetiredNoPendingRequest,
    TooManyLobbies,
    /// The lobby is counting down; nothing changes now.
    Starting,
    /// Only in `Event::Bye`: the service is shutting down.
    ServerStopping,
    Internal,
    /// The account has this many games in flight (queue entries, lobbies and unfinished
    /// matches) already.
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
    /// The match has no artifact: not finished, none made, already pruned, too large, or not the
    /// caller's.
    NoArtifact,
    /// The lobby's slot is taken by another member.
    SlotTaken,
    /// The lobby's slot does not exist.
    NoSuchSlot,
    /// This service keeps no artifact of any match (`[artifacts] save_replays` is off).
    ArtifactsNotKept,
    /// Retired: a ruleset capped the players of a lobby's team. Never sent.
    #[doc(hidden)]
    RetiredTeamTooLarge,
    /// The ruleset does not start for one account alone (`instant_solo` in its configuration).
    NoSoloStart,
    /// The lobby has a password and none was given.
    LobbyPasswordRequired,
    /// The lobby's ruleset has no cheats to turn on.
    NoCheats,
    /// Starting, waiting and running lobby/solo matches hold the global custom capacity.
    CustomMatchCapacity {
        limit: u32,
    },
    /// Activities across all /64 source keys of an IPv6 /48 have reached their cap.
    TooManyGamesFromIpv6Site {
        limit: u32,
    },
    /// Choose one or two distinct queue audiences: HumansOnly or Everyone.
    BadQueueAudiences,
    /// A nickname prefix contains characters outside [A-Za-z0-9_] or exceeds 20 bytes.
    BadLeaderboardSearch,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}
