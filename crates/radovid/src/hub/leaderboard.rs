//! Account-scoped admission for costly read lookups.
use super::*;

impl Hub {
    pub(super) fn charge_lookup_at(
        &mut self,
        id: AccountId,
        now: std::time::Instant,
    ) -> Result<(), Error> {
        let account = self.accounts.get_mut(&id).ok_or(Error::NotLoggedIn)?;
        account
            .lookup_budget
            .take(now)
            .map_err(|retry_after_ms| Error::Throttled { retry_after_ms })
    }

    pub(super) fn charge_lookup(&mut self, id: AccountId) -> Result<(), Error> {
        self.charge_lookup_at(id, std::time::Instant::now())
    }

    pub(super) fn sweep_lookup_accounts(&mut self, now: std::time::Instant) {
        self.accounts.retain(|_, account| !account.is_idle_at(now));
    }

    pub(super) async fn leaderboard(
        &mut self,
        id: AccountId,
        ruleset: RulesetId,
        kind: AccountKind,
        prefix: String,
    ) -> Result<Reply, Error> {
        if !valid_nickname_prefix(&prefix) {
            return Err(Error::BadLeaderboardSearch);
        }
        let config = self.config.ruleset(&ruleset).ok_or(Error::UnknownRuleset)?;
        if !config.rated {
            return Err(Error::NoQueue);
        }
        if !config.admits(self.kind_of(id)) {
            return Err(Error::AudienceMismatch);
        }
        let limit = self.config.limits.leaderboard_size;
        self.charge_lookup(id)?;
        let page = self
            .db
            .leaderboard(ruleset, kind, prefix, limit, id)
            .await
            .map_err(lookup_error)?;
        Ok(Reply::LeaderboardPage { page })
    }
}

pub(super) fn lookup_error(error: crate::db::DbError) -> Error {
    if let crate::db::DbError::Sqlite(rusqlite::Error::SqliteFailure(code, _)) = &error
        && matches!(
            code.code,
            rusqlite::ErrorCode::OperationInterrupted
                | rusqlite::ErrorCode::DatabaseBusy
                | rusqlite::ErrorCode::DatabaseLocked
        )
    {
        return Error::Throttled {
            retry_after_ms: 1000,
        };
    }
    internal(error)
}

#[cfg(test)]
mod tests {
    use super::super::matching_tests::Fixture;
    use super::*;

    #[tokio::test]
    async fn two_accounts_at_their_own_rate_cannot_throttle_another_account() {
        let mut fixture=Fixture::new();
        let (alpha,_)=fixture.account("alpha").await;
        let (beta,_)=fixture.account("beta").await;
        let (victim,_)=fixture.account("victim").await;
        let start=std::time::Instant::now();
        for account in [alpha,beta] {
            for _ in 0..4 { fixture.hub.charge_lookup_at(account,start).expect("account burst"); }
        }
        for step in 1..=40 {
            let now=start+Duration::from_millis(step*250);
            fixture.hub.charge_lookup_at(alpha,now).expect("alpha within its rate");
            fixture.hub.charge_lookup_at(beta,now).expect("beta within its rate");
            fixture.hub.charge_lookup_at(victim,now).expect("other accounts cannot consume the victim's quota");
        }
    }

    #[tokio::test]
    async fn account_lookup_limits_are_shared_by_sessions_but_independent_between_accounts()
     {
        let mut fixture = Fixture::new();
        let (alpha, first) = fixture.account("alpha").await;
        let (beta, _) = fixture.account("beta").await;
        let (gamma, _) = fixture.account("gamma").await;
        let row = fixture
            .hub
            .db
            .account_by_id(alpha)
            .await
            .expect("read")
            .expect("account");
        let (events, _receiver) = event_channel();
        let (reply, answer) = oneshot::channel();
        fixture.hub.attach(Attach {
            account: row,
            token_hash: [0; 32],
            game_fingerprint: fixture.hub.config.rulesets[0].game_fingerprint,
            source: "127.0.0.1".parse().expect("source"),
            events,
            reply,
        });
        let second = answer.await.expect("reply").expect("session").0;
        let now = std::time::Instant::now();
        for session in [first, second, first, second] {
            fixture.hub.charge_lookup_at(fixture.hub.sessions[&session].account, now).expect("shared account burst");
        }
        assert_eq!(fixture.hub.charge_lookup_at(alpha, now), Err(Error::Throttled { retry_after_ms:250 }));
        for account in [beta,gamma] {
            for _ in 0..4 { fixture.hub.charge_lookup_at(account,now).expect("independent account quota"); }
        }
    }

    #[tokio::test]
    async fn a_disconnect_keeps_lookup_debt_until_refill_then_the_clock_forgets_it() {
        let mut fixture = Fixture::new();
        let (alpha, session) = fixture.account("alpha").await;
        let now = std::time::Instant::now();
        fixture.hub.charge_lookup_at(alpha, now).expect("lookup");
        fixture.hub.detach(session);
        assert!(
            fixture.hub.accounts.contains_key(&alpha),
            "a reconnect cannot reset recent lookup debt"
        );
        fixture
            .hub
            .sweep_lookup_accounts(now + Duration::from_secs(1));
        assert!(
            !fixture.hub.accounts.contains_key(&alpha),
            "refilled idle accounts do not leak"
        );
    }

    #[tokio::test]
    async fn an_invalid_nickname_prefix_is_refused_before_database_work() {
        let mut fixture = Fixture::new();
        let (_, session) = fixture.account("alpha").await;
        rusqlite::Connection::open(&fixture.hub.config.database)
            .expect("database")
            .execute("DROP TABLE ratings", [])
            .expect("simulate unavailable ratings");
        for prefix in ["%", "alpha one", "😀", "abcdefghijklmnopqrstu"] {
            assert_eq!(
                fixture
                    .hub
                    .request(
                        session,
                        ClientMsg::Leaderboard {
                            ruleset: RulesetId("ranked".into()),
                            kind: AccountKind::Human,
                            prefix: prefix.into()
                        }
                    )
                    .await,
                Err(Error::BadLeaderboardSearch)
            );
        }
    }
    /// Red: two free accounts, each within its own per-account rate, deny every other
    /// account's profile/leaderboard/ladder lookups for as long as they keep going.
    #[tokio::test]
    async fn probe_two_accounts_at_their_own_rate_starve_everyone_else() {
        let mut fixture = Fixture::new();
        let (alpha, _) = fixture.account("alpha").await;
        let (beta, _) = fixture.account("beta").await;
        let (victim, _) = fixture.account("victim").await;
        let start = std::time::Instant::now();
        let mut victim_served = 0;
        for tick in 0..400u64 {
            let now = start + Duration::from_millis(tick * 25);
            for attacker in [alpha, beta] {
                let _ = fixture.hub.charge_lookup_at(attacker, now);
            }
            if tick % 40 == 39 && fixture.hub.charge_lookup_at(victim, now).is_ok() {
                victim_served += 1;
            }
        }
        eprintln!("PROBE victim served {victim_served} of 10"); assert!(victim_served >= 9, "victim starved");
    }
}
