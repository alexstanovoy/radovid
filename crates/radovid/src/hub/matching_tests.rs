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
                    aborted: None,
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

#[tokio::test]
async fn a_failed_rating_commit_keeps_the_match_pending_and_sends_no_false_result() {
    let mut fixture = Fixture::new();
    let (alpha, _) = fixture.account("alpha").await;
    let (beta, _) = fixture.account("beta").await;
    let game = fixture.game(&[alpha, beta]).await;
    let external =
        rusqlite::Connection::open(&fixture.hub.config.database).expect("storage fixture");
    external.execute_batch("CREATE TRIGGER reject_finish BEFORE UPDATE OF finished ON matches WHEN NEW.finished=1 BEGIN SELECT RAISE(ABORT,'storage unavailable');END;").expect("transient storage failure");
    fixture.win(game).await;
    assert!(
        fixture.hub.matches.contains_key(&game),
        "a failed result commit must not discard its match state"
    );
    assert!(fixture.hub.activities(alpha).matches.contains(&game));
    for receiver in &mut fixture.events {
        while let Ok(message) = receiver.try_recv() {
            assert!(
                !matches!(message, ServerMsg::Event(Event::MatchResult { .. })),
                "must not publish an uncommitted result"
            );
        }
    }
    fixture.hub.retry_finish();
    let failed = tokio::time::timeout(Duration::from_secs(10), fixture._commands.recv())
        .await
        .expect("retry guard")
        .expect("retry reply");
    assert!(matches!(&failed, Cmd::FinishApplied { result: Err(_), .. }));
    fixture.hub.handle(failed).await;
    external
        .execute_batch("DROP TRIGGER reject_finish")
        .expect("storage restored");
    fixture.hub.retry_finish();
    let committed = tokio::time::timeout(Duration::from_secs(10), fixture._commands.recv())
        .await
        .expect("commit guard")
        .expect("commit reply");
    assert!(matches!(
        &committed,
        Cmd::FinishApplied { result: Ok(_), .. }
    ));
    fixture.hub.handle(committed).await;
    assert!(!fixture.hub.matches.contains_key(&game));
    assert!(!fixture.hub.activities(alpha).matches.contains(&game));
    assert_eq!(
        fixture
            .hub
            .db
            .rating(alpha, RulesetId("ranked".into()))
            .await
            .expect("rating")
            .expect("played")
            .games,
        1
    );
    for receiver in &mut fixture.events {
        let message = receiver.try_recv().expect("committed result event");
        let ServerMsg::Event(Event::MatchResult { result }) = message else {
            panic!("result event");
        };
        assert_eq!(result.outcome, Outcome::Winner { team: 0 });
        assert!(
            result
                .seats
                .iter()
                .all(|seat| seat.before.is_some() && seat.after.is_some())
        );
    }
}

#[tokio::test]
async fn an_unseated_rated_winner_is_a_null_crash_instead_of_an_unreplayable_result() {
    let mut fixture = Fixture::new();
    let (alpha, _) = fixture.account("alpha").await;
    let (beta, _) = fixture.account("beta").await;
    let game = fixture.game(&[alpha, beta]).await;
    fixture
        .hub
        .match_ended(
            game,
            End::Result(GameResult {
                winner: Some(7),
                left: None,
                duration_ms: 1000,
                report: Vec::new(),
                artifact_truncated: false,
                aborted: None,
            }),
        )
        .await;
    assert!(
        !fixture.hub.matches.contains_key(&game),
        "invalid game output must not block the durable result queue"
    );
    let result = fixture
        .hub
        .db
        .completed_result(game)
        .await
        .expect("null result");
    assert_eq!(
        (result.outcome, result.reason),
        (Outcome::Null, EndReason::Crashed)
    );
    assert!(
        fixture
            .hub
            .db
            .rating(alpha, RulesetId("ranked".into()))
            .await
            .expect("rating")
            .is_none()
    );
}

#[tokio::test]
async fn pairing_waits_for_pending_results_so_cached_queue_ratings_cannot_be_stale() {
    let mut fixture = Fixture::new();
    let (alpha, _) = fixture.account("alpha").await;
    let (beta, _) = fixture.account("beta").await;
    let game = fixture.game(&[alpha, beta]).await;
    let storage = rusqlite::Connection::open(&fixture.hub.config.database).expect("storage");
    storage.execute_batch("CREATE TRIGGER reject_finish BEFORE UPDATE OF finished ON matches WHEN NEW.finished=1 BEGIN SELECT RAISE(ABORT,'storage unavailable');END;").expect("failure");
    fixture.win(game).await;
    let (_, gamma) = fixture.account("gamma").await;
    let (_, delta) = fixture.account("delta").await;
    fixture.join(gamma, "ranked").await;
    fixture.join(delta, "ranked").await;
    assert!(
        fixture.hub.proposals.is_empty(),
        "do not pair from cached ratings while a completed result is uncommitted"
    );
}

async fn ready_custom_lobby() -> (Fixture, SessionId, SessionId, LobbyId) {
    let mut fixture = Fixture::new();
    Arc::make_mut(&mut fixture.hub.config).lobby_teams = vec![1, 1];
    let (_, owner) = fixture.account("host").await;
    let (_, guest) = fixture.account("guest").await;
    let Reply::Lobby { state } = fixture
        .hub
        .request(
            owner,
            ClientMsg::LobbyCreate {
                ruleset: RulesetId("casual".into()),
                name: "Weekend game".into(),
                audience: Audience::Everyone,
                password: None,
            },
        )
        .await
        .expect("create")
    else {
        panic!("lobby reply");
    };
    let lobby = state.id;
    fixture
        .hub
        .request(
            guest,
            ClientMsg::LobbyJoin {
                lobby,
                password: None,
            },
        )
        .await
        .expect("join");
    fixture
        .hub
        .request(guest, ClientMsg::LobbyReady { lobby, ready: true })
        .await
        .expect("guest ready");
    (fixture, owner, guest, lobby)
}

#[tokio::test]
async fn the_host_cancels_a_start_and_the_expired_callback_keeps_the_room_open() {
    let (mut fixture, host, guest, lobby) = ready_custom_lobby().await;
    tokio::time::pause();
    fixture
        .hub
        .request(host, ClientMsg::LobbyReady { lobby, ready: true })
        .await
        .expect("start");
    tokio::task::yield_now().await;
    assert_eq!(
        fixture
            .hub
            .request(
                guest,
                ClientMsg::LobbyReady {
                    lobby,
                    ready: false
                }
            )
            .await,
        Err(Error::Starting),
        "only the host can stop the start"
    );
    assert_eq!(
        fixture
            .hub
            .request(
                host,
                ClientMsg::LobbyReady {
                    lobby,
                    ready: false
                }
            )
            .await,
        Ok(Reply::Done),
        "the host can stop the start with the existing ready operation"
    );
    let state = fixture.hub.lobbies.get(&lobby).expect("room remains");
    assert!(state.starting.is_none());
    assert!(
        state
            .members
            .iter()
            .filter(|member| member.account != state.owner)
            .all(|member| member.ready)
    );
    assert!(
        state
            .members
            .iter()
            .filter(|member| member.account == state.owner)
            .all(|member| !member.ready)
    );
    let mut latest = None;
    for _ in 0..64 {
        let Ok(event) = fixture.events[1].try_recv() else {
            break;
        };
        if let ServerMsg::Event(Event::LobbyState { state }) = event {
            latest = Some(state);
        }
    }
    assert!(
        latest
            .expect("the guest receives the cancellation")
            .starting_in_ms
            .is_none()
    );
    tokio::time::advance(Duration::from_millis(u64::from(LOBBY_COUNTDOWN_MS))).await;
    fixture.hub.countdown_done(lobby).await;
    assert!(
        fixture.hub.lobbies.contains_key(&lobby),
        "an already queued cancelled callback cannot close the room"
    );
    assert!(
        fixture.hub.matches.is_empty(),
        "the cancelled timer cannot form a match"
    );
}

#[tokio::test]
async fn restarting_a_cancelled_lobby_ignores_the_old_timer_until_the_new_deadline() {
    let (mut fixture, host, _, lobby) = ready_custom_lobby().await;
    tokio::time::pause();
    fixture
        .hub
        .request(host, ClientMsg::LobbyReady { lobby, ready: true })
        .await
        .expect("first start");
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    fixture
        .hub
        .request(
            host,
            ClientMsg::LobbyReady {
                lobby,
                ready: false,
            },
        )
        .await
        .expect("cancel");
    fixture
        .hub
        .request(host, ClientMsg::LobbyReady { lobby, ready: true })
        .await
        .expect("restart");
    tokio::task::yield_now().await;
    let deadline = fixture
        .hub
        .lobbies
        .get(&lobby)
        .expect("restarted room")
        .starting
        .expect("deadline");
    tokio::time::advance(Duration::from_secs(3)).await;
    fixture.hub.countdown_done(lobby).await;
    assert!(
        fixture.hub.lobbies.contains_key(&lobby),
        "the old timer cannot launch the restarted match early"
    );
    assert!(fixture.hub.matches.is_empty());
    // Deliver queued callbacks at exact instants, independently of timer-wheel rounding.
    let before = deadline - Duration::from_millis(1);
    tokio::time::advance(before - tokio::time::Instant::now()).await;
    assert_eq!(tokio::time::Instant::now(), before);
    fixture.hub.countdown_done(lobby).await;
    assert!(
        fixture.hub.lobbies.contains_key(&lobby),
        "one tick before the deadline the room stays open"
    );
    assert!(fixture.hub.matches.is_empty());
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(tokio::time::Instant::now(), deadline);
    fixture.hub.countdown_done(lobby).await;
    assert!(!fixture.hub.lobbies.contains_key(&lobby));
    assert_eq!(
        fixture.hub.matches.len(),
        1,
        "the current deadline forms exactly one match"
    );
}

#[tokio::test]
async fn cancelling_and_restarting_a_lobby_keeps_exactly_one_pending_timer() {
    let (mut fixture, host, _, lobby) = ready_custom_lobby().await;
    tokio::time::pause();
    let runtime = tokio::runtime::Handle::current();
    let baseline = runtime.metrics().num_alive_tasks();
    for _ in 0..4 {
        fixture
            .hub
            .request(host, ClientMsg::LobbyReady { lobby, ready: true })
            .await
            .expect("start");
        tokio::task::yield_now().await;
        assert_eq!(
            runtime.metrics().num_alive_tasks(),
            baseline + 1,
            "one current countdown task"
        );
        fixture
            .hub
            .request(
                host,
                ClientMsg::LobbyReady {
                    lobby,
                    ready: false,
                },
            )
            .await
            .expect("cancel");
        tokio::task::yield_now().await;
        assert_eq!(
            runtime.metrics().num_alive_tasks(),
            baseline,
            "cancellation leaves no sleeping task"
        );
    }
    fixture
        .hub
        .request(host, ClientMsg::LobbyReady { lobby, ready: true })
        .await
        .expect("restart");
    tokio::task::yield_now().await;
    assert_eq!(
        runtime.metrics().num_alive_tasks(),
        baseline + 1,
        "restart leaves exactly one pending timer"
    );
    tokio::time::advance(Duration::from_millis(u64::from(LOBBY_COUNTDOWN_MS) + 1)).await;
    let command = tokio::time::timeout(Duration::from_secs(1), fixture._commands.recv())
        .await
        .expect("current countdown callback")
        .expect("hub command");
    let Cmd::CountdownDone { lobby: fired } = command else {
        panic!("countdown command");
    };
    assert_eq!(fired, lobby);
    assert!(
        fixture._commands.try_recv().is_err(),
        "cancelled sleeps produce no extra callback"
    );
    fixture.hub.countdown_done(fired).await;
    assert!(!fixture.hub.lobbies.contains_key(&lobby));
    assert_eq!(
        fixture.hub.matches.len(),
        1,
        "the current timer still launches the game"
    );
}

impl Fixture {
    async fn version(&mut self, owner: AccountId, name: &str) -> (AccountId, SessionId) {
        let account = self
            .hub
            .db
            .select_version(owner, name.into())
            .await
            .expect("database")
            .expect("version");
        let id = account.id;
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
        (id, answer.await.expect("reply").expect("attached").0)
    }
}

#[tokio::test]
async fn versions_remain_separate_after_widening_and_requeue_and_do_not_hide_eligible_peers() {
    for mode in ["ranked", "casual"] {
        let mut fixture = Fixture::new();
        let (owner, _) = fixture.account("engine").await;
        fixture
            .hub
            .db
            .set_kind(owner, AccountKind::Bot)
            .await
            .expect("kind");
        let (_, first) = fixture.version(owner, "v1").await;
        let (_, second) = fixture.version(owner, "v2").await;
        let a = fixture.join(first, mode).await;
        let b = fixture.join(second, mode).await;
        for entry in [a, b] {
            fixture.hub.entries.get_mut(&entry).expect("entry").since -= Duration::from_secs(600);
        }
        fixture.hub.tick();
        assert!(
            fixture.hub.proposals.is_empty(),
            "widening cannot pair an owner's versions"
        );
        let (peer, peer_session) = fixture.account("peer").await;
        fixture.join(peer_session, mode).await;
        let proposal = *fixture
            .hub
            .proposals
            .keys()
            .next()
            .expect("third party is matched");
        let first_id = fixture.hub.sessions[&first].account;
        fixture
            .hub
            .match_accept(first_id, proposal)
            .await
            .expect("accept");
        fixture.hub.match_decline(peer, proposal).expect("decline");
        fixture.hub.tick();
        assert!(
            fixture.hub.proposals.is_empty(),
            "requeue cannot pair an owner's versions"
        );
        // An incompatible older version cannot hide a compatible newer one.
        fixture.hub.entries.get_mut(&a).expect("entry").rating = 10_000.0;
        let (_, fresh_session) = fixture.account("fresh").await;
        let c = fixture.join(fresh_session, mode).await;
        let selected = fixture
            .hub
            .proposals
            .values()
            .next()
            .expect("eligible version is not starved");
        assert!(selected.entries.contains(&b));
        assert!(selected.entries.contains(&c));
        assert!(!selected.entries.contains(&a));
    }
}

#[tokio::test]
async fn versions_cannot_attach_if_owner_was_banned_or_deleted_after_authentication() {
    for deleted in [false, true] {
        let mut fixture = Fixture::new();
        let (owner, _) = fixture.account("engine").await;
        fixture
            .hub
            .db
            .set_kind(owner, AccountKind::Bot)
            .await
            .expect("kind");
        let stale = fixture
            .hub
            .db
            .select_version(owner, "v1".into())
            .await
            .expect("database")
            .expect("version");
        let error = if deleted {
            fixture.hub.db.delete_account(owner).await.expect("delete");
            Error::BadSession
        } else {
            fixture
                .hub
                .db
                .set_ban(
                    owner,
                    Some(crate::db::Ban {
                        reason: "blocked".into(),
                        until: None,
                    }),
                )
                .await
                .expect("ban");
            Error::Banned {
                reason: "blocked".into(),
                until: None,
            }
        };
        let (events, _receiver) = event_channel();
        let (reply, answer) = oneshot::channel();
        fixture
            .hub
            .handle(Cmd::Attach(Box::new(Attach {
                account: stale,
                token_hash: [0; 32],
                game_fingerprint: fixture.hub.config.rulesets[0].game_fingerprint,
                source: "127.0.0.1".parse().expect("source"),
                events,
                reply,
            })))
            .await;
        assert_eq!(answer.await.expect("reply"), Err(error));
    }
}

#[tokio::test]
async fn a_kind_change_waits_for_the_match_to_end_and_a_bot_main_stays_unrated() {
    for phase in [
        matches::Phase::Waiting,
        matches::Phase::Starting,
        matches::Phase::Running,
    ] {
        let mut fixture = Fixture::new();
        let (admin, admin_session) = fixture.account("admin").await;
        fixture
            .hub
            .db
            .grant_admin("admin".into())
            .await
            .expect("database query");
        fixture
            .hub
            .accounts
            .get_mut(&admin)
            .expect("admin fixture")
            .info
            .admin = true;
        let (human, _) = fixture.account("human").await;
        let (peer, _) = fixture.account("peer").await;
        let game = fixture.game(&[human, peer]).await;
        fixture
            .hub
            .matches
            .get_mut(&game)
            .expect("pending match")
            .phase = phase;
        let change = |kind| ClientMsg::AdminSetKind {
            account: human,
            kind,
        };
        assert_eq!(
            fixture
                .hub
                .request(admin_session, change(AccountKind::Human))
                .await,
            Ok(Reply::Done),
            "same-kind requests remain no-ops"
        );
        let error = fixture
            .hub
            .request(admin_session, change(AccountKind::Bot))
            .await
            .expect_err("a kind change during a match is forbidden");
        assert_eq!(error, Error::Forbidden);
        assert_eq!(error.to_string(), "Forbidden");
        assert_eq!(
            fixture
                .hub
                .db
                .account_by_id(human)
                .await
                .expect("database query")
                .expect("account exists")
                .kind,
            AccountKind::Human
        );
        fixture.win(game).await;
        assert!(
            fixture
                .hub
                .db
                .rating(human, RulesetId("ranked".into()))
                .await
                .expect("database query")
                .is_some()
        );
        assert_eq!(
            fixture
                .hub
                .request(admin_session, change(AccountKind::Bot))
                .await,
            Ok(Reply::Done)
        );
        assert!(
            fixture
                .hub
                .db
                .rating(human, RulesetId("ranked".into()))
                .await
                .expect("database query")
                .is_none(),
            "conversion removes the main's former human rating"
        );
        let (version, _) = fixture.version(human, "v1").await;
        assert!(
            fixture
                .hub
                .db
                .rating(version, RulesetId("ranked".into()))
                .await
                .expect("database query")
                .is_none(),
            "a selected version starts unrated"
        );
        let game = fixture.game(&[version, peer]).await;
        fixture.win(game).await;
        assert!(
            fixture
                .hub
                .db
                .rating(version, RulesetId("ranked".into()))
                .await
                .expect("database query")
                .is_some()
        );
        assert!(
            fixture
                .hub
                .db
                .rating(human, RulesetId("ranked".into()))
                .await
                .expect("database query")
                .is_none(),
            "only the played version receives a rating"
        );
    }
}

#[tokio::test]
async fn lookup_quota_belongs_to_the_main_account_not_to_each_version() {
    let mut fixture = Fixture::new();
    let (owner, _) = fixture.account("engine").await;
    fixture
        .hub
        .db
        .set_kind(owner, AccountKind::Bot)
        .await
        .expect("kind");
    let (first, _) = fixture.version(owner, "v1").await;
    let (second, _) = fixture.version(owner, "v2").await;
    let now = std::time::Instant::now();
    for _ in 0..4 {
        fixture
            .hub
            .charge_lookup_at(first, now)
            .expect("the burst of the main account");
    }
    assert_eq!(
        fixture.hub.charge_lookup_at(second, now),
        Err(Error::Throttled {
            retry_after_ms: 250
        }),
        "another version cannot multiply the quota"
    );
}

#[tokio::test]
async fn a_second_version_cannot_exceed_the_session_cap_of_the_main_account() {
    let mut fixture = Fixture::new();
    let (owner, _) = fixture.account("engine").await;
    fixture
        .hub
        .db
        .set_kind(owner, AccountKind::Bot)
        .await
        .expect("kind");
    // The main account's own session counts too: fill what is left through the first version.
    for _ in 1..MAX_SESSIONS_PER_ACCOUNT {
        fixture.version(owner, "v1").await;
    }
    let second = fixture
        .hub
        .db
        .select_version(owner, "v2".into())
        .await
        .expect("database")
        .expect("version");
    let (events, _receiver) = event_channel();
    let (reply, answer) = oneshot::channel();
    fixture.hub.attach(Attach {
        account: second,
        token_hash: [0; 32],
        game_fingerprint: fixture.hub.config.rulesets[0].game_fingerprint,
        source: "127.0.0.1".parse().expect("source"),
        events,
        reply,
    });
    assert_eq!(
        answer.await.expect("reply").err(),
        Some(Error::TooManySessions),
        "the cap counts the sessions of every version together"
    );
}

#[tokio::test]
async fn a_graceful_shutdown_tells_every_online_bot_version_the_server_is_stopping() {
    let mut fixture = Fixture::new();
    // Only the version is online, as for a real bot: its main account holds no session.
    let owner = fixture
        .hub
        .db
        .create_account(
            "engine".into(),
            "authenticated fixture".into(),
            AccountKind::Bot,
        )
        .await
        .expect("account");
    let (_, session) = fixture.version(owner, "v1").await;
    let events = fixture.events.last_mut().expect("the version's events");
    fixture.hub.shutdown().await;
    let mut told = Vec::new();
    while let Ok(message) = events.try_recv() {
        told.push(message);
    }
    assert!(
        told.iter().any(|message| matches!(
            message,
            ServerMsg::Event(Event::Bye {
                error: Error::ServerStopping
            })
        )),
        "the bot version's session got no ServerStopping: {told:?}"
    );
    assert!(
        !fixture.hub.sessions.contains_key(&session),
        "the bot version's session is detached"
    );
}
