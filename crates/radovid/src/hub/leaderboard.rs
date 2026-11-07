//! Account-scoped admission for costly read lookups.
use super::*;

impl Hub {
    pub(super) fn start_visibility_sweep(&mut self) {
        if self.visibility_sweep_pending {
            return;
        }
        self.visibility_sweep_pending = true;
        let (database, hub) = (self.db.clone(), self.me.clone());
        tokio::spawn(async move {
            if let Err(error) = database.expire_bans(crate::db::now_ms()).await {
                eprintln!("ERROR: refreshing timed rating bans: {error}");
            }
            let _ = hub.send(Cmd::VisibilitySweepDone).await;
        });
    }
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
        let now = std::time::Instant::now();
        #[cfg(test)]
        let now = self.lookup_clock.unwrap_or(now);
        self.charge_lookup_at(id, now)
    }

    pub(super) fn sweep_lookup_accounts(&mut self, now: std::time::Instant) {
        self.accounts.retain(|_, account| !account.is_idle_at(now));
    }

    pub(super) fn prepare_lookup(
        &mut self,
        session: SessionId,
        message: &ClientMsg,
    ) -> Result<(AccountId, Lookup), Error> {
        let id = self
            .sessions
            .get(&session)
            .map(|session| session.account)
            .ok_or(Error::NotLoggedIn)?;
        let lookup = match message {
            ClientMsg::GetAvatars { accounts } => {
                if !valid_avatar_batch(accounts) {
                    return Err(Error::BadAvatarBatch);
                }
                Lookup::Avatars {
                    accounts: accounts.clone(),
                    requester: id,
                }
            }
            ClientMsg::GetAvatar { account } => Lookup::Avatar {
                account: *account,
                requester: id,
            },
            ClientMsg::Leaderboard {
                ruleset,
                kind,
                prefix,
            } => {
                if !valid_nickname_prefix(prefix) {
                    return Err(Error::BadLeaderboardSearch);
                }
                let config = self.config.ruleset(ruleset).ok_or(Error::UnknownRuleset)?;
                if !config.rated {
                    return Err(Error::NoQueue);
                }
                if !config.admits(self.kind_of(id)) {
                    return Err(Error::AudienceMismatch);
                }
                Lookup::Board {
                    ruleset: ruleset.clone(),
                    kind: *kind,
                    prefix: prefix.clone(),
                    limit: self.config.limits.leaderboard_size,
                    requester: id,
                }
            }
            ClientMsg::Account { account } => Lookup::Profile {
                account: *account,
                requester: id,
                modes: self
                    .config
                    .rulesets
                    .iter()
                    .filter(|rule| rule.rated && rule.admits(self.kind_of(id)))
                    .map(|rule| rule.ruleset_id())
                    .collect(),
                online: self
                    .accounts
                    .get(account)
                    .is_some_and(|account| !account.sessions.is_empty()),
            },
            ClientMsg::Ladder {
                ruleset,
                offset,
                limit,
            } => {
                let config = self.config.ruleset(ruleset).ok_or(Error::UnknownRuleset)?;
                if !config.rated {
                    return Err(Error::NoQueue);
                }
                Lookup::Legacy {
                    ruleset: ruleset.clone(),
                    offset: *offset,
                    limit: (*limit).min(MAX_LADDER_PAGE),
                }
            }
            _ => return Err(Error::Internal),
        };
        if self
            .accounts
            .get(&id)
            .is_some_and(|account| account.lookup_pending)
        {
            return Err(Error::Throttled {
                retry_after_ms: 250,
            });
        }
        self.charge_lookup(id)?;
        self.accounts
            .get_mut(&id)
            .ok_or(Error::NotLoggedIn)?
            .lookup_pending = true;
        Ok((id, lookup))
    }

    pub(super) fn lookup_done(&mut self, id: AccountId) {
        if let Some(account) = self.accounts.get_mut(&id) {
            account.lookup_pending = false;
        }
        self.forget_if_idle(id);
    }

    pub(super) fn start_lookup(
        &mut self,
        session: SessionId,
        message: &ClientMsg,
        reply: oneshot::Sender<Result<Reply, Error>>,
    ) {
        let (account, lookup) = match self.prepare_lookup(session, message) {
            Ok(query) => query,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let database = self.db.clone();
        let hub = self.me.clone();
        tokio::spawn(async move {
            let result = lookup.run(&database).await;
            let _ = hub.send(Cmd::LookupDone { account }).await;
            let _ = reply.send(result);
        });
    }
}

pub(super) fn is_lookup(message: &ClientMsg) -> bool {
    matches!(
        message,
        ClientMsg::Leaderboard { .. }
            | ClientMsg::Account { .. }
            | ClientMsg::Ladder { .. }
            | ClientMsg::GetAvatar { .. }
            | ClientMsg::GetAvatars { .. }
    )
}

pub(super) enum Lookup {
    Avatars {
        accounts: Vec<AvatarQuery>,
        requester: AccountId,
    },
    Avatar {
        account: AccountId,
        requester: AccountId,
    },
    Board {
        ruleset: RulesetId,
        kind: AccountKind,
        prefix: String,
        limit: u16,
        requester: AccountId,
    },
    Profile {
        account: AccountId,
        requester: AccountId,
        modes: Vec<RulesetId>,
        online: bool,
    },
    Legacy {
        ruleset: RulesetId,
        offset: u32,
        limit: u8,
    },
}

impl Lookup {
    pub(super) async fn run(self, database: &Db) -> Result<Reply, Error> {
        match self {
            Self::Avatars {
                accounts,
                requester,
            } => Ok(Reply::Avatars {
                changes: database
                    .avatars(accounts, requester)
                    .await
                    .map_err(lookup_error)?,
            }),
            Self::Avatar { account, requester } => {
                let avatar = database
                    .avatar(account, requester)
                    .await
                    .map_err(lookup_error)??;
                Ok(Reply::Avatar { account, avatar })
            }
            Self::Board {
                ruleset,
                kind,
                prefix,
                limit,
                requester,
            } => {
                let page = database
                    .leaderboard(ruleset, kind, prefix, limit, requester)
                    .await
                    .map_err(lookup_error)?;
                Ok(Reply::LeaderboardPage { page })
            }
            Self::Profile {
                account,
                requester,
                modes,
                online,
            } => {
                let mut account = database
                    .public_profile(account, modes, requester)
                    .await
                    .map_err(lookup_error)?
                    .ok_or(Error::UnknownAccount)?;
                account.online = online;
                Ok(Reply::Account { account })
            }
            Self::Legacy {
                ruleset,
                offset,
                limit,
            } => {
                let (total, rows) = database
                    .ladder(ruleset, offset, limit)
                    .await
                    .map_err(lookup_error)?;
                Ok(Reply::LadderPage { total, rows })
            }
        }
    }
}

pub(super) fn lookup_error(error: crate::db::DbError) -> Error {
    if matches!(error, crate::db::DbError::ReadBusy) {
        return Error::Throttled {
            retry_after_ms: 250,
        };
    }
    if let crate::db::DbError::Sqlite(rusqlite::Error::SqliteFailure(code, _)) = &error {
        match code.code {
            rusqlite::ErrorCode::OperationInterrupted => return Error::LookupTooExpensive,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked => {
                return Error::Throttled {
                    retry_after_ms: 1000,
                };
            }
            _ => {}
        }
    }
    internal(error)
}

#[cfg(test)]
mod tests {
    use super::super::matching_tests::Fixture;
    use super::*;

    #[test]
    fn a_lookup_work_limit_is_not_reported_as_retryable_throttling() {
        let error = crate::db::DbError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(9),
            None,
        ));
        assert_eq!(lookup_error(error), Error::LookupTooExpensive);
    }

    #[tokio::test]
    async fn a_blocked_lookup_does_not_block_the_hub() {
        let mut fixture = Fixture::new();
        let (account, _) = fixture.account("alpha").await;
        let db = fixture.hub.db.clone();
        let row = db
            .account_by_id(account)
            .await
            .expect("read")
            .expect("account");
        let (hub, task) = super::super::spawn(Arc::clone(&fixture.hub.config), db.clone());
        let (events, _receiver) = event_channel();
        let (reply, answer) = oneshot::channel();
        hub.tx
            .send(Cmd::Attach(Box::new(Attach {
                account: row,
                token_hash: [0; 32],
                game_fingerprint: fixture.hub.config.rulesets[0].game_fingerprint,
                source: "127.0.0.1".parse().expect("source"),
                events,
                reply,
            })))
            .await
            .expect("attach");
        let session = answer.await.expect("reply").expect("session").0;
        let (held, ready) = std::sync::mpsc::sync_channel(1);
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let locked = db.clone();
        let holder = std::thread::spawn(move || {
            let guard = locked
                .lookup_connection_for_test()
                .lock()
                .expect("lookup connection");
            held.send(()).expect("held");
            released.recv().expect("release");
            drop(guard);
        });
        ready.recv().expect("lookup connection held");
        let (reply, lookup) = oneshot::channel();
        hub.tx
            .send(Cmd::Request {
                session,
                msg: ClientMsg::Leaderboard {
                    ruleset: RulesetId("ranked".into()),
                    kind: AccountKind::Human,
                    prefix: String::new(),
                },
                reply,
            })
            .await
            .expect("lookup");
        let (reply, cheap) = oneshot::channel();
        hub.tx
            .send(Cmd::Request {
                session,
                msg: ClientMsg::Limits { ruleset: None },
                reply,
            })
            .await
            .expect("cheap request");
        let progress = tokio::time::timeout(Duration::from_secs(1), cheap).await;
        release.send(()).expect("release lookup");
        holder.join().expect("holder");
        lookup.await.expect("lookup answer").expect("lookup");
        hub.tx.send(Cmd::Shutdown).await.expect("shutdown");
        task.await.expect("hub stops");
        assert!(
            matches!(progress, Ok(Ok(Ok(Reply::Limits { .. })))),
            "matchmaking hub must progress while a reader waits"
        );
    }

    #[tokio::test]
    async fn two_accounts_at_their_own_rate_cannot_throttle_another_account() {
        let mut fixture = Fixture::new();
        let (alpha, _) = fixture.account("alpha").await;
        let (beta, _) = fixture.account("beta").await;
        let (victim, _) = fixture.account("victim").await;
        let start = std::time::Instant::now();
        for account in [alpha, beta] {
            for _ in 0..4 {
                fixture
                    .hub
                    .charge_lookup_at(account, start)
                    .expect("account burst");
            }
        }
        for step in 1..=40 {
            let now = start + Duration::from_millis(step * 250);
            fixture
                .hub
                .charge_lookup_at(alpha, now)
                .expect("alpha within its rate");
            fixture
                .hub
                .charge_lookup_at(beta, now)
                .expect("beta within its rate");
            fixture
                .hub
                .charge_lookup_at(victim, now)
                .expect("other accounts cannot consume the victim's quota");
        }
    }

    #[tokio::test]
    async fn account_lookup_limits_are_shared_by_sessions_but_independent_between_accounts() {
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
            fixture
                .hub
                .charge_lookup_at(fixture.hub.sessions[&session].account, now)
                .expect("shared account burst");
        }
        assert_eq!(
            fixture.hub.charge_lookup_at(alpha, now),
            Err(Error::Throttled {
                retry_after_ms: 250
            })
        );
        for account in [beta, gamma] {
            for _ in 0..4 {
                fixture
                    .hub
                    .charge_lookup_at(account, now)
                    .expect("independent account quota");
            }
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
        eprintln!("PROBE victim served {victim_served} of 10");
        assert!(victim_served >= 9, "victim starved");
    }
}
