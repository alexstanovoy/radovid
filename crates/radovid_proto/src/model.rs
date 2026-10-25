//! The records the messages carry. Another account is always shown by
//! nickname and id; a login is a credential and only its owner sees it.

use serde::{Deserialize, Serialize};

use crate::{AccountId, LobbyId, MatchId, ProposalId, QueueEntryId, RulesetId, Ticket, UnixMs};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    /// The team of this index won (teams as the ruleset numbers them).
    Winner {
        team: u8,
    },
    Draw,
    /// Nothing counts: nobody came, the game crashed or stopped.
    Null,
}

/// Why a match ended.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    /// A team won.
    Decided,
    Draw,
    /// With `Outcome::Null`: nobody came, or everybody left (the game's exit code 3).
    Abandoned,
    /// With `Outcome::Null`: the game server failed, or ended without a result.
    Crashed,
    /// With `Outcome::Null`: an admin, the time limit or the service's shutdown stopped it.
    Stopped,
    /// A team won because a player on the other left the match for good (the game's `RESULT`
    /// names the leaver's seat): a rated loss for the leaver.
    Left,
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
    pub nickname: String,
    pub kind: AccountKind,
    pub online: bool,
    pub bot_version: Option<String>,
    pub ratings: Vec<(RulesetId, RatingSummary)>,
}

/// One entry of the service's catalog.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RulesetInfo {
    pub id: RulesetId,
    pub name: String,
    /// The game's label; radovid never interprets it.
    pub game: String,
    /// The game wire a client must name in its `Hello` to queue or sit in a
    /// lobby here.
    pub game_fingerprint: u64,
    /// Team sizes; slots are numbered team by team.
    pub teams: Vec<u8>,
    /// Has a queue and a ladder; its queued matches are rated.
    pub rated: bool,
    /// The game's own description of the rules, opaque to radovid.
    pub info: Vec<u8>,
    /// Games in flight an account of the caller's kind may have to queue or
    /// sit here (a human's before login).
    pub games_per_account: u8,
    /// Who may queue and sit here; the service lists a ruleset to the kinds it admits only.
    pub audience: Audience,
    /// Players a lobby of it needs before its host may start it: the seats, unless the
    /// service lets a lobby start with fewer: the seats nobody holds stay empty.
    pub lobby_min_players: u8,
}

/// What bounds an account's play, as it applies to the caller.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Games in flight at once: queue entries, lobbies, join requests and
    /// unfinished matches.
    pub games_per_account: u8,
    /// Milliseconds to accept a proposal; silence counts as a decline.
    pub accept_timeout_ms: u32,
    /// Milliseconds out of the queues after a decline or a silence.
    pub decline_cooldown_ms: u32,
    /// Queue entries across every ruleset the service takes.
    pub queue_capacity: u32,
    /// Games the service runs at once; accepted matches wait for a slot.
    pub game_capacity: u32,
    /// Of those, the most lobby games may hold.
    pub lobby_games_max: u32,
    /// Games in flight of all accounts of one network at once.
    pub games_per_source: u8,
}

/// One queue entry and where it stands.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct QueueEntry {
    pub entry: QueueEntryId,
    pub ruleset: RulesetId,
    /// Milliseconds since it joined.
    pub for_ms: u32,
    /// 1 for the oldest entry of the ruleset's queue.
    pub position: u32,
    /// Entries in the ruleset's queue.
    pub waiting: u32,
    /// Every game slot is taken and enough matches wait for one: no pairing
    /// until a slot frees.
    pub overloaded: bool,
    /// Accepted matches waiting for a game slot.
    pub games_waiting: u32,
}

/// A pairing waiting for its players' answers. The opponents are not named.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    pub proposal: ProposalId,
    /// The caller's queue entry it would consume.
    pub entry: QueueEntryId,
    pub ruleset: RulesetId,
    /// Milliseconds left to answer.
    pub accept_by_ms: u32,
    pub accepted: u8,
    pub of: u8,
}

/// Everything an account has in flight.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct Activities {
    pub queued: Vec<QueueEntry>,
    pub proposals: Vec<Proposal>,
    pub lobbies: Vec<LobbyId>,
    /// Unfinished matches: waiting for a slot, starting or running.
    pub matches: Vec<MatchId>,
    /// Milliseconds before the account may queue again.
    pub cooldown_ms: u32,
    /// Lobbies with a join request of the account pending.
    pub asking: Vec<LobbyId>,
}

/// Why a queue entry is gone without a match.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueLeftReason {
    Cancelled,
    Declined,
    TimedOut,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SeatInfo {
    pub slot: u8,
    pub team: u8,
    pub account: AccountId,
    pub nickname: String,
    pub kind: AccountKind,
    pub bot_version: Option<String>,
    pub rating: Option<Rating>,
}

/// Everything a client needs to take its seat in a running match: where the
/// game server is and the seat's ticket. What the client then speaks there is
/// the game's business.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Handoff {
    pub match_id: MatchId,
    pub ruleset: RulesetId,
    pub rated: bool,
    pub game: String,
    /// The game server's own address, e.g. `wss://game.example.org:20003`.
    pub game_url: String,
    /// For the player, and for the same account's spectators.
    pub ticket: Ticket,
    pub slot: u8,
    pub team: u8,
    /// Everyone, the caller included.
    pub seats: Vec<SeatInfo>,
    pub game_fingerprint: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SeatResult {
    pub slot: u8,
    pub team: u8,
    pub account: AccountId,
    pub nickname: String,
    pub kind: AccountKind,
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
    /// As the game reported it; 0 without a result.
    pub duration_ms: u32,
    /// The game's own report, untouched; empty without a result.
    pub game_report: Vec<u8>,
    pub seats: Vec<SeatResult>,
    pub finished_at: UnixMs,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LobbyMember {
    pub account: AccountId,
    pub nickname: String,
    pub kind: AccountKind,
    pub bot_version: Option<String>,
    pub rating: Option<Rating>,
    pub slot: u8,
    pub team: u8,
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
    /// Has a password: whoever knows it joins without asking.
    pub locked: bool,
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
    pub owner_nickname: String,
    pub audience: Audience,
    pub locked: bool,
    pub members: u8,
    pub seats: u8,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LadderRow {
    pub rank: u32,
    pub account: AccountId,
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
