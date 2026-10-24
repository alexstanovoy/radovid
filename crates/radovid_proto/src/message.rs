//! The messages of `/control`.

use serde::{Deserialize, Serialize};

use crate::*;

/// A password or another secret a client types. Shown as `Secret(..)` by
/// `Debug`, so that no log line carries one.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct Secret(pub String);

impl core::fmt::Debug for Secret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Secret(..)")
    }
}

impl Secret {
    pub fn new(secret: &str) -> Secret {
        Secret(secret.to_owned())
    }
}

/// Every message the client sends: its own sequence number and the request.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ClientFrame {
    pub seq: u32,
    pub msg: ClientMsg,
}

/// What a client asks. Variant 0 is frozen.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ClientMsg {
    /// First message of a connection. `game_fingerprint` is the game wire the
    /// client speaks; rulesets of another one refuse it.
    Hello {
        proto: u16,
        game_fingerprint: u64,
        client: String,
    },
    Register {
        login: String,
        password: Secret,
        kind: AccountKind,
        client: ClientInfo,
    },
    Login {
        login: String,
        password: Secret,
        client: ClientInfo,
    },
    Resume {
        session: SessionToken,
        client: ClientInfo,
    },
    Logout,
    ChangeNickname {
        nickname: String,
    },
    ChangePassword {
        current: Secret,
        new: Secret,
    },
    /// Deletes the credentials and sessions and anonymises the history.
    DeleteAccount {
        password: Secret,
    },
    Account {
        account: AccountId,
    },
    /// Answered before login too.
    Rulesets,
    /// The limits as they apply to the caller (a human's before login), for
    /// one ruleset or in general. Answered before login too.
    Limits {
        ruleset: Option<RulesetId>,
    },
    /// Adds a queue entry; an account may hold several, up to its limit.
    QueueJoin {
        ruleset: RulesetId,
    },
    QueueLeave {
        entry: QueueEntryId,
    },
    MatchAccept {
        proposal: ProposalId,
    },
    MatchDecline {
        proposal: ProposalId,
    },
    LobbyCreate {
        ruleset: RulesetId,
        name: String,
        audience: Audience,
        password: Option<Secret>,
    },
    LobbyList,
    /// Without a password, a request the owner decides; with the right one,
    /// the caller is in at once.
    LobbyAsk {
        lobby: LobbyId,
        password: Option<Secret>,
    },
    /// As `LobbyAsk`, finding the open lobby by its name.
    LobbyJoinByName {
        name: String,
        password: Option<Secret>,
    },
    LobbyAskCancel {
        lobby: LobbyId,
    },
    LobbyDecide {
        lobby: LobbyId,
        account: AccountId,
        allow: bool,
    },
    LobbyKick {
        lobby: LobbyId,
        account: AccountId,
    },
    LobbyReady {
        lobby: LobbyId,
        ready: bool,
    },
    LobbyLeave {
        lobby: LobbyId,
    },
    /// The caller's handoff for one of its running matches.
    MatchHandoff {
        match_id: MatchId,
    },
    /// The caller's handoffs for all of its running matches.
    MyMatches,
    RecentMatches {
        account: AccountId,
        limit: u8,
    },
    Ladder {
        ruleset: RulesetId,
        offset: u32,
        limit: u8,
    },
    AdminBan {
        account: AccountId,
        reason: String,
        until: Option<UnixMs>,
    },
    AdminUnban {
        account: AccountId,
    },
    AdminSetKind {
        account: AccountId,
        kind: AccountKind,
    },
    AdminStopMatch {
        match_id: MatchId,
    },
    /// A piece of a finished match's artifact (the game's replay, kept by the service when the
    /// ruleset names one), from `offset`, for an account that sat in the match.
    MatchArtifact {
        match_id: MatchId,
        offset: u64,
    },
}

/// The answer to a request that succeeded.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Reply {
    Welcome {
        proto: u16,
        server: String,
        rulesets: Vec<RulesetInfo>,
    },
    LoggedIn {
        me: AccountInfo,
        session: SessionToken,
        activities: Activities,
    },
    Done,
    Account {
        account: PublicAccount,
    },
    Rulesets {
        list: Vec<RulesetInfo>,
    },
    Limits {
        limits: Limits,
    },
    Queued {
        entry: QueueEntry,
    },
    Lobby {
        state: LobbyState,
    },
    Lobbies {
        list: Vec<LobbySummary>,
    },
    Handoff {
        handoff: Handoff,
    },
    Handoffs {
        list: Vec<Handoff>,
    },
    Matches {
        list: Vec<MatchResult>,
    },
    LadderPage {
        total: u32,
        rows: Vec<LadderRow>,
    },
    /// Up to `MAX_ARTIFACT_CHUNK` bytes of the artifact from `offset`, and its whole length; empty
    /// `data` at the end.
    Artifact {
        match_id: MatchId,
        offset: u64,
        total: u64,
        data: Vec<u8>,
    },
}

/// Unsolicited news.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Event {
    /// A queue entry's position or the overload changed; at most one per
    /// entry every `QUEUE_STATUS_INTERVAL_MS`.
    QueueStatus {
        entry: QueueEntry,
    },
    /// Accept or decline before `accept_by_ms` runs out.
    MatchProposed {
        proposal: Proposal,
    },
    ProposalUpdate {
        proposal: ProposalId,
        accepted: u8,
        of: u8,
    },
    /// Somebody declined or stayed silent. An accepter's entry is back in its
    /// queue at its old position.
    ProposalFailed {
        proposal: ProposalId,
        back_in_queue: bool,
    },
    QueueLeft {
        entry: QueueEntryId,
        reason: QueueLeftReason,
        cooldown_ms: u32,
    },
    /// Everyone accepted; the match waits for a game slot with `ahead`
    /// matches before it.
    MatchQueued {
        match_id: MatchId,
        ahead: u32,
        overloaded: bool,
    },
    LobbyState {
        state: LobbyState,
    },
    LobbyStarting {
        lobby: LobbyId,
        in_ms: u32,
    },
    LobbyJoined {
        state: LobbyState,
    },
    LobbyDenied {
        lobby: LobbyId,
    },
    LobbyKicked {
        lobby: LobbyId,
    },
    LobbyClosed {
        lobby: LobbyId,
    },
    /// The game server is ready: connect.
    MatchFound {
        handoff: Handoff,
    },
    MatchResult {
        result: MatchResult,
    },
    /// The service is about to close this connection.
    Bye {
        error: Error,
    },
}

/// Everything the service sends. Position 1 is frozen.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ServerMsg {
    Reply { seq: u32, reply: Reply },
    Refused { seq: u32, error: Error },
    Event(Event),
}
