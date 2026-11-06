//! Lobbies: custom unrated games with an owner, a unique name, an optional
//! password, readiness and a countdown nobody can cancel. Whoever may sit in a
//! lobby (it is open, or the password is right) is in at once. An account may
//! sit in several.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use radovid_proto::*;
use tokio::time::Instant;

use super::matches::Origin;
use super::{Cmd, Hub, SessionId, internal, millis_until};

pub(super) struct Member {
    pub account: AccountId,
    pub slot: u8,
    pub ready: bool,
    pub rating: Option<Rating>,
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
    /// The owner turned the game's cheats on; they hold while the ruleset has them.
    pub cheats: bool,
    pub starting: Option<Instant>,
}

impl Lobby {
    /// The slot a newcomer takes: the lowest free one of the team with the fewest seated, the
    /// lower team on a tie, so two players meet across the sides.
    fn lowest_free_slot(&self) -> Option<u8> {
        let free = |slot: &u8| self.members.iter().all(|m| m.slot != *slot);
        let seated = |team: u8| {
            self.members
                .iter()
                .filter(|m| self.teams.get(usize::from(m.slot)) == Some(&team))
                .count()
        };
        (0..self.teams.len() as u8)
            .filter(free)
            .min_by_key(|slot| (seated(self.teams[usize::from(*slot)]), *slot))
    }

    /// No slot is free to sit in (spectators hold none).
    fn is_full(&self) -> bool {
        self.lowest_free_slot().is_none()
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
            ClientMsg::LobbyList => Ok(self.lobby_list("")),
            ClientMsg::LobbySearch { query } => {
                // What a lobby name may hold, as `valid_lobby_name` counts it.
                if query.chars().count() > MAX_LOBBY_NAME_LEN || query.chars().any(char::is_control)
                {
                    return Err(Error::BadLobbyName);
                }
                Ok(self.lobby_list(&query))
            }
            ClientMsg::LobbyJoin { lobby, password } => {
                self.lobby_join(session, lobby, password).await
            }
            ClientMsg::LobbyKick { lobby, account } => self.lobby_kick(id, lobby, account),
            ClientMsg::LobbyReady { lobby, ready } => self.lobby_ready(id, lobby, ready),
            ClientMsg::LobbySlot {
                lobby,
                account,
                slot,
            } => self.lobby_slot(id, lobby, account, slot),
            ClientMsg::LobbyConfigure {
                lobby,
                ruleset,
                audience,
                password,
            } => self.lobby_configure(id, lobby, ruleset, audience, password),
            ClientMsg::LobbyCheats { lobby, on } => self.lobby_cheats(id, lobby, on),
            ClientMsg::LobbyLeave { lobby } => {
                self.member_of(id, lobby)?;
                self.not_starting(lobby)?;
                self.leave_lobby(id, lobby);
                Ok(Reply::Done)
            }
            // `Hub::request` sends only the lobby messages here.
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
                    let team = lobby
                        .teams
                        .get(usize::from(m.slot))
                        .copied()
                        .unwrap_or(if m.slot == LOBBY_NO_SLOT { m.slot } else { 0 });
                    self.member_view(m.account, m.slot, m.ready, m.rating, team)
                })
                .collect(),
            retired_pending: Vec::new(),
            starting_in_ms: lobby.starting.map(millis_until),
            teams: self.config.lobby_teams.clone(),
            cheats: self
                .config
                .ruleset(&lobby.ruleset)
                .is_some_and(|rules| rules.has_cheats())
                .then_some(lobby.cheats),
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
            teams: config.lobby_layout(),
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
            starting: None,
            cheats: false,
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

    /// The open lobbies, newest first, whose name or owner's nickname holds `query`, case folded
    /// (empty matches every one).
    fn lobby_list(&self, query: &str) -> Reply {
        let query = fold_case(query);
        let list = self
            .lobbies
            .values()
            .rev()
            .filter(|l| l.starting.is_none() && !l.is_full())
            .filter_map(|l| {
                let owner_nickname = self
                    .accounts
                    .get(&l.owner)
                    .map(|a| a.info.nickname.as_str())
                    .unwrap_or_default();
                let found = fold_case(&l.name).contains(&query)
                    || fold_case(owner_nickname).contains(&query);
                found.then(|| LobbySummary {
                    id: l.id,
                    name: l.name.clone(),
                    ruleset: l.ruleset.clone(),
                    owner: l.owner,
                    owner_nickname: owner_nickname.to_owned(),
                    audience: l.audience,
                    locked: l.password.is_some(),
                    members: l.members.len() as u8,
                    seats: l.teams.len() as u8,
                })
            })
            .take(MAX_LOBBY_LIST)
            .collect();
        Reply::Lobbies { list }
    }

    /// The seat itself, at once: an open lobby needs no password, a locked one the right one.
    async fn lobby_join(
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
        if l.starting.is_some() {
            return Err(Error::Starting);
        }
        if l.is_full() {
            return Err(Error::LobbyFull);
        }
        if !l.audience.admits(self.kind_of(id)) {
            return Err(Error::AudienceMismatch);
        }
        let rules = config.ruleset(&l.ruleset).ok_or(Error::UnknownRuleset)?;
        match (&password, &l.password) {
            (_, None) => {}
            (None, Some(_)) => return Err(Error::LobbyPasswordRequired),
            (Some(given), Some(set)) if same_secret(given, set) => {}
            (Some(_), Some(_)) => return Err(Error::BadLobbyPassword),
        }
        self.admit_game(session, rules)?;
        let rating = self.rating_for(id, &rules.ruleset_id()).await;
        let state = self.seat_member(lobby, id, rating, source)?;
        Ok(Reply::Lobby { state })
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

    /// Moves a member to a free slot of the lobby, or to none; the owner moves the others.
    fn lobby_slot(
        &mut self,
        id: AccountId,
        lobby: LobbyId,
        account: Option<AccountId>,
        slot: u8,
    ) -> Result<Reply, Error> {
        self.member_of(id, lobby)?;
        self.not_starting(lobby)?;
        let target = account.unwrap_or(id);
        if target != id {
            self.owned_by(id, lobby)?;
        }
        let l = self.lobbies.get_mut(&lobby).ok_or(Error::UnknownLobby)?;
        if !l.holds(target) {
            return Err(Error::NoSuchMember);
        }
        if slot != LOBBY_NO_SLOT {
            if usize::from(slot) >= l.teams.len() {
                return Err(Error::NoSuchSlot);
            }
            if l.members
                .iter()
                .any(|m| m.slot == slot && m.account != target)
            {
                return Err(Error::SlotTaken);
            }
        }
        if let Some(member) = l.members.iter_mut().find(|m| m.account == target) {
            member.slot = slot;
            member.ready = false;
        }
        l.members.sort_by_key(|m| m.slot);
        self.broadcast_lobby(lobby, None);
        Ok(Reply::Done)
    }

    /// The owner's settings of a lobby: the ruleset (the rules only: the slots stay as they are),
    /// who may sit, the password. Everyone is unreadied.
    fn lobby_configure(
        &mut self,
        id: AccountId,
        lobby: LobbyId,
        ruleset: RulesetId,
        audience: Audience,
        password: Option<Secret>,
    ) -> Result<Reply, Error> {
        self.owned_by(id, lobby)?;
        self.not_starting(lobby)?;
        let config = Arc::clone(&self.config);
        let rules = config.ruleset(&ruleset).ok_or(Error::UnknownRuleset)?;
        if password
            .as_ref()
            .is_some_and(|p| !valid_lobby_password(&p.0))
        {
            return Err(Error::BadLobbyPassword);
        }
        let kinds: Vec<AccountKind> = self
            .lobbies
            .get(&lobby)
            .ok_or(Error::UnknownLobby)?
            .members
            .iter()
            .map(|m| self.kind_of(m.account))
            .collect();
        if kinds
            .iter()
            .any(|kind| !audience.admits(*kind) || !rules.admits(*kind))
        {
            return Err(Error::AudienceMismatch);
        }
        let l = self.lobbies.get_mut(&lobby).ok_or(Error::UnknownLobby)?;
        l.ruleset = ruleset;
        l.audience = audience;
        l.password = password;
        // Cheats stay on only under rules that have them; turned on again, they are asked again.
        l.cheats &= rules.has_cheats();
        for member in &mut l.members {
            member.ready = false;
        }
        self.broadcast_lobby(lobby, None);
        Ok(Reply::Done)
    }

    /// The owner's switch of the game's cheats, for a ruleset that has them. Everyone is unreadied:
    /// a member readied for a match without cheats is asked again.
    fn lobby_cheats(&mut self, id: AccountId, lobby: LobbyId, on: bool) -> Result<Reply, Error> {
        self.owned_by(id, lobby)?;
        self.not_starting(lobby)?;
        let config = Arc::clone(&self.config);
        let l = self.lobbies.get_mut(&lobby).ok_or(Error::UnknownLobby)?;
        if !config.ruleset(&l.ruleset).is_some_and(|r| r.has_cheats()) {
            return Err(Error::NoCheats);
        }
        l.cheats = on;
        for member in &mut l.members {
            member.ready = false;
        }
        self.broadcast_lobby(lobby, None);
        Ok(Reply::Done)
    }

    fn lobby_ready(&mut self, id: AccountId, lobby: LobbyId, ready: bool) -> Result<Reply, Error> {
        self.member_of(id, lobby)?;
        self.not_starting(lobby)?;
        let rules = self
            .lobbies
            .get(&lobby)
            .and_then(|l| self.config.ruleset(&l.ruleset));
        let needed = rules.map_or(usize::MAX, |rules| usize::from(rules.lobby_min_players()));
        let starts = self.lobbies.get(&lobby).is_some_and(|l| l.owner == id && ready);
        if starts { self.admit_custom_match()?; }
        let l = self.lobbies.get_mut(&lobby).ok_or(Error::UnknownLobby)?;
        let starts = l.owner == id && ready;
        if starts {
            // The match is the members who hold a slot; a spectator is no one's to wait for.
            let seated = l.members.iter().filter(|m| m.slot != LOBBY_NO_SLOT);
            if seated.clone().count() < needed
                || l.members
                    .iter()
                    .any(|m| m.account == id && m.slot == LOBBY_NO_SLOT)
            {
                return Err(Error::LobbyNotFull);
            }
            if seated.into_iter().any(|m| m.account != id && !m.ready) {
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
        let config = Arc::clone(&self.config);
        // The members who hold a slot play, in slot order and on the sides they chose; the
        // spectators' lobby ends with the countdown.
        let (seated, spectators): (Vec<&Member>, Vec<&Member>) =
            l.members.iter().partition(|m| m.slot != LOBBY_NO_SLOT);
        let members: Vec<(AccountId, IpAddr)> =
            seated.iter().map(|m| (m.account, m.source)).collect();
        let teams: Vec<u8> = seated
            .iter()
            .map(|m| l.teams.get(usize::from(m.slot)).copied().unwrap_or(0))
            .collect();
        for spectator in &spectators {
            if let Some(account) = self.accounts.get_mut(&spectator.account) {
                account.lobbies.retain(|x| *x != lobby);
            }
            self.notify(spectator.account, Event::LobbyClosed { lobby });
            self.forget_if_idle(spectator.account);
        }
        let origin = Origin::Lobby { cheats: l.cheats };
        let formed = match config.ruleset(&l.ruleset) {
            Some(rules) => self
                .form_match(rules, origin, members.clone(), Some(teams))
                .await
                .is_ok(),
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
        for member in &l.members {
            if let Some(account) = self.accounts.get_mut(&member.account) {
                account.lobbies.retain(|x| *x != lobby);
            }
            self.notify(member.account, Event::LobbyClosed { lobby });
            self.forget_if_idle(member.account);
        }
    }
}

/// `text` with its case folded, so that two spellings that differ only by case compare equal:
/// lower case, then upper, then lower again, a character at a time (`ẞ`, `ß` and `SS` all become
/// `ss`, a final `ς` becomes `σ`). Unicode's full case folding without its tables.
fn fold_case(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase)
        .collect()
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
