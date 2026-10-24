//! Game slots: at most `game_capacity` launched games, lobby games at most
//! `lobby_games_max` of them, each on a port of its own. Formed matches wait
//! in one queue in the order they were formed and are told where they stand.

use std::sync::Arc;

use radovid_proto::*;
use tokio::sync::oneshot;

use super::Hub;
use super::matches::Phase;
use crate::launcher::LaunchSpec;
use crate::supervisor::Supervisor;
use crate::template::{self, SeatSpec, Vars};

impl Hub {
    /// Launches waiting matches while slots and ports are free: the oldest
    /// first, a lobby match only while lobby games hold fewer than their share.
    pub(super) fn try_launch(&mut self) {
        let capacity = self.config.limits.game_capacity as usize;
        let lobby_share = self.config.limits.lobby_games_max as usize;
        loop {
            let launched = self.matches.values().filter(|m| m.phase != Phase::Waiting);
            let (all, lobby) = launched.fold((0, 0), |(all, lobby), m| {
                (all + 1, lobby + usize::from(m.lobby))
            });
            if all >= capacity || self.free_ports.is_empty() {
                break;
            }
            let next = self.waiting.iter().position(|m| {
                self.matches
                    .get(m)
                    .is_some_and(|m| !m.lobby || lobby < lobby_share)
            });
            let Some(next) = next.and_then(|i| self.waiting.remove(i)) else {
                break;
            };
            self.launch(next);
        }
        self.tell_waiting();
    }

    fn launch(&mut self, match_id: MatchId) {
        let config = Arc::clone(&self.config);
        let (Some(state), Some(port)) =
            (self.matches.get_mut(&match_id), self.free_ports.pop_first())
        else {
            return;
        };
        let Some(ruleset) = config.ruleset(&state.ruleset) else {
            return;
        };
        let names = seat_names(
            &state
                .seats
                .iter()
                .map(|s| {
                    self.accounts
                        .get(&s.account)
                        .map_or_else(String::new, |a| a.info.nickname.clone())
                })
                .collect::<Vec<_>>(),
        );
        let seats: Vec<SeatSpec> = state
            .seats
            .iter()
            .zip(names)
            .map(|(seat, name)| SeatSpec {
                ticket: seat.ticket,
                team: seat.team,
                name,
                kind: self
                    .accounts
                    .get(&seat.account)
                    .map_or(AccountKind::Human, |a| a.info.kind),
            })
            .collect();
        let spec = LaunchSpec {
            match_id,
            port,
            seats,
        };
        let vars = Vars {
            match_id,
            port,
            seats: &spec.seats,
            artifact: None,
        };
        state.game_url = template::expand(&ruleset.launcher.public_url, &vars);
        state.phase = Phase::Starting;
        let (stop, stopped) = oneshot::channel();
        state.stop = Some(stop);
        let first_word = state.told_ahead.is_none();
        state.told_ahead = Some(0);
        let accounts: Vec<AccountId> = state.seats.iter().map(|s| s.account).collect();
        let supervisor = Supervisor {
            match_id,
            ruleset: ruleset.clone(),
            spec,
            hub: self.me.clone(),
        };
        tokio::spawn(supervisor.run(stopped));
        if first_word {
            for account in accounts {
                self.notify(
                    account,
                    Event::MatchQueued {
                        match_id,
                        ahead: 0,
                        overloaded: false,
                    },
                );
            }
        }
    }

    /// Tells the seats of every waiting match how many wait before it, when
    /// that changed.
    fn tell_waiting(&mut self) {
        let mut news = Vec::new();
        for (ahead, match_id) in self.waiting.iter().enumerate() {
            let Some(state) = self.matches.get_mut(match_id) else {
                continue;
            };
            let ahead = ahead as u32;
            if state.told_ahead != Some(ahead) {
                state.told_ahead = Some(ahead);
                news.push((
                    *match_id,
                    ahead,
                    state.seats.iter().map(|s| s.account).collect::<Vec<_>>(),
                ));
            }
        }
        for (match_id, ahead, accounts) in news {
            for account in accounts {
                self.notify(
                    account,
                    Event::MatchQueued {
                        match_id,
                        ahead,
                        overloaded: true,
                    },
                );
            }
        }
    }
}

/// Names for the game: the nicknames, an account id's worth of suffix where
/// two coincide (`_<slot>`), never empty.
fn seat_names(nicknames: &[String]) -> Vec<String> {
    nicknames
        .iter()
        .enumerate()
        .map(|(slot, nickname)| {
            let clean: String = nickname
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            if clean.is_empty() || nicknames.iter().filter(|n| *n == nickname).count() > 1 {
                format!("{clean}_{slot}")
            } else {
                clean
            }
        })
        .collect()
}
