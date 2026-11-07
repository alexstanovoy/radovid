//! Deterministic hub contracts: persisted ratings, passage of queue time and concurrent results.
use super::*;
use crate::config::RatingWindowConfig;
use crate::launcher::GameResult;
use std::sync::atomic::{AtomicU32, Ordering};

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) struct Fixture {
    pub(super) hub: Hub,
    _commands: mpsc::Receiver<Cmd>,
    events: Vec<mpsc::Receiver<ServerMsg>>,
    _directory: Scratch,
}

impl Fixture {
    pub(super) fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let directory = Scratch(std::env::temp_dir().join(format!(
            "radovid_windows_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        std::fs::create_dir(&directory.0).expect("scratch");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut config = Config::load(&root.join("radovid.example.toml")).expect("config");
        config.database = directory.0.join("hub.db");
        config.limits.games_per_source = 200;
        config.rulesets.truncate(1);
        config.rulesets[0].id = "ranked".into();
        config.rulesets[0].rating_window = RatingWindowConfig {
            initial: Some(100),
            widen_per_minute: Some(100),
            maximum: Some(500),
        };
        config.rulesets[0].launcher.binary = Some(directory.0.join("no_game_process"));
        let mut casual = config.rulesets[0].clone();
        casual.id = "casual".into();
        casual.rated = false;
        casual.rating_ruleset = Some("ranked".into());
        casual.rating_window = Default::default();
        config.rulesets.push(casual);
        let db = Db::open(&config.database).expect("database");
        let (commands, receiver) = mpsc::channel(1024);
        Self {
            hub: Hub::new(Arc::new(config), db, commands),
            _commands: receiver,
            events: Vec::new(),
            _directory: directory,
        }
    }

    pub(super) async fn account(&mut self, name: &str) -> (AccountId, SessionId) {
        let id = self
            .hub
            .db
            .create_account(
                name.into(),
                "authenticated fixture".into(),
                AccountKind::Human,
                None,
            )
            .await
            .expect("account");
        let account = self
            .hub
            .db
            .account_by_id(id)
            .await
            .expect("read")
            .expect("exists");
        let (events, receiver) = event_channel();
        self.events.push(receiver);
        let (reply, answer) = oneshot::channel();
        self.hub.attach(Attach {
            account,
            token_hash: [0; 32],
            game_fingerprint: self.hub.config.rulesets[0].game_fingerprint,
            source: "127.0.0.1".parse().expect("source"),
            events,
            reply,
        });
        (id, answer.await.expect("reply").expect("session").0)
    }

    async fn join(&mut self, session: SessionId, ruleset: &str) -> QueueEntryId {
        let Reply::Queued { entry } = self
            .hub
            .request(
                session,
                ClientMsg::QueueJoin {
                    ruleset: RulesetId(ruleset.into()),
                },
            )
            .await
            .expect("queue")
        else {
            panic!("queued reply");
        };
        entry.entry
    }

    async fn join_audiences(
        &mut self,
        session: SessionId,
        ruleset: &str,
        audiences: &[Audience],
    ) -> Vec<QueueEntry> {
        let Reply::QueuesJoined { entries } = self
            .hub
            .request(
                session,
                ClientMsg::QueueJoinAudiences {
                    ruleset: RulesetId(ruleset.into()),
                    audiences: audiences.to_vec(),
                },
            )
            .await
            .expect("queue")
        else {
            panic!("linked queue reply");
        };
        entries
    }

    async fn game(&mut self, accounts: &[AccountId]) -> MatchId {
        let rules = self.hub.config.rulesets[0].clone();
        let source = "127.0.0.1".parse().expect("source");
        self.hub
            .form_match(
                &rules,
                matches::Origin::Queue,
                accounts.iter().map(|id| (*id, source)).collect(),
                None,
            )
            .await
            .expect("game")
    }

    async fn win(&mut self, game: MatchId) {
        self.hub
            .match_ended(
                game,
                End::Result(GameResult {
                    winner: Some(0),
                    left: None,
                    duration_ms: 1000,
                    report: Vec::new(),
                    artifact_truncated: false,
                }),
            )
            .await;
    }
}

#[tokio::test]
async fn combined_linked_unranked_entries_share_the_persisted_rating_and_start() {
    let mut fixture = Fixture::new();
    let (alpha, session) = fixture.account("alpha").await;
    let (beta, _) = fixture.account("beta").await;
    let game = fixture.game(&[alpha, beta]).await;
    fixture.win(game).await;
    let entries = fixture
        .join_audiences(
            session,
            "casual",
            &[Audience::HumansOnly, Audience::Everyone],
        )
        .await;
    assert_eq!(entries.len(), 2);
    let rating = fixture
        .hub
        .db
        .rating(alpha, RulesetId("ranked".into()))
        .await
        .expect("rating")
        .expect("stored")
        .glicko
        .rating;
    assert!(rating > 1500.0);
    for entry in &entries {
        let cached = &fixture.hub.entries[&entry.entry];
        assert_eq!(cached.rating, rating);
        assert_eq!(cached.rating_ruleset, RulesetId("ranked".into()));
    }
    assert_eq!(
        fixture.hub.entries[&entries[0].entry].since,
        fixture.hub.entries[&entries[1].entry].since
    );
}

#[tokio::test]
async fn combined_rating_lookup_failure_inserts_neither_linked_place() {
    let mut fixture = Fixture::new();
    let (alpha, session) = fixture.account("alpha").await;
    rusqlite::Connection::open(&fixture.hub.config.database)
        .expect("open")
        .execute("DROP TABLE ratings", [])
        .expect("simulate database failure");
    let next = fixture.hub.next_entry;
    assert_eq!(
        fixture
            .hub
            .request(
                session,
                ClientMsg::QueueJoinAudiences {
                    ruleset: RulesetId("ranked".into()),
                    audiences: vec![Audience::HumansOnly, Audience::Everyone],
                }
            )
            .await,
        Err(Error::Internal)
    );
    assert!(fixture.hub.entries.is_empty());
    assert!(fixture.hub.accounts[&alpha].entries.is_empty());
    assert_eq!(fixture.hub.next_entry, next);
}

#[tokio::test]
async fn combined_a_large_humans_queue_yields_to_everyone_on_budget_refill() {
    let mut fixture = Fixture::new();
    Arc::make_mut(&mut fixture.hub.config).limits.game_capacity = 1;
    Arc::make_mut(&mut fixture.hub.config)
        .limits
        .lobby_games_max = 1;
    let (_, first) = fixture.account("blocker_a").await;
    let (_, second) = fixture.account("blocker_b").await;
    fixture.join(first, "ranked").await;
    fixture.join(second, "ranked").await;
    assert_eq!(fixture.hub.proposals.len(), 1);
    for index in 0..8 {
        let (_, session) = fixture.account(&format!("player_{index}")).await;
        let audience = if index < 6 {
            Audience::HumansOnly
        } else {
            Audience::Everyone
        };
        fixture.join_audiences(session, "ranked", &[audience]).await;
    }
    let blocker = *fixture.hub.proposals.keys().next().expect("blocker");
    for entry in fixture
        .hub
        .proposals
        .remove(&blocker)
        .expect("proposal")
        .entries
    {
        fixture.hub.remove_entry(entry);
    }
    Arc::make_mut(&mut fixture.hub.config).limits.game_capacity = 8;
    fixture.hub.pair_work = 6;
    fixture.hub.pair_all();
    assert_eq!(
        fixture.hub.proposals.len(),
        1,
        "one full humans scan spends the budget"
    );
    fixture.hub.pair_work = 6;
    fixture.hub.pair_all();
    assert!(
        fixture
            .hub
            .proposals
            .values()
            .any(|proposal| proposal
                .entries
                .iter()
                .any(|id| fixture.hub.entries[id].audience == Audience::Everyone)),
        "everyone must get the next refill even for the same ruleset"
    );
}

#[tokio::test]
async fn reaching_the_window_cap_notifies_both_linked_places_without_a_position_change() {
    let mut fixture = Fixture::new();
    let (_, session) = fixture.account("alpha").await;
    let entries = fixture
        .join_audiences(
            session,
            "ranked",
            &[Audience::HumansOnly, Audience::Everyone],
        )
        .await;
    assert!(entries.iter().all(|entry| !entry.window_capped));
    for status in &entries {
        let entry = fixture.hub.entries.get_mut(&status.entry).expect("entry");
        entry.since -= Duration::from_secs(240);
        entry.age_status(Duration::from_secs(10));
    }
    fixture.hub.send_queue_statuses();
    let mut capped = Vec::new();
    while let Ok(message) = fixture.events[0].try_recv() {
        if let ServerMsg::Event(Event::QueueStatus { entry }) = message
            && entry.window_capped
        {
            capped.push(entry.entry);
        }
    }
    capped.sort();
    assert_eq!(
        capped,
        entries.iter().map(|entry| entry.entry).collect::<Vec<_>>(),
        "the capped transition is news even when positions are unchanged"
    );
}

#[tokio::test]
async fn guard_only_nonfinite_rating_refresh_cancels_both_linked_places() {
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut fixture = Fixture::new();
        let (alpha, session) = fixture.account("alpha").await;
        fixture
            .join_audiences(
                session,
                "ranked",
                &[Audience::HumansOnly, Audience::Everyone],
            )
            .await;
        fixture
            .hub
            .refresh_queued_rating(alpha, &RulesetId("ranked".into()), invalid);
        assert!(
            fixture.hub.entries.is_empty(),
            "invalid committed rating {invalid:?} must not enter the selector"
        );
        assert!(fixture.hub.accounts[&alpha].entries.is_empty());
        assert_eq!(fixture.hub.cooldown_ms(alpha), 0);
    }
}

#[tokio::test]
async fn review_every_clock_tick_refills_the_pairing_budget() {
    let mut fixture = Fixture::new();
    fixture.hub.pair_work = 0;
    let (_, first) = fixture.account("alpha").await;
    let (_, second) = fixture.account("beta").await;
    fixture.join(first, "ranked").await;
    fixture.join(second, "ranked").await;
    assert!(fixture.hub.proposals.is_empty());
    fixture.hub.tick();
    assert_eq!(
        fixture.hub.proposals.len(),
        1,
        "a clock tick refills without a second elapsed-time gate"
    );
}

#[tokio::test]
async fn tick_widens_both_players_windows_using_persisted_mode_ratings() {
    let mut fixture = Fixture::new();
    let (alpha, first) = fixture.account("alpha").await;
    let (beta, second) = fixture.account("beta").await;
    let game = fixture.game(&[alpha, beta]).await;
    fixture.win(game).await;
    let a = fixture.join(first, "ranked").await;
    let casual = fixture.join(first, "casual").await;
    assert_eq!(
        fixture.hub.entries[&casual].rating, fixture.hub.entries[&a].rating,
        "unranked admission reads the existing rated sibling's ladder"
    );
    let b = fixture.join(second, "ranked").await;
    assert!(
        fixture.hub.proposals.is_empty(),
        "stored ratings differ by more than the initial window"
    );
    fixture.hub.entries.get_mut(&a).expect("entry").since -= Duration::from_secs(180);
    fixture.hub.tick();
    assert!(
        fixture.hub.proposals.is_empty(),
        "an old player cannot widen the fresh peer's window"
    );
    fixture.hub.entries.get_mut(&b).expect("entry").since -= Duration::from_secs(180);
    fixture.hub.tick();
    assert_eq!(
        fixture.hub.proposals.len(),
        1,
        "a tick makes progress without a new join"
    );
}

#[tokio::test]
async fn a_concurrent_rated_result_refreshes_ranked_and_unranked_searches() {
    let mut fixture = Fixture::new();
    let (alpha, first) = fixture.account("alpha").await;
    let (beta, _) = fixture.account("beta").await;
    let game = fixture.game(&[alpha, beta]).await;
    let a = fixture.join(first, "ranked").await;
    let casual = fixture.join(first, "casual").await;
    assert_eq!(fixture.hub.entries[&a].rating, 1500.0);
    fixture.win(game).await;
    let rating = fixture
        .hub
        .db
        .rating(alpha, RulesetId("ranked".into()))
        .await
        .expect("read")
        .expect("rated")
        .glicko
        .rating;
    assert!(rating > 1500.0);
    assert_eq!(fixture.hub.entries[&a].rating, rating);
    assert_eq!(fixture.hub.entries[&casual].rating, rating);
}

#[tokio::test]
async fn a_closed_event_channel_during_pairing_cannot_reserve_an_entry_twice() {
    let mut fixture = Fixture::new();
    fixture.hub.pair_work = 0;
    let mut entries = Vec::new();
    for name in ["alpha", "beta", "gamma", "delta"] {
        let (_, session) = fixture.account(name).await;
        entries.push(fixture.join(session, "ranked").await);
    }
    assert!(fixture.hub.proposals.is_empty());
    fixture.events[0].close();
    fixture.hub.pair_work = queue::QUEUE_SCAN_BUDGET;
    fixture.hub.pair_all();
    let reserved: Vec<_> = fixture
        .hub
        .proposals
        .values()
        .flat_map(|p| p.entries.iter().copied())
        .collect();
    let unique: std::collections::HashSet<_> = reserved.iter().copied().collect();
    assert_eq!(
        reserved.len(),
        unique.len(),
        "a notification can synchronously detach and fail a proposal"
    );
    assert!(!reserved.contains(&entries[0]));
    assert_eq!(fixture.hub.proposals.len(), 1);
    for (id, proposal) in &fixture.hub.proposals {
        for entry in &proposal.entries {
            assert_eq!(fixture.hub.entries[entry].proposal, Some(*id));
        }
    }
}

#[tokio::test]
async fn a_status_sweep_inspects_queue_positions_at_most_once_per_entry() {
    let mut fixture = Fixture::new();
    fixture.hub.pair_work = 0;
    for index in 0..64 {
        let (_, session) = fixture.account(&format!("player_{index}")).await;
        fixture.join(session, "ranked").await;
    }
    for entry in fixture.hub.entries.values_mut() {
        entry.age_status(Duration::from_secs(5));
    }
    fixture.hub.position_scans.store(0, Ordering::Relaxed);
    fixture.hub.send_queue_statuses();
    let scans = fixture.hub.position_scans.load(Ordering::Relaxed);
    assert!(scans <= 64, "position inspections: {scans}");
}

#[tokio::test]
async fn a_queue_that_spends_the_budget_yields_to_other_queues_on_refill() {
    let mut fixture = Fixture::new();
    fixture.hub.pair_work = 0;
    for index in 0..8 {
        let (_, session) = fixture.account(&format!("player_{index}")).await;
        fixture
            .join(session, if index < 6 { "ranked" } else { "casual" })
            .await;
    }
    fixture.hub.pair_work = 6;
    fixture.hub.pair_all();
    assert_eq!(fixture.hub.proposals.len(), 1);
    fixture.hub.pair_work = 6;
    fixture.hub.pair_all();
    assert!(
        fixture
            .hub
            .proposals
            .values()
            .any(|p| p.ruleset.0 == "casual"),
        "the small queue gets a turn on the next refill"
    );
}

#[tokio::test]
async fn a_queue_deferred_before_its_first_scan_goes_first_on_refill() {
    let mut fixture = Fixture::new();
    fixture.hub.pair_work = 0;
    for index in 0..8 {
        let (_, session) = fixture.account(&format!("player_{index}")).await;
        fixture
            .join(session, if index < 6 { "ranked" } else { "casual" })
            .await;
    }
    fixture.hub.pair_next = 3; // casual/everyone, after ranked's two audience queues
    fixture.hub.pair_work = 6;
    fixture.hub.pair_all();
    assert_eq!(fixture.hub.proposals.len(), 1);
    fixture.hub.pair_work = 6;
    fixture.hub.pair_all();
    assert!(
        fixture
            .hub
            .proposals
            .values()
            .any(|p| p.ruleset.0 == "ranked"),
        "a large queue needs the next full budget to make progress"
    );
}

#[tokio::test]
async fn pairing_calls_while_capacity_is_full_preserve_the_next_queues_turn() {
    let mut fixture = Fixture::new();
    Arc::make_mut(&mut fixture.hub.config).limits.game_capacity = 1;
    Arc::make_mut(&mut fixture.hub.config)
        .limits
        .lobby_games_max = 1;
    fixture.hub.pair_work = 0;
    for index in 0..8 {
        let (_, session) = fixture.account(&format!("player_{index}")).await;
        fixture
            .join(session, if index < 6 { "ranked" } else { "casual" })
            .await;
    }
    fixture.hub.pair_work = 100;
    fixture.hub.pair_all();
    assert_eq!(fixture.hub.proposals.len(), 1);
    fixture.hub.pair_all();
    let proposal = *fixture.hub.proposals.keys().next().expect("first proposal");
    let completed = fixture.hub.proposals.remove(&proposal).expect("proposal");
    for entry in completed.entries {
        fixture.hub.remove_entry(entry);
    }
    fixture.hub.pair_all();
    assert!(
        fixture
            .hub
            .proposals
            .values()
            .any(|p| p.ruleset.0 == "casual"),
        "a paused call must not take the small queue's next turn"
    );
}
