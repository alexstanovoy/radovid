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
    // Schema 6 removes the ratings bot accounts earned before versions existed.
    assert_eq!(bot.total, 0);
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

#[tokio::test]
async fn existing_ratings_gain_indexed_account_kind_and_a_versionless_bot_can_become_human() {
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
    old.execute("INSERT INTO accounts(id,login,nickname,hash,kind,created) VALUES(1,'robot','Robot','kept hash',1,123),(2,'person','Person','hash',0,123)",[]).expect("accounts");
    old.execute(
        "INSERT INTO ratings VALUES(1,'duel',1800,80,0.06,7,4,1,2,456),(2,'duel',1700,90,0.06,5,3,0,2,457)",
        [],
    )
    .expect("existing ratings");
    drop(old);
    let db = Db::open(&path).expect("open and migrate");
    {
        let connection = db.conn.lock().expect("connection");
        let (kind, rating, games): (i64, f64, u32) = connection
            .query_row(
                "SELECT kind,rating,games FROM ratings WHERE account=2 AND ruleset='duel'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("migrated indexed kind");
        assert_eq!((kind, rating, games), (0, 1700.0, 5));
        let bot_ratings: i64 = connection
            .query_row("SELECT COUNT(*) FROM ratings WHERE account=1", [], |row| {
                row.get(0)
            })
            .expect("count");
        assert_eq!(bot_ratings, 0, "bot versions start from zero");
        let hash: String = connection
            .query_row("SELECT hash FROM accounts WHERE id=1", [], |row| row.get(0))
            .expect("credentials");
        assert_eq!(hash, "kept hash");
        connection
            .execute("UPDATE accounts SET kind=1 WHERE id=2", [])
            .expect("reclassification");
        let kind: i64 = connection
            .query_row("SELECT kind FROM ratings WHERE account=2", [], |row| {
                row.get(0)
            })
            .expect("synchronised kind");
        assert_eq!(kind, 1);
    }
    db.set_kind(AccountId(1), AccountKind::Human)
        .await
        .expect("a bot account with no version becomes human");
    let account = db
        .account_by_id(AccountId(1))
        .await
        .expect("query")
        .expect("exists");
    assert_eq!(account.kind, AccountKind::Human);
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

fn schema_five_database(connection: &Connection) {
    connection
        .execute_batch(include_str!("../../tests/fixtures/schema_5.sql"))
        .expect("the frozen schema five");
    connection.execute_batch("INSERT INTO accounts(id,login,nickname,hash,kind,created) VALUES
        (1,'human','Human','hash',0,123),(2,'robot','Robot','hash',1,123),(3,'other','Other','hash',1,123);
        INSERT INTO ratings(account,ruleset,rating,deviation,volatility,games,wins,draws,losses,last_played)
        SELECT id,'duel',1800,80,0.06,7,4,1,2,456 FROM accounts;").expect("records");
}

fn rating_rows(db: &Db, kind: i64) -> i64 {
    let connection = db.conn.lock().expect("connection");
    connection
        .query_row(
            "SELECT COUNT(*) FROM ratings WHERE account IN (SELECT id FROM accounts WHERE kind=?1)",
            [kind],
            |row| row.get(0),
        )
        .expect("count")
}

#[test]
fn schema_six_migration_removes_bot_ratings_once_and_keeps_human_ratings_and_accounts() {
    let directory = std::env::temp_dir().join(format!("radovid_schema_six_{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("scratch");
    let path = directory.join("ratings.db");
    schema_five_database(&Connection::open(&path).expect("database"));

    let db = Db::init(Connection::open(&path).expect("database")).expect("migrate");
    assert_eq!(rating_rows(&db, 0), 1, "the human rating is kept");
    assert_eq!(rating_rows(&db, 1), 0, "the bot ratings are removed");
    let accounts: i64 = db
        .conn
        .lock()
        .expect("connection")
        .query_row("SELECT COUNT(*) FROM accounts", [], |row| row.get(0))
        .expect("accounts");
    assert_eq!(accounts, 3);

    // A bot rating earned after the migration survives the next start.
    db.conn
        .lock()
        .expect("connection")
        .execute("INSERT INTO ratings(account,ruleset,rating,deviation,volatility,games,wins,draws,losses,last_played) VALUES(2,'duel',1500,350,0.06,1,1,0,0,9)", [])
        .expect("new bot rating");
    drop(db);
    let db = Db::init(Connection::open(&path).expect("database")).expect("reopen");
    assert_eq!(rating_rows(&db, 1), 1, "migration runs once");
    drop(db);
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn schema_six_migration_reports_how_many_bot_rating_rows_it_removed() {
    let mut connection = Connection::open_in_memory().expect("database");
    schema_five_database(&connection);
    let transaction = connection.transaction().expect("transaction");
    assert_eq!(versions::migrate(&transaction).expect("migrate"), 2);
}
