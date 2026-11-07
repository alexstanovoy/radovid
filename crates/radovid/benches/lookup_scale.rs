//! Explicit disk-backed population benchmark; never part of the lib suite.
use radovid::db::Db;
use radovid_proto::*;
use rusqlite::{Connection, params};
use std::path::PathBuf;
use std::time::Instant;

const ACCOUNTS: i64 = 1_000_000;
const MODES: [&str;3] = ["duel","teams","race"];

fn nickname(id:i64)->String {
    let alphabet=b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut number=(id as u64).wrapping_mul(0x9E3779B97F4A7C15);
    let mut name=String::new();
    for _ in 0..8 { name.push(alphabet[(number%36) as usize] as char); number/=36; }
    format!("{name}{id}")
}

fn database()->Db {
    let target=std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(||PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    let directory=target.join("lookup_scale/schema_4_seed_v1");
    let path=directory.join("ratings.db");
    let ready=directory.join("complete");
    if !ready.exists() {
        if directory.exists() { std::fs::remove_dir_all(&directory).expect("incomplete owned fixture"); }
        std::fs::create_dir_all(&directory).expect("benchmark directory");
        let db=Db::open(&path).expect("schema");
        drop(db);
        let mut connection=Connection::open(&path).expect("seed connection");
        let transaction=connection.transaction().expect("seed transaction");
        {
            let mut accounts=transaction.prepare("INSERT INTO accounts(id,login,nickname,hash,kind,created) VALUES(?1,'login'||?1,?2,'fixture',0,0)").expect("accounts");
            let mut ratings=transaction.prepare("INSERT INTO ratings(account,ruleset,rating,deviation,volatility,games,wins,draws,losses,last_played) VALUES(?1,?2,?3,50,0.06,1,1,0,0,100)").expect("ratings");
            for id in 1..=ACCOUNTS {
                accounts.execute(params![id,nickname(id)]).expect("account");
                for mode in MODES { ratings.execute(params![id,mode,1000+(id*7919)%2000]).expect("rating"); }
            }
        }
        transaction.commit().expect("seed commit");
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").expect("seed checkpoint");
        std::fs::write(&ready,b"one million accounts, three modes, schema4 fixture v1").expect("complete marker");
    }
    let connection=Connection::open(&path).expect("verify fixture");
    let counts:(i64,i64)=connection.query_row("SELECT (SELECT COUNT(*) FROM accounts),(SELECT COUNT(*) FROM ratings)",[],|row|Ok((row.get(0)?,row.get(1)?))).expect("counts");
    assert_eq!(counts,(ACCOUNTS,ACCOUNTS*MODES.len() as i64));
    Db::open(&path).expect("benchmark database")
}

fn main() {
    let db=database();
    let runtime=tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        let requester=AccountId(ACCOUNTS as u64);
        for prefix in [String::new(),nickname(ACCOUNTS),"a".into()] {
            let began=Instant::now();
            let page=db.leaderboard(RulesetId("duel".into()),AccountKind::Human,prefix.clone(),100,requester).await.expect("board within instruction budget");
            assert_eq!(page.own.expect("own row").rank,ACCOUNTS as u32);
            eprintln!("MILLION board/search {prefix:?}: {:?}",began.elapsed());
        }
        let began=Instant::now();
        let profile=db.public_profile(requester,MODES.map(|mode|RulesetId(mode.into())).to_vec(),requester).await.expect("profile survives rank budget exhaustion").expect("profile");
        assert_eq!(profile.ratings.len(),MODES.len());
        assert!(profile.ratings.iter().all(|(_,rating)|rating.games==1));
        eprintln!("MILLION three-mode profile {:?}, ranks {:?}",began.elapsed(),profile.ratings.iter().map(|(_,rating)|rating.rank).collect::<Vec<_>>());
        let began=Instant::now();
        let (total,rows)=db.ladder(RulesetId("duel".into()),ACCOUNTS as u32-100,100).await.expect("legacy within instruction budget");
        assert_eq!((total,rows.len()),(ACCOUNTS as u32,100));
        eprintln!("MILLION deep offset: {:?}",began.elapsed());
        let mut searches=Vec::new();
        let (admitted,mut ready)=tokio::sync::mpsc::channel(16);
        for index in 0..16 {
            let database=db.clone();
            let admitted=admitted.clone();
            searches.push(tokio::spawn(async move {
                let mut request=Box::pin(database.leaderboard(RulesetId("duel".into()),AccountKind::Human,"a".into(),1000,AccountId(ACCOUNTS as u64-index*2000)));
                let first=std::future::poll_fn(|context|std::task::Poll::Ready(request.as_mut().poll(context))).await;
                admitted.send(()).await.expect("request first polled");
                match first {std::task::Poll::Ready(result)=>result,std::task::Poll::Pending=>request.await}
            }));
        }
        for _ in 0..16 {ready.recv().await.expect("sixteen requests admitted or already complete");}
        let began=Instant::now();
        db.public_profile(AccountId(1),vec![RulesetId("duel".into())],AccountId(1)).await.expect("victim profile");
        let victim=began.elapsed();
        for task in searches { task.await.expect("task").expect("search"); }
        eprintln!("MILLION sixteen-first-polled-search victim {victim:?}, wave completion {:?}",began.elapsed());
    });
}
