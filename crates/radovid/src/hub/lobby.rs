//! Lobbies: custom games with an owner, join requests, readiness and a
//! countdown nobody can cancel.

use std::time::{Duration, Instant};

use bota_proto::SlotId;
use radovid_proto::*;

use super::{Cmd, Hub, Where, internal};

pub(super) struct Member {
    pub account: AccountId,
    pub slot: u8,
    pub ready: bool,
    pub rating: Option<Rating>,
}

pub(super) struct Lobby {
    pub id: LobbyId,
    pub name: String,
    pub ruleset: RulesetId,
    pub seats: u8,
    pub owner: AccountId,
    pub audience: Audience,
    /// Sorted by slot.
    pub members: Vec<Member>,
    /// Join requests, oldest first.
    pub pending: Vec<AccountId>,
    pub starting: Option<Instant>,
    pub match_id: Option<MatchId>,
}

impl Lobby {
    fn lowest_free_slot(&self) -> Option<u8> {
        (0..self.seats).find(|slot| self.members.iter().all(|m| m.slot != *slot))
    }

    fn is_full(&self) -> bool {
        self.members.len() >= usize::from(self.seats)
    }

    fn holds(&self, id: AccountId) -> bool {
        self.members.iter().any(|m| m.account == id)
    }
}

impl Hub {
    fn member_view(
        &self,
        account: AccountId,
        slot: u8,
        ready: bool,
        rating: Option<Rating>,
    ) -> LobbyMember {
        let (login, nickname, kind, bot_version) = self.accounts.get(&account).map_or_else(
            || (String::new(), String::new(), AccountKind::Human, None),
            |a| {
                (
                    a.info.login.clone(),
                    a.info.nickname.clone(),
                    a.info.kind,
                    a.bot_version.clone(),
                )
            },
        );
        LobbyMember {
            account,
            login,
            nickname,
            kind,
            bot_version: (kind == AccountKind::Bot).then_some(bot_version).flatten(),
            rating,
            slot: SlotId(slot),
            team: Self::team_of_slot(slot),
            ready,
        }
    }

    pub(super) fn lobby_state(&self, lobby: &Lobby) -> LobbyState {
        LobbyState {
            id: lobby.id,
            name: lobby.name.clone(),
            ruleset: lobby.ruleset.clone(),
            owner: lobby.owner,
            audience: lobby.audience,
            rated: false,
            members: lobby
                .members
                .iter()
                .map(|m| self.member_view(m.account, m.slot, m.ready, m.rating))
                .collect(),
            pending: lobby
                .pending
                .iter()
                .map(|account| self.member_view(*account, 0, false, None))
                .collect(),
            starting_in_ms: lobby.starting.map(|deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as u32
            }),
        }
    }

    fn broadcast_lobby(&mut self, lobby: LobbyId) {
        self.broadcast_lobby_except(lobby, None);
    }

    /// Sends the whole lobby state to everyone in it but `except`.
    fn broadcast_lobby_except(&mut self, lobby: LobbyId, except: Option<AccountId>) {
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

    pub(super) async fn lobby_create(
        &mut self,
        id: AccountId,
        wire: u64,
        ruleset: RulesetId,
        name: String,
        audience: Audience,
    ) -> Result<Reply, Error> {
        let seats = self
            .config
            .ruleset(&ruleset)
            .ok_or(Error::UnknownRuleset)?
            .seats;
        if !valid_lobby_name(&name) {
            return Err(Error::BadLobbyName);
        }
        self.check_wire(wire)?;
        self.require_idle(id)?;
        let kind = self
            .accounts
            .get(&id)
            .map(|a| a.info.kind)
            .ok_or(Error::Internal)?;
        if !audience.admits(kind) {
            return Err(Error::AudienceMismatch);
        }
        if self.lobbies.len() >= MAX_LOBBIES {
            return Err(Error::TooManyLobbies);
        }
        let rating = self.rating_for(id, &ruleset).await;
        let lobby_id = LobbyId(self.next_lobby);
        self.next_lobby = self.next_lobby.wrapping_add(1).max(1);
        let lobby = Lobby {
            id: lobby_id,
            name,
            ruleset,
            seats,
            owner: id,
            audience,
            members: vec![Member {
                account: id,
                slot: 0,
                ready: false,
                rating,
            }],
            pending: Vec::new(),
            starting: None,
            match_id: None,
        };
        let state = self.lobby_state(&lobby);
        self.lobbies.insert(lobby_id, lobby);
        self.cancel_ask(id);
        if let Some(account) = self.accounts.get_mut(&id) {
            account.at = Where::InLobby(lobby_id);
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

    pub(super) fn lobby_list(&self) -> Reply {
        let list = self
            .lobbies
            .values()
            .rev()
            .filter(|l| l.starting.is_none() && !l.is_full())
            .take(MAX_LOBBY_LIST)
            .map(|l| {
                let owner = self.accounts.get(&l.owner);
                LobbySummary {
                    id: l.id,
                    name: l.name.clone(),
                    ruleset: l.ruleset.clone(),
                    owner: l.owner,
                    owner_login: owner.map(|a| a.info.login.clone()).unwrap_or_default(),
                    owner_nickname: owner.map(|a| a.info.nickname.clone()).unwrap_or_default(),
                    audience: l.audience,
                    members: l.members.len() as u8,
                    seats: l.seats,
                }
            })
            .collect();
        Reply::Lobbies { list }
    }

    pub(super) fn lobby_ask(
        &mut self,
        id: AccountId,
        wire: u64,
        lobby: LobbyId,
    ) -> Result<Reply, Error> {
        self.require_idle(id)?;
        let l = self.lobbies.get(&lobby).ok_or(Error::UnknownLobby)?;
        if l.starting.is_some() {
            return Err(Error::Starting);
        }
        if l.is_full() || l.pending.len() >= MAX_PENDING_PER_LOBBY {
            return Err(Error::LobbyFull);
        }
        let kind = self
            .accounts
            .get(&id)
            .map(|a| a.info.kind)
            .ok_or(Error::Internal)?;
        if !l.audience.admits(kind) {
            return Err(Error::AudienceMismatch);
        }
        self.check_wire(wire)?;
        self.cancel_ask(id);
        if let Some(l) = self.lobbies.get_mut(&lobby) {
            l.pending.push(id);
        }
        if let Some(account) = self.accounts.get_mut(&id) {
            account.asking = Some(lobby);
        }
        self.broadcast_lobby(lobby);
        Ok(Reply::Done)
    }

    pub(super) fn lobby_ask_cancel(&mut self, id: AccountId) -> Result<Reply, Error> {
        if self.accounts.get(&id).and_then(|a| a.asking).is_none() {
            return Err(Error::NoPendingRequest);
        }
        self.cancel_ask(id);
        Ok(Reply::Done)
    }

    /// Withdraws the pending join request of an account, if it has one.
    pub(super) fn cancel_ask(&mut self, id: AccountId) {
        let Some(lobby) = self.accounts.get_mut(&id).and_then(|a| a.asking.take()) else {
            return;
        };
        if let Some(l) = self.lobbies.get_mut(&lobby) {
            l.pending.retain(|account| *account != id);
            self.broadcast_lobby(lobby);
        }
    }

    fn lobby_of_owner(&self, id: AccountId) -> Result<LobbyId, Error> {
        match self.accounts.get(&id).map(|a| &a.at) {
            Some(Where::InLobby(lobby))
                if self.lobbies.get(lobby).is_some_and(|l| l.owner == id) =>
            {
                Ok(*lobby)
            }
            _ => Err(Error::NotLobbyOwner),
        }
    }

    fn lobby_of_member(&self, id: AccountId) -> Result<LobbyId, Error> {
        match self.accounts.get(&id).map(|a| &a.at) {
            Some(Where::InLobby(lobby)) => Ok(*lobby),
            _ => Err(Error::NotInLobby),
        }
    }

    fn not_starting(&self, lobby: LobbyId) -> Result<(), Error> {
        match self.lobbies.get(&lobby) {
            Some(l) if l.starting.is_some() => Err(Error::Starting),
            Some(_) => Ok(()),
            None => Err(Error::UnknownLobby),
        }
    }

    pub(super) async fn lobby_decide(
        &mut self,
        id: AccountId,
        asker: AccountId,
        allow: bool,
    ) -> Result<Reply, Error> {
        let lobby = self.lobby_of_owner(id)?;
        self.not_starting(lobby)?;
        let l = self.lobbies.get_mut(&lobby).ok_or(Error::UnknownLobby)?;
        let at = l
            .pending
            .iter()
            .position(|a| *a == asker)
            .ok_or(Error::NoSuchRequest)?;
        if !allow {
            l.pending.remove(at);
            self.clear_asking(asker);
            self.notify(asker, Event::LobbyDenied { lobby });
            self.broadcast_lobby(lobby);
            return Ok(Reply::Done);
        }
        if l.is_full() {
            return Err(Error::LobbyFull);
        }
        let ruleset = l.ruleset.clone();
        l.pending.remove(at);
        self.clear_asking(asker);
        if let Err(error) = self.require_idle(asker) {
            self.broadcast_lobby(lobby);
            return Err(error);
        }
        let rating = self.rating_for(asker, &ruleset).await;
        let l = self.lobbies.get_mut(&lobby).ok_or(Error::UnknownLobby)?;
        let slot = l.lowest_free_slot().ok_or(Error::LobbyFull)?;
        l.members.push(Member {
            account: asker,
            slot,
            ready: false,
            rating,
        });
        l.members.sort_by_key(|m| m.slot);
        if let Some(account) = self.accounts.get_mut(&asker) {
            account.at = Where::InLobby(lobby);
        }
        let state = self.lobby_state(&self.lobbies[&lobby]);
        self.notify(asker, Event::LobbyJoined { state });
        self.broadcast_lobby_except(lobby, Some(asker));
        Ok(Reply::Done)
    }

    fn clear_asking(&mut self, id: AccountId) {
        if let Some(account) = self.accounts.get_mut(&id) {
            account.asking = None;
        }
    }

    pub(super) fn lobby_kick(&mut self, id: AccountId, target: AccountId) -> Result<Reply, Error> {
        let lobby = self.lobby_of_owner(id)?;
        self.not_starting(lobby)?;
        let l = self.lobbies.get_mut(&lobby).ok_or(Error::UnknownLobby)?;
        if target == id || !l.holds(target) {
            return Err(Error::NoSuchMember);
        }
        l.members.retain(|m| m.account != target);
        if let Some(account) = self.accounts.get_mut(&target) {
            account.at = Where::Idle;
        }
        self.notify(target, Event::LobbyKicked { lobby });
        self.forget_if_idle(target);
        self.broadcast_lobby(lobby);
        Ok(Reply::Done)
    }

    pub(super) async fn lobby_ready(&mut self, id: AccountId, ready: bool) -> Result<Reply, Error> {
        let lobby = self.lobby_of_member(id)?;
        self.not_starting(lobby)?;
        let l = self.lobbies.get_mut(&lobby).ok_or(Error::UnknownLobby)?;
        let is_owner = l.owner == id;
        if is_owner && ready {
            if !l.is_full() {
                return Err(Error::LobbyNotFull);
            }
            if l.members.iter().any(|m| m.account != id && !m.ready) {
                return Err(Error::NotAllReady);
            }
        }
        if let Some(member) = l.members.iter_mut().find(|m| m.account == id) {
            member.ready = ready;
        }
        if is_owner && ready {
            self.start_countdown(lobby).await?;
        } else {
            self.broadcast_lobby(lobby);
        }
        Ok(Reply::Done)
    }

    /// Creates the match so the game server is up when the countdown ends, and
    /// starts the clock.
    async fn start_countdown(&mut self, lobby: LobbyId) -> Result<(), Error> {
        let l = self.lobbies.get(&lobby).ok_or(Error::UnknownLobby)?;
        let members: Vec<AccountId> = l.members.iter().map(|m| m.account).collect();
        let ruleset = l.ruleset.clone();
        let match_id = match self.create_match(ruleset, false, &members).await {
            Ok(id) => id,
            Err(error) => {
                if let Some(l) = self.lobbies.get_mut(&lobby)
                    && let Some(owner) = l.members.iter_mut().find(|m| m.account == l.owner)
                {
                    owner.ready = false;
                }
                return Err(error);
            }
        };
        let deadline = Instant::now() + Duration::from_millis(u64::from(LOBBY_COUNTDOWN_MS));
        if let Some(l) = self.lobbies.get_mut(&lobby) {
            l.starting = Some(deadline);
            l.match_id = Some(match_id);
        }
        // The match exists but its seats are still in the lobby until the end of the count.
        if let Some(state) = self.matches.get_mut(&match_id) {
            state.released = false;
        }
        for member in members {
            self.notify(
                member,
                Event::LobbyStarting {
                    in_ms: LOBBY_COUNTDOWN_MS,
                },
            );
        }
        self.broadcast_lobby(lobby);
        let tx = self.me.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(u64::from(LOBBY_COUNTDOWN_MS))).await;
            let _ = tx.send(Cmd::CountdownDone { lobby }).await;
        });
        Ok(())
    }

    pub(super) fn countdown_done(&mut self, lobby: LobbyId) {
        let Some(l) = self.lobbies.remove(&lobby) else {
            return;
        };
        let Some(match_id) = l.match_id else {
            return;
        };
        for member in &l.members {
            if let Some(account) = self.accounts.get_mut(&member.account) {
                account.at = Where::InMatch(match_id);
            }
        }
        if let Some(state) = self.matches.get_mut(&match_id) {
            state.released = true;
        }
        self.try_announce(match_id);
    }

    pub(super) fn lobby_leave(&mut self, id: AccountId) -> Result<Reply, Error> {
        let lobby = self.lobby_of_member(id)?;
        self.not_starting(lobby)?;
        self.leave_lobby(id, lobby);
        Ok(Reply::Done)
    }

    /// Takes an account out of its lobby; the owner leaving closes it.
    pub(super) fn leave_lobby(&mut self, id: AccountId, lobby: LobbyId) {
        let Some(l) = self.lobbies.get(&lobby) else {
            if let Some(account) = self.accounts.get_mut(&id) {
                account.at = Where::Idle;
            }
            return;
        };
        if l.owner == id {
            self.close_lobby(lobby);
            return;
        }
        if let Some(l) = self.lobbies.get_mut(&lobby) {
            l.members.retain(|m| m.account != id);
        }
        if let Some(account) = self.accounts.get_mut(&id) {
            account.at = Where::Idle;
        }
        self.forget_if_idle(id);
        self.broadcast_lobby(lobby);
    }

    pub(super) fn close_lobby(&mut self, lobby: LobbyId) {
        let Some(l) = self.lobbies.remove(&lobby) else {
            return;
        };
        for asker in &l.pending {
            self.clear_asking(*asker);
            self.notify(*asker, Event::LobbyDenied { lobby });
        }
        for member in &l.members {
            if let Some(account) = self.accounts.get_mut(&member.account) {
                account.at = Where::Idle;
            }
            self.notify(member.account, Event::LobbyClosed { lobby });
        }
        for member in &l.members {
            self.forget_if_idle(member.account);
        }
    }
}
