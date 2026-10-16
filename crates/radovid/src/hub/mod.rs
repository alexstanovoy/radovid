//! The hub: one task that owns every piece of live state (sessions, presence,
//! queues, lobbies, running matches) and serves requests one at a time, so the
//! invariant "an account is in at most one of queue, lobby, match" cannot be
//! broken by a race.

mod admin;
mod lobby;
mod matches;
mod queue;

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use bota_proto::{HeroId, SeatKey, SlotId, Team};
use radovid_proto::*;
use tokio::sync::{Semaphore, mpsc, oneshot};

use crate::backend::Backend;
use crate::config::Config;
use crate::db::{AccountRow, Db};
use crate::supervisor::End;

pub use matches::GameRoute;

/// Capacity of a session's event queue; a session that falls this far behind is
/// dropped.
const EVENT_QUEUE: usize = 256;

pub type SessionId = u64;

/// Everything that reaches the hub.
pub enum Cmd {
    /// A connection logged in.
    Attach(Box<Attach>),
    Request {
        session: SessionId,
        msg: ClientMsg,
        reply: oneshot::Sender<Result<Reply, Error>>,
    },
    /// A control connection closed.
    Detach { session: SessionId },
    /// A game connection wants the container behind its key.
    GameRoute {
        key: SeatKey,
        spectator: bool,
        reply: oneshot::Sender<Option<GameRoute>>,
    },
    GameClosed { key: SeatKey, spectator: bool },
    MatchUp { match_id: MatchId, addr: SocketAddr },
    MatchStarted { match_id: MatchId, picks: Vec<(SlotId, HeroId)> },
    MatchEnded { match_id: MatchId, end: End },
    CountdownDone { lobby: LobbyId },
    /// Stop the service: every running match is stopped.
    Shutdown,
}

pub struct Attach {
    pub account: AccountRow,
    pub bota_wire: u64,
    pub events: mpsc::Sender<ServerMsg>,
    pub reply: oneshot::Sender<Result<(SessionId, Presence), Error>>,
}

/// A handle to the hub.
#[derive(Clone)]
pub struct HubHandle {
    pub tx: mpsc::Sender<Cmd>,
}

pub fn event_channel() -> (mpsc::Sender<ServerMsg>, mpsc::Receiver<ServerMsg>) {
    mpsc::channel(EVENT_QUEUE)
}

/// Where an account is.
#[derive(Clone, Debug)]
enum Where {
    Idle,
    Queued { ruleset: RulesetId, since: Instant },
    InLobby(LobbyId),
    InMatch(MatchId),
}

struct Account {
    info: AccountInfo,
    bot_version: Option<String>,
    at: Where,
    sessions: Vec<SessionId>,
    /// The lobby a join request is pending in.
    asking: Option<LobbyId>,
}

struct Session {
    account: AccountId,
    events: mpsc::Sender<ServerMsg>,
    bota_wire: u64,
}

pub struct Hub {
    config: Arc<Config>,
    db: Db,
    backend: Arc<dyn Backend>,
    me: mpsc::Sender<Cmd>,
    slots: Arc<Semaphore>,
    accounts: HashMap<AccountId, Account>,
    sessions: HashMap<SessionId, Session>,
    next_session: SessionId,
    queues: HashMap<RulesetId, Vec<(AccountId, Instant)>>,
    lobbies: BTreeMap<LobbyId, lobby::Lobby>,
    next_lobby: u32,
    matches: HashMap<MatchId, matches::MatchState>,
    routes: HashMap<[u8; 32], matches::Route>,
}

/// Starts the hub task.
pub fn spawn(config: Arc<Config>, db: Db, backend: Arc<dyn Backend>) -> (HubHandle, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel(1024);
    let mut hub = Hub {
        slots: Arc::new(Semaphore::new(config.max_matches)),
        config,
        db,
        backend,
        me: tx.clone(),
        accounts: HashMap::new(),
        sessions: HashMap::new(),
        next_session: 1,
        queues: HashMap::new(),
        lobbies: BTreeMap::new(),
        next_lobby: 1,
        matches: HashMap::new(),
        routes: HashMap::new(),
    };
    let task = tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            if matches!(cmd, Cmd::Shutdown) {
                hub.shutdown();
                // The matches report their ends; serve until they have.
                while !hub.matches.is_empty() {
                    match tokio::time::timeout(std::time::Duration::from_secs(15), rx.recv()).await {
                        Ok(Some(cmd)) => hub.handle(cmd).await,
                        _ => break,
                    }
                }
                return;
            }
            hub.handle(cmd).await;
        }
    });
    (HubHandle { tx }, task)
}

impl Hub {
    async fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Attach(attach) => self.attach(*attach),
            Cmd::Request { session, msg, reply } => {
                let result = self.request(session, msg).await;
                let _ = reply.send(result);
            }
            Cmd::Detach { session } => self.detach(session),
            Cmd::GameRoute { key, spectator, reply } => {
                let _ = reply.send(self.route(&key, spectator));
            }
            Cmd::GameClosed { key, spectator } => self.release_route(&key, spectator),
            Cmd::MatchUp { match_id, addr } => self.match_up(match_id, addr),
            Cmd::MatchStarted { match_id, picks } => self.match_started(match_id, picks),
            Cmd::MatchEnded { match_id, end } => self.match_ended(match_id, end).await,
            Cmd::CountdownDone { lobby } => self.countdown_done(lobby),
            Cmd::Shutdown => {}
        }
    }

    fn attach(&mut self, attach: Attach) {
        let Attach { account, bota_wire, events, reply } = attach;
        let id = account.id;
        let existing = self.accounts.get(&id).map_or(0, |a| a.sessions.len());
        if existing >= MAX_SESSIONS_PER_ACCOUNT {
            let _ = reply.send(Err(Error::TooManySessions));
            return;
        }
        let session = self.next_session;
        self.next_session += 1;
        self.sessions.insert(session, Session { account: id, events, bota_wire });
        let entry = self.accounts.entry(id).or_insert_with(|| Account {
            info: account.info(),
            bot_version: None,
            at: Where::Idle,
            sessions: Vec::new(),
            asking: None,
        });
        entry.info = account.info();
        entry.bot_version = account.bot_version.clone();
        entry.sessions.push(session);
        let presence = self.presence_of(id);
        let _ = reply.send(Ok((session, presence)));
    }

    fn detach(&mut self, session: SessionId) {
        let Some(gone) = self.sessions.remove(&session) else {
            return;
        };
        let id = gone.account;
        let Some(account) = self.accounts.get_mut(&id) else {
            return;
        };
        account.sessions.retain(|s| *s != session);
        if account.sessions.is_empty() {
            self.account_went_away(id);
        }
    }

    /// The last control session of an account is gone (3.3 of the design).
    fn account_went_away(&mut self, id: AccountId) {
        self.cancel_ask(id);
        match self.accounts.get(&id).map(|a| a.at.clone()) {
            Some(Where::Queued { ruleset, .. }) => self.leave_queue(id, &ruleset),
            Some(Where::InLobby(lobby)) => {
                let counting = self.lobbies.get(&lobby).is_some_and(|l| l.starting.is_some());
                if !counting {
                    self.leave_lobby(id, lobby);
                }
            }
            _ => {}
        }
        self.forget_if_idle(id);
    }

    fn forget_if_idle(&mut self, id: AccountId) {
        if self
            .accounts
            .get(&id)
            .is_some_and(|a| a.sessions.is_empty() && matches!(a.at, Where::Idle))
        {
            self.accounts.remove(&id);
        }
    }

    pub(crate) fn presence_of(&self, id: AccountId) -> Presence {
        match self.accounts.get(&id).map(|a| &a.at) {
            None | Some(Where::Idle) => Presence::Idle,
            Some(Where::Queued { ruleset, since }) => Presence::Queued {
                ruleset: ruleset.clone(),
                for_ms: since.elapsed().as_millis().min(u128::from(u32::MAX)) as u32,
            },
            Some(Where::InLobby(lobby)) => Presence::InLobby { lobby: *lobby },
            Some(Where::InMatch(match_id)) => Presence::InMatch { match_id: *match_id },
        }
    }

    /// Refuses unless the account is idle.
    fn require_idle(&self, id: AccountId) -> Result<(), Error> {
        match self.presence_of(id) {
            Presence::Idle => Ok(()),
            presence => Err(Error::Busy { presence }),
        }
    }

    /// Sends an event to every session of an account. A session whose queue is
    /// full is dropped, which closes its connection.
    fn notify(&mut self, id: AccountId, event: Event) {
        let Some(account) = self.accounts.get(&id) else {
            return;
        };
        let mut dead = Vec::new();
        for session in &account.sessions {
            if let Some(s) = self.sessions.get(session)
                && s.events.try_send(ServerMsg::Event(event.clone())).is_err()
            {
                dead.push(*session);
            }
        }
        for session in dead {
            self.detach(session);
        }
    }

    /// Closes every session of an account after telling it why.
    fn bye(&mut self, id: AccountId, error: Error) {
        let sessions = self.accounts.get(&id).map(|a| a.sessions.clone()).unwrap_or_default();
        for session in sessions {
            if let Some(s) = self.sessions.get(&session) {
                let _ = s.events.try_send(ServerMsg::Event(Event::Bye { error: error.clone() }));
            }
            self.detach(session);
        }
    }

    async fn request(&mut self, session: SessionId, msg: ClientMsg) -> Result<Reply, Error> {
        let (id, wire) = {
            let s = self.sessions.get(&session).ok_or(Error::NotLoggedIn)?;
            (s.account, s.bota_wire)
        };
        match msg {
            ClientMsg::Hello { .. }
            | ClientMsg::Register { .. }
            | ClientMsg::Login { .. }
            | ClientMsg::Resume { .. } => Err(Error::AlreadyLoggedIn),
            // The control task answers these itself.
            ClientMsg::Logout | ClientMsg::ChangePassword { .. } => Err(Error::Internal),
            ClientMsg::ChangeNickname { nickname } => self.change_nickname(id, nickname).await,
            ClientMsg::Account { account } => self.public_account(account).await,
            ClientMsg::Rulesets => Ok(Reply::Rulesets {
                list: self.config.rulesets.iter().map(|r| r.info()).collect(),
            }),
            ClientMsg::QueueJoin { ruleset } => self.queue_join(id, wire, ruleset).await,
            ClientMsg::QueueLeave => self.queue_leave(id),
            ClientMsg::LobbyCreate { ruleset, name, audience } => {
                self.lobby_create(id, wire, ruleset, name, audience).await
            }
            ClientMsg::LobbyList => Ok(self.lobby_list()),
            ClientMsg::LobbyAsk { lobby } => self.lobby_ask(id, wire, lobby),
            ClientMsg::LobbyAskCancel => self.lobby_ask_cancel(id),
            ClientMsg::LobbyDecide { account, allow } => self.lobby_decide(id, account, allow).await,
            ClientMsg::LobbyKick { account } => self.lobby_kick(id, account),
            ClientMsg::LobbyReady { ready } => self.lobby_ready(id, ready).await,
            ClientMsg::LobbyLeave => self.lobby_leave(id),
            ClientMsg::MatchHandoff => self.match_handoff(id),
            ClientMsg::RecentMatches { account, limit } => self.recent_matches(account, limit).await,
            ClientMsg::Ladder { ruleset, offset, limit } => self.ladder(ruleset, offset, limit).await,
            ClientMsg::AdminBan { account, reason, until } => self.admin_ban(id, account, reason, until).await,
            ClientMsg::AdminUnban { account } => self.admin_unban(id, account).await,
            ClientMsg::AdminSetKind { account, kind } => self.admin_set_kind(id, account, kind).await,
            ClientMsg::AdminStopMatch { match_id } => self.admin_stop(id, match_id),
        }
    }

    async fn change_nickname(&mut self, id: AccountId, nickname: String) -> Result<Reply, Error> {
        if !valid_nickname(&nickname) {
            return Err(Error::BadNickname);
        }
        self.db.set_nickname(id, nickname.clone()).await.map_err(internal)?;
        if let Some(account) = self.accounts.get_mut(&id) {
            account.info.nickname = nickname;
        }
        Ok(Reply::Done)
    }

    fn shutdown(&mut self) {
        for state in self.matches.values_mut() {
            if let Some(stop) = state.stop.take() {
                let _ = stop.send(());
            }
        }
        let ids: Vec<AccountId> = self.accounts.keys().copied().collect();
        for id in ids {
            self.bye(id, Error::ServerStopping);
        }
    }

    pub(crate) fn team_of_slot(slot: u8) -> Team {
        if slot % 2 == 0 { Team::Radiant } else { Team::Dire }
    }
}

pub(crate) fn internal(error: impl std::fmt::Display) -> Error {
    eprintln!("radovid: internal error: {error}");
    Error::Internal
}
