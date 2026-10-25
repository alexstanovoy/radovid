//! Lobbies: custom unrated games with an owner, a unique name, an optional
//! password (whoever knows it is in at once), join requests, readiness and a
//! countdown nobody can cancel. An account may sit in several.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use radovid_proto::*;
use tokio::time::Instant;

use super::{Cmd, Hub, SessionId, internal, millis_until};

pub(super) struct Member {
    pub account: AccountId,
    pub slot: u8,
    pub ready: bool,
    pub rating: Option<Rating>,
    pub source: IpAddr,
}

pub(super) struct Asker {
    pub account: AccountId,
    pub source: IpAddr,
}

pub(super) struct Lobby {
    pub id: LobbyId,
    pub name: String,
    pub ruleset: RulesetId,
    /// The team of each slot.
    pub teams: Vec<u8>,
    pub owner: AccountId,
    pub audience: Audience,
    pub password: Option<Secret>,
    /// Sorted by slot.
    pub members: Vec<Member>,
    /// Join requests, oldest first.
    pub pending: Vec<Asker>,
    pub starting: Option<Instant>,
}

impl Lobby {
    fn lowest_free_slot(&self) -> Option<u8> {
        (0..self.teams.len() as u8).find(|slot| self.members.iter().all(|m| m.slot != *slot))
    }

    fn is_full(&self) -> bool {
        self.members.len() >= self.teams.len()
    }

    fn holds(&self, id: AccountId) -> bool {
        self.members.iter().any(|m| m.account == id)
    }
}

impl Hub {
    pub(super) async fn lobby_request(
        &mut self,
        session: SessionId,
        id: AccountId,
        msg: ClientMsg,
    ) -> Result<Reply, Error> {
        match msg {
            ClientMsg::LobbyCreate {
                ruleset,
                name,
                audience,
                password,
            } => {
                self.lobby_create(session, ruleset, name, audience, password)
                    .await
            }
            ClientMsg::LobbyList => Ok(self.lobby_list()),
            ClientMsg::LobbyAsk { lobby, password } => {
                self.lobby_ask(session, lobby, password).await
            }
            ClientMsg::LobbyJoinByName { name, password } => {
                let lobby = self
                    .lobbies
                    .values()
                    .find(|l| l.name.eq_ignore_ascii_case(&name))
                    .map(|l| l.id)
                    .ok_or(Error::UnknownLobby)?;
                self.lobby_ask(session, lobby, password).await
            }
            ClientMsg::LobbyAskCancel { lobby } => {
                if !self
                    .accounts
                    .get(&id)
                    .is_some_and(|a| a.asking.contains(&lobby))
                {
                    return Err(Error::NoPendingRequest);
                }
                self.cancel_ask(id, lobby);
                Ok(Reply::Done)
            }
            ClientMsg::LobbyDecide {
                lobby,
                account,
                allow,
            } => self.lobby_decide(id, lobby, account, allow).await,
            ClientMsg::LobbyKick { lobby, account } => self.lobby_kick(id, lobby, account),
            ClientMsg::LobbyReady { lobby, ready } => self.lobby_ready(id, lobby, ready),
            ClientMsg::LobbyLeave { lobby } => {
                self.member_of(id, lobby)?;
                self.not_starting(lobby)?;
                self.leave_lobby(id, lobby);
                Ok(Reply::Done)
            }
            _ => Err(Error::Internal),
        }
    }

    fn member_view(
        &self,
        account: AccountId,
        slot: u8,
        ready: bool,
        rating: Option<Rating>,
        team: u8,
    ) -> LobbyMember {
        let (nickname, kind, bot_version) = self.accounts.get(&account).map_or_else(
            || (String::new(), AccountKind::Human, None),
            |a| (a.info.nickname.clone(), a.info.kind, a.bot_version.clone()),
        );
        LobbyMember {
            account,
            nickname,
            kind,
            bot_version: (kind == AccountKind::Bot).then_some(bot_version).flatten(),
            rating,
            slot,
            team,
            ready,
        }
    }

    fn lobby_state(&self, lobby: &Lobby) -> LobbyState {
        LobbyState {
            id: lobby.id,
            name: lobby.name.clone(),
            ruleset: lobby.ruleset.clone(),
            owner: lobby.owner,
            audience: lobby.audience,
            rated: false,
            locked: lobby.password.is_some(),
            members: lobby
                .members
                .iter()
                .map(|m| {
                    let team = lobby.teams.get(usize::from(m.slot)).copied().unwrap_or(0);
                    self.member_view(m.account, m.slot, m.ready, m.rating, team)
                })
                .collect(),
            pending: lobby
                .pending
                .iter()
                .map(|a| self.member_view(a.account, 0, false, None, 0))
                .collect(),
            starting_in_ms: lobby.starting.map(millis_until),
        }
    }

    /// Sends the whole lobby state to everyone in it but `except`.
    fn broadcast_lobby(&mut self, lobby: LobbyId, except: Option<AccountId>) {
        let Some(l) = self.lobbies.get(&lobby) else {
            return;
        };
        let state = self.lobby_state(l);
        let members: Vec<AccountId> = l
            .members
            .iter()
            .map(|m| m.account)
            .filter(|m| Some(*m) != except)
            .collect();
        for member in members {
            self.notify(
                member,
                Event::LobbyState {
                    state: state.clone(),
                },
            );
        }
    }

    async fn lobby_create(
        &mut self,
        session: SessionId,
        ruleset: RulesetId,
        name: String,
        audience: Audience,
        password: Option<Secret>,
    ) -> Result<Reply, Error> {
        let config = Arc::clone(&self.config);
        let rules = config.ruleset(&ruleset).ok_or(Error::UnknownRuleset)?;
        if !valid_lobby_name(&name) {
            return Err(Error::BadLobbyName);
        }
        if password
            .as_ref()
            .is_some_and(|p| !valid_lobby_password(&p.0))
        {
            return Err(Error::BadLobbyPassword);
        }
        self.admit_game(session, rules)?;
        let (id, source) = self
            .sessions
            .get(&session)
            .map(|s| (s.account, s.source))
            .ok_or(Error::NotLoggedIn)?;
        if !audience.admits(self.kind_of(id)) {
            return Err(Error::AudienceMismatch);
        }
        if self.lobbies.len() >= MAX_LOBBIES {
            return Err(Error::TooManyLobbies);
        }
        if self
            .lobbies
            .values()
            .any(|l| l.name.eq_ignore_ascii_case(&name))
        {
            return Err(Error::LobbyNameTaken);
        }
        let rating = self.rating_for(id, &ruleset).await;
        let lobby_id = LobbyId(self.next_lobby);
        self.next_lobby = self.next_lobby.wrapping_add(1).max(1);
        let lobby = Lobby {
            id: lobby_id,
            name,
            teams: (0..rules.seats()).map(|slot| rules.team_of(slot)).collect(),
            ruleset,
            owner: id,
            audience,
            password,
            members: vec![Member {
                account: id,
                slot: 0,
                ready: false,
                rating,
                source,
            }],
            pending: Vec::new(),
            starting: None,
        };
        let state = self.lobby_state(&lobby);
        self.lobbies.insert(lobby_id, lobby);
        if let Some(account) = self.accounts.get_mut(&id) {
            account.lobbies.push(lobby_id);
        }
        Ok(Reply::Lobby { state })
    }

    async fn rating_for(&self, id: AccountId, ruleset: &RulesetId) -> Option<Rating> {
        match self.db.rating(id, ruleset.clone()).await {
            Ok(row) => row.map(|r| r.glicko.public()),
            Err(error) => {
                internal(error);
                None
            }
        }
    }

    fn lobby_list(&self) -> Reply {
        let list = self
            .lobbies
            .values()
            .rev()
            .filter(|l| l.starting.is_none() && !l.is_full())
            .take(MAX_LOBBY_LIST)
            .map(|l| LobbySummary {
                id: l.id,
                name: l.name.clone(),
                ruleset: l.ruleset.clone(),
                owner: l.owner,
                owner_nickname: self
                    .accounts
                    .get(&l.owner)
                    .map(|a| a.info.nickname.clone())
                    .unwrap_or_default(),
                audience: l.audience,
                locked: l.password.is_some(),
                members: l.members.len() as u8,
                seats: l.teams.len() as u8,
            })
            .collect();
        Reply::Lobbies { list }
    }

    /// A join request, or with the lobby's password the seat itself.
    async fn lobby_ask(
        &mut self,
        session: SessionId,
        lobby: LobbyId,
        password: Option<Secret>,
    ) -> Result<Reply, Error> {
        let (id, source) = self
            .sessions
            .get(&session)
            .map(|s| (s.account, s.source))
            .ok_or(Error::NotLoggedIn)?;
        let config = Arc::clone(&self.config);
        let l = self.lobbies.get(&lobby).ok_or(Error::UnknownLobby)?;
        if l.holds(id) {
            return Ok(Reply::Lobby {
                state: self.lobby_state(l),
            });
        }
        if l.pending.iter().any(|a| a.account == id) {
            return Ok(Reply::Done);
        }
        if l.starting.is_some() {
            return Err(Error::Starting);
        }
        if l.is_full() || l.pending.len() >= MAX_PENDING_PER_LOBBY {
            return Err(Error::LobbyFull);
        }
        if !l.audience.admits(self.kind_of(id)) {
            return Err(Error::AudienceMismatch);
        }
        let rules = config.ruleset(&l.ruleset).ok_or(Error::UnknownRuleset)?;
        let direct = match (&password, &l.password) {
            (Some(given), Some(set)) if same_secret(given, set) => true,
            (Some(_), Some(_)) => return Err(Error::BadLobbyPassword),
            _ => false,
        };
        self.admit_game(session, rules)?;
        if direct {
            let rating = self.rating_for(id, &rules.ruleset_id()).await;
            let state = self.seat_member(lobby, id, rating, source)?;
            return Ok(Reply::Lobby { state });
        }
        if let Some(l) = self.lobbies.get_mut(&lobby) {
            l.pending.push(Asker {
                account: id,
                source,
            });
        }
        if let Some(account) = self.accounts.get_mut(&id) {
            account.asking.push(lobby);
        }
        self.broadcast_lobby(lobby, None);
        Ok(Reply::Done)
    }

    /// Gives an account the lowest free seat and tells the others.
    fn seat_member(
        &mut self,
        lobby: LobbyId,
        account: AccountId,
        rating: Option<Rating>,
        source: IpAddr,
    ) -> Result<LobbyState, Error> {
        let l = self.lobbies.get_mut(&lobby).ok_or(Error::UnknownLobby)?;
        let slot = l.lowest_free_slot().ok_or(Error::LobbyFull)?;
        l.members.push(Member {
            account,
            slot,
            ready: false,
            rating,
            source,
        });
        l.members.sort_by_key(|m| m.slot);
        if let Some(a) = self.accounts.get_mut(&account) {
            a.lobbies.push(lobby);
        }
        self.broadcast_lobby(lobby, Some(account));
        Ok(self.lobby_state(&self.lobbies[&lobby]))
    }

    /// Withdraws a pending join request of an account, if it has one there.
    pub(super) fn cancel_ask(&mut self, id: AccountId, lobby: LobbyId) {
        if let Some(account) = self.accounts.get_mut(&id) {
            account.asking.retain(|l| *l != lobby);
        }
        if let Some(l) = self.lobbies.get_mut(&lobby) {
            l.pending.retain(|a| a.account != id);
            self.broadcast_lobby(lobby, None);
        }
        self.forget_if_idle(id);
    }

    fn owned_by(&self, id: AccountId, lobby: LobbyId) -> Result<(), Error> {
        match self.lobbies.get(&lobby) {
            Some(l) if l.owner == id => Ok(()),
            Some(_) => Err(Error::NotLobbyOwner),
            None => Err(Error::UnknownLobby),
        }
    }

    fn member_of(&self, id: AccountId, lobby: LobbyId) -> Result<(), Error> {
        match self.lobbies.get(&lobby) {
            Some(l) if l.holds(id) => Ok(()),
            _ => Err(Error::NotInLobby { lobby }),
        }
    }

    fn not_starting(&self, lobby: LobbyId) -> Result<(), Error> {
        match self.lobbies.get(&lobby) {
            Some(l) if l.starting.is_some() => Err(Error::Starting),
            Some(_) => Ok(()),
            None => Err(Error::UnknownLobby),
        }
    }

    async fn lobby_decide(
        &mut self,
        id: AccountId,
        lobby: LobbyId,
        asker: AccountId,
        allow: bool,
    ) -> Result<Reply, Error> {
        self.owned_by(id, lobby)?;
        self.not_starting(lobby)?;
        let l = self.lobbies.get_mut(&lobby).ok_or(Error::UnknownLobby)?;
        let at = l
            .pending
            .iter()
            .position(|a| a.account == asker)
            .ok_or(Error::NoSuchRequest)?;
        if allow && l.is_full() {
            return Err(Error::LobbyFull);
        }
        let ruleset = l.ruleset.clone();
        let Asker { source, .. } = l.pending.remove(at);
        if let Some(account) = self.accounts.get_mut(&asker) {
            account.asking.retain(|x| *x != lobby);
        }
        if !allow {
            self.notify(asker, Event::LobbyDenied { lobby });
            self.forget_if_idle(asker);
            self.broadcast_lobby(lobby, None);
            return Ok(Reply::Done);
        }
        let rating = self.rating_for(asker, &ruleset).await;
        let state = self.seat_member(lobby, asker, rating, source)?;
        self.notify(asker, Event::LobbyJoined { state });
        Ok(Reply::Done)
    }

    fn lobby_kick(
        &mut self,
        id: AccountId,
        lobby: LobbyId,
        target: AccountId,
    ) -> Result<Reply, Error> {
        self.owned_by(id, lobby)?;
        self.not_starting(lobby)?;
        let l = self.lobbies.get_mut(&lobby).ok_or(Error::UnknownLobby)?;
        if target == id || !l.holds(target) {
            return Err(Error::NoSuchMember);
        }
        l.members.retain(|m| m.account != target);
        if let Some(account) = self.accounts.get_mut(&target) {
            account.lobbies.retain(|x| *x != lobby);
        }
        self.notify(target, Event::LobbyKicked { lobby });
        self.forget_if_idle(target);
        self.broadcast_lobby(lobby, None);
        Ok(Reply::Done)
    }

    fn lobby_ready(&mut self, id: AccountId, lobby: LobbyId, ready: bool) -> Result<Reply, Error> {
        self.member_of(id, lobby)?;
        self.not_starting(lobby)?;
        let needed = self
            .lobbies
            .get(&lobby)
            .and_then(|l| self.config.ruleset(&l.ruleset))
            .map_or(usize::MAX, |rules| usize::from(rules.lobby_min_players()));
        let l = self.lobbies.get_mut(&lobby).ok_or(Error::UnknownLobby)?;
        let starts = l.owner == id && ready;
        if starts {
            if l.members.len() < needed {
                return Err(Error::LobbyNotFull);
            }
            if l.members.iter().any(|m| m.account != id && !m.ready) {
                return Err(Error::NotAllReady);
            }
        }
        if let Some(member) = l.members.iter_mut().find(|m| m.account == id) {
            member.ready = ready;
        }
        if starts {
            self.start_countdown(lobby);
        } else {
            self.broadcast_lobby(lobby, None);
        }
        Ok(Reply::Done)
    }

    /// Starts the clock; at its end the match is formed and waits for a slot.
    fn start_countdown(&mut self, lobby: LobbyId) {
        let countdown = Duration::from_millis(u64::from(LOBBY_COUNTDOWN_MS));
        let Some(l) = self.lobbies.get_mut(&lobby) else {
            return;
        };
        l.starting = Some(Instant::now() + countdown);
        let members: Vec<AccountId> = l.members.iter().map(|m| m.account).collect();
        for member in members {
            self.notify(
                member,
                Event::LobbyStarting {
                    lobby,
                    in_ms: LOBBY_COUNTDOWN_MS,
                },
            );
        }
        self.broadcast_lobby(lobby, None);
        let me = self.me.clone();
        tokio::spawn(async move {
            tokio::time::sleep(countdown).await;
            let _ = me.send(Cmd::CountdownDone { lobby }).await;
        });
    }

    pub(super) async fn countdown_done(&mut self, lobby: LobbyId) {
        let Some(l) = self.lobbies.remove(&lobby) else {
            return;
        };
        for asker in &l.pending {
            if let Some(account) = self.accounts.get_mut(&asker.account) {
                account.asking.retain(|x| *x != lobby);
            }
            self.notify(asker.account, Event::LobbyDenied { lobby });
            self.forget_if_idle(asker.account);
        }
        let config = Arc::clone(&self.config);
        let members: Vec<(AccountId, IpAddr)> =
            l.members.iter().map(|m| (m.account, m.source)).collect();
        let formed = match config.ruleset(&l.ruleset) {
            Some(rules) => self.form_match(rules, true, members.clone()).await.is_ok(),
            None => false,
        };
        for (member, _) in &members {
            if let Some(account) = self.accounts.get_mut(member) {
                account.lobbies.retain(|x| *x != lobby);
            }
            if !formed {
                self.notify(*member, Event::LobbyClosed { lobby });
                self.forget_if_idle(*member);
            }
        }
        if formed {
            self.try_launch();
        }
    }

    /// Takes an account out of a lobby; the owner leaving closes it.
    pub(super) fn leave_lobby(&mut self, id: AccountId, lobby: LobbyId) {
        let Some(l) = self.lobbies.get_mut(&lobby) else {
            return;
        };
        if l.owner == id {
            self.close_lobby(lobby);
            return;
        }
        l.members.retain(|m| m.account != id);
        if let Some(account) = self.accounts.get_mut(&id) {
            account.lobbies.retain(|x| *x != lobby);
        }
        self.forget_if_idle(id);
        self.broadcast_lobby(lobby, None);
    }

    fn close_lobby(&mut self, lobby: LobbyId) {
        let Some(l) = self.lobbies.remove(&lobby) else {
            return;
        };
        for asker in &l.pending {
            if let Some(account) = self.accounts.get_mut(&asker.account) {
                account.asking.retain(|x| *x != lobby);
            }
            self.notify(asker.account, Event::LobbyDenied { lobby });
            self.forget_if_idle(asker.account);
        }
        for member in &l.members {
            if let Some(account) = self.accounts.get_mut(&member.account) {
                account.lobbies.retain(|x| *x != lobby);
            }
            self.notify(member.account, Event::LobbyClosed { lobby });
            self.forget_if_idle(member.account);
        }
    }
}

/// Whether two secrets are equal, in time that depends on their lengths
/// only: how far a guess matched must not show in the answer's timing.
fn same_secret(given: &Secret, set: &Secret) -> bool {
    let (given, set) = (given.0.as_bytes(), set.0.as_bytes());
    let mut differ = given.len() ^ set.len();
    for at in 0..given.len().max(set.len()) {
        let (a, b) = (given.get(at).copied(), set.get(at).copied());
        differ |= usize::from(a.unwrap_or(0) ^ b.unwrap_or(0));
    }
    std::hint::black_box(differ) == 0
}
