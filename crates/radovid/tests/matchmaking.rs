//! The queue, proposals, the launcher contract, results and ratings, several
//! games per account, the game slots and the admin tools.

mod common;

use common::*;
use radovid_client::Client;
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
        // A version of the line this radovid does not know is refused, not guessed at.
        ("futureversion", EndReason::Crashed),
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
    let env = keeping(rules);
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

/// A game of a newer contract that only adds fields to its `RESULT` line is still read.
#[test]
fn a_result_line_with_fields_of_a_newer_game_is_read() {
    let env = start(rules("future"));
    let (mut a, mut b) = (env.bot("alpha"), env.bot("beta"));
    pair_up(&mut a, &mut b, &solo());
    let result = b.wait_result();
    assert_eq!(result.outcome, Outcome::Winner { team: 1 });
    assert_eq!(result.reason, EndReason::Decided);
}

/// Every game is told the newest RESULT version this radovid reads, so a game may print the
/// fields of a newer line to it and the old line to an older radovid.
#[test]
fn a_game_is_told_the_result_version_radovid_reads() {
    let env = start(rules("versioned"));
    let (mut a, mut b) = (env.bot("alpha"), env.bot("beta"));
    pair_up(&mut a, &mut b, &solo());
    assert_eq!(b.wait_result().outcome, Outcome::Winner { team: 0 });
}

/// The artifact is served once the game process has exited and its files are closed, not when
/// its `RESULT` line is read: a game that goes on writing after the line has the whole file
/// served, never a part of it.
#[test]
fn an_artifact_is_served_only_after_the_game_has_exited() {
    let dir = artifact_dir("artifact-slow");
    let mut rules = rules("slowfile");
    let launcher = &mut rules[0].launcher;
    launcher.artifact = Some(format!("{}/replay-{{match_id}}.brp", dir.display()));
    launcher
        .artifact_env
        .insert("GAME_ARTIFACT".to_owned(), "{artifact}".to_owned());
    let env = keeping(rules);
    let (mut a, _b, match_id, _) = finished_match(&env, &dir, |env, name| env.bot(name));
    let Ok(Reply::Artifact { data, .. }) = fetch_artifact(&mut a, match_id) else {
        panic!("the artifact is served");
    };
    assert_eq!(
        data,
        format!("replay of {} and its end", match_id.0).as_bytes()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Plays a match of `rules[0]` between two accounts made by `make`, and gives the folder its
/// replay goes to and the match's id.
fn finished_match(
    env: &Env,
    dir: &std::path::Path,
    make: impl Fn(&Env, &str) -> Account,
) -> (Account, Account, MatchId, std::path::PathBuf) {
    let (mut a, mut b) = (make(env, "alpha"), make(env, "beta"));
    pair_up(&mut a, &mut b, &solo());
    let result = a.wait_result();
    let file = dir.join(format!("replay-{}.brp", result.match_id.0));
    (a, b, result.match_id, file)
}

/// A service that keeps the artifacts of its matches.
fn keeping(rules: Vec<radovid::config::RulesetConfig>) -> Env {
    start_configured(rules, |config| config.artifacts.save_replays = true)
}

fn artifact_dir(what: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("radovid-{what}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    dir
}

fn fetch_artifact(who: &mut Account, match_id: MatchId) -> Result<Reply, Error> {
    who.ask(ClientMsg::MatchArtifact {
        match_id,
        offset: 0,
    })
}

/// A match that crashed, was abandoned or was left keeps its replay for its seats as well: the
/// game wrote what it had.
#[test]
fn the_replay_of_a_lost_abandoned_or_left_match_is_served_too() {
    let dir = artifact_dir("artifact-lost");
    for behaviour in ["crash", "abandon", "leave0"] {
        let mut rules = rules(behaviour);
        rules[0].launcher.artifact = Some(format!("{}/replay-{{match_id}}.brp", dir.display()));
        let env = keeping(rules);
        let (mut a, _b, match_id, file) = finished_match(&env, &dir, |env, name| env.bot(name));
        std::fs::write(&file, behaviour).expect("replay");
        let Ok(Reply::Artifact { data, .. }) = fetch_artifact(&mut a, match_id) else {
            panic!("{behaviour}: the replay is served");
        };
        assert_eq!(data, behaviour.as_bytes());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `[artifacts] save_replays` decides whether a game is asked to leave a replay at all: on, it
/// is told where (`artifact_args`, `artifact_env`) and its seats fetch what it wrote; off
/// (the default), it is told nothing, no file is made, and a fetch is answered "not kept".
#[test]
fn replays_are_asked_for_kept_and_served_only_when_the_service_saves_them() {
    for saving in [true, false] {
        let dir = artifact_dir(if saving { "save-on" } else { "save-off" });
        let mut rules = rules("team0");
        let launcher = &mut rules[0].launcher;
        launcher.artifact = Some(format!("{}/replay-{{match_id}}.brp", dir.display()));
        launcher
            .artifact_env
            .insert("GAME_ARTIFACT".to_owned(), "{artifact}".to_owned());
        launcher.artifact_args = vec!["{artifact}".to_owned()];
        let env = start_configured(rules, |config| config.artifacts.save_replays = saving);
        let (mut a, _b, match_id, file) = finished_match(&env, &dir, |env, name| env.bot(name));
        let fetched = fetch_artifact(&mut a, match_id);
        if saving {
            let Ok(Reply::Artifact { data, .. }) = fetched else {
                panic!("a kept replay is served: {fetched:?}");
            };
            assert_eq!(data, format!("replay of {}", match_id.0).as_bytes());
        } else {
            assert_eq!(fetched.err(), Some(Error::ArtifactsNotKept));
            assert!(!file.exists(), "the game was not asked to write one");
            assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 0);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A launcher that names `{artifact}` among the arguments every launch gets would be started
/// with an empty one when replays are not kept: the configuration is refused.
#[test]
fn the_artifact_placeholder_belongs_to_the_arguments_that_are_left_out_when_not_kept() {
    let mut rules = rules("team0");
    rules[0].launcher.artifact = Some("/tmp/replay-{match_id}.brp".to_owned());
    rules[0].launcher.args.push("{artifact}".to_owned());
    let error = try_start(rules, |_| {}).err().expect("refused");
    assert!(error.contains("artifact_args"), "{error}");
}

/// A file past the ruleset's `artifact_max_mib` is never served, one at the cap is, and a link in
/// the file's place is no file.
#[test]
fn an_artifact_past_the_size_cap_or_a_link_is_not_served() {
    let dir = artifact_dir("artifact-cap");
    let mut rules = rules("team0");
    rules[0].launcher.artifact = Some(format!("{}/replay-{{match_id}}.brp", dir.display()));
    // The ruleset's own cap rules, not the service-wide one (64 MiB here).
    rules[0].artifact_max_mib = Some(1);
    let env = keeping(rules);
    let (mut a, _b, match_id, file) = finished_match(&env, &dir, |env, name| env.bot(name));
    let cap = 1 << 20;
    std::fs::write(&file, vec![7u8; cap + 1]).expect("replay");
    assert_eq!(
        fetch_artifact(&mut a, match_id).err(),
        Some(Error::NoArtifact),
        "one byte over the cap"
    );
    std::fs::write(&file, vec![7u8; cap]).expect("replay");
    let Ok(Reply::Artifact { total, data, .. }) = fetch_artifact(&mut a, match_id) else {
        panic!("a file at the cap is served");
    };
    assert_eq!((total, data.len()), (cap as u64, cap));
    let secret = dir.join("secret.txt");
    std::fs::write(&secret, "not a replay").expect("secret");
    std::fs::remove_file(&file).expect("remove");
    std::os::unix::fs::symlink(&secret, &file).expect("link");
    assert_eq!(
        fetch_artifact(&mut a, match_id).err(),
        Some(Error::NoArtifact),
        "a link is no replay"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A ruleset that opens its replays to its audience serves any account the audience admits, and
/// still not one it does not.
#[test]
fn a_ruleset_can_open_its_replays_to_its_audience() {
    let dir = artifact_dir("artifact-audience");
    let mut rules = rules("team0");
    rules[0].launcher.artifact = Some(format!("{}/replay-{{match_id}}.brp", dir.display()));
    rules[0].artifact_access = radovid::config::ArtifactAccess::Audience;
    rules[0].audience = radovid::config::AudienceConfig::HumansOnly;
    let env = keeping(rules);
    let (_a, _b, match_id, file) = finished_match(&env, &dir, |env, name| env.human(name));
    std::fs::write(&file, b"replay").expect("replay");
    let mut onlooker = env.human("gamma");
    let Ok(Reply::Artifact { data, .. }) = fetch_artifact(&mut onlooker, match_id) else {
        panic!("a human is in the audience");
    };
    assert_eq!(data, b"replay");
    let mut bot = env.bot("delta");
    assert_eq!(
        fetch_artifact(&mut bot, match_id).err(),
        Some(Error::NoArtifact),
        "a bot is outside the audience"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A ruleset for bots only is not listed to humans and refuses their queue joins.
#[test]
fn a_ruleset_for_bots_is_neither_listed_to_humans_nor_open_to_them() {
    let mut arena = ruleset("arena", true, "team0", "0.3");
    arena.audience = radovid::config::AudienceConfig::BotsOnly;
    let env = start(vec![ruleset("solo", true, "team0", "0.3"), arena]);
    let mut hana = env.human("hana");
    let mut beta = env.bot("beta");
    let ids = |list: Reply| match list {
        Reply::Rulesets { list } => list.into_iter().map(|r| r.id.0).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    assert_eq!(ids(hana.ask(ClientMsg::Rulesets).expect("list")), ["solo"]);
    assert_eq!(
        ids(beta.ask(ClientMsg::Rulesets).expect("list")),
        ["solo", "arena"]
    );
    let arena = RulesetId("arena".to_owned());
    assert_eq!(
        hana.ask(ClientMsg::QueueJoin {
            ruleset: arena.clone()
        }),
        Err(Error::AudienceMismatch)
    );
    assert!(beta.ask(ClientMsg::QueueJoin { ruleset: arena }).is_ok());
}

fn named(
    dir: &std::path::Path,
    access: radovid::config::ArtifactAccess,
) -> radovid::config::ArtifactConfig {
    radovid::config::ArtifactConfig {
        path: format!("{}/meta-{{match_id}}.meta", dir.display()),
        args: Vec::new(),
        env: [("GAME_META".to_owned(), "{artifact}".to_owned())].into(),
        max_kib: Some(4),
        max_mib: None,
        access,
        retention_days: Some(365),
        cap_mib: Some(64),
        kept: true,
    }
}

fn fetch_named(
    who: &mut Account,
    match_id: MatchId,
    name: &str,
    offset: u64,
) -> Result<Reply, Error> {
    who.ask(ClientMsg::MatchArtifactNamed {
        match_id,
        name: name.to_owned(),
        offset,
    })
}

/// A ruleset names files besides the replay, each with its own switch, size and access: the
/// `meta` file here is served, as bytes the service cannot read, to the ruleset's audience
/// while the replay, which the service does not keep, is not; an unknown or malformed name is
/// no artifact.
#[test]
fn a_named_artifact_has_its_own_access_and_switch_and_the_replay_keeps_its_own() {
    use radovid::config::ArtifactAccess;
    let dir = artifact_dir("named");
    let mut rules = rules("team0");
    rules[0].launcher.artifact = Some(format!("{}/replay-{{match_id}}.brp", dir.display()));
    rules[0]
        .launcher
        .artifact_env
        .insert("GAME_ARTIFACT".to_owned(), "{artifact}".to_owned());
    rules[0]
        .artifacts
        .insert("meta".to_owned(), named(&dir, ArtifactAccess::Audience));
    let env = start_configured(rules, |config| assert!(!config.artifacts.save_replays));
    let (mut a, _b, match_id, _) = finished_match(&env, &dir, |env, name| env.bot(name));
    let mut onlooker = env.bot("gamma");
    let expected = [
        &[0xff, 0x00][..],
        format!("meta of {}", match_id.0).as_bytes(),
    ]
    .concat();
    let Ok(Reply::ArtifactNamed {
        name, total, data, ..
    }) = fetch_named(&mut a, match_id, "meta", 0)
    else {
        panic!("the meta is served");
    };
    assert_eq!(
        (name.as_str(), total, data),
        ("meta", expected.len() as u64, expected.clone())
    );
    let Ok(Reply::ArtifactNamed { data, .. }) = fetch_named(&mut a, match_id, "meta", 3) else {
        panic!("a later chunk");
    };
    assert_eq!(data, expected[3..]);
    assert!(
        matches!(
            fetch_named(&mut onlooker, match_id, "meta", 0),
            Ok(Reply::ArtifactNamed { .. })
        ),
        "the audience may fetch it"
    );
    assert_eq!(
        fetch_named(&mut a, match_id, "replay", 0).err(),
        Some(Error::ArtifactsNotKept)
    );
    assert_eq!(
        fetch_artifact(&mut a, match_id).err(),
        Some(Error::ArtifactsNotKept)
    );
    for name in ["nothing", "Not Valid", "", &"x".repeat(33)] {
        assert_eq!(
            fetch_named(&mut a, match_id, name, 0).err(),
            Some(Error::NoArtifact),
            "{name:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// An artifact whose access is the seats' is the seats' alone, one switched off is not asked of
/// the game and answered "not kept", and a file past its own size is not served.
#[test]
fn a_named_artifact_is_for_the_seats_by_default_and_off_or_over_its_size_it_is_not_served() {
    use radovid::config::ArtifactAccess;
    let dir = artifact_dir("named-seats");
    let mut rules = rules("team0");
    rules[0]
        .artifacts
        .insert("meta".to_owned(), named(&dir, ArtifactAccess::Seats));
    let mut off = named(&dir, ArtifactAccess::Seats);
    off.path = format!("{}/off-{{match_id}}.bin", dir.display());
    off.kept = false;
    rules[0].artifacts.insert("off".to_owned(), off);
    let env = start_configured(rules, |_| {});
    let (mut a, _b, match_id, _) = finished_match(&env, &dir, |env, name| env.bot(name));
    let mut onlooker = env.bot("gamma");
    assert!(matches!(
        fetch_named(&mut a, match_id, "meta", 0),
        Ok(Reply::ArtifactNamed { .. })
    ));
    assert_eq!(
        fetch_named(&mut onlooker, match_id, "meta", 0).err(),
        Some(Error::NoArtifact)
    );
    assert_eq!(
        fetch_named(&mut a, match_id, "off", 0).err(),
        Some(Error::ArtifactsNotKept)
    );
    assert!(
        !dir.join(format!("off-{}.bin", match_id.0)).exists(),
        "the game was not asked"
    );
    std::fs::write(
        dir.join(format!("meta-{}.meta", match_id.0)),
        vec![0u8; 4 * 1024 + 1],
    )
    .expect("a long file");
    assert_eq!(
        fetch_named(&mut a, match_id, "meta", 0).err(),
        Some(Error::NoArtifact)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A client of the oldest protocol version served still logs in and fetches the replay, and the
/// replay is the artifact named `replay`.
#[test]
fn a_client_of_the_oldest_protocol_fetches_the_replay_and_the_replay_is_the_artifact_so_named() {
    let dir = artifact_dir("oldest");
    let mut rules = rules("team0");
    rules[0].launcher.artifact = Some(format!("{}/replay-{{match_id}}.brp", dir.display()));
    rules[0]
        .launcher
        .artifact_env
        .insert("GAME_ARTIFACT".to_owned(), "{artifact}".to_owned());
    let env = keeping(rules);
    let (a, _b, match_id, _) = finished_match(&env, &dir, |env, name| env.bot(name));
    let mut old =
        Client::connect_as(&env.url, &env.tls, MIN_PROTO_VERSION, FINGERPRINT).expect("served");
    let logged = old
        .login("alpha", PASSWORD, ClientInfo::default())
        .expect("login");
    let mut old = Account {
        client: old,
        me: logged.me,
        token: logged.session,
    };
    let Ok(Reply::Artifact { data, .. }) = fetch_artifact(&mut old, match_id) else {
        panic!("the replay is served to the oldest client");
    };
    assert_eq!(data, format!("replay of {}", match_id.0).as_bytes());
    let mut new = a;
    let Ok(Reply::ArtifactNamed { data: named, .. }) = fetch_named(&mut new, match_id, "replay", 0)
    else {
        panic!("the replay by name");
    };
    assert_eq!(named, data);
    let _ = std::fs::remove_dir_all(&dir);
}

/// What a configuration may not say about artifacts is refused when the service starts.
#[test]
fn a_configuration_spelling_artifacts_wrongly_is_refused() {
    use radovid::config::ArtifactAccess;
    let dir = artifact_dir("named-config");
    let meta = || named(&dir, ArtifactAccess::Seats);
    let refused = |mutate: &dyn Fn(&mut radovid::config::RulesetConfig)| {
        let mut rules = rules("team0");
        mutate(&mut rules[0]);
        try_start(rules, |_| {}).err().expect("refused")
    };
    let error = refused(&|r| {
        r.launcher.artifact = Some("/tmp/r-{match_id}.brp".to_owned());
        r.artifacts.insert("replay".to_owned(), meta());
    });
    assert!(error.contains("spelled twice"), "{error}");
    let error = refused(&|r| {
        r.artifacts.insert("Bad Name".to_owned(), meta());
    });
    assert!(error.contains("[a-z0-9_]"), "{error}");
    let error = refused(&|r| {
        r.artifacts.insert("one".to_owned(), meta());
        r.artifacts.insert("two".to_owned(), meta());
    });
    assert!(error.contains("names a path another one does"), "{error}");
    let error = refused(&|r| {
        let mut both = meta();
        both.max_mib = Some(1);
        r.artifacts.insert("both".to_owned(), both);
    });
    assert!(error.contains("max_kib or max_mib"), "{error}");
    let error = refused(&|r| {
        let mut far = meta();
        far.max_kib = None;
        far.max_mib = Some(2);
        far.cap_mib = Some(1);
        r.artifacts.insert("far".to_owned(), far);
    });
    assert!(error.contains("at most its cap"), "{error}");
    let _ = std::fs::remove_dir_all(&dir);
}
