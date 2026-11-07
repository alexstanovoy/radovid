use super::*;

#[tokio::test]
async fn schema_three_migration_backfills_visible_totals_and_preserves_ratings_and_credentials() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .execute_batch(include_str!("../../tests/fixtures/schema_3.sql"))
        .expect("schema three");
    connection.execute_batch("INSERT INTO accounts(id,login,nickname,hash,kind,created,banned,deleted) VALUES
        (1,'one','One','kept hash',0,123,0,0),(2,'two','Two','hash',1,123,0,0),
        (3,'three','Three','hash',0,123,1,0),(4,'four','Four','hash',0,123,0,1);
        INSERT INTO ratings(account,ruleset,rating,deviation,volatility,games,wins,draws,losses,last_played)
        SELECT id,'duel',1800,80,0.06,7,4,1,2,456 FROM accounts;").expect("old records");
    let db = Db::init(connection).expect("migrate");
    let page = db
        .leaderboard(
            RulesetId("duel".into()),
            AccountKind::Human,
            String::new(),
            100,
            AccountId(1),
        )
        .await
        .expect("indexed board");
    assert_eq!(page.total, 1);
    assert_eq!(
        (
            page.rows[0].account,
            page.rows[0].rating.rating,
            page.rows[0].games
        ),
        (AccountId(1), 1800.0, 7)
    );
    let bot = db
        .leaderboard(
            RulesetId("duel".into()),
            AccountKind::Bot,
            String::new(),
            100,
            AccountId(2),
        )
        .await
        .expect("bot board");
    assert_eq!(bot.total, 1);
    assert_eq!(
        db.account_by_id(AccountId(1))
            .await
            .expect("query")
            .expect("credentials")
            .hash,
        "kept hash"
    );
}

#[test]
fn visible_ladder_totals_follow_every_rating_and_account_transition_atomically() {
    let db = Db::init(Connection::open_in_memory().expect("database")).expect("schema");
    let connection = db.conn.lock().expect("writer");
    connection.execute_batch("INSERT INTO accounts(id,login,nickname,hash,kind,created) VALUES(1,'one','One','hash',0,0),(2,'two','Two','hash',1,0);
        INSERT INTO ratings(account,ruleset,rating,deviation,volatility,games,wins,draws,losses,last_played) VALUES(1,'duel',1500,350,0.06,0,0,0,0,0),(2,'duel',1500,350,0.06,1,1,0,0,0);").expect("fixture");
    let counts = || {
        connection.prepare("SELECT kind,total FROM ladder_totals WHERE ruleset='duel' AND total>0 ORDER BY kind").expect("maintained totals").query_map([],|row| Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?))).expect("query").collect::<rusqlite::Result<Vec<_>>>().expect("counts")
    };
    assert_eq!(counts(), vec![(1, 1)]);
    connection
        .execute("UPDATE ratings SET games=1 WHERE account=1", [])
        .expect("first game");
    assert_eq!(counts(), vec![(0, 1), (1, 1)]);
    connection
        .execute("UPDATE accounts SET kind=0 WHERE id=2", [])
        .expect("reclassify");
    assert_eq!(counts(), vec![(0, 2)]);
    connection
        .execute_batch("BEGIN; UPDATE accounts SET banned=1 WHERE id=1; ROLLBACK;")
        .expect("rollback ban");
    assert_eq!(counts(), vec![(0, 2)]);
    connection
        .execute("UPDATE accounts SET banned=1 WHERE id=1", [])
        .expect("ban");
    assert_eq!(counts(), vec![(0, 1)]);
    connection
        .execute("UPDATE accounts SET banned=0 WHERE id=1", [])
        .expect("unban");
    assert_eq!(counts(), vec![(0, 2)]);
    connection
        .execute("UPDATE accounts SET deleted=1 WHERE id=2", [])
        .expect("delete");
    assert_eq!(counts(), vec![(0, 1)]);
    connection
        .execute("DELETE FROM ratings WHERE account=1", [])
        .expect("remove rating");
    assert!(counts().is_empty());
}

#[test]
fn existing_ratings_gain_indexed_account_kind_without_losing_account_or_rating_data() {
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let directory = Scratch(
        std::env::temp_dir().join(format!("radovid_kind_migration_{}", std::process::id())),
    );
    std::fs::create_dir(&directory.0).expect("scratch");
    let path = directory.0.join("ratings.db");
    let old = Connection::open(&path).expect("old database");
    old.execute_batch(include_str!("../../tests/fixtures/schema_2.sql"))
        .expect("schema 2");
    old.pragma_update(None, "user_version", 2)
        .expect("old version");
    old.execute("INSERT INTO accounts(id,login,nickname,hash,kind,created) VALUES(1,'robot','Robot','kept hash',1,123)",[]).expect("account");
    old.execute(
        "INSERT INTO ratings VALUES(1,'duel',1800,80,0.06,7,4,1,2,456)",
        [],
    )
    .expect("existing rating");
    drop(old);
    let db = Db::open(&path).expect("open and migrate");
    let connection = db.conn.lock().expect("connection");
    let (kind, rating, games): (i64, f64, u32) = connection
        .query_row(
            "SELECT kind,rating,games FROM ratings WHERE account=1 AND ruleset='duel'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("migrated indexed kind");
    assert_eq!((kind, rating, games), (1, 1800.0, 7));
    let hash: String = connection
        .query_row("SELECT hash FROM accounts WHERE id=1", [], |row| row.get(0))
        .expect("credentials");
    assert_eq!(hash, "kept hash");
    connection
        .execute("UPDATE accounts SET kind=0 WHERE id=1", [])
        .expect("reclassification");
    let kind: i64 = connection
        .query_row("SELECT kind FROM ratings WHERE account=1", [], |row| {
            row.get(0)
        })
        .expect("synchronised kind");
    assert_eq!(kind, 0);
}

#[tokio::test]
async fn schema_four_migration_adds_avatars_without_changing_visible_ladder_totals() {
    let connection = Connection::open_in_memory().expect("database");
    connection
        .execute_batch(include_str!("../../tests/fixtures/schema_4.sql"))
        .expect("schema four");
    connection.execute_batch("INSERT INTO accounts(id,login,nickname,hash,kind,created) VALUES(1,'one','One','kept hash',0,123);
        INSERT INTO ratings(account,ruleset,rating,deviation,volatility,games,wins,draws,losses,last_played) VALUES(1,'duel',1800,80,0.06,7,4,1,2,456);").expect("old records");
    let db = Db::init(connection).expect("migrate");
    assert_eq!(
        db.avatar(AccountId(1), AccountId(1))
            .await
            .expect("new endpoint"),
        Ok(None)
    );
    let image = crate::avatar::tests::image(32, 32);
    let stored = db
        .change_avatar(AccountId(1), Some(image), UnixMs(1))
        .await
        .expect("storage")
        .expect("valid")
        .expect("avatar");
    let page = db
        .leaderboard(
            RulesetId("duel".into()),
            AccountKind::Human,
            String::new(),
            100,
            AccountId(1),
        )
        .await
        .expect("board");
    assert_eq!(page.total, 1);
    assert_eq!(
        (page.rows[0].rating.rating, page.rows[0].games),
        (1800.0, 7)
    );
    assert_eq!(page.rows[0].avatar, Some(stored.version));
    assert_eq!(
        db.account_by_id(AccountId(1))
            .await
            .expect("query")
            .expect("credentials")
            .hash,
        "kept hash"
    );
}
