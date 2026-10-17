//! Matches: creating them, handing the seats to their players, and recording
//! how they ended.

use std::collections::HashMap;
use std::net::SocketAddr;

use bota_proto::{HeroId, SeatKey, SlotId, Team};
use radovid_proto::*;
use tokio::sync::oneshot;

use super::{Hub, Where, internal};
use crate::backend::MatchSpec;
use crate::db::{Finish, NewSeat, RatingChange, now_ms};
use crate::rating::Glicko;
use crate::supervisor::{End, Supervisor};

pub(super) struct SeatState {
    pub account: AccountId,
    pub slot: u8,
    pub key: SeatKey,
    pub hero: Option<HeroId>,
}

pub(super) struct MatchState {
    pub ruleset: RulesetId,
    pub rated: bool,
    pub seats: Vec<SeatState>,
    /// The game server answered the referee.
    pub up: bool,
    pub addr: Option<SocketAddr>,
    /// The seats have left their lobby (or never had one).
    pub released: bool,
    pub announced: bool,
    pub stop: Option<oneshot::Sender<()>>,
    /// Each seat's rating when the match was formed, for the lobby's and handoff's display.
    pub ratings: HashMap<AccountId, Rating>,
}

pub(super) struct Route {
    pub addr: SocketAddr,
    pub spectators: usize,
}

/// Where a game connection is to be bridged.
pub struct GameRoute {
    pub addr: SocketAddr,
}

impl Hub {
    /// Registers a match with its seats and starts a task to run it. The
    /// accounts become `InMatch` unless the caller keeps them elsewhere (a
    /// lobby's countdown).
    pub(super) async fn create_match(
        &mut self,
        ruleset: RulesetId,
        rated: bool,
        members: &[AccountId],
    ) -> Result<MatchId, Error> {
        let config = self
            .config
            .ruleset(&ruleset)
            .ok_or(Error::UnknownRuleset)?
            .clone();
        let seats: Vec<NewSeat> = members
            .iter()
            .enumerate()
            .map(|(slot, account)| NewSeat {
                account: *account,
                slot: SlotId(slot as u8),
                team: Self::team_of_slot(slot as u8),
            })
            .collect();
        let match_id = self
            .db
            .create_match(ruleset.clone(), rated, seats)
            .await
            .map_err(internal)?;
        let keyed = members
            .iter()
            .enumerate()
            .map(|(slot, account)| {
                Ok(SeatState {
                    account: *account,
                    slot: slot as u8,
                    key: random_key().map_err(internal)?,
                    hero: None,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let names = self.seat_names(members);
        let spec = MatchSpec {
            match_id,
            seats: keyed
                .iter()
                .zip(names)
                .map(|(seat, name)| (seat.key, name))
                .collect(),
            referee_key: random_key().map_err(internal)?,
            rules_args: config.bota_args.clone(),
            start_after_secs: config.start_after_secs,
        };
        let ratings = self.seat_ratings(members, &ruleset).await;
        let (stop_tx, stop_rx) = oneshot::channel();
        let supervisor = Supervisor {
            match_id,
            spec,
            backend: self.backend.clone(),
            hub: self.me.clone(),
            slots: self.slots.clone(),
        };
        tokio::spawn(supervisor.run(stop_rx));
        let in_lobby = !rated;
        self.matches.insert(
            match_id,
            MatchState {
                ruleset,
                rated,
                seats: keyed,
                up: false,
                addr: None,
                released: !in_lobby,
                announced: false,
                stop: Some(stop_tx),
                ratings,
            },
        );
        if !in_lobby {
            for account in members {
                if let Some(a) = self.accounts.get_mut(account) {
                    a.at = Where::InMatch(match_id);
                }
            }
        }
        Ok(match_id)
    }

    /// Each member's stored rating; a member without one or a failed lookup shows none.
    async fn seat_ratings(
        &self,
        members: &[AccountId],
        ruleset: &RulesetId,
    ) -> HashMap<AccountId, Rating> {
        let mut ratings = HashMap::new();
        for account in members {
            if let Ok(Some(row)) = self.db.rating(*account, ruleset.clone()).await {
                ratings.insert(*account, row.glicko.public());
            }
        }
        ratings
    }

    /// Nicknames, with the login appended where two coincide.
    fn seat_names(&self, members: &[AccountId]) -> Vec<String> {
        let infos: Vec<(&str, &str)> = members
            .iter()
            .map(|m| {
                self.accounts.get(m).map_or(("", ""), |a| {
                    (a.info.nickname.as_str(), a.info.login.as_str())
                })
            })
            .collect();
        infos
            .iter()
            .map(|(nickname, login)| {
                if infos.iter().filter(|(n, _)| n == nickname).count() > 1 {
                    format!("{nickname}_{login}")
                } else {
                    (*nickname).to_owned()
                }
            })
            .collect()
    }

    pub(super) fn match_up(&mut self, match_id: MatchId, addr: SocketAddr) {
        if let Some(state) = self.matches.get_mut(&match_id) {
            state.up = true;
            state.addr = Some(addr);
        }
        self.try_announce(match_id);
    }

    pub(super) fn match_started(&mut self, match_id: MatchId, picks: Vec<(SlotId, HeroId)>) {
        if let Some(state) = self.matches.get_mut(&match_id) {
            for (slot, hero) in picks {
                if let Some(seat) = state.seats.iter_mut().find(|s| s.slot == slot.0) {
                    seat.hero = Some(hero);
                }
            }
        }
    }

    /// Opens the game path for the seats and tells the players, once the game
    /// server is up and the seats have left their lobby.
    pub(super) fn try_announce(&mut self, match_id: MatchId) {
        let Some(state) = self.matches.get_mut(&match_id) else {
            return;
        };
        let (Some(addr), true, true, false) =
            (state.addr, state.up, state.released, state.announced)
        else {
            return;
        };
        state.announced = true;
        for seat in &state.seats {
            self.routes.insert(
                seat.key.0,
                Route {
                    addr,
                    spectators: 0,
                },
            );
        }
        let accounts: Vec<AccountId> = state.seats.iter().map(|s| s.account).collect();
        for account in accounts {
            if let Some(handoff) = self.handoff_for(match_id, account) {
                self.notify(account, Event::MatchFound { handoff });
            }
        }
    }

    pub(super) fn match_handoff(&self, id: AccountId) -> Result<Reply, Error> {
        let Some(Where::InMatch(match_id)) = self.accounts.get(&id).map(|a| a.at.clone()) else {
            return Err(Error::NotInMatch);
        };
        self.handoff_for(match_id, id)
            .map(|handoff| Reply::Handoff { handoff })
            .ok_or(Error::NotInMatch)
    }

    fn seat_info(&self, state: &MatchState, seat: &SeatState) -> SeatInfo {
        let account = self.accounts.get(&seat.account);
        let kind = account.map_or(AccountKind::Human, |a| a.info.kind);
        SeatInfo {
            slot: SlotId(seat.slot),
            team: Self::team_of_slot(seat.slot),
            account: seat.account,
            login: account.map(|a| a.info.login.clone()).unwrap_or_default(),
            nickname: account.map(|a| a.info.nickname.clone()).unwrap_or_default(),
            kind,
            bot_version: account
                .and_then(|a| a.bot_version.clone())
                .filter(|_| kind == AccountKind::Bot),
            rating: state.ratings.get(&seat.account).copied(),
        }
    }

    fn handoff_for(&self, match_id: MatchId, id: AccountId) -> Option<Handoff> {
        let state = self.matches.get(&match_id)?;
        if !state.announced {
            return None;
        }
        let mine = state.seats.iter().find(|s| s.account == id)?;
        Some(Handoff {
            match_id,
            ruleset: state.ruleset.clone(),
            rated: state.rated,
            game_url: self.config.public_game_url.clone(),
            seat_key: mine.key,
            slot: SlotId(mine.slot),
            team: Self::team_of_slot(mine.slot),
            seats: state
                .seats
                .iter()
                .map(|s| self.seat_info(state, s))
                .collect(),
            bota_wire: bota_proto::WIRE_FINGERPRINT,
        })
    }

    /// Where a connection with the key goes. Spectators are counted; a
    /// player connection is passed on as it is, and the game server refuses
    /// it (`SeatHeld`) while a live connection holds the seat.
    pub(super) fn route(&mut self, key: &SeatKey, spectator: bool) -> Option<GameRoute> {
        let route = self.routes.get_mut(&key.0)?;
        if spectator {
            if route.spectators >= MAX_SPECTATORS_PER_KEY {
                return None;
            }
            route.spectators += 1;
        }
        Some(GameRoute { addr: route.addr })
    }

    pub(super) fn release_route(&mut self, key: &SeatKey, spectator: bool) {
        if spectator && let Some(route) = self.routes.get_mut(&key.0) {
            route.spectators = route.spectators.saturating_sub(1);
        }
    }

    pub(super) async fn match_ended(&mut self, match_id: MatchId, end: End) {
        let Some(state) = self.matches.remove(&match_id) else {
            return;
        };
        for seat in &state.seats {
            self.routes.remove(&seat.key.0);
        }
        let (outcome, reason, duration, stats) = match end {
            End::Over { winner, stats } => {
                let outcome = match winner {
                    Team::Radiant => Outcome::Radiant,
                    Team::Dire => Outcome::Dire,
                    Team::Neutral => Outcome::Draw,
                };
                let reason = if outcome == Outcome::Draw {
                    EndReason::TimeCap
                } else {
                    EndReason::Decided
                };
                (outcome, reason, stats.duration, Some(stats))
            }
            End::Crashed(why) => {
                eprintln!("radovid: match {} lost: {why}", match_id.0);
                (Outcome::Null, EndReason::Crashed, 0, None)
            }
            End::Stopped => (Outcome::Null, EndReason::Stopped, 0, None),
        };
        let finished_at = now_ms();
        let changes = if state.rated && stats.is_some() {
            self.rate(&state, outcome, finished_at).await
        } else {
            Vec::new()
        };
        let finish = Finish {
            match_id,
            ruleset: state.ruleset.clone(),
            outcome,
            reason,
            duration_ticks: duration,
            stats: stats.clone(),
            heroes: state
                .seats
                .iter()
                .map(|s| (SlotId(s.slot), s.hero))
                .collect(),
            changes: changes.clone(),
            finished_at,
        };
        if let Err(error) = self.db.finish_match(finish).await {
            internal(error);
        }
        let result = MatchResult {
            match_id,
            ruleset: state.ruleset.clone(),
            rated: state.rated,
            outcome,
            reason,
            duration_ticks: duration,
            stats,
            seats: state
                .seats
                .iter()
                .map(|seat| {
                    let info = self.seat_info(&state, seat);
                    let change = changes.iter().find(|c| c.account == seat.account);
                    SeatResult {
                        slot: info.slot,
                        team: info.team,
                        account: info.account,
                        login: info.login,
                        nickname: info.nickname,
                        kind: info.kind,
                        hero: seat.hero,
                        before: change.map(|c| c.before.public()),
                        after: change.map(|c| c.after.public()),
                    }
                })
                .collect(),
            finished_at,
        };
        for seat in &state.seats {
            self.release_seat(seat.account, match_id);
            self.notify(
                seat.account,
                Event::MatchResult {
                    result: result.clone(),
                },
            );
            self.forget_if_idle(seat.account);
        }
    }

    /// An account whose match is over is idle again; so is one still counting
    /// down in a lobby whose match died.
    fn release_seat(&mut self, account: AccountId, match_id: MatchId) {
        let at = self.accounts.get(&account).map(|a| a.at.clone());
        match at {
            Some(Where::InMatch(m)) if m == match_id => {
                if let Some(a) = self.accounts.get_mut(&account) {
                    a.at = Where::Idle;
                }
            }
            Some(Where::InLobby(lobby))
                if self
                    .lobbies
                    .get(&lobby)
                    .is_some_and(|l| l.match_id == Some(match_id)) =>
            {
                if let Some(l) = self.lobbies.remove(&lobby) {
                    for member in l.members {
                        if let Some(a) = self.accounts.get_mut(&member.account) {
                            a.at = Where::Idle;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// The Glicko-2 update of both seats of a rated match: the deviation grown
    /// for idle time first, each side against the other's standing before the game.
    async fn rate(&self, state: &MatchState, outcome: Outcome, now: UnixMs) -> Vec<RatingChange> {
        let mut before = Vec::new();
        for seat in &state.seats {
            let row = match self.db.rating(seat.account, state.ruleset.clone()).await {
                Ok(row) => row,
                Err(error) => {
                    internal(error);
                    return Vec::new();
                }
            };
            let standing = match row {
                Some(row) => {
                    let idle_days = now.0.saturating_sub(row.last_played.0) as f64 / 86_400_000.0;
                    row.glicko.after_idle(idle_days)
                }
                None => Glicko::NEW,
            };
            before.push((seat.account, standing));
        }
        if before.len() != 2 {
            return Vec::new();
        }
        let score_of_first = match outcome {
            Outcome::Radiant => 1.0,
            Outcome::Dire => 0.0,
            Outcome::Draw => 0.5,
            Outcome::Null => return Vec::new(),
        };
        let (a, b) = (before[0], before[1]);
        vec![
            RatingChange {
                account: a.0,
                before: a.1,
                after: a.1.updated(&[(b.1, score_of_first)]),
                score: score_of_first,
            },
            RatingChange {
                account: b.0,
                before: b.1,
                after: b.1.updated(&[(a.1, 1.0 - score_of_first)]),
                score: 1.0 - score_of_first,
            },
        ]
    }
}

fn random_key() -> Result<SeatKey, getrandom::Error> {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key)?;
    Ok(SeatKey(key))
}
