//! The queue: one FIFO per ruleset, pairing the oldest accounts as soon as
//! enough wait.

use std::time::Instant;

use radovid_proto::*;

use super::{Hub, Where, internal};

impl Hub {
    pub(super) async fn queue_join(
        &mut self,
        id: AccountId,
        wire: u64,
        ruleset: RulesetId,
    ) -> Result<Reply, Error> {
        let config = self.config.ruleset(&ruleset).ok_or(Error::UnknownRuleset)?;
        if !config.queue {
            return Err(Error::NoQueue);
        }
        let seats = usize::from(config.seats);
        self.check_wire(wire)?;
        self.require_idle(id)?;
        let waiting = {
            let queue = self.queues.entry(ruleset.clone()).or_default();
            queue.push((id, Instant::now()));
            queue.len() as u32
        };
        if let Some(account) = self.accounts.get_mut(&id) {
            account.at = Where::Queued {
                ruleset: ruleset.clone(),
                since: Instant::now(),
            };
        }
        self.pair(&ruleset, seats).await;
        Ok(Reply::Queued { waiting })
    }

    pub(super) fn queue_leave(&mut self, id: AccountId) -> Result<Reply, Error> {
        match self.accounts.get(&id).map(|a| a.at.clone()) {
            Some(Where::Queued { ruleset, .. }) => {
                self.leave_queue(id, &ruleset);
                Ok(Reply::Done)
            }
            _ => Err(Error::NotInQueue),
        }
    }

    pub(super) fn leave_queue(&mut self, id: AccountId, ruleset: &RulesetId) {
        if let Some(queue) = self.queues.get_mut(ruleset) {
            queue.retain(|(account, _)| *account != id);
        }
        if let Some(account) = self.accounts.get_mut(&id) {
            account.at = Where::Idle;
        }
    }

    /// Forms matches while enough accounts wait: the oldest first, in arrival order.
    async fn pair(&mut self, ruleset: &RulesetId, seats: usize) {
        loop {
            let queue = self.queues.entry(ruleset.clone()).or_default();
            if queue.len() < seats {
                return;
            }
            let members: Vec<AccountId> =
                queue.drain(..seats).map(|(account, _)| account).collect();
            if let Err(error) = self.create_match(ruleset.clone(), true, &members).await {
                eprintln!("radovid: cannot form a match: {error}");
                for account in members {
                    if let Some(a) = self.accounts.get_mut(&account) {
                        a.at = Where::Idle;
                    }
                    self.notify(
                        account,
                        Event::Bye {
                            error: internal("match creation"),
                        },
                    );
                }
            }
        }
    }

    /// Refuses a client built against another game wire than the servers run.
    pub(super) fn check_wire(&self, client: u64) -> Result<(), Error> {
        let server = bota_proto::WIRE_FINGERPRINT;
        if client == server {
            Ok(())
        } else {
            Err(Error::BotaWireMismatch { server })
        }
    }
}
