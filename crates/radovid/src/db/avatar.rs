use super::*;
use sha2::{Digest, Sha256};

const WRITE_INTERVAL_MS: u64 = 60_000;

impl Db {
    pub(crate) async fn change_avatar(
        &self,
        account: AccountId,
        png: Option<Vec<u8>>,
        now: UnixMs,
    ) -> DbResult<Result<Option<Avatar>, Error>> {
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            if !can_view(&transaction, account, account, now)? { return Ok(Err(Error::UnknownAccount)); }
            let previous: Option<i64> = transaction.query_row("SELECT attempted_at FROM avatars WHERE account=?1", [account.0 as i64], |row|row.get(0)).optional()?;
            if let Some(previous) = previous {
                let elapsed = now.0.saturating_sub(previous as u64);
                if elapsed < WRITE_INTERVAL_MS {
                    return Ok(Err(Error::Throttled { retry_after_ms: (WRITE_INTERVAL_MS - elapsed) as u32 }));
                }
            }
            transaction.execute("INSERT INTO avatars(account,attempted_at) VALUES(?1,?2) ON CONFLICT(account) DO UPDATE SET attempted_at=excluded.attempted_at",params![account.0 as i64,now.0 as i64])?;
            if let Some(bytes) = &png && let Err(error) = crate::avatar::validate(bytes) {
                transaction.commit()?;
                return Ok(Err(error));
            }
            let avatar = png.map(|png| Avatar { version: AvatarVersion(Sha256::digest(&png).into()), png });
            transaction.execute("UPDATE avatars SET version=?2,png=?3 WHERE account=?1",params![account.0 as i64,avatar.as_ref().map(|image|image.version.0.as_slice()),avatar.as_ref().map(|image|image.png.as_slice())])?;
            transaction.commit()?;
            Ok(Ok(avatar))
        }).await
    }

    pub(crate) async fn avatar(
        &self,
        account: AccountId,
        requester: AccountId,
    ) -> DbResult<Result<Option<Avatar>, Error>> {
        self.read(move |connection| {
            super::leaderboard::read_lookup(connection, |connection| {
                if !can_view(connection, account, requester, now_ms())? {
                    return Ok(Err(Error::UnknownAccount));
                }
                let avatar = connection
                    .query_row(
                        "SELECT version,png FROM avatars WHERE account=?1 AND png IS NOT NULL",
                        [account.0 as i64],
                        |row| {
                            let version = AvatarVersion(row.get(0)?);
                            let bytes = row.get_ref(1)?.as_blob()?;
                            if crate::avatar::validate(bytes).is_err()
                                || Sha256::digest(bytes).as_slice() != version.0
                            {
                                return Err(rusqlite::Error::InvalidQuery);
                            }
                            Ok(Avatar {
                                version,
                                png: bytes.to_vec(),
                            })
                        },
                    )
                    .optional()?;
                Ok(Ok(avatar))
            })
        })
        .await
    }
}

fn can_view(
    connection: &Connection,
    account: AccountId,
    requester: AccountId,
    now: UnixMs,
) -> rusqlite::Result<bool> {
    connection.query_row("SELECT 1 FROM accounts WHERE id=?1 AND deleted=0 AND (?1=?2 OR banned=0 OR ban_until BETWEEN 0 AND ?3)",params![account.0 as i64,requester.0 as i64,now.0 as i64], |_| Ok(true)).optional().map(|found|found.unwrap_or(false))
}

pub(super) fn version(
    connection: &Connection,
    account: AccountId,
) -> rusqlite::Result<Option<AvatarVersion>> {
    connection
        .query_row(
            "SELECT version FROM avatars WHERE account=?1 AND version IS NOT NULL",
            [account.0 as i64],
            |row| row.get(0).map(AvatarVersion),
        )
        .optional()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            Self(std::env::temp_dir().join(format!(
                "avatar_{}_{}.db",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            )))
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
            }
        }
    }
    async fn account(db: &Db, name: &str) -> AccountId {
        db.create_account(name.into(), "hash".into(), AccountKind::Human, None)
            .await
            .expect("account")
    }

    #[tokio::test]
    async fn profiles_and_both_leaderboard_paths_publish_the_avatar_cache_version() {
        let db = Db::init(Connection::open_in_memory().expect("connection")).expect("db");
        let alpha = account(&db, "alpha").await;
        let png = crate::avatar::tests::image(32, 32);
        let avatar = db
            .change_avatar(alpha, Some(png), UnixMs(1))
            .await
            .expect("write")
            .expect("valid")
            .expect("avatar");
        db.run(move |connection| connection.execute("INSERT INTO ratings(account,ruleset,rating,deviation,volatility,games,wins,draws,losses,last_played) VALUES(?1,'duel',1500,350,0.06,1,1,0,0,1)",[alpha.0 as i64]).map(|_|())).await.expect("played fixture");
        let profile = db
            .public_profile(alpha, vec![RulesetId("duel".into())], alpha)
            .await
            .expect("query")
            .expect("profile");
        assert_eq!(profile.avatar, Some(avatar.version));
        let page = db
            .leaderboard(
                RulesetId("duel".into()),
                AccountKind::Human,
                String::new(),
                100,
                alpha,
            )
            .await
            .expect("board");
        assert_eq!(page.rows[0].avatar, Some(avatar.version));
        assert_eq!(page.own.expect("own").avatar, Some(avatar.version));
        let (_, rows) = db
            .ladder(RulesetId("duel".into()), 0, 100)
            .await
            .expect("legacy");
        assert_eq!(rows[0].avatar, Some(avatar.version));
    }

    #[tokio::test]
    async fn avatar_writes_are_per_account_and_persist_rate_debt_across_database_reopen() {
        let scratch = Scratch::new();
        let db = Db::open(&scratch.0).expect("database");
        let alpha = account(&db, "alpha").await;
        let beta = account(&db, "beta").await;
        let now = UnixMs(100_000);
        let png = crate::avatar::tests::image(32, 32);
        let stored = db
            .change_avatar(alpha, Some(png.clone()), now)
            .await
            .expect("write")
            .expect("valid avatar")
            .expect("present");
        assert_eq!(stored.png, png);
        assert_eq!(
            db.change_avatar(alpha, None, now).await.expect("request"),
            Err(Error::Throttled {
                retry_after_ms: 60_000
            })
        );
        assert!(
            db.change_avatar(beta, None, now)
                .await
                .expect("other account")
                .is_ok()
        );
        let reopened = Db::open(&scratch.0).expect("reopen");
        assert_eq!(
            reopened
                .change_avatar(alpha, None, UnixMs(now.0 + 1))
                .await
                .expect("request"),
            Err(Error::Throttled {
                retry_after_ms: 59_999
            })
        );
        assert_eq!(
            reopened
                .change_avatar(alpha, None, UnixMs(now.0 + 60_000))
                .await
                .expect("remove"),
            Ok(None)
        );
    }

    #[tokio::test]
    async fn invalid_avatar_attempt_consumes_quota_and_never_replaces_the_image() {
        let db = Db::init(Connection::open_in_memory().expect("connection")).expect("db");
        let account = account(&db, "alpha").await;
        let first = db
            .change_avatar(
                account,
                Some(crate::avatar::tests::image(32, 32)),
                UnixMs(1),
            )
            .await
            .expect("storage")
            .expect("upload");
        assert_eq!(
            db.change_avatar(account, Some(vec![0]), UnixMs(60_001))
                .await
                .expect("request"),
            Err(Error::BadAvatar)
        );
        assert_eq!(db.avatar(account, account).await.expect("read"), Ok(first));
        assert_eq!(
            db.change_avatar(account, None, UnixMs(60_002))
                .await
                .expect("request"),
            Err(Error::Throttled {
                retry_after_ms: 59_999
            })
        );
        assert_eq!(
            db.change_avatar(account, None, UnixMs(120_001))
                .await
                .expect("remove"),
            Ok(None)
        );
        assert_eq!(
            db.avatar(account, account).await.expect("removed image"),
            Ok(None)
        );
    }

    #[tokio::test]
    async fn avatar_visibility_hides_banned_and_deleted_accounts_and_deletion_erases_bytes() {
        let db = Db::init(Connection::open_in_memory().expect("connection")).expect("db");
        let alpha = account(&db, "alpha").await;
        let beta = account(&db, "beta").await;
        let stored = db
            .change_avatar(alpha, Some(crate::avatar::tests::image(32, 32)), UnixMs(1))
            .await
            .expect("storage")
            .expect("upload");
        db.set_ban(
            alpha,
            Some(Ban {
                reason: "fixture".into(),
                until: None,
            }),
        )
        .await
        .expect("ban");
        assert_eq!(
            db.avatar(alpha, beta).await.expect("public fetch"),
            Err(Error::UnknownAccount)
        );
        assert_eq!(
            db.avatar(alpha, alpha).await.expect("own fetch"),
            Ok(stored.clone())
        );
        db.set_ban(alpha, None).await.expect("unban");
        assert_eq!(
            db.avatar(alpha, beta).await.expect("public fetch"),
            Ok(stored)
        );
        db.delete_account(alpha).await.expect("delete");
        assert_eq!(
            db.avatar(alpha, alpha).await.expect("deleted self"),
            Err(Error::UnknownAccount)
        );
        assert_eq!(
            db.avatar(alpha, beta).await.expect("deleted other"),
            Err(Error::UnknownAccount)
        );
        let bytes: i64 = db
            .run(move |connection| {
                connection.query_row(
                    "SELECT COUNT(*) FROM avatars WHERE account=?1",
                    [alpha.0 as i64],
                    |row| row.get(0),
                )
            })
            .await
            .expect("stored bytes");
        assert_eq!(bytes, 0);
    }

    #[tokio::test]
    async fn corrupt_stored_avatar_hash_is_refused_without_serving_unvalidated_bytes() {
        let db = Db::init(Connection::open_in_memory().expect("connection")).expect("db");
        let alpha = account(&db, "alpha").await;
        db.change_avatar(alpha, Some(crate::avatar::tests::image(32, 32)), UnixMs(1))
            .await
            .expect("storage")
            .expect("upload");
        db.run(|connection| {
            connection
                .execute("UPDATE avatars SET version=zeroblob(32)", [])
                .map(|_| ())
        })
        .await
        .expect("corruption fixture");
        assert!(matches!(
            db.avatar(alpha, alpha).await,
            Err(DbError::Sqlite(rusqlite::Error::InvalidQuery))
        ));
    }
}
