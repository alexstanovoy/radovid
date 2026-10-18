//! Proposals: a pairing every player must accept before a match exists. A
//! decline or a silence fails it; the accepters keep their queue entries and
//! places, the others lose theirs and cool down.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use radovid_proto::*;
use tokio::time::{Instant, sleep_until};

use super::{Cmd, Hub, millis_until};

pub(super) struct ProposalState {
    pub ruleset: RulesetId,
    /// Slot order: the oldest entry first.
    pub entries: Vec<QueueEntryId>,
    pub accepted: Vec<bool>,
    pub deadline: Instant,
}

impl ProposalState {
    fn accepted_count(&self) -> u8 {
        self.accepted.iter().filter(|a| **a).count() as u8
    }
}

impl Hub {
    pub(super) fn propose(&mut self, ruleset: RulesetId, entries: Vec<QueueEntryId>) {
        let id = ProposalId(self.next_proposal);
        self.next_proposal = self.next_proposal.wrapping_add(1).max(1);
        let timeout = Duration::from_secs(u64::from(self.config.limits.accept_timeout_secs));
        let deadline = Instant::now() + timeout;
        for entry in &entries {
            if let Some(e) = self.entries.get_mut(entry) {
                e.proposal = Some(id);
            }
        }
        self.proposals.insert(
            id,
            ProposalState {
                ruleset,
                accepted: vec![false; entries.len()],
                entries: entries.clone(),
                deadline,
            },
        );
        let me = self.me.clone();
        tokio::spawn(async move {
            sleep_until(deadline).await;
            let _ = me.send(Cmd::ProposalDeadline { proposal: id }).await;
        });
        for entry in entries {
            let Some(account) = self.entries.get(&entry).map(|e| e.account) else {
                continue;
            };
            if let Some(proposal) = self.proposal_for(entry) {
                self.notify(account, Event::MatchProposed { proposal });
            }
        }
    }

    /// The open proposal an entry sits in, as its account sees it.
    pub(super) fn proposal_for(&self, entry: QueueEntryId) -> Option<Proposal> {
        let e = self.entries.get(&entry)?;
        let id = e.proposal?;
        let p = self.proposals.get(&id)?;
        Some(Proposal {
            proposal: id,
            entry,
            ruleset: p.ruleset.clone(),
            accept_by_ms: millis_until(p.deadline),
            accepted: p.accepted_count(),
            of: p.entries.len() as u8,
        })
    }

    /// The caller's entry in a proposal.
    fn own_entry(&self, id: AccountId, proposal: ProposalId) -> Result<usize, Error> {
        let p = self
            .proposals
            .get(&proposal)
            .ok_or(Error::UnknownProposal)?;
        p.entries
            .iter()
            .position(|e| self.entries.get(e).is_some_and(|e| e.account == id))
            .ok_or(Error::UnknownProposal)
    }

    pub(super) async fn match_accept(
        &mut self,
        id: AccountId,
        proposal: ProposalId,
    ) -> Result<Reply, Error> {
        let slot = self.own_entry(id, proposal)?;
        let p = self
            .proposals
            .get_mut(&proposal)
            .ok_or(Error::UnknownProposal)?;
        if p.deadline <= Instant::now() {
            self.proposal_deadline(proposal);
            return Err(Error::ProposalExpired);
        }
        if p.accepted[slot] {
            return Ok(Reply::Done);
        }
        p.accepted[slot] = true;
        let (accepted, of) = (p.accepted_count(), p.entries.len() as u8);
        let accounts: Vec<AccountId> = p
            .entries
            .iter()
            .filter_map(|e| self.entries.get(e).map(|e| e.account))
            .collect();
        for account in accounts {
            self.notify(
                account,
                Event::ProposalUpdate {
                    proposal,
                    accepted,
                    of,
                },
            );
        }
        if accepted == of {
            self.proposal_accepted(proposal).await;
        }
        Ok(Reply::Done)
    }

    pub(super) fn match_decline(
        &mut self,
        id: AccountId,
        proposal: ProposalId,
    ) -> Result<Reply, Error> {
        let slot = self.own_entry(id, proposal)?;
        let entry = self.proposals[&proposal].entries[slot];
        self.fail_proposal(proposal, &[entry], QueueLeftReason::Declined, true);
        Ok(Reply::Done)
    }

    /// Time is up: whoever has not accepted declined.
    pub(super) fn proposal_deadline(&mut self, proposal: ProposalId) {
        let Some(p) = self.proposals.get(&proposal) else {
            return;
        };
        let silent: Vec<QueueEntryId> = p
            .entries
            .iter()
            .zip(&p.accepted)
            .filter(|(_, accepted)| !**accepted)
            .map(|(entry, _)| *entry)
            .collect();
        self.fail_proposal(proposal, &silent, QueueLeftReason::TimedOut, true);
    }

    /// Ends a proposal without a match: the entries in `out` leave their
    /// queue (their accounts cooling down if `cooldown`), the others are free
    /// again at their old places.
    pub(super) fn fail_proposal(
        &mut self,
        proposal: ProposalId,
        out: &[QueueEntryId],
        reason: QueueLeftReason,
        cooldown: bool,
    ) {
        let Some(p) = self.proposals.remove(&proposal) else {
            return;
        };
        for entry in p.entries {
            let Some(account) = self.entries.get(&entry).map(|e| e.account) else {
                continue;
            };
            if out.contains(&entry) {
                self.remove_entry(entry);
                let cooldown_ms = if cooldown {
                    self.decline_cooldown(account)
                } else {
                    0
                };
                self.notify(
                    account,
                    Event::QueueLeft {
                        entry,
                        reason,
                        cooldown_ms,
                    },
                );
                self.forget_if_idle(account);
            } else {
                if let Some(e) = self.entries.get_mut(&entry) {
                    e.proposal = None;
                }
                self.notify(
                    account,
                    Event::ProposalFailed {
                        proposal,
                        back_in_queue: true,
                    },
                );
            }
        }
        self.pair_all();
    }

    /// Everyone accepted: the entries become the seats of a match, which
    /// waits for a game slot.
    async fn proposal_accepted(&mut self, proposal: ProposalId) {
        let Some(p) = self.proposals.get(&proposal) else {
            return;
        };
        let config = Arc::clone(&self.config);
        let Some(rules) = config.ruleset(&p.ruleset) else {
            return;
        };
        let entries = p.entries.clone();
        let members: Vec<(AccountId, IpAddr)> = entries
            .iter()
            .filter_map(|e| self.entries.get(e).map(|e| (e.account, e.source)))
            .collect();
        match self.form_match(rules, false, members).await {
            Ok(_) => {
                self.proposals.remove(&proposal);
                for entry in entries {
                    self.remove_entry(entry);
                }
                self.try_launch();
                self.pair_all();
            }
            Err(_) => self.fail_proposal(proposal, &[], QueueLeftReason::Cancelled, false),
        }
    }
}
