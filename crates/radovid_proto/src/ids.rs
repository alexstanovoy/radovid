//! Identifiers and secrets.

use serde::{Deserialize, Serialize};

/// An account, as the database numbers it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountId(pub u64);

/// A lobby, numbered per service process.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LobbyId(pub u32);

/// A match, as the database numbers it. Also the public label the game server
/// runs the match under; no randomness comes from it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MatchId(pub u64);

/// One place of an account in a queue, numbered per service process. An
/// account may hold several, one per game it wants.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueueEntryId(pub u32);

/// A pairing the players still have to accept, numbered per service process.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProposalId(pub u32);

/// A ruleset: `[a-z0-9_]`, at most [`MAX_RULESET_ID_LEN`](crate::MAX_RULESET_ID_LEN)
/// bytes. Never reused for other rules.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RulesetId(pub String);

/// What `Resume` presents instead of a password. Compared only as a hash.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct SessionToken(pub [u8; 32]);

impl core::fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SessionToken(..)")
    }
}

/// The credential of one seat of one match: 32 random bytes the service gives
/// only to the game server it starts (by environment) and to the seat's
/// account (in its `Handoff`). The game checks it, in constant time; radovid
/// never does.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct Ticket(pub [u8; 32]);

impl core::fmt::Debug for Ticket {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Ticket(..)")
    }
}

impl Ticket {
    /// 64 lower-case hex digits: how a launcher receives it.
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// A stored moment, in milliseconds since the Unix epoch. Used only for moments
/// that outlive a connection; everything the client must act on is relative.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnixMs(pub u64);
