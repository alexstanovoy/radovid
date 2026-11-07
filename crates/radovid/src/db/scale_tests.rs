//! Release-scale contracts from the public lookup denial-of-service review.
use super::*;
use std::time::{Duration, Instant};

const ACCOUNTS: i64 = 1_000_000;
struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn nickname(id: i64) -> String {
    let alphabet = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut number = (id as u64).wrapping_mul(0x9E3779B97F4A7C15);
    let mut name = String::new();
    for _ in 0..8 {
        name.push(alphabet[(number % 36) as usize] as char);
        number /= 36;
    }
    format!("{name}{id}")
}

fn fixture(tag: &str) -> (Scratch, Db) {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("radovid_million_{tag}_{}", std::process::id())));
    std::fs::create_dir(&scratch.0).expect("scratch");
    let db = Db::open(&scratch.0.join("ratings.db")).expect("db");
    {
        let mut connection = db.conn.lock().expect("writer");
        let transaction = connection.transaction().expect("transaction");
        {
            let mut account=transaction.prepare("INSERT INTO accounts(id,login,nickname,hash,kind,created) VALUES(?1,'login'||?1,?2,'fixture',0,0)").expect("accounts");
            let mut rating=transaction.prepare("INSERT INTO ratings(account,ruleset,rating,deviation,volatility,games,wins,draws,losses,last_played) VALUES(?1,'duel',?2,50,0.06,1,1,0,0,100)").expect("ratings");
            for id in 1..=ACCOUNTS {
                account.execute(params![id, nickname(id)]).expect("account");
                rating
                    .execute(params![id, 1000 + (id * 7919) % 2000])
                    .expect("rating");
            }
        }
        transaction.commit().expect("commit");
    }
    (scratch, db)
}

fn bottom() -> AccountId {
    AccountId(
        (1..=ACCOUNTS)
            .min_by_key(|id| (1000 + (id * 7919) % 2000, -id))
            .expect("bottom") as u64,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn one_million_rated_accounts_serve_board_search_profile_and_legacy_with_bounded_latency() {
    let (_scratch, db) = fixture("latency");
    let rule = RulesetId("duel".into());
    let mut durations = Vec::new();
    for prefix in [String::new(), nickname(bottom().0 as i64), "a".into()] {
        let began = Instant::now();
        let page = db
            .leaderboard(
                rule.clone(),
                AccountKind::Human,
                prefix.clone(),
                100,
                bottom(),
            )
            .await
            .expect("available at one million");
        durations.push(began.elapsed());
        assert_eq!(page.own.expect("own position").rank, ACCOUNTS as u32);
        eprintln!("MILLION board/search {prefix:?}: {:?}", began.elapsed());
    }
    let began = Instant::now();
    db.public_profile(bottom(), vec![rule.clone()], bottom())
        .await
        .expect("profile");
    durations.push(began.elapsed());
    eprintln!("MILLION profile: {:?}", began.elapsed());
    let began = Instant::now();
    let (total, rows) = db
        .ladder(rule, ACCOUNTS as u32 - 100, 100)
        .await
        .expect("deep legacy offset");
    durations.push(began.elapsed());
    assert_eq!((total, rows.len()), (ACCOUNTS as u32, 100));
    eprintln!("MILLION deep legacy offset: {:?}", began.elapsed());
    assert!(
        durations
            .iter()
            .all(|elapsed| *elapsed < Duration::from_millis(500)),
        "each lookup must finish within half a second: {durations:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn eight_accounts_with_one_search_in_flight_cannot_starve_another_profile_at_one_million() {
    let (_scratch, db) = fixture("starvation");
    let (ready, mut started) = tokio::sync::mpsc::channel(8);
    let requester = bottom();
    let mut attackers = Vec::new();
    for index in 0..8 {
        let requester = AccountId(requester.0 - index * 2000);
        let (database, ready) = (db.clone(), ready.clone());
        attackers.push(tokio::spawn(async move {
            ready.send(()).await.expect("started");
            database
                .leaderboard(
                    RulesetId("duel".into()),
                    AccountKind::Human,
                    "a".into(),
                    1000,
                    requester,
                )
                .await
        }));
    }
    for _ in 0..8 {
        started.recv().await.expect("all searches started");
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while db.lookup_pending_for_test() < 8 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("eight actual admitted reads, not just spawned tasks");
    let began = Instant::now();
    let result = db
        .public_profile(AccountId(1), vec![RulesetId("duel".into())], AccountId(1))
        .await;
    let elapsed = began.elapsed();
    for attacker in attackers {
        attacker
            .await
            .expect("task")
            .expect("bounded search available");
    }
    result.expect("victim served");
    eprintln!("MILLION eight-search victim: {elapsed:?}");
    assert!(
        elapsed < Duration::from_secs(2),
        "eight in-flight searches delayed a profile by {elapsed:?}"
    );
}
