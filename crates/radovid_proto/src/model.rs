//! The records the messages carry. Another account is always shown by
//! nickname and id; a login is a credential and only its owner sees it.

use serde::{Deserialize, Serialize};

use crate::{
    AccountId, LobbyId, MAX_LEADERBOARD_ROWS, MAX_RATED_MODES, MatchId, ProposalId, QueueEntryId,
    RulesetId, Ticket, UnixMs,
};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccountKind {
    Human,
    Bot,
}

/// Who may sit in a lobby or enter a queue. Queues support Everyone and HumansOnly.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    /// Position in this account's Humans/Bots ladder when queried.
    /// Profiles keep the mode summary with None when its rank work budget is exhausted.
    pub rank: Option<u32>,
}

/// The largest rating spread each waiting player accepts, in rating points.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RatingWindow {
    pub initial: u32,
    pub widen_per_minute: u32,
    pub maximum: u32,
}

impl RatingWindow {
    /// Saturates at the configured maximum, including for untrusted wire values.
    pub fn after_ms(self, waiting_ms: u32) -> u32 {
        let growth = u64::from(self.widen_per_minute) * u64::from(waiting_ms) / 60_000;
        (u64::from(self.initial) + growth).min(u64::from(self.maximum)) as u32
    }
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
    #[serde(deserialize_with = "profile_ratings")]
    pub ratings: Vec<(RulesetId, RatingSummary)>,
    pub avatar: Option<AvatarVersion>,
}

/// Content hash; cache keys also include the service and account.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AvatarVersion(pub [u8; 32]);

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Avatar {
    pub version: AvatarVersion,
    #[serde(deserialize_with = "avatar_png")]
    pub png: Vec<u8>,
}

/// The version actually cached by a caller; None requests any current image.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct AvatarQuery {pub account: AccountId,pub version:Option<AvatarVersion>}

/// A changed image, or an absent/hidden image whose cached copy must be cleared.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AvatarChange {pub account:AccountId,pub avatar:Option<Avatar>}

pub fn valid_avatar_batch(accounts:&[AvatarQuery])->bool {
    if accounts.is_empty() || accounts.len()>crate::MAX_AVATAR_BATCH {return false;}
    let mut seen=std::collections::BTreeSet::new();
    accounts.iter().all(|query|query.account.0>0 && query.account.0<=i64::MAX as u64 && seen.insert(query.account.0))
}

pub(crate) fn avatar_queries<'de,D:serde::Deserializer<'de>>(deserializer:D)->Result<Vec<AvatarQuery>,D::Error> {
    bounded_list::<D,AvatarQuery,{crate::MAX_AVATAR_BATCH}>(deserializer)
}
pub(crate) fn avatar_changes<'de,D:serde::Deserializer<'de>>(deserializer:D)->Result<Vec<AvatarChange>,D::Error> {
    bounded_list::<D,AvatarChange,{crate::MAX_AVATAR_BATCH}>(deserializer)
}

pub(crate) fn avatar_png<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<u8>, D::Error> {
    bounded_list::<D, u8, { crate::MAX_AVATAR_BYTES }>(deserializer)
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
    /// Owns a ladder; its queued matches are rated.
    pub rated: bool,
    /// The game's own description of the rules, opaque to radovid.
    pub info: Vec<u8>,
    /// Retired numeric limit; its wire position is reserved and always zero.
    #[doc(hidden)]
    pub retired_games_per_account: u8,
    /// Who may queue and sit here; the service lists a ruleset to the kinds it admits only.
    pub audience: Audience,
    /// Players a lobby of it needs before its host may start it: the seats, unless the
    /// service lets a lobby start with fewer: the seats nobody holds stay empty.
    pub lobby_min_players: u8,
    /// The ruleset a new lobby is of unless its host chooses another (the service's
    /// `lobby_ruleset`); one ruleset of the catalog at most.
    pub lobby_default: bool,
    /// Games in flight for this ruleset, as they apply to the caller.
    pub games_per_account: AccountGameLimit,
    /// An unrated queue uses this rated sibling's ladder for matchmaking only.
    pub rating_ruleset: Option<RulesetId>,
    /// None for a ruleset offered only through lobbies or solo starts.
    pub rating_window: Option<RatingWindow>,
}

/// An explicit per-account cap. No numeric value is repurposed as unlimited.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountGameLimit {
    Limited(u8),
    Unlimited,
}

/// What bounds an account's play, as it applies to the caller.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Retired numeric limit; its wire position is reserved and always zero.
    #[doc(hidden)]
    pub retired_games_per_account: u8,
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
    /// Games in flight at once: queue entries, lobbies and unfinished matches.
    pub games_per_account: AccountGameLimit,
    /// Starting, waiting and running lobby/solo matches, including countdowns.
    pub custom_match_capacity: u32,
    /// Activities across all /64 sources in one IPv6 /48.
    pub games_per_ipv6_48: u32,
    pub ranked_window: RatingWindow,
    pub unranked_window: RatingWindow,
    /// Rows returned by the leaderboard endpoint; independent of the legacy Ladder page.
    pub leaderboard_size: u16,
}

/// A mode's Humans or Bots ladder. Search results retain their unfiltered ranks.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LeaderboardPage {
    pub ruleset: RulesetId,
    pub kind: AccountKind,
    pub prefix: String,
    pub limit: u16,
    /// Rows matching the prefix, before the response limit.
    pub total: u32,
    #[serde(deserialize_with = "leaderboard_rows")]
    pub rows: Vec<LadderRow>,
    /// The requester's row in their own category, even when viewing the other category.
    /// None means the requester has not played rated games in this mode.
    pub own: Option<LadderRow>,
}

fn leaderboard_rows<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<LadderRow>, D::Error> {
    bounded_list::<D, LadderRow, { MAX_LEADERBOARD_ROWS as usize }>(deserializer)
}

fn profile_ratings<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<(RulesetId, RatingSummary)>, D::Error> {
    bounded_list::<D, (RulesetId, RatingSummary), { MAX_RATED_MODES }>(deserializer)
}

fn bounded_list<'de, D, T, const MAX: usize>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    struct Entries<T, const MAX: usize>(std::marker::PhantomData<T>);
    impl<'de, T: serde::Deserialize<'de>, const MAX: usize> serde::de::Visitor<'de>
        for Entries<T, MAX>
    {
        type Value = Vec<T>;
        fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(formatter, "at most {MAX} entries")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> Result<Self::Value, A::Error> {
            if sequence.size_hint().is_some_and(|size| size > MAX) {
                return Err(serde::de::Error::custom("too many entries"));
            }
            let mut entries = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(MAX));
            while let Some(entry) = sequence.next_element()? {
                if entries.len() == MAX {
                    return Err(serde::de::Error::custom("too many entries"));
                }
                entries.push(entry);
            }
            Ok(entries)
        }
    }
    deserializer.deserialize_seq(Entries::<T, MAX>(std::marker::PhantomData))
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
    /// The audience of this queue: HumansOnly or Everyone.
    pub audience: Audience,
    /// The first entry of this linked search. Leaving either entry cancels that search.
    pub search: QueueEntryId,
    /// This search's rating window has reached its configured maximum.
    /// It does not imply a compatible opponent exists or bound the remaining wait.
    pub window_capped: bool,
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
    pub audience: Audience,
    pub search: QueueEntryId,
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
    /// Retired: the lobbies the account asked to join. Kept with its type, so that the fields
    /// after it stay where they were on the wire; always empty.
    #[doc(hidden)]
    pub retired_asking: Vec<LobbyId>,
}

/// Why a queue entry ended.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueLeftReason {
    Cancelled,
    Declined,
    TimedOut,
    /// This linked search was consumed by an accepted match.
    Matched,
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
    /// Has a password: whoever knows it joins.
    pub locked: bool,
    pub members: Vec<LobbyMember>,
    /// Retired: the join requests awaiting the owner. Kept with its type, so that the fields
    /// after it stay where they were on the wire; always empty.
    #[doc(hidden)]
    pub retired_pending: Vec<LobbyMember>,
    pub starting_in_ms: Option<u32>,
    /// The lobby's slots: the size of each team, slots numbered team by team. The service's, the
    /// same for every ruleset.
    pub teams: Vec<u8>,
    /// The game's cheats in the lobby's match: none when its ruleset has none, else whether the
    /// owner turned them on (`LobbyCheats`). Every member's client reads it: none older than
    /// version 12 is served.
    pub cheats: Option<bool>,
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
    pub avatar: Option<AvatarVersion>,
}
