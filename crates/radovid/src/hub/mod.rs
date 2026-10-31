//! The hub: one task that owns every piece of live state (sessions, queue
//! entries, proposals, lobbies, matches, game slots) and serves requests one
//! at a time, so no limit can be broken by a race.

mod admin;
mod limits;
mod lobby;
mod matches;
mod proposal;
mod queue;
mod slots;

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use radovid_proto::*;
use tokio::sync::{mpsc, oneshot};

use crate::config::Config;
use crate::db::{AccountRow, DELETED_NICKNAME, Db};
use crate::supervisor::End;

/// Capacity of a session's event queue; a session that falls this far behind is
/// dropped.
const EVENT_QUEUE: usize = 256;
/// How often the hub looks at queue positions and cooldowns.
const TICK: Duration = Duration::from_secs(1);

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
    Detach {
        session: SessionId,
    },
    /// The password changed: every session of the account on another token
    /// ends.
    EndOtherSessions {
        account: AccountId,
        keep: [u8; 32],
    },
    /// The account is deleted: everything it has in flight but its running
    /// matches ends, and so do its sessions.
    AccountDeleted {
        account: AccountId,
        done: oneshot::Sender<()>,
    },
    ProposalDeadline {
        proposal: ProposalId,
    },
    CountdownDone {
        lobby: LobbyId,
    },
    /// The game server printed `READY`.
    MatchReady {
        match_id: MatchId,
    },
    MatchEnded {
        match_id: MatchId,
        end: End,
    },
    /// A game server is gone and its port may be handed out again.
    PortFree {
        port: u16,
    },
    Tick,
    /// Stop the service: every match is stopped.
    Shutdown,
}

pub struct Attach {
    pub account: AccountRow,
    /// The SHA-256 of the session token it logged in with.
    pub token_hash: [u8; 32],
    /// The game wire the client named in its Hello.
    pub game_fingerprint: u64,
    /// The connection's source (an IPv4 address or an IPv6 /64).
    pub source: IpAddr,
    pub events: mpsc::Sender<ServerMsg>,
    pub reply: oneshot::Sender<Result<(SessionId, Activities), Error>>,
}

/// A handle to the hub.
#[derive(Clone)]
pub struct HubHandle {
    pub tx: mpsc::Sender<Cmd>,
}

pub fn event_channel() -> (mpsc::Sender<ServerMsg>, mpsc::Receiver<ServerMsg>) {
    mpsc::channel(EVENT_QUEUE)
}

/// What the hub keeps of an account that is online or has games in flight.
struct Account {
    info: AccountInfo,
    bot_version: Option<String>,
    sessions: Vec<SessionId>,
    entries: Vec<QueueEntryId>,
    lobbies: Vec<LobbyId>,
    /// Lobbies with a join request of the account pending.
    asking: Vec<LobbyId>,
    /// Unfinished matches: waiting for a slot, starting or running.
    matches: Vec<MatchId>,
}

impl Account {
    /// Games in flight: what the per-account limit counts.
    fn in_flight(&self) -> usize {
        self.entries.len() + self.lobbies.len() + self.asking.len() + self.matches.len()
    }

    fn is_idle(&self) -> bool {
        self.sessions.is_empty() && self.in_flight() == 0
    }
}

struct Session {
    account: AccountId,
    token_hash: [u8; 32],
    events: mpsc::Sender<ServerMsg>,
    game_fingerprint: u64,
    source: IpAddr,
}

pub struct Hub {
    config: Arc<Config>,
    db: Db,
    me: mpsc::Sender<Cmd>,
    accounts: HashMap<AccountId, Account>,
    sessions: HashMap<SessionId, Session>,
    next_session: SessionId,
    entries: HashMap<QueueEntryId, queue::Entry>,
    /// Entry ids per ruleset, oldest first.
    queues: HashMap<RulesetId, Vec<QueueEntryId>>,
    next_entry: u32,
    proposals: HashMap<ProposalId, proposal::ProposalState>,
    next_proposal: u32,
    lobbies: BTreeMap<LobbyId, lobby::Lobby>,
    next_lobby: u32,
    matches: HashMap<MatchId, matches::MatchState>,
    /// Matches waiting for a game slot, in the order they were formed.
    waiting: VecDeque<MatchId>,
    free_ports: BTreeSet<u16>,
    penalties: HashMap<AccountId, limits::Penalty>,
}

/// Starts the hub task and its clock.
/// Keeps the artifact directories within their age and size limits, now and every hour.
fn prune_artifacts(config: &Arc<Config>) {
    let config = Arc::clone(config);
    tokio::spawn(async move {
        let mut hourly = tokio::time::interval(Duration::from_secs(3600));
        loop {
            hourly.tick().await;
            let config = Arc::clone(&config);
            let _ = tokio::task::spawn_blocking(move || {
                let mut seen = Vec::new();
                for ruleset in &config.rulesets {
                    for artifact in config.artifacts_of(ruleset) {
                        let Ok(template) = crate::artifacts::Template::parse(&artifact.path) else {
                            continue;
                        };
                        if !seen.contains(&template) {
                            let age =
                                Duration::from_secs(u64::from(artifact.retention_days) * 86_400);
                            crate::artifacts::prune(&template, age, artifact.cap_bytes);
                            seen.push(template);
                        }
                    }
                }
            })
            .await;
        }
    });
}

pub fn spawn(config: Arc<Config>, db: Db) -> (HubHandle, tokio::task::JoinHandle<()>) {
    prune_artifacts(&config);
    let (tx, mut rx) = mpsc::channel(1024);
    let mut hub = Hub {
        free_ports: config.game_ports.ports().collect(),
        config,
        db,
        me: tx.clone(),
        accounts: HashMap::new(),
        sessions: HashMap::new(),
        next_session: 1,
        entries: HashMap::new(),
        queues: HashMap::new(),
        next_entry: 1,
        proposals: HashMap::new(),
        next_proposal: 1,
        lobbies: BTreeMap::new(),
        next_lobby: 1,
        matches: HashMap::new(),
        waiting: VecDeque::new(),
        penalties: HashMap::new(),
    };
    let clock = tx.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TICK);
        loop {
            tick.tick().await;
            match clock.try_send(Cmd::Tick) {
                Err(mpsc::error::TrySendError::Closed(_)) => return,
                _ => continue,
            }
        }
    });
    let task = tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            if matches!(cmd, Cmd::Shutdown) {
                hub.shutdown().await;
                // The matches report their ends; serve until they have.
                while !hub.matches.is_empty() {
                    match tokio::time::timeout(Duration::from_secs(15), rx.recv()).await {
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
            Cmd::Request {
                session,
                msg,
                reply,
            } => {
                let result = self.request(session, msg).await;
                let _ = reply.send(result);
            }
            Cmd::Detach { session } => self.detach(session),
            Cmd::EndOtherSessions { account, keep } => self.end_other_sessions(account, keep),
            Cmd::AccountDeleted { account, done } => {
                self.account_deleted(account);
                let _ = done.send(());
            }
            Cmd::ProposalDeadline { proposal } => self.proposal_deadline(proposal),
            Cmd::CountdownDone { lobby } => self.countdown_done(lobby).await,
            Cmd::MatchReady { match_id } => self.match_ready(match_id),
            Cmd::MatchEnded { match_id, end } => self.match_ended(match_id, end).await,
            Cmd::PortFree { port } => {
                self.free_ports.insert(port);
                self.try_launch();
            }
            Cmd::Tick => self.tick(),
            Cmd::Shutdown => {}
        }
    }

    fn attach(&mut self, attach: Attach) {
        let Attach {
            account,
            token_hash,
            game_fingerprint,
            source,
            events,
            reply,
        } = attach;
        let id = account.id;
        let existing = self.accounts.get(&id).map_or(0, |a| a.sessions.len());
        if existing >= MAX_SESSIONS_PER_ACCOUNT {
            let _ = reply.send(Err(Error::TooManySessions));
            return;
        }
        let session = self.next_session;
        self.next_session += 1;
        self.sessions.insert(
            session,
            Session {
                account: id,
                token_hash,
                events,
                game_fingerprint,
                source,
            },
        );
        let entry = self.accounts.entry(id).or_insert_with(|| Account {
            info: account.info(),
            bot_version: None,
            sessions: Vec::new(),
            entries: Vec::new(),
            lobbies: Vec::new(),
            asking: Vec::new(),
            matches: Vec::new(),
        });
        entry.info = account.info();
        entry.bot_version = account.bot_version.clone();
        entry.sessions.push(session);
        let activities = self.activities(id);
        let _ = reply.send(Ok((session, activities)));
    }

    /// Everything an account has in flight, as `LoggedIn` shows it.
    fn activities(&self, id: AccountId) -> Activities {
        let Some(account) = self.accounts.get(&id) else {
            return Activities::default();
        };
        let mut proposals = Vec::new();
        for entry in &account.entries {
            if let Some(proposal) = self.proposal_for(*entry) {
                proposals.push(proposal);
            }
        }
        Activities {
            queued: account
                .entries
                .iter()
                .filter_map(|e| self.entry_status(*e))
                .collect(),
            proposals,
            lobbies: account.lobbies.clone(),
            matches: account.matches.clone(),
            cooldown_ms: self.cooldown_ms(id),
            asking: account.asking.clone(),
        }
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

    /// The last control session of an account is gone: its queue entries
    /// (an open proposal counts as declined), join requests and lobbies end
    /// (but not a lobby counting down); its matches run on.
    fn account_went_away(&mut self, id: AccountId) {
        self.drop_entries(id, true);
        self.drop_lobbies(id);
        self.forget_if_idle(id);
    }

    fn drop_entries(&mut self, id: AccountId, cooldown: bool) {
        let entries = self
            .accounts
            .get(&id)
            .map(|a| a.entries.clone())
            .unwrap_or_default();
        for entry in entries {
            self.leave_queue(entry, QueueLeftReason::Cancelled, cooldown);
        }
    }

    fn drop_lobbies(&mut self, id: AccountId) {
        let (asking, lobbies) = self
            .accounts
            .get(&id)
            .map(|a| (a.asking.clone(), a.lobbies.clone()))
            .unwrap_or_default();
        for lobby in asking {
            self.cancel_ask(id, lobby);
        }
        for lobby in lobbies {
            if self
                .lobbies
                .get(&lobby)
                .is_some_and(|l| l.starting.is_none())
            {
                self.leave_lobby(id, lobby);
            }
        }
    }

    fn account_deleted(&mut self, id: AccountId) {
        self.drop_entries(id, false);
        self.drop_lobbies(id);
        if let Some(account) = self.accounts.get_mut(&id) {
            account.info.nickname = DELETED_NICKNAME.to_owned();
            account.bot_version = None;
        }
        self.bye(id, Error::BadSession);
    }

    fn forget_if_idle(&mut self, id: AccountId) {
        if self.accounts.get(&id).is_some_and(Account::is_idle) {
            self.accounts.remove(&id);
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
        let sessions = self
            .accounts
            .get(&id)
            .map(|a| a.sessions.clone())
            .unwrap_or_default();
        self.bye_sessions(sessions, error);
    }

    /// Closes every session of the account on a token other than `keep`:
    /// whoever logged in with the old password is out, not only unable to
    /// resume.
    fn end_other_sessions(&mut self, id: AccountId, keep: [u8; 32]) {
        let others = self.accounts.get(&id).map_or_else(Vec::new, |a| {
            a.sessions
                .iter()
                .copied()
                .filter(|s| self.sessions.get(s).is_some_and(|s| s.token_hash != keep))
                .collect()
        });
        self.bye_sessions(others, Error::BadSession);
    }

    fn bye_sessions(&mut self, sessions: Vec<SessionId>, error: Error) {
        for session in sessions {
            if let Some(s) = self.sessions.get(&session) {
                let _ = s.events.try_send(ServerMsg::Event(Event::Bye {
                    error: error.clone(),
                }));
            }
            self.detach(session);
        }
    }

    async fn request(&mut self, session: SessionId, msg: ClientMsg) -> Result<Reply, Error> {
        let id = self
            .sessions
            .get(&session)
            .map(|s| s.account)
            .ok_or(Error::NotLoggedIn)?;
        match msg {
            ClientMsg::Hello { .. }
            | ClientMsg::Register { .. }
            | ClientMsg::Login { .. }
            | ClientMsg::Resume { .. } => Err(Error::AlreadyLoggedIn),
            // The control task answers these itself.
            ClientMsg::Logout
            | ClientMsg::ChangePassword { .. }
            | ClientMsg::DeleteAccount { .. } => Err(Error::Internal),
            ClientMsg::ChangeNickname { nickname } => self.change_nickname(id, nickname).await,
            ClientMsg::Account { account } => self.public_account(account).await,
            ClientMsg::Rulesets => Ok(Reply::Rulesets {
                list: self.config.ruleset_infos(self.kind_of(id)),
            }),
            ClientMsg::Limits { ruleset } => self.limits(id, ruleset),
            ClientMsg::QueueJoin { ruleset } => self.queue_join(session, ruleset),
            ClientMsg::QueueLeave { entry } => self.queue_leave(id, entry),
            ClientMsg::MatchAccept { proposal } => self.match_accept(id, proposal).await,
            ClientMsg::MatchDecline { proposal } => self.match_decline(id, proposal),
            ClientMsg::MyMatches => Ok(self.my_matches(id)),
            ClientMsg::MatchHandoff { match_id } => self.match_handoff(id, match_id),
            ClientMsg::MatchArtifact { match_id, offset } => {
                self.match_artifact(id, match_id, offset).await
            }
            ClientMsg::MatchArtifactNamed {
                match_id,
                name,
                offset,
            } => self.match_artifact_named(id, match_id, name, offset).await,
            ClientMsg::RecentMatches { account, limit } => {
                self.recent_matches(account, limit).await
            }
            ClientMsg::Ladder {
                ruleset,
                offset,
                limit,
            } => self.ladder(ruleset, offset, limit).await,
            ClientMsg::AdminBan {
                account,
                reason,
                until,
            } => self.admin_ban(id, account, reason, until).await,
            ClientMsg::AdminUnban { account } => self.admin_unban(id, account).await,
            ClientMsg::AdminSetKind { account, kind } => {
                self.admin_set_kind(id, account, kind).await
            }
            ClientMsg::AdminStopMatch { match_id } => self.admin_stop(id, match_id).await,
            ClientMsg::StartSolo { ruleset } => self.start_solo(session, id, ruleset).await,
            lobby => self.lobby_request(session, id, lobby).await,
        }
    }

    fn kind_of(&self, id: AccountId) -> AccountKind {
        self.accounts
            .get(&id)
            .map_or(AccountKind::Human, |a| a.info.kind)
    }

    fn limits(&self, id: AccountId, ruleset: Option<RulesetId>) -> Result<Reply, Error> {
        let ruleset = match &ruleset {
            Some(ruleset) => Some(self.config.ruleset(ruleset).ok_or(Error::UnknownRuleset)?),
            None => None,
        };
        Ok(Reply::Limits {
            limits: self.config.limits_for(self.kind_of(id), ruleset),
        })
    }

    async fn change_nickname(&mut self, id: AccountId, nickname: String) -> Result<Reply, Error> {
        if !valid_nickname(&nickname) {
            return Err(Error::BadNickname);
        }
        self.db
            .set_nickname(id, nickname.clone())
            .await
            .map_err(internal)?;
        if let Some(account) = self.accounts.get_mut(&id) {
            account.info.nickname = nickname;
        }
        Ok(Reply::Done)
    }

    /// Once a second: queue statuses, and forgetting cooldowns that are over.
    fn tick(&mut self) {
        self.send_queue_statuses();
        self.sweep_penalties();
    }

    async fn shutdown(&mut self) {
        let waiting: Vec<MatchId> = self.waiting.drain(..).collect();
        for match_id in waiting {
            self.match_ended(match_id, End::Stopped).await;
        }
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
}

pub(crate) fn internal(error: impl std::fmt::Display) -> Error {
    eprintln!("radovid: internal error: {error}");
    Error::Internal
}

/// Milliseconds until `deadline`, for the wire.
fn millis_until(deadline: tokio::time::Instant) -> u32 {
    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
    left.as_millis().min(u128::from(u32::MAX)) as u32
}
