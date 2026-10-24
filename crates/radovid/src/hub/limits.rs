//! What an account may start: the per-account and per-source limits on games
//! in flight and the cooldown after a declined proposal.

use std::net::IpAddr;
use std::time::Duration;

use radovid_proto::*;
use tokio::time::Instant;

use super::{Hub, SessionId, millis_until};
use crate::config::RulesetConfig;

/// What `QueueFull` suggests waiting.
const QUEUE_FULL_RETRY_MS: u32 = 10_000;

/// The cooldowns of one account; forgotten once nothing in it counts.
#[derive(Default)]
pub(super) struct Penalty {
    decline_until: Option<Instant>,
}

impl Penalty {
    fn spent(&self, now: Instant) -> bool {
        self.decline_until.is_none_or(|t| t <= now)
    }
}

impl Hub {
    /// Whether the caller may start one more game of `ruleset` (queue it,
    /// open or join a lobby for it).
    pub(super) fn admit_game(
        &self,
        session: SessionId,
        ruleset: &RulesetConfig,
    ) -> Result<(), Error> {
        let s = self.sessions.get(&session).ok_or(Error::NotLoggedIn)?;
        if s.game_fingerprint != ruleset.game_fingerprint {
            return Err(Error::GameVersionMismatch {
                server: ruleset.game_fingerprint,
            });
        }
        let account = self.accounts.get(&s.account).ok_or(Error::NotLoggedIn)?;
        if !ruleset.admits(account.info.kind) {
            return Err(Error::AudienceMismatch);
        }
        let limit = ruleset.games_per_account(&self.config.limits, account.info.kind);
        if account.in_flight() >= usize::from(limit) {
            return Err(Error::TooManyGames { limit });
        }
        let per_source = self.config.limits.games_per_source;
        if self.source_in_flight(s.source) >= usize::from(per_source) {
            return Err(Error::TooManyGamesFromSource { limit: per_source });
        }
        Ok(())
    }

    /// `admit_game`, and room in the queues and no decline cooldown.
    pub(super) fn admit_queue(
        &self,
        session: SessionId,
        ruleset: &RulesetConfig,
    ) -> Result<(), Error> {
        if !ruleset.rated {
            return Err(Error::NoQueue);
        }
        let id = self
            .sessions
            .get(&session)
            .ok_or(Error::NotLoggedIn)?
            .account;
        if let Some(until) = self
            .penalties
            .get(&id)
            .and_then(|p| p.decline_until)
            .filter(|t| *t > Instant::now())
        {
            return Err(Error::QueueCooldown {
                retry_after_ms: millis_until(until),
            });
        }
        self.admit_game(session, ruleset)?;
        if self.entries.len() >= self.config.limits.queue_capacity as usize {
            return Err(Error::QueueFull {
                retry_after_ms: QUEUE_FULL_RETRY_MS,
            });
        }
        Ok(())
    }

    /// Games in flight of every account at `source`: queue entries, lobby
    /// seats, join requests and unfinished matches started from there.
    fn source_in_flight(&self, source: IpAddr) -> usize {
        let entries = self.entries.values().filter(|e| e.source == source).count();
        let lobbies: usize = self
            .lobbies
            .values()
            .map(|l| {
                l.members.iter().filter(|m| m.source == source).count()
                    + l.pending.iter().filter(|p| p.source == source).count()
            })
            .sum();
        let matches: usize = self
            .matches
            .values()
            .map(|m| m.seats.iter().filter(|s| s.source == source).count())
            .sum();
        entries + lobbies + matches
    }

    /// Milliseconds before the account may queue again.
    pub(super) fn cooldown_ms(&self, id: AccountId) -> u32 {
        let Some(penalty) = self.penalties.get(&id) else {
            return 0;
        };
        let now = Instant::now();
        penalty
            .decline_until
            .filter(|t| *t > now)
            .map_or(0, millis_until)
    }

    /// A decline, or a silence: out of the queues for a while. Returns how long.
    pub(super) fn decline_cooldown(&mut self, id: AccountId) -> u32 {
        let secs = self.config.limits.decline_cooldown_secs;
        if secs > 0 {
            let penalty = self.penalties.entry(id).or_default();
            penalty.decline_until = Some(Instant::now() + Duration::from_secs(u64::from(secs)));
        }
        secs * 1000
    }

    pub(super) fn sweep_penalties(&mut self) {
        let now = Instant::now();
        self.penalties.retain(|_, p| !p.spent(now));
    }
}
