//! The messages of `/control`.

use serde::{Deserialize, Serialize};

use crate::*;

/// Every message the client sends: its own sequence number and the request.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ClientFrame {
    pub seq: u32,
    pub msg: ClientMsg,
}

/// What a client asks. Variant 0 is frozen.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ClientMsg {
    /// First message of a connection.
    Hello {
        proto: u16,
        bota_wire: u64,
        client: String,
    },
    Register {
        login: String,
        password: String,
        kind: AccountKind,
        client: ClientInfo,
    },
    Login {
        login: String,
        password: String,
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
        current: String,
        new: String,
    },
    Account {
        account: AccountId,
    },
    Rulesets,
    QueueJoin {
        ruleset: RulesetId,
    },
    QueueLeave,
    LobbyCreate {
        ruleset: RulesetId,
        name: String,
        audience: Audience,
    },
    LobbyList,
    LobbyAsk {
        lobby: LobbyId,
    },
    LobbyAskCancel,
    LobbyDecide {
        account: AccountId,
        allow: bool,
    },
    LobbyKick {
        account: AccountId,
    },
    LobbyReady {
        ready: bool,
    },
    LobbyLeave,
    MatchHandoff,
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
}

/// The answer to a request that succeeded.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Reply {
    Welcome {
        proto: u16,
        bota_wire: u64,
        server: String,
    },
    LoggedIn {
        me: AccountInfo,
        session: SessionToken,
        presence: Presence,
    },
    Done,
    Account {
        account: PublicAccount,
    },
    Rulesets {
        list: Vec<RulesetInfo>,
    },
    Queued {
        waiting: u32,
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
    Matches {
        list: Vec<MatchResult>,
    },
    LadderPage {
        total: u32,
        rows: Vec<LadderRow>,
    },
}

/// Unsolicited news.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Event {
    LobbyState {
        state: LobbyState,
    },
    LobbyStarting {
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
