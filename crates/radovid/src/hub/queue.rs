//! The humans and everyone queues of each rated ruleset, in arrival order. An
//! entry keeps its place while it sits in a proposal and after a proposal
//! fails; the pairer forms proposals from the oldest free entries.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use radovid_proto::*;
use tokio::time::Instant;

use super::{Hub, SessionId};

pub(super) struct Entry {
    pub account: AccountId,
    pub ruleset: RulesetId,
    pub since: Instant,
    pub source: IpAddr,
    pub proposal: Option<ProposalId>,
    pub audience: Audience,
    pub search: QueueEntryId,
    /// What the account was last told of the entry's standing, and when.
    told: (Standing, Instant),
}

/// The parts of a `QueueEntry` whose change is news.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Standing {
    position: u32,
    waiting: u32,
    overloaded: bool,
    games_waiting: u32,
}

impl Hub {
    pub(super) fn queue_join(
        &mut self,
        session: SessionId,
        ruleset: RulesetId,
    ) -> Result<Reply, Error> {
        let mut entries = self.join_queues(session, ruleset, &[Audience::Everyone])?;
        Ok(Reply::Queued {
            entry: entries.remove(0),
        })
    }

    pub(super) fn queue_join_audiences(
        &mut self,
        session: SessionId,
        ruleset: RulesetId,
        audiences: Vec<Audience>,
    ) -> Result<Reply, Error> {
        Ok(Reply::QueuesJoined {
            entries: self.join_queues(session, ruleset, &audiences)?,
        })
    }

    fn join_queues(
        &mut self,
        session: SessionId,
        ruleset: RulesetId,
        audiences: &[Audience],
    ) -> Result<Vec<QueueEntry>, Error> {
        self.check_queue_audiences(session, audiences)?;
        let config = Arc::clone(&self.config);
        let rules = config.ruleset(&ruleset).ok_or(Error::UnknownRuleset)?;
        self.admit_queue(session, rules)?;
        if audiences.len()
            > (config.limits.queue_capacity as usize).saturating_sub(self.entries.len())
        {
            return Err(Error::QueueFull {
                retry_after_ms: 10_000,
            });
        }
        let (account, source) = self
            .sessions
            .get(&session)
            .map(|s| (s.account, s.source))
            .ok_or(Error::NotLoggedIn)?;
        let search = QueueEntryId(self.next_entry);
        let mut entries = Vec::with_capacity(audiences.len());
        for &audience in audiences {
            let id = self.insert_queue_entry(account, source, ruleset.clone(), audience, search);
            entries.push(self.entry_status(id).ok_or(Error::Internal)?);
        }
        self.pair_all();
        Ok(entries)
    }

    fn check_queue_audiences(
        &self,
        session: SessionId,
        audiences: &[Audience],
    ) -> Result<(), Error> {
        if audiences.is_empty()
            || audiences.len() > MAX_QUEUE_AUDIENCES
            || audiences.contains(&Audience::BotsOnly)
            || (audiences.len() == 2 && audiences[0] == audiences[1])
        {
            return Err(Error::BadQueueAudiences);
        }
        let session = self.sessions.get(&session).ok_or(Error::NotLoggedIn)?;
        let account = self
            .accounts
            .get(&session.account)
            .ok_or(Error::NotLoggedIn)?;
        if audiences
            .iter()
            .any(|audience| !audience.admits(account.info.kind))
        {
            return Err(Error::AudienceMismatch);
        }
        Ok(())
    }

    fn insert_queue_entry(
        &mut self,
        account: AccountId,
        source: IpAddr,
        ruleset: RulesetId,
        audience: Audience,
        search: QueueEntryId,
    ) -> QueueEntryId {
        let id = QueueEntryId(self.next_entry);
        self.next_entry = self.next_entry.wrapping_add(1).max(1);
        let now = Instant::now();
        let never = (
            Standing {
                position: 0,
                waiting: 0,
                overloaded: false,
                games_waiting: 0,
            },
            now,
        );
        self.entries.insert(
            id,
            Entry {
                account,
                ruleset: ruleset.clone(),
                since: now,
                source,
                proposal: None,
                audience,
                search,
                told: never,
            },
        );
        self.queues.entry((ruleset, audience)).or_default().push(id);
        if let Some(a) = self.accounts.get_mut(&account) {
            a.entries.push(id);
            a.searches.insert(search);
        }
        let status = self
            .entry_status(id)
            .expect("the inserted queue entry exists");
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.told = (standing(&status), now);
        }
        id
    }

    pub(super) fn queue_leave(
        &mut self,
        id: AccountId,
        entry: QueueEntryId,
    ) -> Result<Reply, Error> {
        if self.entries.get(&entry).is_none_or(|e| e.account != id) {
            return Err(Error::NotInQueue);
        }
        self.leave_queue(entry, QueueLeftReason::Cancelled, true);
        Ok(Reply::Done)
    }

    /// Takes an entry out of its queue and tells its account. An entry in an
    /// open proposal declines it, with the cooldown if `cooldown`.
    pub(super) fn leave_queue(
        &mut self,
        entry: QueueEntryId,
        reason: QueueLeftReason,
        cooldown: bool,
    ) {
        let Some(e) = self.entries.get(&entry) else {
            return;
        };
        if let Some(proposal) = e.proposal {
            self.fail_proposal(proposal, &[entry], reason, cooldown);
            return;
        }
        let account = e.account;
        self.remove_search(entry, reason, 0);
        self.forget_if_idle(account);
    }

    /// Consume exactly this search, leaving independent concurrent searches alone.
    pub(super) fn remove_search(
        &mut self,
        entry: QueueEntryId,
        reason: QueueLeftReason,
        cooldown_ms: u32,
    ) {
        let Some(e) = self.entries.get(&entry) else {
            return;
        };
        let (account, search) = (e.account, e.search);
        let entries: Vec<_> = self
            .accounts
            .get(&account)
            .into_iter()
            .flat_map(|account| &account.entries)
            .filter(|id| self.entries.get(id).is_some_and(|e| e.search == search))
            .copied()
            .collect();
        for entry in entries {
            self.remove_entry(entry);
            self.notify(
                account,
                Event::QueueLeft {
                    entry,
                    reason,
                    cooldown_ms,
                },
            );
        }
    }

    /// Forgets an entry everywhere; the caller tells whom it concerns.
    pub(super) fn remove_entry(&mut self, entry: QueueEntryId) {
        let Some(e) = self.entries.remove(&entry) else {
            return;
        };
        if let Some(queue) = self.queues.get_mut(&(e.ruleset, e.audience)) {
            queue.retain(|id| *id != entry);
        }
        if let Some(account) = self.accounts.get_mut(&e.account) {
            account.entries.retain(|id| *id != entry);
            if !account.entries.iter().any(|id| {
                self.entries
                    .get(id)
                    .is_some_and(|other| other.search == e.search)
            }) {
                account.searches.remove(&e.search);
            }
        }
    }

    /// Where an entry stands.
    pub(super) fn entry_status(&self, entry: QueueEntryId) -> Option<QueueEntry> {
        let e = self.entries.get(&entry)?;
        let queue = self.queues.get(&(e.ruleset.clone(), e.audience))?;
        let position = queue.iter().position(|id| *id == entry)? as u32 + 1;
        Some(QueueEntry {
            entry,
            ruleset: e.ruleset.clone(),
            for_ms: e.since.elapsed().as_millis().min(u128::from(u32::MAX)) as u32,
            position,
            waiting: queue.len() as u32,
            overloaded: self.pairing_paused(),
            games_waiting: self.waiting.len() as u32,
            audience: e.audience,
            search: e.search,
        })
    }

    /// Pairing waits while as many queued matches are being formed or wait
    /// for a slot as the service has slots: more could not start anyway.
    pub(super) fn pairing_paused(&self) -> bool {
        let waiting = self
            .waiting
            .iter()
            .filter(|m| self.matches.get(m).is_some_and(|m| !m.lobby))
            .count();
        self.proposals.len() + waiting >= self.config.limits.game_capacity as usize
    }

    /// Forms every proposal the queues allow.
    pub(super) fn pair_all(&mut self) {
        let config = Arc::clone(&self.config);
        for ruleset in config.rulesets.iter().filter(|r| r.rated) {
            for audience in [Audience::HumansOnly, Audience::Everyone] {
                while !self.pairing_paused() {
                    let Some(picked) =
                        self.pick(&(ruleset.ruleset_id(), audience), ruleset.seats())
                    else {
                        break;
                    };
                    self.propose(ruleset.ruleset_id(), picked);
                }
            }
        }
    }

    /// The oldest free entries that fill `seats`: never one account twice,
    /// and an entry from a source already picked only when no other fits.
    fn pick(&self, queue: &(RulesetId, Audience), seats: usize) -> Option<Vec<QueueEntryId>> {
        let free: Vec<(QueueEntryId, &Entry)> = self
            .queues
            .get(queue)?
            .iter()
            .filter_map(|id| self.entries.get(id).map(|e| (*id, e)))
            .filter(|(_, e)| e.proposal.is_none())
            .collect();
        for start in 0..free.len() {
            let mut picked = vec![free[start]];
            while picked.len() < seats {
                let fits = |(_, e): &&(QueueEntryId, &Entry)| {
                    picked.iter().all(|(_, p)| p.account != e.account)
                };
                let rest = &free[start + 1..];
                let next = rest
                    .iter()
                    .filter(fits)
                    .find(|(_, e)| picked.iter().all(|(_, p)| p.source != e.source))
                    .or_else(|| rest.iter().find(fits));
                match next {
                    Some(next) => picked.push(*next),
                    None => break,
                }
            }
            if picked.len() == seats {
                return Some(picked.into_iter().map(|(id, _)| id).collect());
            }
        }
        None
    }

    /// Tells every free entry whose standing changed, at most once per
    /// `QUEUE_STATUS_INTERVAL_MS`.
    pub(super) fn send_queue_statuses(&mut self) {
        let now = Instant::now();
        let interval = Duration::from_millis(u64::from(QUEUE_STATUS_INTERVAL_MS));
        let due: Vec<(AccountId, QueueEntry)> = self
            .entries
            .iter()
            .filter(|(_, e)| e.proposal.is_none() && now.duration_since(e.told.1) >= interval)
            .filter_map(|(id, e)| {
                let status = self.entry_status(*id)?;
                (standing(&status) != e.told.0).then_some((e.account, status))
            })
            .collect();
        for (account, status) in due {
            if let Some(e) = self.entries.get_mut(&status.entry) {
                e.told = (standing(&status), now);
            }
            self.notify(account, Event::QueueStatus { entry: status });
        }
    }
}

fn standing(status: &QueueEntry) -> Standing {
    Standing {
        position: status.position,
        waiting: status.waiting,
        overloaded: status.overloaded,
        games_waiting: status.games_waiting,
    }
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
