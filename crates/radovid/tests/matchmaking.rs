//! The queue, proposals, the launcher contract, results and ratings, several
//! games per account, the game slots and the admin tools.

mod common;

use common::*;
use radovid_client::proto::*;

fn rules(behaviour: &str) -> Vec<radovid::config::RulesetConfig> {
    vec![ruleset("solo", true, behaviour, "0.3")]
}

#[test]
fn the_game_is_told_each_seats_account_kind() {
    let env = start(rules("team0"));
    let mut hana = env.human("hana");
    let mut beta = env.bot("beta");
    hana.queue(&solo());
    beta.queue(&solo());
    let offered = hana.wait_proposal();
    hana.expect_done(ClientMsg::MatchAccept {
        proposal: offered.proposal,
    });
    let theirs = beta.wait_proposal();
    beta.expect_done(ClientMsg::MatchAccept {
        proposal: theirs.proposal,
    });
    let result = hana.wait_result();
    let (_, _, seats) = started_with(&result.game_report);
    let mut kinds: Vec<(String, String)> = seats
        .into_iter()
        .map(|(_, _, name, kind)| (name, kind))
        .collect();
    kinds.sort();
    assert_eq!(
        kinds,
        vec![
            ("beta".to_owned(), "bot".to_owned()),
            ("hana".to_owned(), "human".to_owned())
        ]
    );
}

#[test]
fn two_bots_accept_are_launched_play_and_are_rated() {
    let env = start(rules("team0"));
    let mut alpha = env.bot("alpha");
    let mut beta = env.bot("beta");

    let first = alpha.queue(&solo());
    assert_eq!(
        (first.position, first.waiting, first.overloaded),
        (1, 1, false)
    );
    beta.queue(&solo());
    let offered = alpha.wait_proposal();
    assert_eq!(
        (offered.accepted, offered.of, offered.entry),
        (0, 2, first.entry)
    );
    assert!(offered.accept_by_ms > 25_000, "{offered:?}");
    alpha.expect_done(ClientMsg::MatchAccept {
        proposal: offered.proposal,
    });
    let theirs = beta.wait_proposal();
    let update = beta.wait_for(10, |event| match event {
        Event::ProposalUpdate { accepted, of, .. } => Some((accepted, of)),
        _ => None,
    });
    assert_eq!(update, (1, 2));
    beta.expect_done(ClientMsg::MatchAccept {
        proposal: theirs.proposal,
    });
    let queued = alpha.wait_for(10, |event| match event {
        Event::MatchQueued {
            match_id,
            ahead,
            overloaded,
        } => Some((match_id, ahead, overloaded)),
        _ => None,
    });
    assert_eq!((queued.1, queued.2), (0, false));

    // The handoff: one match, two seats, the longer waiting account first, the
    // game server's own URL.
    let (for_alpha, for_beta) = (alpha.wait_match_found(), beta.wait_match_found());
    assert_eq!(for_alpha.match_id, queued.0);
    assert_eq!(for_alpha.match_id, for_beta.match_id);
    assert!(for_alpha.rated);
    assert_eq!((for_alpha.slot, for_beta.slot), (0, 1));
    assert_eq!((for_alpha.team, for_beta.team), (0, 1));
    assert_ne!(for_alpha.ticket, for_beta.ticket);
    assert_eq!(
        (for_alpha.game.as_str(), for_alpha.game_fingerprint),
        ("testgame", FINGERPRINT)
    );
    let seat = &for_alpha.seats[1];
    assert_eq!(
        (
            seat.nickname.as_str(),
            seat.kind,
            seat.bot_version.as_deref()
        ),
        ("beta", AccountKind::Bot, Some("test-1"))
    );
    let Ok(Reply::Handoffs { list }) = alpha.ask(ClientMsg::MyMatches) else {
        panic!("my matches");
    };
    assert_eq!(list, vec![for_alpha.clone()]);

    let result = alpha.wait_result();
    assert_eq!(
        (
            result.outcome,
            result.reason,
            result.rated,
            result.duration_ms
        ),
        (Outcome::Winner { team: 0 }, EndReason::Decided, true, 1234)
    );
    // The game was started with the tickets of the handoffs, in its
    // environment, on the port of the URL.
    let (match_id, port, seats) = started_with(&result.game_report);
    assert_eq!(match_id, for_alpha.match_id.0.to_string());
    assert_eq!(
        for_alpha.game_url,
        format!("wss://game.example.org:{port}/m{match_id}")
    );
    assert_eq!(
        seats,
        vec![
            (
                for_alpha.ticket.to_hex(),
                "0".to_owned(),
                "alpha".to_owned(),
                "bot".to_owned()
            ),
            (
                for_beta.ticket.to_hex(),
                "1".to_owned(),
                "beta".to_owned(),
                "bot".to_owned()
            ),
        ]
    );
    let (winner, loser) = (&result.seats[0], &result.seats[1]);
    assert_eq!(winner.nickname, "alpha");
    assert_eq!(
        winner.before,
        Some(Rating {
            rating: 1500.0,
            deviation: 350.0
        })
    );
    let after = winner.after.expect("rated");
    assert!(
        after.rating > 1500.0 && after.deviation < 350.0,
        "{after:?}"
    );
    assert!(loser.after.expect("rated").rating < 1500.0);
    assert_eq!(beta.wait_result().match_id, result.match_id);

    // The ladder and the history show it; both are free again.
    let Ok(Reply::LadderPage { total, rows }) = alpha.ask(ClientMsg::Ladder {
        ruleset: solo(),
        offset: 0,
        limit: 10,
    }) else {
        panic!("ladder");
    };
    assert_eq!(total, 2);
    assert_eq!(
        (rows[0].rank, rows[0].account, rows[0].kind, rows[0].wins),
        (1, alpha.me.id, AccountKind::Bot, 1)
    );
    assert_eq!(
        (rows[1].rank, rows[1].account, rows[1].losses),
        (2, beta.me.id, 1)
    );
    let Ok(Reply::Matches { list }) = alpha.ask(ClientMsg::RecentMatches {
        account: alpha.me.id,
        limit: 5,
    }) else {
        panic!("matches");
    };
    assert_eq!(list, vec![result.clone()]);
    assert_eq!(
        alpha.ask(ClientMsg::MatchHandoff {
            match_id: result.match_id
        }),
        Err(Error::UnknownMatch)
    );
}

#[test]
fn a_decline_returns_the_accepter_to_its_place_and_cools_the_decliner_down() {
    let env = start_configured(rules("team0"), |c| c.limits.accept_timeout_secs = 2);
    let mut early = env.bot("early");
    let mut decliner = env.bot("decliner");
    let mut later = env.bot("later");
    let entry = early.queue(&solo()).entry;
    decliner.queue(&solo());
    let offer = early.accept();
    let theirs = decliner.wait_proposal();
    assert_eq!(theirs.proposal, offer.proposal);
    // A third account queues meanwhile: behind the two in the proposal.
    assert_eq!(later.queue(&solo()).position, 3);
    decliner.expect_done(ClientMsg::MatchDecline {
        proposal: theirs.proposal,
    });
    let left = decliner.wait_for(10, |event| match event {
        Event::QueueLeft {
            reason,
            cooldown_ms,
            ..
        } => Some((reason, cooldown_ms)),
        _ => None,
    });
    assert_eq!(left, (QueueLeftReason::Declined, 120_000));
    match decliner.ask(ClientMsg::QueueJoin { ruleset: solo() }) {
        Err(Error::QueueCooldown { retry_after_ms }) => {
            assert!(
                (110_000..=120_000).contains(&retry_after_ms),
                "{retry_after_ms}"
            )
        }
        other => panic!("{other:?}"),
    }
    let failed = early.wait_for(10, |event| match event {
        Event::ProposalFailed {
            proposal,
            back_in_queue,
        } => Some((proposal, back_in_queue)),
        _ => None,
    });
    assert_eq!(failed, (offer.proposal, true));

    // The accepter keeps its entry and place; the next pairing takes it with
    // the third account. Silence is a decline too.
    let again = early.wait_proposal();
    assert_eq!(again.entry, entry);
    early.expect_done(ClientMsg::MatchAccept {
        proposal: again.proposal,
    });
    later.wait_proposal();
    let left = later.wait_for(10, |event| match event {
        Event::QueueLeft { reason, .. } => Some(reason),
        _ => None,
    });
    assert_eq!(left, QueueLeftReason::TimedOut);
    assert_eq!(
        later.ask(ClientMsg::MatchAccept {
            proposal: again.proposal
        }),
        Err(Error::UnknownProposal)
    );
    early.wait_for(10, |event| {
        matches!(event, Event::ProposalFailed { .. }).then_some(())
    });
    let mut fresh = env.connect();
    let back = fresh
        .login("early", PASSWORD, ClientInfo::default())
        .expect("login");
    assert_eq!(back.activities.queued.len(), 1);
    assert_eq!(
        (
            back.activities.queued[0].entry,
            back.activities.queued[0].position
        ),
        (entry, 1)
    );
    let cooled = env
        .connect()
        .login("later", PASSWORD, ClientInfo::default());
    assert!(cooled.expect("login").activities.cooldown_ms > 100_000);
}

#[test]
fn an_account_plays_several_games_at_once_up_to_its_limit() {
    let env = start_configured(rules("team1"), |c| {
        c.limits.games_per_account = radovid::config::PerKind { human: 2, bot: 3 }
    });
    let mut many = env.bot("many");
    let mut other = env.bot("other");
    let mut third = env.bot("third");
    let entries: Vec<QueueEntry> = (0..3).map(|_| many.queue(&solo())).collect();
    assert_eq!(
        entries.iter().map(|e| e.position).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        many.ask(ClientMsg::QueueJoin { ruleset: solo() }),
        Err(Error::TooManyGames { limit: 3 })
    );
    let Ok(Reply::Limits { limits }) = many.ask(ClientMsg::Limits {
        ruleset: Some(solo()),
    }) else {
        panic!("limits");
    };
    assert_eq!(limits.games_per_account, 3);

    // Nobody plays himself: two opponents take two of the entries.
    other.queue(&solo());
    third.queue(&solo());
    let offers = [many.accept(), many.accept()];
    assert_ne!(offers[0].entry, offers[1].entry);
    other.accept();
    third.accept();
    let handoffs = [many.wait_match_found(), many.wait_match_found()];
    assert_ne!(handoffs[0].match_id, handoffs[1].match_id);
    assert!(handoffs.iter().all(|h| h.team == 0));

    // Both results are rated, one after the other.
    let results = [many.wait_result(), many.wait_result()];
    assert!(
        results
            .iter()
            .all(|r| r.outcome == Outcome::Winner { team: 1 })
    );
    // The second starts from the first's result as stored, its deviation
    // grown only for the moments between them.
    let first_after = results[0].seats[0].after.expect("rated");
    let second_before = results[1].seats[0].before.expect("rated");
    assert_eq!(second_before.rating, first_after.rating);
    assert!((second_before.deviation - first_after.deviation).abs() < 0.01);
    let Ok(Reply::Account { account }) = many.ask(ClientMsg::Account {
        account: many.me.id,
    }) else {
        panic!("account");
    };
    assert_eq!(
        (account.ratings[0].1.games, account.ratings[0].1.losses),
        (2, 2)
    );
    // The third entry still waits; the two played ones are gone.
    let Ok(Reply::Queued { entry }) = many.ask(ClientMsg::QueueJoin { ruleset: solo() }) else {
        panic!("queue");
    };
    assert_eq!(entry.position, 2);
    many.expect_done(ClientMsg::QueueLeave { entry: entry.entry });
    assert_eq!(
        many.ask(ClientMsg::QueueLeave { entry: entry.entry }),
        Err(Error::NotInQueue)
    );
}

#[test]
fn matches_wait_for_a_game_slot_and_the_queues_are_bounded() {
    // Each game lasts two seconds: long enough to fill the one slot.
    let env = start_configured(vec![ruleset("solo", true, "team0", "2")], |c| {
        c.limits.game_capacity = 1;
        c.limits.lobby_games_max = 1;
        c.limits.queue_capacity = 3;
        c.limits.games_per_source = 5;
    });
    let mut a = env.bot("aaa");
    let mut b = env.bot("bbb");
    let mut c = env.bot("ccc");
    let mut d = env.bot("ddd");
    for account in [&mut a, &mut b] {
        account.queue(&solo());
    }
    // One proposal fills the one slot's worth of pairing: the next entries wait.
    let waiting = c.queue(&solo());
    assert!(waiting.overloaded, "{waiting:?}");
    assert_eq!(
        d.ask(ClientMsg::QueueJoin { ruleset: solo() }),
        Err(Error::QueueFull {
            retry_after_ms: 10_000
        })
    );
    a.accept();
    b.accept();
    a.wait_match_found();
    // The slot is taken, the pairing goes on: the next match waits for it.
    d.queue(&solo());
    c.accept();
    d.accept();
    let told = c.wait_for(10, |event| match event {
        Event::MatchQueued {
            ahead, overloaded, ..
        } => Some((ahead, overloaded)),
        _ => None,
    });
    assert_eq!(told, (0, true));
    c.quiet(150, |event| matches!(event, Event::MatchFound { .. }));
    a.wait_result();
    let handoff = c.wait_match_found();
    assert_eq!(handoff.seats[1].nickname, "ddd");

    // The running match holds two of the source's five games in flight.
    let mut e = env.bot("eee");
    let mut f = env.bot("fff");
    let mut g = env.bot("ggg");
    e.queue(&solo());
    f.queue(&solo());
    g.queue(&solo());
    assert_eq!(
        env.bot("hhh").ask(ClientMsg::QueueJoin { ruleset: solo() }),
        Err(Error::TooManyGamesFromSource { limit: 5 })
    );
}

#[test]
fn draws_count_half_and_a_crash_or_a_match_nobody_came_to_counts_nothing() {
    let env = start(rules("draw"));
    let (mut a, mut b) = (env.bot("alpha"), env.bot("beta"));
    pair_up(&mut a, &mut b, &solo());
    let result = a.wait_result();
    assert_eq!(
        (result.outcome, result.reason),
        (Outcome::Draw, EndReason::Draw)
    );
    for seat in &result.seats {
        let after = seat.after.expect("rated");
        assert!(
            (after.rating - 1500.0).abs() < 1e-6 && after.deviation < 350.0,
            "{after:?}"
        );
    }
    env.stop();

    for (behaviour, reason) in [
        ("crash", EndReason::Crashed),
        ("badteam", EndReason::Crashed),
        ("abandon", EndReason::Abandoned),
    ] {
        let env = start(rules(behaviour));
        let (mut a, mut b) = (env.bot("alpha"), env.bot("beta"));
        pair_up(&mut a, &mut b, &solo());
        let result = b.wait_result();
        assert_eq!(
            (result.outcome, result.reason),
            (Outcome::Null, reason),
            "{behaviour}"
        );
        assert!(result.game_report.is_empty() && result.seats.iter().all(|s| s.after.is_none()));
        let Ok(Reply::LadderPage { total, .. }) = b.ask(ClientMsg::Ladder {
            ruleset: solo(),
            offset: 0,
            limit: 10,
        }) else {
            panic!("ladder");
        };
        assert_eq!(total, 0);
    }
}

/// A game's stderr is drained whatever it holds: a line that is no UTF-8
/// stopped the reading, the next write killed the game, and its result was
/// lost.
#[test]
fn a_game_whose_stderr_is_no_text_still_reports_its_result() {
    let env = start(rules("noisy"));
    let (mut a, mut b) = (env.bot("alpha"), env.bot("beta"));
    pair_up(&mut a, &mut b, &solo());
    let result = a.wait_result();
    assert_eq!(
        (result.outcome, result.reason),
        (Outcome::Winner { team: 0 }, EndReason::Decided)
    );
}

#[test]
fn a_game_that_never_gets_ready_or_runs_too_long_is_stopped() {
    let mut never = ruleset("solo", true, "noready", "0");
    never.launcher.start_timeout_secs = 1;
    let env = start(vec![never]);
    let (mut a, mut b) = (env.bot("alpha"), env.bot("beta"));
    a.queue(&solo());
    b.queue(&solo());
    a.accept();
    b.accept();
    let result = a.wait_result();
    assert_eq!(result.reason, EndReason::Crashed);
    env.stop();

    let mut long = ruleset("solo", true, "hang", "0");
    long.max_duration_secs = 1;
    let env = start(vec![long]);
    let (mut a, mut b) = (env.bot("alpha"), env.bot("beta"));
    pair_up(&mut a, &mut b, &solo());
    assert_eq!(a.wait_result().reason, EndReason::Stopped);
}

#[test]
fn a_second_session_finds_the_running_match_and_an_admin_stops_it() {
    let env = start(rules("hang"));
    let (mut a, mut b) = (env.bot("alpha"), env.bot("beta"));
    let (handoff, _) = pair_up(&mut a, &mut b, &solo());

    let mut second = env.connect();
    let logged = second
        .login("alpha", PASSWORD, ClientInfo::default())
        .expect("login");
    assert_eq!(logged.activities.matches, vec![handoff.match_id]);
    let reply = second
        .request(ClientMsg::MatchHandoff {
            match_id: handoff.match_id,
        })
        .expect("handoff");
    assert_eq!(
        reply,
        Reply::Handoff {
            handoff: handoff.clone()
        }
    );

    env.account("root_user", AccountKind::Human);
    env.grant_admin("root_user");
    let mut root = env.login("root_user");
    assert_eq!(
        a.ask(ClientMsg::AdminStopMatch {
            match_id: handoff.match_id
        }),
        Err(Error::Forbidden)
    );
    root.expect_done(ClientMsg::AdminStopMatch {
        match_id: handoff.match_id,
    });
    let result = a.wait_result();
    assert_eq!(
        (result.outcome, result.reason),
        (Outcome::Null, EndReason::Stopped)
    );
    assert_eq!(b.wait_result().outcome, Outcome::Null);
    assert_eq!(
        root.ask(ClientMsg::AdminStopMatch {
            match_id: handoff.match_id
        }),
        Err(Error::UnknownMatch)
    );
}

#[test]
fn bans_remove_the_banned_from_the_queue_and_keep_them_out_until_lifted() {
    let env = start(rules("team0"));
    env.account("root_user", AccountKind::Human);
    env.grant_admin("root_user");
    let mut root = env.login("root_user");

    let mut troll = env.bot("troll");
    troll.queue(&solo());
    assert_eq!(
        troll.ask(ClientMsg::AdminBan {
            account: troll.me.id,
            reason: "x".to_owned(),
            until: None
        }),
        Err(Error::Forbidden)
    );
    root.expect_done(ClientMsg::AdminBan {
        account: troll.me.id,
        reason: "spam".to_owned(),
        until: None,
    });
    let told = troll.wait_for(10, |event| match event {
        Event::Bye { error } => Some(error),
        _ => None,
    });
    assert_eq!(
        told,
        Error::Banned {
            reason: "spam".to_owned(),
            until: None
        }
    );
    let mut again = env.connect();
    match again.login("troll", PASSWORD, ClientInfo::default()) {
        Err(radovid_client::ClientError::Refused(Error::Banned {
            reason,
            until: None,
        })) => {
            assert_eq!(reason, "spam")
        }
        other => panic!("{other:?}"),
    }

    // The banned left the queue: the next account waits alone.
    let mut honest = env.bot("honest");
    assert_eq!(honest.queue(&solo()).position, 1);
    honest.quiet(300, |event| matches!(event, Event::MatchProposed { .. }));

    root.expect_done(ClientMsg::AdminUnban {
        account: troll.me.id,
    });
    again
        .login("troll", PASSWORD, ClientInfo::default())
        .expect("welcome back");
    root.expect_done(ClientMsg::AdminSetKind {
        account: troll.me.id,
        kind: AccountKind::Human,
    });
    let Ok(Reply::Account { account }) = honest.ask(ClientMsg::Account {
        account: troll.me.id,
    }) else {
        panic!("account");
    };
    assert_eq!(account.kind, AccountKind::Human);
}

#[test]
fn the_service_refuses_game_servers_of_another_wire_and_tickets_on_command_lines() {
    let mut other = ruleset("solo", true, "team0", "0.1");
    other.launcher.version_probe = vec!["--probe".to_owned(), "0123456789abcdef".to_owned()];
    let error = try_start(vec![other], |_| {}).err().expect("refused");
    assert!(
        error.contains("0123456789abcdef") && error.contains(&format!("{FINGERPRINT:016x}")),
        "{error}"
    );

    let mut leaky = ruleset("solo", true, "team0", "0.1");
    leaky.launcher.args.push("{ticket_0}".to_owned());
    let error = try_start(vec![leaky], |_| {}).err().expect("refused");
    assert!(error.contains("env"), "{error}");
}

/// A player who leaves for good loses the rated match, and the
/// match no longer holds one of his games.
#[test]
fn a_leaver_loses_the_rated_match_and_is_freed() {
    let env = start(rules("leave0"));
    let (mut a, mut b) = (env.bot("alpha"), env.bot("beta"));
    pair_up(&mut a, &mut b, &solo());
    let result = a.wait_result();
    assert_eq!(
        (result.outcome, result.reason),
        (Outcome::Winner { team: 1 }, EndReason::Left)
    );
    let rating = |slot: u8| {
        let seat = result.seats.iter().find(|s| s.slot == slot).expect("seat");
        (
            seat.before.expect("rated").rating,
            seat.after.expect("rated").rating,
        )
    };
    assert!(rating(0).1 < rating(0).0, "the leaver lost rating");
    assert!(rating(1).1 > rating(1).0, "the stayer won rating");
    let Ok(Reply::Handoffs { list }) = a.ask(ClientMsg::MyMatches) else {
        panic!("my matches");
    };
    assert!(list.is_empty(), "a decided match holds no game");
    env.stop();

    let env = start(rules("badleave"));
    let (mut a, mut b) = (env.bot("alpha"), env.bot("beta"));
    pair_up(&mut a, &mut b, &solo());
    let result = b.wait_result();
    assert_eq!(
        (result.outcome, result.reason),
        (Outcome::Null, EndReason::Crashed),
        "a leaver cannot be the winner"
    );
}

/// The replay a ruleset names goes, in chunks, to the seats of the finished match only.
#[test]
fn a_finished_matchs_artifact_goes_to_its_seats_in_chunks() {
    let dir = std::env::temp_dir().join(format!("radovid-artifact-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let mut rules = rules("team0");
    rules[0].launcher.artifact = Some(format!("{}/replay-{{match_id}}.brp", dir.display()));
    let env = start(rules);
    let (mut a, mut b, mut c) = (env.bot("alpha"), env.bot("beta"), env.bot("gamma"));
    pair_up(&mut a, &mut b, &solo());
    let result = a.wait_result();
    let size = MAX_ARTIFACT_CHUNK + 1000;
    let bytes: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    let file = dir.join(format!("replay-{}.brp", result.match_id.0));
    std::fs::write(&file, &bytes).expect("replay");
    let fetch = |who: &mut Account, offset: u64| {
        who.ask(ClientMsg::MatchArtifact {
            match_id: result.match_id,
            offset,
        })
    };
    let Ok(Reply::Artifact { total, data, .. }) = fetch(&mut b, 0) else {
        panic!("first chunk");
    };
    assert_eq!((total, data.len()), (size as u64, MAX_ARTIFACT_CHUNK));
    assert_eq!(data, bytes[..MAX_ARTIFACT_CHUNK]);
    let Ok(Reply::Artifact { data, .. }) = fetch(&mut a, MAX_ARTIFACT_CHUNK as u64) else {
        panic!("second chunk");
    };
    assert_eq!(data, bytes[MAX_ARTIFACT_CHUNK..]);
    assert_eq!(
        fetch(&mut c, 0).err(),
        Some(Error::NoArtifact),
        "not a seat"
    );
    assert!(matches!(
        a.ask(ClientMsg::MatchArtifact {
            match_id: MatchId(999),
            offset: 0
        }),
        Err(Error::NoArtifact)
    ));
    let _ = std::fs::remove_dir_all(&dir);
}
