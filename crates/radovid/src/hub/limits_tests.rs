/// Component E2E: inject authenticated source keys at the real hub boundary;
/// no test configures host IPv6 addresses or trusts forwarded client headers.
#[tokio::test]
async fn ipv6_site_cap_combines_distinct_subnets_and_releases_cancelled_entries() {
    use crate::{config::Config, db::Db, hub::{self, Attach, Cmd, HubHandle}};
    use radovid_proto::*;
    use std::sync::Arc;
    use tokio::sync::oneshot;

    async fn request(hub: &HubHandle, session: u64, msg: ClientMsg) -> Result<Reply, Error> {
        let (reply, answer) = oneshot::channel();
        hub.tx.send(Cmd::Request { session, msg, reply }).await.expect("request");
        answer.await.expect("answer")
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut config = Config::load(&root.join("radovid.example.toml")).expect("base config");
    let dir = std::env::temp_dir().join(format!("radovid-site-capacity-{}", std::process::id()));
    std::fs::create_dir(&dir).expect("test directory");
    config.database = dir.join("hub.db");
    config.rulesets.truncate(1);
    config.rulesets[0].id = "solo".into();
    config.rulesets[0].artifacts.clear();
    let fingerprint = config.rulesets[0].game_fingerprint;
    config.limits.games_per_source = 2;
    config.limits.games_per_ipv6_48 = 2;
    let db = Db::open(&config.database).expect("db");
    let id = db.create_account("player".into(), "unused-auth-boundary".into(), AccountKind::Human, None).await.expect("account");
    let (hub, task) = hub::spawn(Arc::new(config), db.clone());
    let mut sessions = Vec::new();
    let mut receivers = Vec::new();
    for source in ["2001:db8:1:1::1", "2001:db8:1:2::1", "2001:db8:1:3::1", "2001:db8:2:1::1"] {
        let (events, receiver) = hub::event_channel();
        receivers.push(receiver);
        let (reply, answer) = oneshot::channel();
        hub.tx.send(Cmd::Attach(Box::new(Attach {
            account: db.account_by_id(id).await.expect("account read").expect("exists"),
            token_hash: [0; 32], game_fingerprint: fingerprint,
            source: crate::throttle::source(source.parse().expect("IPv6")), events, reply,
        }))).await.expect("attach");
        sessions.push(answer.await.expect("attached").expect("session").0);
    }
    let join = || ClientMsg::QueueJoin { ruleset: RulesetId("solo".into()) };
    let Reply::Queued { entry } = request(&hub, sessions[0], join()).await.expect("first /64") else { panic!("queue") };
    request(&hub, sessions[1], join()).await.expect("second /64");
    assert_eq!(request(&hub, sessions[2], join()).await.expect_err("same /48" ).to_string(), "TooManyGamesFromIpv6Site { limit: 2 }");
    request(&hub, sessions[3], join()).await.expect("another /48");
    request(&hub, sessions[0], ClientMsg::QueueLeave { entry: entry.entry }).await.expect("release");
    request(&hub, sessions[2], join()).await.expect("site capacity released");
    request(&hub, sessions[3], join()).await.expect("second of source");
    assert_eq!(request(&hub, sessions[3], join()).await, Err(Error::TooManyGamesFromSource { limit: 2 }));
    hub.tx.send(Cmd::Shutdown).await.expect("shutdown");
    task.await.expect("hub stopped");
    drop(db);
    std::fs::remove_dir_all(dir).expect("test cleanup");
}
