//! Read-only lookups and the admin tools.

use radovid_proto::*;

use super::matches::Phase;
use super::{Hub, internal};
use crate::db::{Ban, now_ms};
use crate::supervisor::End;

impl Hub {
    pub(super) async fn public_account(&mut self, id: AccountId) -> Result<Reply, Error> {
        let row = self
            .db
            .account_by_id(id)
            .await
            .map_err(internal)?
            .ok_or(Error::UnknownAccount)?;
        let ratings = self.db.ratings_of(id).await.map_err(internal)?;
        let online = self
            .accounts
            .get(&id)
            .is_some_and(|a| !a.sessions.is_empty());
        let kind = row.kind;
        Ok(Reply::Account {
            account: PublicAccount {
                id,
                nickname: row.nickname,
                kind,
                online,
                bot_version: row.bot_version.filter(|_| kind == AccountKind::Bot),
                ratings: ratings
                    .into_iter()
                    .map(|(ruleset, rating)| (ruleset, rating.summary()))
                    .collect(),
            },
        })
    }

    pub(super) async fn recent_matches(
        &mut self,
        account: AccountId,
        limit: u8,
    ) -> Result<Reply, Error> {
        self.db
            .account_by_id(account)
            .await
            .map_err(internal)?
            .ok_or(Error::UnknownAccount)?;
        let list = self
            .db
            .recent_matches(account, limit.min(MAX_RECENT_MATCHES))
            .await
            .map_err(internal)?;
        Ok(Reply::Matches { list })
    }

    pub(super) async fn ladder(
        &mut self,
        ruleset: RulesetId,
        offset: u32,
        limit: u8,
    ) -> Result<Reply, Error> {
        let config = self.config.ruleset(&ruleset).ok_or(Error::UnknownRuleset)?;
        if !config.rated {
            return Err(Error::NoQueue);
        }
        let (total, rows) = self
            .db
            .ladder(ruleset, offset, limit.min(MAX_LADDER_PAGE))
            .await
            .map_err(internal)?;
        Ok(Reply::LadderPage { total, rows })
    }

    fn require_admin(&self, id: AccountId) -> Result<(), Error> {
        if self.accounts.get(&id).is_some_and(|a| a.info.admin) {
            Ok(())
        } else {
            Err(Error::Forbidden)
        }
    }

    async fn require_account(&self, id: AccountId) -> Result<(), Error> {
        self.db
            .account_by_id(id)
            .await
            .map_err(internal)?
            .map(|_| ())
            .ok_or(Error::UnknownAccount)
    }

    pub(super) async fn admin_ban(
        &mut self,
        admin: AccountId,
        target: AccountId,
        reason: String,
        until: Option<UnixMs>,
    ) -> Result<Reply, Error> {
        self.require_admin(admin)?;
        if !valid_ban_reason(&reason) {
            return Err(Error::Malformed);
        }
        self.require_account(target).await?;
        let ban = Ban {
            reason: reason.clone(),
            until,
        };
        self.db.set_ban(target, Some(ban)).await.map_err(internal)?;
        if until.is_none_or(|u| u > now_ms()) {
            // Sessions end, and with them the queue and lobby places; a running
            // match finishes and counts.
            self.bye(target, Error::Banned { reason, until });
        }
        Ok(Reply::Done)
    }

    pub(super) async fn admin_unban(
        &mut self,
        admin: AccountId,
        target: AccountId,
    ) -> Result<Reply, Error> {
        self.require_admin(admin)?;
        self.require_account(target).await?;
        self.db.set_ban(target, None).await.map_err(internal)?;
        Ok(Reply::Done)
    }

    pub(super) async fn admin_set_kind(
        &mut self,
        admin: AccountId,
        target: AccountId,
        kind: AccountKind,
    ) -> Result<Reply, Error> {
        self.require_admin(admin)?;
        self.require_account(target).await?;
        self.db.set_kind(target, kind).await.map_err(internal)?;
        if let Some(account) = self.accounts.get_mut(&target) {
            account.info.kind = kind;
        }
        Ok(Reply::Done)
    }

    /// Stops a match: a waiting one ends at once, a launched one when its
    /// game server is gone. Either is a null.
    pub(super) async fn admin_stop(
        &mut self,
        admin: AccountId,
        match_id: MatchId,
    ) -> Result<Reply, Error> {
        self.require_admin(admin)?;
        let state = self.matches.get_mut(&match_id).ok_or(Error::UnknownMatch)?;
        match state.stop.take() {
            Some(stop) => {
                let _ = stop.send(());
            }
            None if state.phase == Phase::Waiting => {
                self.match_ended(match_id, End::Stopped).await;
            }
            None => {}
        }
        Ok(Reply::Done)
    }
}
