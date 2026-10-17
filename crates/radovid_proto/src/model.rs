//! The records the messages carry.

use bota_proto::{HeroId, MatchStats, SlotId, Team};
use serde::{Deserialize, Serialize};

use crate::{AccountId, LobbyId, MatchId, RulesetId, SeatKey, UnixMs};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountKind {
    Human,
    Bot,
}

/// Who may sit in a lobby.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Audience {
    Everyone,
    HumansOnly,
    BotsOnly,
}

impl Audience {
    /// Whether an account of this kind fits.
    pub fn admits(self, kind: AccountKind) -> bool {
        matches!(
            (self, kind),
            (Audience::Everyone, _)
                | (Audience::HumansOnly, AccountKind::Human)
                | (Audience::BotsOnly, AccountKind::Bot)
        )
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Radiant,
    Dire,
    Draw,
    Null,
}

/// Where an account is. Exactly one holds at any moment: this is the
/// invariant "in at most one of queue, lobby, match".
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Presence {
    Idle,
    Queued { ruleset: RulesetId, for_ms: u32 },
    InLobby { lobby: LobbyId },
    InMatch { match_id: MatchId },
}

/// Glicko-2 rating and deviation on the 1500 scale; the volatility stays on
/// the server.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Rating {
    pub rating: f64,
    pub deviation: f64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct RatingSummary {
    pub rating: Rating,
    pub games: u32,
    pub wins: u32,
    pub draws: u32,
    pub losses: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct ClientInfo {
    pub bot_version: Option<String>,
}

/// The caller's own account.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AccountInfo {
    pub id: AccountId,
    pub login: String,
    pub nickname: String,
    pub kind: AccountKind,
    pub admin: bool,
}

/// Anyone's account.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PublicAccount {
    pub id: AccountId,
    pub login: String,
    pub nickname: String,
    pub kind: AccountKind,
    pub online: bool,
    pub bot_version: Option<String>,
    pub ratings: Vec<(RulesetId, RatingSummary)>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RulesetInfo {
    pub id: RulesetId,
    pub name: String,
    pub seats: u8,
    /// The hero set the ruleset allows, for display ahead of a match.
    pub heroes: Vec<HeroId>,
    /// Has a queue and a ladder; its queued matches are rated.
    pub queue: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LobbyMember {
    pub account: AccountId,
    pub login: String,
    pub nickname: String,
    pub kind: AccountKind,
    pub bot_version: Option<String>,
    pub rating: Option<Rating>,
    pub slot: SlotId,
    pub team: Team,
    pub ready: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LobbyState {
    pub id: LobbyId,
    pub name: String,
    pub ruleset: RulesetId,
    pub owner: AccountId,
    pub audience: Audience,
    /// Always false today: lobbies are unrated.
    pub rated: bool,
    pub members: Vec<LobbyMember>,
    /// Join requests awaiting the owner; slot, team and ready mean nothing there.
    pub pending: Vec<LobbyMember>,
    pub starting_in_ms: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LobbySummary {
    pub id: LobbyId,
    pub name: String,
    pub ruleset: RulesetId,
    pub owner: AccountId,
    pub owner_login: String,
    pub owner_nickname: String,
    pub audience: Audience,
    pub members: u8,
    pub seats: u8,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SeatInfo {
    pub slot: SlotId,
    pub team: Team,
    pub account: AccountId,
    pub login: String,
    pub nickname: String,
    pub kind: AccountKind,
    pub bot_version: Option<String>,
    pub rating: Option<Rating>,
}

/// Everything a client needs to take its seat in a running match.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Handoff {
    pub match_id: MatchId,
    pub ruleset: RulesetId,
    pub rated: bool,
    /// The service's own `/game` path.
    pub game_url: String,
    /// The ticket for the player and for the same account's spectators.
    pub seat_key: SeatKey,
    pub slot: SlotId,
    pub team: Team,
    /// Everyone, the caller included.
    pub seats: Vec<SeatInfo>,
    /// Fingerprint of the game server's wire.
    pub bota_wire: u64,
}

/// Why a match ended.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    /// A side won.
    Decided,
    /// Every draw: the clock ran out, or both sides lost on the same tick.
    TimeCap,
    /// With `Outcome::Null`: the game server died.
    Crashed,
    /// With `Outcome::Null`: an admin stopped the match.
    Stopped,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SeatResult {
    pub slot: SlotId,
    pub team: Team,
    pub account: AccountId,
    pub login: String,
    pub nickname: String,
    pub kind: AccountKind,
    /// From the match start; absent if the match died before it.
    pub hero: Option<HeroId>,
    /// Absent when unrated or null.
    pub before: Option<Rating>,
    pub after: Option<Rating>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MatchResult {
    pub match_id: MatchId,
    pub ruleset: RulesetId,
    pub rated: bool,
    pub outcome: Outcome,
    pub reason: EndReason,
    pub duration_ticks: u32,
    /// Absent for a null match.
    pub stats: Option<MatchStats>,
    pub seats: Vec<SeatResult>,
    pub finished_at: UnixMs,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LadderRow {
    pub rank: u32,
    pub account: AccountId,
    pub login: String,
    pub nickname: String,
    pub kind: AccountKind,
    pub bot_version: Option<String>,
    pub rating: Rating,
    pub games: u32,
    pub wins: u32,
    pub draws: u32,
    pub losses: u32,
    pub last_played: Option<UnixMs>,
}
