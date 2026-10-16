//! Identifiers.

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

/// A ruleset: `[a-z0-9_]`, at most [`MAX_RULESET_ID_LEN`](crate::MAX_RULESET_ID_LEN)
/// bytes, spelled as in the game server's `--rules`.
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

/// A stored moment, in milliseconds since the Unix epoch. Used only for moments
/// that outlive a connection; everything the client must act on is relative.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnixMs(pub u64);
