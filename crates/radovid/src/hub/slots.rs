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
            // A game whose result was told at its line may still be on its way out, holding
            // its port: it counts until it is gone.
            let running = self.config.game_ports.count() - self.free_ports.len();
            if all.max(running) >= capacity || self.free_ports.is_empty() {
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
                    self.accounts.get(&s.account).map_or_else(
                        || (String::new(), s.account),
                        |a| (a.info.nickname.clone(), a.identity.owner(s.account)),
                    )
                })
                .collect::<Vec<_>>(),
        );
        let seats: Vec<SeatSpec> = state
            .seats
            .iter()
            .zip(names)
            .map(|(seat, name)| SeatSpec {
                account: seat.account,
                version: self
                    .accounts
                    .get(&seat.account)
                    .and_then(|account| account.identity.version().map(str::to_owned)),
                ticket: seat.ticket,
                team: seat.team,
                name,
                kind: self
                    .accounts
                    .get(&seat.account)
                    .map_or(AccountKind::Human, |a| a.info.kind),
            })
            .collect();
        // Cheats are a lobby's, never a rated match's.
        assert!(
            !(state.cheats && (state.rated || !state.lobby)),
            "match {match_id:?}: cheats in a rated or queued match"
        );
        let spec = LaunchSpec {
            public_identity: self
                .config
                .public_identity
                .as_deref()
                .and_then(radovid_proto::shareable_matchmaker_identity),
            match_id,
            port,
            cheats: state.cheats,
            seats,
            artifacts: self
                .config
                .artifacts_of(ruleset)
                .into_iter()
                .filter(|artifact| artifact.kept)
                .collect(),
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
            patience: self.config.patience,
            match_id,
            ruleset: ruleset.clone(),
            rated: state.rated,
            spec,
            hub: self.me.clone(),
        };
        let database = self.db.clone();
        tokio::spawn(async move {
            let mut supervisor = supervisor;
            supervisor.hide_private_profile_bindings(&database).await;
            supervisor.run(stopped).await;
        });
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

/// Names for the game: the nicknames cleaned to `[A-Za-z0-9_]`, with the slot as a suffix where
/// two different owners share one (nicknames are not unique) and for empty ones. Versions of one
/// bot share its nickname and owner: the version tells them apart, so they keep the plain name.
fn seat_names(seats: &[(String, AccountId)]) -> Vec<String> {
    seats
        .iter()
        .enumerate()
        .map(|(slot, (nickname, owner))| {
            let clean: String = nickname
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            let shared_with_another_owner = seats
                .iter()
                .any(|(other, other_owner)| other == nickname && other_owner != owner);
            if clean.is_empty() || shared_with_another_owner {
                format!("{clean}_{slot}")
            } else {
                clean
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(seats: &[(&str, u64)]) -> Vec<String> {
        let seats: Vec<(String, AccountId)> = seats
            .iter()
            .map(|(nickname, owner)| ((*nickname).to_owned(), AccountId(*owner)))
            .collect();
        seat_names(&seats)
    }

    #[test]
    fn two_humans_of_one_nickname_get_distinct_game_names() {
        assert_eq!(names(&[("Ann", 1), ("Ann", 2)]), ["Ann_0", "Ann_1"]);
    }

    #[test]
    fn versions_of_one_bot_keep_the_bot_name_because_the_version_tells_them_apart() {
        assert_eq!(names(&[("Bot", 7), ("Bot", 7)]), ["Bot", "Bot"]);
    }

    #[test]
    fn a_human_named_like_a_bot_is_told_apart_from_its_versions() {
        assert_eq!(
            names(&[("Bot", 7), ("Bot", 7), ("Bot", 2)]),
            ["Bot_0", "Bot_1", "Bot_2"]
        );
    }

    #[test]
    fn unique_and_empty_nicknames_are_cleaned_and_never_empty() {
        assert_eq!(names(&[("a b", 1), ("", 2)]), ["a_b", "_1"]);
    }
}
