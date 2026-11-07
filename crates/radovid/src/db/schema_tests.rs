use super::*;

#[test]
fn existing_ratings_gain_indexed_account_kind_without_losing_account_or_rating_data() {
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch { fn drop(&mut self) { let _=std::fs::remove_dir_all(&self.0); } }
    let directory=Scratch(std::env::temp_dir().join(format!("radovid_kind_migration_{}",std::process::id())));
    std::fs::create_dir(&directory.0).expect("scratch");
    let path=directory.0.join("ratings.db");
    let old=Connection::open(&path).expect("old database");
    old.execute_batch(include_str!("../../tests/fixtures/schema_2.sql")).expect("schema 2");
    old.pragma_update(None,"user_version",2).expect("old version");
    old.execute("INSERT INTO accounts(id,login,nickname,hash,kind,created) VALUES(1,'robot','Robot','kept hash',1,123)",[]).expect("account");
    old.execute("INSERT INTO ratings VALUES(1,'duel',1800,80,0.06,7,4,1,2,456)",[]).expect("existing rating");
    drop(old);
    let db=Db::open(&path).expect("open and migrate");
    let connection=db.conn.lock().expect("connection");
    let (kind,rating,games):(i64,f64,u32)=connection.query_row("SELECT kind,rating,games FROM ratings WHERE account=1 AND ruleset='duel'",[],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).expect("migrated indexed kind");
    assert_eq!((kind,rating,games),(1,1800.0,7));
    let hash:String=connection.query_row("SELECT hash FROM accounts WHERE id=1",[],|row| row.get(0)).expect("credentials");
    assert_eq!(hash,"kept hash");
    connection.execute("UPDATE accounts SET kind=0 WHERE id=1",[]).expect("reclassification");
    let kind:i64=connection.query_row("SELECT kind FROM ratings WHERE account=1",[],|row| row.get(0)).expect("synchronised kind");
    assert_eq!(kind,0);
}
