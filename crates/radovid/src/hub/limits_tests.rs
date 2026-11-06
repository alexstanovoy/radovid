use std::sync::Arc;

use radovid_proto::*;
use tokio::sync::{mpsc, oneshot};

use crate::config::Config;
use crate::db::{AccountRow, Db};
use crate::hub::{self, Attach, Cmd, HubHandle};

async fn request(hub: &HubHandle, session: u64, msg: ClientMsg) -> Result<Reply, Error> {
    let (reply, answer) = oneshot::channel();
    hub.tx
        .send(Cmd::Request {
            session,
            msg,
            reply,
        })
        .await
        .expect("request");
    answer.await.expect("answer")
}

async fn attach(
    hub: &HubHandle,
    account: AccountRow,
    fingerprint: u64,
    source: &str,
) -> (u64, mpsc::Receiver<ServerMsg>) {
    let (events, receiver) = hub::event_channel();
    let (reply, answer) = oneshot::channel();
    hub.tx
        .send(Cmd::Attach(Box::new(Attach {
            account,
            token_hash: [0; 32],
            game_fingerprint: fingerprint,
            source: crate::throttle::source(source.parse().expect("IPv6")),
            events,
            reply,
        })))
        .await
        .expect("attach");
    (
        answer.await.expect("attached").expect("session").0,
        receiver,
    )
}

fn configuration() -> Config {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut config = Config::load(&root.join("radovid.example.toml")).expect("base config");
    let dir = std::env::temp_dir().join(format!("radovid-site-capacity-{}", std::process::id()));
    std::fs::create_dir(&dir).expect("test directory");
    config.database = dir.join("hub.db");
    config.rulesets.truncate(1);
    config.rulesets[0].id = "solo".into();
    config.rulesets[0].artifacts.clear();
    config.limits.games_per_source = 2;
    config.limits.games_per_ipv6_48 = 2;
    config
}

/// Component E2E: inject authenticated source keys at the real hub boundary;
/// no test configures host IPv6 addresses or trusts forwarded client headers.
#[tokio::test]
async fn ipv6_site_cap_combines_distinct_subnets_and_releases_cancelled_entries() {
    let config = configuration();
    let dir = config.database.parent().expect("test directory").to_owned();
    let fingerprint = config.rulesets[0].game_fingerprint;
    let db = Db::open(&config.database).expect("db");
    let id = db
        .create_account(
            "player".into(),
            "unused-auth-boundary".into(),
            AccountKind::Human,
            None,
        )
        .await
        .expect("account");
    let (hub, task) = hub::spawn(Arc::new(config), db.clone());
    let mut sessions = Vec::new();
    let mut receivers = Vec::new();
    for source in [
        "2001:db8:1:1::1",
        "2001:db8:1:2::1",
        "2001:db8:1:3::1",
        "2001:db8:2:1::1",
    ] {
        let account = db
            .account_by_id(id)
            .await
            .expect("account read")
            .expect("exists");
        let (session, receiver) = attach(&hub, account, fingerprint, source).await;
        sessions.push(session);
        receivers.push(receiver);
    }
    assert_site_admission(&hub, &sessions).await;
    hub.tx.send(Cmd::Shutdown).await.expect("shutdown");
    task.await.expect("hub stopped");
    drop(db);
    std::fs::remove_dir_all(dir).expect("test cleanup");
}

async fn assert_site_admission(hub: &HubHandle, sessions: &[u64]) {
    let join = || ClientMsg::QueueJoin {
        ruleset: RulesetId("solo".into()),
    };
    let Reply::Queued { entry } = request(hub, sessions[0], join()).await.expect("first /64")
    else {
        panic!("queue")
    };
    request(hub, sessions[1], join()).await.expect("second /64");
    assert_eq!(
        request(hub, sessions[2], join()).await,
        Err(Error::TooManyGamesFromIpv6Site { limit: 2 })
    );
    request(hub, sessions[3], join())
        .await
        .expect("another /48");
    request(
        hub,
        sessions[0],
        ClientMsg::QueueLeave { entry: entry.entry },
    )
    .await
    .expect("release");
    request(hub, sessions[2], join())
        .await
        .expect("site capacity released");
    request(hub, sessions[3], join())
        .await
        .expect("second of source");
    assert_eq!(
        request(hub, sessions[3], join()).await,
        Err(Error::TooManyGamesFromSource { limit: 2 })
    );
}
