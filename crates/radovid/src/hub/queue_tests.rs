//! Queue policy at the authenticated hub boundary; no launcher or network timing involved.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use radovid_proto::*;
use tokio::sync::{mpsc, oneshot};

use crate::config::Config;
use crate::db::{AccountRow, Db};
use crate::hub::{self, Attach, Cmd, HubHandle};

struct Fixture {
    hub: HubHandle,
    task: Option<tokio::task::JoinHandle<()>>,
    db: Db,
    config: Config,
    receivers: Vec<mpsc::Receiver<ServerMsg>>,
    _directory: Scratch,
}

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "radovid_queue_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).expect("scratch directory");
        Self(directory)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Fixture {
    fn new(capacity: u32) -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut config = Config::load(&root.join("radovid.example.toml")).expect("configuration");
        let directory = Scratch::new();
        config.database = directory.0.join("hub.db");
        config.rulesets.truncate(1);
        config.rulesets[0].id = "duel".into();
        config.artifacts.save_replays = false;
        config.rulesets[0].artifacts.clear();
        // Acceptance is tested before launch; no game process is needed for these assertions.
        config.rulesets[0].launcher.binary = Some(directory.0.join("no_game_process"));
        config.limits.queue_capacity = capacity;
        config.limits.games_per_account.human = 1;
        config.limits.games_per_account.bot = 1;
        let db = Db::open(&config.database).expect("database");
        let (hub, task) = hub::spawn(Arc::new(config.clone()), db.clone());
        Self {
            hub,
            task: Some(task),
            db,
            config,
            receivers: Vec::new(),
            _directory: directory,
        }
    }

    async fn attach(&mut self, account: AccountRow) -> (u64, Activities) {
        let (events, receiver) = hub::event_channel();
        self.receivers.push(receiver);
        let (reply, answer) = oneshot::channel();
        self.hub
            .tx
            .send(Cmd::Attach(Box::new(Attach {
                account,
                token_hash: [0; 32],
                game_fingerprint: self.config.rulesets[0].game_fingerprint,
                source: "127.0.0.1".parse().expect("loopback"),
                events,
                reply,
            })))
            .await
            .expect("attach");
        answer.await.expect("answer").expect("session")
    }

    async fn account(&mut self, login: &str, kind: AccountKind) -> (AccountRow, u64) {
        let id = self
            .db
            .create_account(login.into(), "authenticated fixture".into(), kind, None)
            .await
            .expect("account");
        let account = self
            .db
            .account_by_id(id)
            .await
            .expect("read")
            .expect("exists");
        let (session, _) = self.attach(account.clone()).await;
        (account, session)
    }

    async fn ask(&self, session: u64, msg: ClientMsg) -> Result<Reply, Error> {
        let (reply, answer) = oneshot::channel();
        self.hub
            .tx
            .send(Cmd::Request {
                session,
                msg,
                reply,
            })
            .await
            .expect("request");
        answer.await.expect("answer")
    }

    async fn finish(mut self) {
        self.hub.tx.send(Cmd::Shutdown).await.expect("shutdown");
        self.task
            .take()
            .expect("running task")
            .await
            .expect("stopped");
    }
}

fn join(audiences: &[Audience]) -> ClientMsg {
    ClientMsg::QueueJoinAudiences {
        ruleset: RulesetId("duel".into()),
        audiences: audiences.to_vec(),
    }
}

fn joined(reply: Result<Reply, Error>) -> Vec<QueueEntry> {
    match reply.expect("queue admission") {
        Reply::QueuesJoined { entries } => entries,
        other => panic!("queue reply: {other:?}"),
    }
}

fn queue_lefts(
    receiver: &mut mpsc::Receiver<ServerMsg>,
) -> Vec<(QueueEntryId, QueueLeftReason, u32)> {
    let mut left = Vec::new();
    for _ in 0..hub::EVENT_QUEUE {
        match receiver.try_recv() {
            Ok(ServerMsg::Event(Event::QueueLeft {
                entry,
                reason,
                cooldown_ms,
            })) => left.push((entry, reason, cooldown_ms)),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    left.sort_by_key(|(entry, ..)| entry.0);
    left
}

#[tokio::test]
async fn accepting_a_linked_search_announces_and_consumes_both_places() {
    let mut fixture = Fixture::new(16);
    let (human, session) = fixture.account("human", AccountKind::Human).await;
    let (_, peer) = fixture.account("peer", AccountKind::Human).await;
    let entries = joined(
        fixture
            .ask(session, join(&[Audience::HumansOnly, Audience::Everyone]))
            .await,
    );
    joined(fixture.ask(peer, join(&[Audience::HumansOnly])).await);
    let proposal = fixture.attach(human.clone()).await.1.proposals[0].proposal;
    fixture
        .ask(session, ClientMsg::MatchAccept { proposal })
        .await
        .expect("first accept");
    fixture
        .ask(peer, ClientMsg::MatchAccept { proposal })
        .await
        .expect("second accept");
    assert!(
        fixture.attach(human).await.1.queued.is_empty(),
        "acceptance consumes both raw queue places"
    );
    assert_eq!(
        queue_lefts(&mut fixture.receivers[0]),
        entries
            .iter()
            .map(|entry| (entry.entry, QueueLeftReason::Matched, 0))
            .collect::<Vec<_>>()
    );
    fixture.finish().await;
}

#[tokio::test]
async fn a_proposal_timeout_announces_and_removes_both_places() {
    let mut fixture = Fixture::new(16);
    let (human, session) = fixture.account("human", AccountKind::Human).await;
    let (_, peer) = fixture.account("peer", AccountKind::Human).await;
    let entries = joined(
        fixture
            .ask(session, join(&[Audience::HumansOnly, Audience::Everyone]))
            .await,
    );
    joined(fixture.ask(peer, join(&[Audience::HumansOnly])).await);
    let proposal = fixture.attach(human.clone()).await.1.proposals[0].proposal;
    fixture
        .hub
        .tx
        .send(Cmd::ProposalDeadline { proposal })
        .await
        .expect("deadline");
    assert!(
        fixture.attach(human).await.1.queued.is_empty(),
        "timeout removes both raw places"
    );
    let cooldown_ms = fixture.config.limits.decline_cooldown_secs * 1000;
    assert_eq!(
        queue_lefts(&mut fixture.receivers[0]),
        entries
            .iter()
            .map(|entry| (entry.entry, QueueLeftReason::TimedOut, cooldown_ms))
            .collect::<Vec<_>>()
    );
    fixture.finish().await;
}

#[tokio::test]
async fn another_live_session_keeps_both_search_places_after_disconnect() {
    let mut fixture = Fixture::new(16);
    let (human, session) = fixture.account("human", AccountKind::Human).await;
    fixture.attach(human.clone()).await;
    let entries = joined(
        fixture
            .ask(session, join(&[Audience::HumansOnly, Audience::Everyone]))
            .await,
    );
    fixture
        .hub
        .tx
        .send(Cmd::Detach { session })
        .await
        .expect("first session closes");
    let snapshot = fixture.attach(human).await.1;
    assert_eq!(
        snapshot
            .queued
            .iter()
            .map(|entry| entry.entry)
            .collect::<Vec<_>>(),
        entries.iter().map(|entry| entry.entry).collect::<Vec<_>>(),
        "a second session keeps the entire search alive"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn a_panicking_queue_test_cleans_its_scratch_directory() {
    let fixture = Fixture::new(16);
    let directory = fixture
        .config
        .database
        .parent()
        .expect("directory")
        .to_owned();
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _fixture = fixture;
        panic!("intentional fixture unwind");
    }));
    assert!(panicked.is_err());
    let leaked = directory.exists();
    if leaked {
        std::fs::remove_dir_all(&directory).expect("clean the red reproduction's own directory");
    }
    assert!(
        !leaked,
        "a fixture panic must not leave its scratch directory"
    );
}

#[tokio::test]
async fn humans_and_everyone_are_separate_and_bots_cannot_join_humans() {
    let mut fixture = Fixture::new(16);
    let (human, human_session) = fixture.account("human", AccountKind::Human).await;
    let (bot, bot_session) = fixture.account("bot", AccountKind::Bot).await;
    let humans = joined(
        fixture
            .ask(human_session, join(&[Audience::HumansOnly]))
            .await,
    );
    let everyone = joined(fixture.ask(bot_session, join(&[Audience::Everyone])).await);
    assert_eq!(humans[0].audience, Audience::HumansOnly);
    assert_eq!(everyone[0].audience, Audience::Everyone);
    assert!(fixture.attach(human).await.1.proposals.is_empty());
    assert!(fixture.attach(bot).await.1.proposals.is_empty());
    assert_eq!(
        fixture
            .ask(bot_session, join(&[Audience::HumansOnly]))
            .await,
        Err(Error::AudienceMismatch)
    );
    fixture.finish().await;
}

#[tokio::test]
async fn a_linked_search_counts_as_one_game_and_can_be_reserved_by_only_one_proposal() {
    let mut fixture = Fixture::new(16);
    let (human, human_session) = fixture.account("human", AccountKind::Human).await;
    let (_, peer_session) = fixture.account("peer", AccountKind::Human).await;
    let (bot, bot_session) = fixture.account("bot", AccountKind::Bot).await;
    let entries = joined(
        fixture
            .ask(
                human_session,
                join(&[Audience::HumansOnly, Audience::Everyone]),
            )
            .await,
    );
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].search, entries[1].search);
    assert_eq!(
        fixture
            .ask(human_session, join(&[Audience::Everyone]))
            .await,
        Err(Error::TooManyGames { limit: 1 })
    );
    joined(
        fixture
            .ask(peer_session, join(&[Audience::HumansOnly]))
            .await,
    );
    joined(fixture.ask(bot_session, join(&[Audience::Everyone])).await);
    let state = fixture.attach(human).await.1;
    assert_eq!(
        state.proposals.len(),
        1,
        "one proposal owns the whole linked search"
    );
    assert_eq!(
        state.queued.len(),
        2,
        "both places survive until everyone accepts"
    );
    assert!(
        fixture.attach(bot).await.1.proposals.is_empty(),
        "the sibling entry is reserved too"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn leaving_either_entry_removes_the_entire_linked_search() {
    let mut fixture = Fixture::new(16);
    let (human, session) = fixture.account("human", AccountKind::Human).await;
    let entries = joined(
        fixture
            .ask(session, join(&[Audience::HumansOnly, Audience::Everyone]))
            .await,
    );
    assert_eq!(
        fixture
            .ask(
                session,
                ClientMsg::QueueLeave {
                    entry: entries[1].entry
                }
            )
            .await,
        Ok(Reply::Done)
    );
    assert!(fixture.attach(human).await.1.queued.is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn leaving_the_reserved_sibling_cancels_the_proposed_search() {
    let mut fixture = Fixture::new(16);
    let (human, session) = fixture.account("human", AccountKind::Human).await;
    let (_, peer) = fixture.account("peer", AccountKind::Human).await;
    let entries = joined(
        fixture
            .ask(session, join(&[Audience::HumansOnly, Audience::Everyone]))
            .await,
    );
    joined(fixture.ask(peer, join(&[Audience::HumansOnly])).await);
    let sibling = entries
        .iter()
        .find(|entry| entry.audience == Audience::Everyone)
        .expect("sibling");
    assert_eq!(
        fixture
            .ask(
                session,
                ClientMsg::QueueLeave {
                    entry: sibling.entry
                }
            )
            .await,
        Ok(Reply::Done)
    );
    assert!(
        fixture.attach(human).await.1.queued.is_empty(),
        "cancelling the reserved sibling removes the whole proposed search"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn changing_a_queued_human_to_a_bot_cancels_its_humans_proposal() {
    let mut fixture = Fixture::new(16);
    let (mut admin, _) = fixture.account("admin", AccountKind::Human).await;
    admin.admin = true;
    let admin_session = fixture.attach(admin).await.0;
    let (human, session) = fixture.account("human", AccountKind::Human).await;
    let (_, peer) = fixture.account("peer", AccountKind::Human).await;
    joined(
        fixture
            .ask(session, join(&[Audience::HumansOnly, Audience::Everyone]))
            .await,
    );
    joined(fixture.ask(peer, join(&[Audience::HumansOnly])).await);
    assert_eq!(
        fixture
            .ask(
                admin_session,
                ClientMsg::AdminSetKind {
                    account: human.id,
                    kind: AccountKind::Bot
                }
            )
            .await,
        Ok(Reply::Done)
    );
    let updated = fixture
        .db
        .account_by_id(human.id)
        .await
        .expect("read")
        .expect("account");
    let snapshot = fixture.attach(updated).await.1;
    assert!(
        snapshot.queued.is_empty(),
        "a newly classified bot must leave its human search"
    );
    assert!(
        snapshot.proposals.is_empty(),
        "a human-only proposal cannot survive that kind change"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn a_failed_proposal_restores_both_original_places() {
    let mut fixture = Fixture::new(16);
    let (human, session) = fixture.account("human", AccountKind::Human).await;
    let (_, peer) = fixture.account("peer", AccountKind::Human).await;
    let (_, bot) = fixture.account("bot", AccountKind::Bot).await;
    let entries = joined(
        fixture
            .ask(session, join(&[Audience::HumansOnly, Audience::Everyone]))
            .await,
    );
    joined(fixture.ask(peer, join(&[Audience::HumansOnly])).await);
    let proposed = fixture.attach(human.clone()).await.1.proposals[0].proposal;
    fixture
        .ask(peer, ClientMsg::MatchDecline { proposal: proposed })
        .await
        .expect("decline");
    let snapshot = fixture.attach(human.clone()).await.1;
    assert_eq!(
        snapshot
            .queued
            .iter()
            .map(|entry| entry.entry)
            .collect::<Vec<_>>(),
        entries.iter().map(|entry| entry.entry).collect::<Vec<_>>()
    );
    assert!(snapshot.proposals.is_empty());
    joined(fixture.ask(bot, join(&[Audience::Everyone])).await);
    assert_eq!(
        fixture.attach(human).await.1.proposals[0].audience,
        Audience::Everyone
    );
    fixture.finish().await;
}

#[tokio::test]
async fn losing_the_last_session_removes_both_queue_entries() {
    let mut fixture = Fixture::new(16);
    let (human, session) = fixture.account("human", AccountKind::Human).await;
    joined(
        fixture
            .ask(session, join(&[Audience::HumansOnly, Audience::Everyone]))
            .await,
    );
    fixture
        .hub
        .tx
        .send(Cmd::Detach { session })
        .await
        .expect("disconnect");
    assert!(fixture.attach(human).await.1.queued.is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn dual_admission_is_atomic_and_rejects_unsupported_or_repeated_audiences() {
    let mut fixture = Fixture::new(1);
    let (human, session) = fixture.account("human", AccountKind::Human).await;
    assert!(matches!(
        fixture
            .ask(session, join(&[Audience::HumansOnly, Audience::Everyone]))
            .await,
        Err(Error::QueueFull { .. })
    ));
    assert!(
        fixture.attach(human).await.1.queued.is_empty(),
        "no partial admission"
    );
    for audiences in [
        vec![],
        vec![Audience::Everyone, Audience::Everyone],
        vec![Audience::BotsOnly],
    ] {
        assert_eq!(
            fixture.ask(session, join(&audiences)).await,
            Err(Error::BadQueueAudiences)
        );
    }
    fixture.finish().await;
}
