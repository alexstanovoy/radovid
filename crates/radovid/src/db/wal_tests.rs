//! Checkpoint-starvation regression using a small disk-backed fixture.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture() -> (Scratch, Db) {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let directory = Scratch(target.join("test_data").join(format!(
        "wal_overlap_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )));
    std::fs::create_dir_all(&directory.0).expect("scratch");
    let db = Db::open(&directory.0.join("ratings.db")).expect("database");
    {
        let mut connection = db.conn.lock().expect("writer");
        let transaction = connection.transaction().expect("fixture transaction");
        for id in 1..=4096i64 {
            transaction.execute("INSERT INTO accounts(id,login,nickname,hash,kind,created) VALUES(?1,'l'||?1,'a'||?1,'fixture',0,0)",[id]).expect("account");
            transaction.execute("INSERT INTO ratings(account,ruleset,rating,deviation,volatility,games,wins,draws,losses,last_played) VALUES(?1,'duel',?2,50,0.06,1,1,0,0,100)",params![id,1000+(id*7919)%2000]).expect("rating");
        }
        transaction.commit().expect("seed commit");
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .expect("seed checkpoint");
    }
    (directory, db)
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_database_checkpoints_sqlites_actual_sidecar() {
    let (scratch, db) = fixture();
    drop(db);
    let aliases = scratch.0.join("aliases");
    std::fs::create_dir(&aliases).expect("alias directory");
    let alias = aliases.join("linked.db");
    std::os::unix::fs::symlink(
        std::fs::canonicalize(scratch.0.join("ratings.db")).expect("actual database"),
        &alias,
    )
    .expect("database symlink");
    let mut db = Db::open(&alias).expect("open through symlink");
    Arc::get_mut(db.wal.as_mut().expect("WAL policy"))
        .expect("unique policy")
        .set_limit_for_test(0);
    db.set_nickname(AccountId(1), "Changed".into())
        .await
        .expect("write");
    db.wal.as_ref().expect("policy").wait_for_test().await;
    assert!(!aliases.join("linked.db-wal").exists());
    assert_eq!(
        std::fs::metadata(scratch.0.join("ratings.db-wal"))
            .map(|file| file.len())
            .unwrap_or(0),
        0,
        "measure the resolved database's WAL, not the symlink name"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn checkpoint_closes_reader_admission_without_holding_the_writer_turn_while_snapshots_drain() {
    let (scratch, mut db) = fixture();
    Arc::get_mut(db.wal.as_mut().expect("WAL policy"))
        .expect("unique policy")
        .set_limit_for_test(0);
    let policy = Arc::clone(db.wal.as_ref().expect("WAL policy"));
    let (began, started) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::sync_channel(1);
    let held = db.clone();
    let snapshot = tokio::spawn(async move {
        held.read(move |connection| {
            let transaction = connection.transaction()?;
            transaction.query_row("SELECT COUNT(*) FROM accounts", [], |row| {
                row.get::<_, i64>(0)
            })?;
            let _ = began.send(());
            released.recv().expect("release snapshot");
            transaction.commit()
        })
        .await
    });
    started.await.expect("snapshot started");
    db.set_nickname(AccountId(1), "Renamed".into())
        .await
        .expect("commit returns while checkpoint waits");
    let pending = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while policy.snapshots.try_read().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let writer_permits = db.writer_turn.available_permits();
    release.send(()).expect("release snapshot");
    snapshot
        .await
        .expect("snapshot task")
        .expect("snapshot commit");
    policy.wait_for_test().await;
    pending.expect("checkpoint closes admission");
    assert_eq!(
        writer_permits, 1,
        "reader drain must not reserve the writer turn"
    );
    assert_eq!(
        std::fs::metadata(scratch.0.join("ratings.db-wal"))
            .map(|file| file.len())
            .unwrap_or(0),
        0
    );
}

#[tokio::test]
async fn an_external_snapshot_keeps_normal_writes_and_login_reads_available_below_the_hard_cap() {
    let (scratch,mut db)=fixture();
    Arc::get_mut(db.wal.as_mut().expect("policy")).expect("unique").set_limit_for_test(0);
    let outside=Connection::open(scratch.0.join("ratings.db")).expect("external");
    outside.execute_batch("BEGIN; SELECT COUNT(*) FROM accounts;").expect("snapshot");
    let mut failures=Vec::new();
    for round in 0..20 {
        if let Err(error)=db.set_nickname(AccountId(1),format!("Kept{round}")).await {failures.push(error.to_string());}
        if let Err(error)=db.account_by_login("l1".into()).await {failures.push(error.to_string());}
    }
    outside.execute_batch("COMMIT").expect("release external");
    assert!(failures.is_empty(),"normal writes/login reads failed below hard cap: {failures:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn probe_wal_stays_bounded_under_overlapping_reads() {
    let (scratch, db) = fixture();
    let stop = Arc::new(AtomicBool::new(false));
    let mut readers = Vec::new();
    for index in 0..16 {
        let (database, stop) = (db.clone(), stop.clone());
        readers.push(tokio::spawn(async move {
            let mut completed = 0;
            for _ in 0..8000 {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let _ = database
                    .leaderboard(
                        RulesetId("duel".into()),
                        AccountKind::Human,
                        "a".into(),
                        1000,
                        AccountId(1 + index),
                    )
                    .await;
                completed += 1;
            }
            completed
        }));
    }
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while db.lookup_pending_for_test() < 8 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("overlapping reads admitted");
    let mut peak = 0;
    let mut failure = None;
    for round in 0..2000i64 {
        if let Err(error)=db.run(move |connection| {
            let transaction=connection.transaction()?;
            for slot in 0..10 { transaction.execute("UPDATE ratings SET rating=rating+0.5,games=games+1,wins=wins+1 WHERE account=?1",[1+((round*10+slot)*7)%4096])?; }
            transaction.commit()
        }).await { failure=Some(error);break; }
        let bytes = std::fs::metadata(scratch.0.join("ratings.db-wal"))
            .map(|file| file.len())
            .unwrap_or(0);
        peak = peak.max(bytes);
    }
    stop.store(true, Ordering::Relaxed);
    let mut completed = 0;
    for reader in readers {
        completed += reader.await.expect("reader");
    }
    db.wal.as_ref().expect("policy").wait_for_test().await;
    assert!(failure.is_none(), "write failed: {failure:?}");
    assert!(completed > 0);
    eprintln!("WAL overlap peak {peak} bytes, {completed} reads");
    assert!(
        peak < wal::HARD_LIMIT_BYTES,
        "WAL grew to {peak} bytes under overlapping reads"
    );
}

#[tokio::test]
async fn the_hard_wal_limit_refuses_mutations_but_keeps_authentication_reads_available() {
    let (scratch,mut db)=fixture();
    let policy=Arc::get_mut(db.wal.as_mut().expect("policy")).expect("unique");
    policy.set_limit_for_test(0);policy.set_hard_limit_for_test(1024);
    let external=Connection::open(scratch.0.join("ratings.db")).expect("external");
    external.execute_batch("BEGIN;SELECT COUNT(*) FROM accounts;").expect("snapshot");
    db.set_nickname(AccountId(1),"Kept".into()).await.expect("first commit");
    let policy=Arc::clone(db.wal.as_ref().expect("policy"));
    policy.wait_for_test().await;
    assert!(matches!(db.set_nickname(AccountId(1),"Rejected".into()).await,Err(DbError::WalLimit)));
    assert_eq!(db.account_by_login("l1".into()).await.expect("login read").expect("account").nickname,"Kept");
    external.execute_batch("COMMIT").expect("release");
    policy.reset_retry_for_test();policy.schedule(tokio::runtime::Handle::current());policy.wait_for_test().await;
    db.set_nickname(AccountId(1),"Recovered".into()).await.expect("write recovers");
}
