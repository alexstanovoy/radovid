use super::*;

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn fixture() -> (Scratch, Db, MatchId, Vec<NewSeat>) {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "radovid_result_recovery_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).expect("scratch");
    let directory = Scratch(path);
    let db = Db::open(&directory.0.join("main.db")).expect("database");
    let mut seats = Vec::new();
    for (index, name) in ["alpha", "beta"].into_iter().enumerate() {
        let account = db
            .create_account(name.into(), "hash".into(), AccountKind::Human, None)
            .await
            .expect("account");
        seats.push(NewSeat {
            account,
            slot: index as u8,
            team: index as u8,
        });
    }
    let game = db
        .create_match(RulesetId("duel".into()), true, seats.clone())
        .await
        .expect("match");
    (directory, db, game, seats)
}

fn finish(game: MatchId, seats: Vec<NewSeat>) -> Finish {
    Finish {
        match_id: game,
        ruleset: RulesetId("duel".into()),
        rate: true,
        outcome: Outcome::Winner { team: 0 },
        reason: EndReason::Decided,
        duration_ms: 1000,
        report: vec![1, 2, 3],
        seats,
        finished_at: UnixMs(1000),
    }
}

#[tokio::test]
async fn a_failed_finish_replays_after_database_reopen_before_unfinished_games_are_lost() {
    let (directory, db, game, seats) = fixture().await;
    let outside = Connection::open(directory.0.join("main.db")).expect("outside");
    outside.execute_batch("CREATE TRIGGER reject_finish BEFORE UPDATE OF finished ON matches WHEN NEW.finished=1 BEGIN SELECT RAISE(ABORT,'storage unavailable');END;").expect("transient failure");
    assert!(db.finish_match(finish(game, seats.clone())).await.is_err());
    drop(db);
    outside
        .execute_batch("DROP TRIGGER reject_finish")
        .expect("storage restored");
    let reopened = Db::open(&directory.0.join("main.db")).expect("reopen");
    assert_eq!(reopened.recover_results().await.expect("recover"), 1);
    assert_eq!(
        reopened
            .fail_unfinished_matches()
            .await
            .expect("recovery order"),
        0
    );
    let rating = reopened
        .rating(seats[0].account, RulesetId("duel".into()))
        .await
        .expect("rating")
        .expect("rated");
    assert_eq!(rating.games, 1);
    let results = reopened
        .public_recent_matches(
            seats[0].account,
            seats[0].account,
            1,
            vec![RulesetId("duel".into())],
        )
        .await
        .expect("result")
        .expect("own history is visible");
    assert_eq!(results[0].outcome, Outcome::Winner { team: 0 });
    assert_eq!(results[0].game_report, [1, 2, 3]);
}

#[tokio::test]
async fn replaying_an_already_committed_finish_cannot_rate_the_same_match_twice() {
    let (_directory, db, game, seats) = fixture().await;
    db.finish_match(finish(game, seats.clone()))
        .await
        .expect("first commit");
    db.finish_match(finish(game, seats.clone()))
        .await
        .expect("replay");
    let rating = db
        .rating(seats[0].account, RulesetId("duel".into()))
        .await
        .expect("rating")
        .expect("rated");
    assert_eq!(
        rating.games, 1,
        "replaying a pending record must not apply Elo twice"
    );
}

#[tokio::test]
async fn a_preexisting_temporary_hard_link_never_truncates_an_unrelated_file() {
    let (directory, db, game, seats) = fixture().await;
    let sentinel = directory.0.join("unrelated");
    std::fs::write(&sentinel, b"keep these bytes").expect("sentinel");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o600))
        .expect("private fixture");
    let temporary = directory
        .0
        .join("main.db.pending_results")
        .join(format!("{:020}.tmp", game.0));
    std::fs::hard_link(&sentinel, temporary).expect("hard link fixture");
    let _ = db.queue_finish(finish(game, seats)).await;
    assert_eq!(
        std::fs::read(sentinel).expect("unchanged sentinel"),
        b"keep these bytes"
    );
}

#[tokio::test]
async fn a_complete_temporary_record_is_published_and_replayed_after_restart() {
    let (directory, db, game, seats) = fixture().await;
    db.queue_finish(finish(game, seats.clone()))
        .await
        .expect("durable record");
    let folder = directory.0.join("main.db.pending_results");
    std::fs::rename(
        folder.join(format!("{:020}.result", game.0)),
        folder.join(format!("{:020}.tmp", game.0)),
    )
    .expect("simulate pre-publication crash");
    drop(db);
    let reopened = Db::open(&directory.0.join("main.db")).expect("recover temporary publication");
    assert_eq!(reopened.recover_results().await.expect("replay"), 1);
    assert_eq!(
        reopened
            .rating(seats[0].account, RulesetId("duel".into()))
            .await
            .expect("rating")
            .expect("played")
            .games,
        1
    );
}

#[tokio::test]
async fn corrupted_pending_results_are_preserved_and_never_silently_marked_lost() {
    let (directory, db, game, seats) = fixture().await;
    db.queue_finish(finish(game, seats))
        .await
        .expect("durable record");
    let path = directory
        .0
        .join("main.db.pending_results")
        .join(format!("{:020}.result", game.0));
    let mut bytes = std::fs::read(&path).expect("record");
    *bytes.last_mut().expect("payload") ^= 1;
    std::fs::write(&path, bytes).expect("corruption fixture");
    drop(db);
    assert!(Db::open(&directory.0.join("main.db")).is_err());
    assert!(
        path.exists(),
        "keep corrupt records for recovery instead of discarding data"
    );
    let main = Connection::open(directory.0.join("main.db")).expect("inspect main");
    let finished: bool = main
        .query_row(
            "SELECT finished FROM matches WHERE id=?1",
            [game.0 as i64],
            |row| row.get(0),
        )
        .expect("status");
    assert!(!finished);
}

#[tokio::test]
async fn finished_results_are_synced_before_the_durable_pending_record_is_removed() {
    let (_directory, db, game, seats) = fixture().await;
    db.run(|connection|connection.execute_batch("CREATE TRIGGER require_full BEFORE UPDATE OF finished ON matches WHEN NEW.finished=1 AND (SELECT synchronous FROM pragma_synchronous)!=2 BEGIN SELECT RAISE(ABORT,'finish must use FULL');END;")).await.expect("durability assertion");
    db.finish_match(finish(game, seats))
        .await
        .expect("durable main commit");
    let synchronous = db
        .inspect(|connection| {
            connection.pragma_query_value(None, "synchronous", |row| row.get::<_, i32>(0))
        })
        .await
        .expect("restored mode");
    assert_eq!(
        synchronous, 1,
        "ordinary writes retain their normal sync policy"
    );
}

#[tokio::test]
async fn a_match_reservation_is_durable_before_a_game_can_produce_a_pending_result() {
    let (_directory, db, _game, seats) = fixture().await;
    db.run(|connection|connection.execute_batch("CREATE TRIGGER require_reservation_full BEFORE INSERT ON matches WHEN (SELECT synchronous FROM pragma_synchronous)!=2 BEGIN SELECT RAISE(ABORT,'reservation must use FULL');END;")).await.expect("reservation durability assertion");
    db.create_match(RulesetId("duel".into()), true, seats)
        .await
        .expect("durable reservation");
}

#[tokio::test]
async fn overlapping_players_pending_results_replay_in_completion_order_not_match_id_order() {
    let (directory, db, first, seats) = fixture().await;
    let second = db
        .create_match(RulesetId("duel".into()), true, seats.clone())
        .await
        .expect("second game");
    let win = finish(second, seats.clone());
    let mut loss = finish(first, seats.clone());
    loss.outcome = Outcome::Winner { team: 1 };
    db.queue_finish(win).await.expect("first completion");
    db.queue_finish(loss).await.expect("second completion");
    drop(db);
    let recovered = Db::open(&directory.0.join("main.db")).expect("reopen");
    assert_eq!(recovered.recover_results().await.expect("replay"), 2);
    let (_control, control, first_control, control_seats) = fixture().await;
    let second_control = control
        .create_match(RulesetId("duel".into()), true, control_seats.clone())
        .await
        .expect("control second");
    control
        .finish_match(finish(second_control, control_seats.clone()))
        .await
        .expect("control first completion");
    let mut loss = finish(first_control, control_seats);
    loss.outcome = Outcome::Winner { team: 1 };
    control
        .finish_match(loss)
        .await
        .expect("control second completion");
    let actual = recovered
        .rating(seats[0].account, RulesetId("duel".into()))
        .await
        .expect("recovered rating")
        .expect("rated");
    let expected = control
        .rating(AccountId(1), RulesetId("duel".into()))
        .await
        .expect("control rating")
        .expect("rated");
    assert_eq!(
        actual.glicko.public(),
        expected.glicko.public(),
        "recovery must preserve the same Glicko update order"
    );
    assert_eq!(actual.games, 2);
}

#[test]
fn reopening_an_existing_database_performs_a_real_startup_durability_write() {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "radovid_startup_sync_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).expect("scratch");
    let directory = Scratch(path);
    let path = directory.0.join("main.db");
    drop(Db::open(&path).expect("initial schema"));
    let outside = Connection::open(&path).expect("old service");
    outside
        .pragma_update(None, "synchronous", "NORMAL")
        .expect("old policy");
    outside.execute_batch("PRAGMA wal_autocheckpoint=0;INSERT INTO accounts(login,nickname,hash,kind,created) VALUES('legacy','legacy','hash',0,0);").expect("old unflushed state");
    let wal = directory.0.join("main.db-wal");
    let before = std::fs::metadata(&wal).expect("old WAL").len();
    let _reopened = Db::open(&path).expect("upgrade durability barrier");
    let after = std::fs::metadata(wal).expect("new WAL").len();
    assert!(
        after > before,
        "startup must append a FULL-synchronous write, not only change a pragma or commit an empty transaction"
    );
}

#[tokio::test]
async fn acknowledging_a_committed_result_removes_a_leftover_temporary_publication_too() {
    let (directory, db, game, seats) = fixture().await;
    db.queue_finish(finish(game, seats.clone()))
        .await
        .expect("published");
    let folder = directory.0.join("main.db.pending_results");
    let temporary = folder.join(format!("{:020}.tmp", game.0));
    std::fs::hard_link(folder.join(format!("{:020}.result", game.0)), &temporary)
        .expect("simulate incomplete publication cleanup");
    db.finish_match(finish(game, seats))
        .await
        .expect("commit and acknowledge");
    assert!(
        !temporary.exists(),
        "acknowledgement cannot leak temporary records outside the pending capacity bound"
    );
}
