//! Matches: forming them from accepted proposals and lobbies, handing the
//! seats to their players once the game server is ready, and recording how
//! they ended.

use std::collections::HashMap;
use std::net::IpAddr;

use radovid_proto::*;
use tokio::sync::oneshot;

use super::{Hub, internal};
use crate::config::RulesetConfig;
use crate::db::{Finish, NewSeat, now_ms};
use crate::supervisor::End;

pub(super) struct SeatState {
    pub account: AccountId,
    pub slot: u8,
    pub team: u8,
    pub ticket: Ticket,
    /// Where the seat's queue entry or lobby seat came from.
    pub source: IpAddr,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    /// Formed, waiting for a game slot.
    Waiting,
    /// Launched, before the game's `READY`.
    Starting,
    /// The players may connect.
    Running,
}

pub(super) struct MatchState {
    pub ruleset: RulesetId,
    pub rated: bool,
    /// Came from a lobby: holds one of the `lobby_games_max` slots.
    pub lobby: bool,
    pub seats: Vec<SeatState>,
    pub phase: Phase,
    pub stop: Option<oneshot::Sender<()>>,
    /// Each seat's rating when the match was formed, for display.
    pub ratings: HashMap<AccountId, Rating>,
    /// Where the players connect, once launched.
    pub game_url: String,
    /// The last `ahead` the seats were told in `MatchQueued`.
    pub told_ahead: Option<u32>,
}

impl Hub {
    /// Records a match with its seats in slot order and puts it at the end of
    /// the slot queue; the caller then calls `try_launch`.
    pub(super) async fn form_match(
        &mut self,
        ruleset: &RulesetConfig,
        lobby: bool,
        members: Vec<(AccountId, IpAddr)>,
    ) -> Result<MatchId, Error> {
        let seats: Vec<NewSeat> = members
            .iter()
            .enumerate()
            .map(|(slot, (account, _))| NewSeat {
                account: *account,
                slot: slot as u8,
                team: ruleset.team_of(slot),
            })
            .collect();
        let rated = ruleset.rated && !lobby;
        let match_id = self
            .db
            .create_match(ruleset.ruleset_id(), rated, seats.clone())
            .await
            .map_err(internal)?;
        let mut states = Vec::new();
        for (seat, (_, source)) in seats.iter().zip(&members) {
            states.push(SeatState {
                account: seat.account,
                slot: seat.slot,
                team: seat.team,
                ticket: random_ticket().map_err(internal)?,
                source: *source,
            });
        }
        let ratings = self.seat_ratings(&members, &ruleset.ruleset_id()).await;
        self.matches.insert(
            match_id,
            MatchState {
                ruleset: ruleset.ruleset_id(),
                rated,
                lobby,
                seats: states,
                phase: Phase::Waiting,
                stop: None,
                ratings,
                game_url: String::new(),
                told_ahead: None,
            },
        );
        for (account, _) in &members {
            if let Some(a) = self.accounts.get_mut(account) {
                a.matches.push(match_id);
            }
        }
        self.waiting.push_back(match_id);
        Ok(match_id)
    }

    /// Each member's stored rating; a member without one or a failed lookup shows none.
    async fn seat_ratings(
        &self,
        members: &[(AccountId, IpAddr)],
        ruleset: &RulesetId,
    ) -> HashMap<AccountId, Rating> {
        let mut ratings = HashMap::new();
        for (account, _) in members {
            if let Ok(Some(row)) = self.db.rating(*account, ruleset.clone()).await {
                ratings.insert(*account, row.glicko.public());
            }
        }
        ratings
    }

    /// The game server is ready: every seat gets its handoff.
    pub(super) fn match_ready(&mut self, match_id: MatchId) {
        let Some(state) = self.matches.get_mut(&match_id) else {
            return;
        };
        state.phase = Phase::Running;
        let accounts: Vec<AccountId> = state.seats.iter().map(|s| s.account).collect();
        for account in accounts {
            if let Some(handoff) = self.handoff_for(match_id, account) {
                self.notify(account, Event::MatchFound { handoff });
            }
        }
    }

    pub(super) fn match_handoff(&self, id: AccountId, match_id: MatchId) -> Result<Reply, Error> {
        let state = self.matches.get(&match_id).ok_or(Error::UnknownMatch)?;
        if state.seats.iter().all(|s| s.account != id) {
            return Err(Error::UnknownMatch);
        }
        self.handoff_for(match_id, id)
            .map(|handoff| Reply::Handoff { handoff })
            .ok_or(Error::NotInMatch)
    }

    pub(super) fn my_matches(&self, id: AccountId) -> Reply {
        let list = self
            .accounts
            .get(&id)
            .map(|a| {
                a.matches
                    .iter()
                    .filter_map(|m| self.handoff_for(*m, id))
                    .collect()
            })
            .unwrap_or_default();
        Reply::Handoffs { list }
    }

    fn seat_info(&self, state: &MatchState, seat: &SeatState) -> SeatInfo {
        let account = self.accounts.get(&seat.account);
        let kind = account.map_or(AccountKind::Human, |a| a.info.kind);
        SeatInfo {
            slot: seat.slot,
            team: seat.team,
            account: seat.account,
            nickname: account.map(|a| a.info.nickname.clone()).unwrap_or_default(),
            kind,
            bot_version: account
                .and_then(|a| a.bot_version.clone())
                .filter(|_| kind == AccountKind::Bot),
            rating: state.ratings.get(&seat.account).copied(),
        }
    }

    /// The account's handoff, once the match runs.
    fn handoff_for(&self, match_id: MatchId, id: AccountId) -> Option<Handoff> {
        let state = self.matches.get(&match_id)?;
        if state.phase != Phase::Running {
            return None;
        }
        let rules = self.config.ruleset(&state.ruleset)?;
        let mine = state.seats.iter().find(|s| s.account == id)?;
        Some(Handoff {
            match_id,
            ruleset: state.ruleset.clone(),
            rated: state.rated,
            game: rules.game.clone(),
            game_url: state.game_url.clone(),
            ticket: mine.ticket,
            slot: mine.slot,
            team: mine.team,
            seats: state
                .seats
                .iter()
                .map(|s| self.seat_info(state, s))
                .collect(),
            game_fingerprint: rules.game_fingerprint,
        })
    }

    /// Records the end, rates a rated match with a result (arrival order, one
    /// transaction each), tells every seat, and hands the slot on.
    pub(super) async fn match_ended(&mut self, match_id: MatchId, end: End) {
        let Some(state) = self.matches.remove(&match_id) else {
            return;
        };
        self.waiting.retain(|m| *m != match_id);
        let leaver = match &end {
            End::Result(result) => result.abandoned,
            _ => None,
        };
        let (outcome, reason, duration_ms, report) = match end {
            End::Result(result) => match result.winner {
                Some(team) => (
                    Outcome::Winner { team },
                    if result.abandoned.is_some() {
                        EndReason::Abandon
                    } else {
                        EndReason::Decided
                    },
                    result.duration_ms,
                    result.report,
                ),
                None => (
                    Outcome::Draw,
                    EndReason::Draw,
                    result.duration_ms,
                    result.report,
                ),
            },
            End::Abandoned => (Outcome::Null, EndReason::Abandoned, 0, Vec::new()),
            End::Crashed(why) => {
                eprintln!("radovid: match {} lost: {why}", match_id.0);
                (Outcome::Null, EndReason::Crashed, 0, Vec::new())
            }
            End::Stopped => (Outcome::Null, EndReason::Stopped, 0, Vec::new()),
        };
        let finished_at = now_ms();
        let finish = Finish {
            match_id,
            ruleset: state.ruleset.clone(),
            rate: state.rated && outcome != Outcome::Null,
            outcome,
            reason,
            duration_ms,
            report: report.clone(),
            seats: state
                .seats
                .iter()
                .map(|s| NewSeat {
                    account: s.account,
                    slot: s.slot,
                    team: s.team,
                })
                .collect(),
            finished_at,
        };
        let changes = self.db.finish_match(finish).await.unwrap_or_else(|error| {
            internal(error);
            Vec::new()
        });
        let result = MatchResult {
            match_id,
            ruleset: state.ruleset.clone(),
            rated: state.rated,
            outcome,
            reason,
            duration_ms,
            game_report: report,
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
                        nickname: info.nickname,
                        kind: info.kind,
                        before: change.map(|c| c.before.public()),
                        after: change.map(|c| c.after.public()),
                    }
                })
                .collect(),
            finished_at,
        };
        for seat in &state.seats {
            if let Some(a) = self.accounts.get_mut(&seat.account) {
                a.matches.retain(|m| *m != match_id);
            }
            match reason {
                EndReason::Abandoned => self.note_abandon(seat.account),
                EndReason::Abandon if Some(seat.slot) == leaver => self.note_abandon(seat.account),
                EndReason::Abandon => self.note_played(seat.account),
                EndReason::Decided | EndReason::Draw => self.note_played(seat.account),
                EndReason::Crashed | EndReason::Stopped => {}
            }
            self.notify(
                seat.account,
                Event::MatchResult {
                    result: result.clone(),
                },
            );
            self.forget_if_idle(seat.account);
        }
        self.try_launch();
        self.pair_all();
    }
}

fn random_ticket() -> Result<Ticket, getrandom::Error> {
    let mut ticket = [0u8; 32];
    getrandom::fill(&mut ticket)?;
    Ok(Ticket(ticket))
}
