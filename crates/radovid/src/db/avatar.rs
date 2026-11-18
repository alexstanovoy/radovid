use super::*;
use sha2::{Digest, Sha256};

const WRITE_INTERVAL_MS: u64 = 60_000;

/// What a batch lookup answers: the changes among the first `answered` queries.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AvatarBatch {
    pub changes: Vec<AvatarChange>,
    pub answered: usize,
}

impl Db {
    /// Replaces or removes the account's avatar. The attempt is charged first, so that an upload
    /// that turns out invalid spends the account's quota too; the pixels are decoded and encoded
    /// again off the database's writer, which only ever sees the finished bytes.
    pub(crate) async fn change_avatar(
        &self,
        account: AccountId,
        png: Option<Vec<u8>>,
        now: UnixMs,
    ) -> DbResult<Result<Option<Avatar>, Error>> {
        if let Err(error) = self
            .run(move |connection| charge_attempt(connection, account, now))
            .await?
        {
            return Ok(Err(error));
        }
        let avatar = match png {
            None => None,
            Some(bytes) => {
                let canonical =
                    tokio::task::spawn_blocking(move || crate::avatar::canonicalize(&bytes))
                        .await
                        .map_err(|_| DbError::Join)?;
                match canonical {
                    Ok(png) => Some(image(png)),
                    Err(error) => return Ok(Err(error)),
                }
            }
        };
        self.run(move |connection| store(connection, account, avatar))
            .await
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
                Ok(Ok(stored_image(connection, account)?))
            })
        })
        .await
    }

    /// The images among the first `accounts` that differ from the versions the caller holds. The
    /// reply stops before the query whose image would take it past `MAX_AVATAR_REPLY_BYTES`
    /// (the first image always goes in), and says how many queries it covers.
    pub(crate) async fn avatars(
        &self,
        accounts: Vec<AvatarQuery>,
        requester: AccountId,
    ) -> DbResult<AvatarBatch> {
        if !valid_avatar_batch(&accounts) {
            return Err(DbError::Sqlite(rusqlite::Error::InvalidQuery));
        }
        self.read(move |connection| {
            super::leaderboard::read_lookup(connection, |connection| {
                let now = now_ms();
                let mut changes = Vec::new();
                let mut bytes = 0;
                for (answered, query) in accounts.iter().enumerate() {
                    let avatar = if can_view(connection, query.account, requester, now)? {
                        stored_image(connection, query.account)?
                    } else {
                        None
                    };
                    if avatar.as_ref().map(|image| image.version) == query.version {
                        continue;
                    }
                    let size = avatar.as_ref().map_or(0, |image| image.png.len());
                    if bytes > 0 && bytes + size > MAX_AVATAR_REPLY_BYTES {
                        return Ok(AvatarBatch { changes, answered });
                    }
                    bytes += size;
                    changes.push(AvatarChange {
                        account: query.account,
                        avatar,
                    });
                }
                Ok(AvatarBatch {
                    changes,
                    answered: accounts.len(),
                })
            })
        })
        .await
    }
}

#[cfg(test)]
impl Db {
    /// Gives the account exactly these bytes as its avatar, as a fixture: no quota is spent and
    /// nothing is decoded, so a test can hold a hundred heavy pictures.
    pub(crate) async fn put_avatar_bytes(&self, account: AccountId, png: &[u8]) {
        let (version, png) = (Sha256::digest(png).to_vec(), png.to_vec());
        self.run(move |connection| {
            connection
                .execute(
                    "INSERT INTO avatars(account,version,png,attempted_at) VALUES(?1,?2,?3,0)",
                    params![account.0 as i64, version, png],
                )
                .map(|_| ())
        })
        .await
        .expect("stored fixture");
    }
}

/// Spends the account's one change a minute, or says when the next may be made.
fn charge_attempt(
    connection: &mut Connection,
    account: AccountId,
    now: UnixMs,
) -> rusqlite::Result<Result<(), Error>> {
    let transaction = connection.transaction()?;
    if !can_view(&transaction, account, account, now)? {
        return Ok(Err(Error::UnknownAccount));
    }
    let previous: Option<i64> = transaction
        .query_row(
            "SELECT attempted_at FROM avatars WHERE account=?1",
            [account.0 as i64],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(previous) = previous {
        let elapsed = now.0.saturating_sub(previous as u64);
        if elapsed < WRITE_INTERVAL_MS {
            return Ok(Err(Error::Throttled {
                retry_after_ms: (WRITE_INTERVAL_MS - elapsed) as u32,
            }));
        }
    }
    transaction.execute(
        "INSERT INTO avatars(account,attempted_at) VALUES(?1,?2) ON CONFLICT(account) DO UPDATE SET attempted_at=excluded.attempted_at",
        params![account.0 as i64, now.0 as i64],
    )?;
    transaction.commit()?;
    Ok(Ok(()))
}

fn store(
    connection: &mut Connection,
    account: AccountId,
    avatar: Option<Avatar>,
) -> rusqlite::Result<Result<Option<Avatar>, Error>> {
    let changed = connection.execute(
        "UPDATE avatars SET version=?2,png=?3 WHERE account=?1",
        params![
            account.0 as i64,
            avatar.as_ref().map(|image| image.version.0.as_slice()),
            avatar.as_ref().map(|image| image.png.as_slice())
        ],
    )?;
    // The account can be deleted between the charge and the store, and then its row is gone.
    Ok(if changed == 1 {
        Ok(avatar)
    } else {
        Err(Error::UnknownAccount)
    })
}

fn image(png: Vec<u8>) -> Avatar {
    Avatar {
        version: AvatarVersion(Sha256::digest(&png).into()),
        png,
    }
}

/// The stored image, if it is still what the service wrote: the bytes must hash to their version
/// and keep the avatar envelope. Pixels were validated when they were stored, and a read must not
/// decode (a page of a hundred avatars would cost a hundred decodes).
fn stored_image(connection: &Connection, account: AccountId) -> rusqlite::Result<Option<Avatar>> {
    connection.query_row("SELECT CASE WHEN typeof(version)='blob' AND length(version)=32 THEN version END,CASE WHEN typeof(png)='blob' AND length(png) BETWEEN 1 AND ?2 THEN png END FROM avatars WHERE account=?1 AND png IS NOT NULL",params![account.0 as i64,MAX_AVATAR_BYTES as i64],|row| {
        let version=row.get_ref(0)?.as_blob();
        let png=row.get_ref(1)?.as_blob();
        if let (Ok(version),Ok(bytes))=(version,png)
            && let Ok(hash)=<[u8;32]>::try_from(version)
            && Sha256::digest(bytes).as_slice()==hash
            && radovid_proto::avatar_image_data(bytes).is_ok()
        {
            return Ok(Some(Avatar{version:AvatarVersion(hash),png:bytes.to_vec()}));
        }
        eprintln!("WARN: account {} has an invalid stored avatar; treating it as absent",account.0);
        Ok(None)
    }).optional().map(Option::flatten)
}

fn can_view(
    connection: &Connection,
    account: AccountId,
    requester: AccountId,
    now: UnixMs,
) -> rusqlite::Result<bool> {
    super::public_matches::account_visible(connection, account, requester, now)
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

    struct Scratch {
        database: std::path::PathBuf,
        directory: std::path::PathBuf,
    }
    impl Scratch {
        fn new() -> Self {
            use std::os::unix::fs::DirBuilderExt;
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let directory = std::env::temp_dir().join(format!(
                "avatar_storage_{}_{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .expect("unique private scratch directory");
            Self {
                database: directory.join("avatar.db"),
                directory,
            }
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
    async fn account(db: &Db, name: &str) -> AccountId {
        db.create_account(name.into(), "hash".into(), AccountKind::Human)
            .await
            .expect("account")
    }

    #[tokio::test]
    async fn profiles_and_both_leaderboard_paths_publish_the_avatar_cache_version() {
        let db = Db::init(Connection::open_in_memory().expect("connection")).expect("db");
        let alpha = account(&db, "alpha").await;
        let png = crate::avatar::tests::avatar();
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
        let db = Db::open(&scratch.database).expect("database");
        let alpha = account(&db, "alpha").await;
        let beta = account(&db, "beta").await;
        let now = UnixMs(100_000);
        let png = crate::avatar::tests::avatar();
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
        let reopened = Db::open(&scratch.database).expect("reopen");
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
            .change_avatar(account, Some(crate::avatar::tests::avatar()), UnixMs(1))
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
            .change_avatar(alpha, Some(crate::avatar::tests::avatar()), UnixMs(1))
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
    async fn corrupt_stored_avatar_hash_is_absent_without_serving_unvalidated_bytes() {
        let db = Db::init(Connection::open_in_memory().expect("connection")).expect("db");
        let alpha = account(&db, "alpha").await;
        db.change_avatar(alpha, Some(crate::avatar::tests::avatar()), UnixMs(1))
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
        assert_eq!(
            db.avatar(alpha, alpha)
                .await
                .expect("corrupt images are absent"),
            Ok(None)
        );
    }
    #[tokio::test]
    async fn uploaded_rgb_images_store_only_reencoded_pixels_without_suggested_palette_payloads() {
        let db = Db::init(Connection::open_in_memory().expect("connection")).expect("db");
        let owner = account(&db, "owner").await;
        let raw = crate::avatar::tests::image_with_chunks(
            png::ColorType::Rgb,
            &[(png::chunk::PLTE, b"<script>payload</script>")],
        );
        crate::avatar::validate(&raw).expect("valid RGB carrier");
        let stored = db
            .change_avatar(owner, Some(raw), UnixMs(1))
            .await
            .expect("storage")
            .expect("upload")
            .expect("image");
        assert!(
            !stored.png.windows(8).any(|bytes| bytes == b"<script>"),
            "untrusted suggested palette must not be stored verbatim"
        );
        assert!(
            !stored.png.windows(4).any(|bytes| bytes == b"PLTE"),
            "RGB canonical encoding has no palette"
        );
        assert_eq!(
            stored.version,
            AvatarVersion(Sha256::digest(&stored.png).into())
        );
    }

    #[test]
    fn avatar_storage_scratch_removes_the_pending_result_sidecar() {
        let path;
        {
            let scratch = Scratch::new();
            path =
                std::path::PathBuf::from(format!("{}.pending_results", scratch.database.display()));
            drop(Db::open(&scratch.database).expect("database"));
            assert!(path.is_dir());
        }
        assert!(
            !path.exists(),
            "storage fixture must remove its pending-result directory"
        );
    }

    #[tokio::test]
    async fn a_stored_image_that_no_longer_decodes_is_absent_instead_of_an_internal_error() {
        let db = Db::init(Connection::open_in_memory().expect("db")).expect("schema");
        let owner = account(&db, "owner").await;
        db.run(move|connection|connection.execute("INSERT INTO avatars(account,version,png,attempted_at) VALUES(?1,zeroblob(32),x'00',0)",[owner.0 as i64]).map(|_|())).await.expect("corruption fixture");
        assert_eq!(
            db.avatar(owner, owner).await.expect("recoverable image"),
            Ok(None)
        );
    }

    #[tokio::test]
    async fn batch_fetch_hides_banned_unknown_and_corrupt_images_and_clears_cached_versions() {
        let db = Db::init(Connection::open_in_memory().expect("db")).expect("schema");
        let owner = account(&db, "owner").await;
        let viewer = account(&db, "viewer").await;
        let avatar = db
            .change_avatar(owner, Some(crate::avatar::tests::avatar()), UnixMs(1))
            .await
            .expect("storage")
            .expect("upload")
            .expect("image");
        db.set_ban(
            owner,
            Some(Ban {
                reason: "fixture".into(),
                until: None,
            }),
        )
        .await
        .expect("ban");
        let query = AvatarQuery {
            account: owner,
            version: Some(avatar.version),
        };
        assert_eq!(
            db.avatars(vec![query], viewer)
                .await
                .expect("hidden")
                .changes,
            vec![AvatarChange {
                account: owner,
                avatar: None
            }]
        );
        assert!(
            db.avatars(
                vec![AvatarQuery {
                    account: owner,
                    version: None
                }],
                viewer
            )
            .await
            .expect("no cached image")
            .changes
            .is_empty()
        );
        db.set_ban(owner, None).await.expect("unban");
        db.run(move |connection| {
            connection
                .execute(
                    "UPDATE avatars SET png=x'00' WHERE account=?1",
                    [owner.0 as i64],
                )
                .map(|_| ())
        })
        .await
        .expect("corrupt fixture");
        assert_eq!(
            db.avatars(vec![query], viewer)
                .await
                .expect("corrupt means absent")
                .changes,
            vec![AvatarChange {
                account: owner,
                avatar: None
            }]
        );
        db.delete_account(owner).await.expect("delete");
        assert_eq!(
            db.avatars(vec![query], viewer)
                .await
                .expect("deleted")
                .changes,
            vec![AvatarChange {
                account: owner,
                avatar: None
            }]
        );
    }
}
