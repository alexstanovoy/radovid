//! The queue, the handoff to a game server, the result and the ratings, the
//! one-activity invariant, reconnects, spectators and the admin tools.

mod common;

use bota_client_core::Transport;
use bota_proto::{HeroId, ServerMsg, Team};
use common::*;
use radovid_client::proto::*;
use radovid_client::{ClientError, spectate, take_seat};

fn rulesets(outcome: &str) -> Vec<radovid::config::RulesetConfig> {
    vec![ruleset(
        "solo_mid",
        true,
        &["--outcome", outcome, "--after-ms", "300"],
    )]
}

#[test]
fn two_queued_bots_are_matched_play_and_are_rated() {
    let env = start(rulesets("radiant"));
    let mut alpha = env.bot("alpha");
    let mut beta = env.bot("beta");
    let (for_alpha, for_beta) = pair_up(&mut alpha, &mut beta, "solo_mid");

    // The handoff: one match, two seats, the longer waiting account first.
    assert_eq!(for_alpha.match_id, for_beta.match_id);
    assert!(for_alpha.rated);
    assert_eq!((for_alpha.slot.0, for_beta.slot.0), (0, 1));
    assert_eq!((for_alpha.team, for_beta.team), (Team::Radiant, Team::Dire));
    assert_ne!(for_alpha.seat_key, for_beta.seat_key);
    assert_eq!(for_alpha.bota_wire, bota_proto::WIRE_FINGERPRINT);
    assert!(for_alpha.game_url.starts_with("wss://") && for_alpha.game_url.ends_with("/game"));
    let seat = &for_alpha.seats[1];
    assert_eq!(
        (seat.login.as_str(), seat.kind, seat.bot_version.as_deref()),
        ("beta", AccountKind::Bot, Some("test-1"))
    );

    // One activity at a time.
    let busy = Error::Busy {
        presence: Presence::InMatch {
            match_id: for_alpha.match_id,
        },
    };
    assert_eq!(
        alpha.ask(ClientMsg::QueueJoin {
            ruleset: RulesetId("solo_mid".to_owned())
        }),
        Err(busy.clone())
    );
    let lobby = ClientMsg::LobbyCreate {
        ruleset: RulesetId("solo_mid".to_owned()),
        name: "x".to_owned(),
        audience: Audience::Everyone,
    };
    assert_eq!(alpha.ask(lobby), Err(busy));
    assert!(matches!(
        alpha.ask(ClientMsg::MatchHandoff),
        Ok(Reply::Handoff { .. })
    ));

    let alpha_game = play(for_alpha, env.tls.clone());
    let beta_game = play(for_beta, env.tls.clone());
    assert_eq!(alpha_game.join().expect("alpha"), Some(Team::Radiant));
    assert_eq!(beta_game.join().expect("beta"), Some(Team::Radiant));

    let result = alpha.wait_result();
    assert_eq!(
        (result.outcome, result.reason, result.rated),
        (Outcome::Radiant, EndReason::Decided, true)
    );
    assert_eq!(result.duration_ticks, 27_000);
    let (winner, loser) = (&result.seats[0], &result.seats[1]);
    assert_eq!(winner.login, "alpha");
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

    // The ladder, the profile and the history show it; both are free again.
    let Ok(Reply::LadderPage { total, rows }) = alpha.ask(ClientMsg::Ladder {
        ruleset: RulesetId("solo_mid".to_owned()),
        offset: 0,
        limit: 10,
    }) else {
        panic!("ladder");
    };
    assert_eq!(total, 2);
    assert_eq!(
        (
            rows[0].rank,
            rows[0].login.as_str(),
            rows[0].kind,
            rows[0].wins
        ),
        (1, "alpha", AccountKind::Bot, 1)
    );
    assert_eq!(
        (rows[1].rank, rows[1].login.as_str(), rows[1].losses),
        (2, "beta", 1)
    );
    let Ok(Reply::Matches { list }) = alpha.ask(ClientMsg::RecentMatches {
        account: alpha.me.id,
        limit: 5,
    }) else {
        panic!("matches");
    };
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].seats[0].hero, Some(HeroId::SHADOW_FIEND));
    assert_eq!(list[0].seats[0].after, winner.after);
    assert_eq!(presence_of(&env, "alpha"), Presence::Idle);
    assert_eq!(
        alpha.ask(ClientMsg::QueueJoin {
            ruleset: RulesetId("solo_mid".to_owned())
        }),
        Ok(Reply::Queued { waiting: 1 })
    );
    assert_eq!(alpha.ask(ClientMsg::QueueLeave), Ok(Reply::Done));
    assert_eq!(alpha.ask(ClientMsg::QueueLeave), Err(Error::NotInQueue));
}

#[test]
fn a_drawn_match_is_half_a_win_and_a_lost_game_server_changes_no_rating() {
    let env = start(rulesets("draw"));
    let mut a = env.bot("alpha");
    let mut b = env.bot("beta");
    let (ha, hb) = pair_up(&mut a, &mut b, "solo_mid");
    let games = [play(ha, env.tls.clone()), play(hb, env.tls.clone())];
    for game in games {
        assert_eq!(game.join().expect("game"), Some(Team::Neutral));
    }
    let result = a.wait_result();
    assert_eq!(
        (result.outcome, result.reason),
        (Outcome::Draw, EndReason::TimeCap)
    );
    for seat in &result.seats {
        let after = seat.after.expect("rated");
        assert!(
            (after.rating - 1500.0).abs() < 1e-6 && after.deviation < 350.0,
            "{after:?}"
        );
    }
    let Ok(Reply::Account { account }) = a.ask(ClientMsg::Account { account: a.me.id }) else {
        panic!("account");
    };
    assert_eq!(
        (account.ratings[0].1.games, account.ratings[0].1.draws),
        (1, 1)
    );
    env.stop();

    // The game server dies mid-match: null, nothing rated, both free.
    let env = start(rulesets("crash"));
    let mut a = env.bot("alpha");
    let mut b = env.bot("beta");
    let (ha, hb) = pair_up(&mut a, &mut b, "solo_mid");
    let games = [play(ha, env.tls.clone()), play(hb, env.tls.clone())];
    for game in games {
        assert_eq!(
            game.join().expect("game"),
            None,
            "the stream ends without a result"
        );
    }
    let result = b.wait_result();
    assert_eq!(
        (result.outcome, result.reason),
        (Outcome::Null, EndReason::Crashed)
    );
    assert!(
        result.stats.is_none()
            && result
                .seats
                .iter()
                .all(|s| s.before.is_none() && s.after.is_none())
    );
    let Ok(Reply::LadderPage { total, .. }) = b.ask(ClientMsg::Ladder {
        ruleset: RulesetId("solo_mid".to_owned()),
        offset: 0,
        limit: 10,
    }) else {
        panic!("ladder");
    };
    assert_eq!(total, 0);
    assert_eq!(presence_of(&env, "beta"), Presence::Idle);
}

#[test]
fn a_returning_player_takes_its_seat_back_once_the_old_connection_ends_and_its_account_may_watch() {
    let env = start(rulesets("hang"));
    let mut a = env.bot("alpha");
    let mut b = env.bot("beta");
    let (ha, hb) = pair_up(&mut a, &mut b, "solo_mid");
    let (old, _) = take_seat(&ha, HeroId::SHADOW_FIEND, &env.tls).expect("first seat");
    let (_other, _) = take_seat(&hb, HeroId::SHADOW_FIEND, &env.tls).expect("second seat");

    // The same handoff while the first link lives is refused: a seat never changes hands
    // under a live connection.
    assert!(take_seat(&ha, HeroId::SHADOW_FIEND, &env.tls).is_err());

    // Once that link has ended, the same handoff takes the seat back.
    drop(old);
    let (mut back, seated) = (0..50)
        .find_map(|_| {
            let taken = take_seat(&ha, HeroId::SHADOW_FIEND, &env.tls).ok();
            if taken.is_none() {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            taken
        })
        .expect("reconnect");
    assert_eq!(seated.slot.0, 0);
    loop {
        match back.recv().expect("readable") {
            Some(ServerMsg::MatchStart { .. }) => break,
            Some(_) => {}
            None => panic!("closed before the match start"),
        }
    }

    // The same account, on another session, learns where it is and watches through its seat.
    let mut second_session = env.connect();
    let logged = second_session
        .login("alpha", PASSWORD, ClientInfo::default())
        .expect("login");
    assert_eq!(
        logged.presence,
        Presence::InMatch {
            match_id: ha.match_id
        }
    );
    let Ok(Reply::Handoff { handoff }) = second_session
        .request(ClientMsg::MatchHandoff)
        .map_err(|_| ())
    else {
        panic!("handoff");
    };
    assert_eq!(handoff.seat_key, ha.seat_key);
    let mut watcher = spectate(&handoff, &env.tls).expect("spectator");
    assert!(watcher.recv().expect("readable").is_some());

    // Another account's key is no key for this seat; nonsense opens nothing.
    let mut wrong = ha.clone();
    wrong.seat_key = bota_proto::SeatKey([5; 32]);
    assert!(spectate(&wrong, &env.tls).is_err());

    // An admin ends the match: nothing counts.
    env.account("root_user", AccountKind::Human);
    env.grant_admin("root_user");
    let mut root = env.connect();
    root.login("root_user", PASSWORD, ClientInfo::default())
        .expect("admin login");
    assert_eq!(
        root.request(ClientMsg::AdminStopMatch {
            match_id: ha.match_id
        })
        .expect("stop"),
        Reply::Done
    );
    let result = a.wait_result();
    assert_eq!(
        (result.outcome, result.reason),
        (Outcome::Null, EndReason::Stopped)
    );
    assert_eq!(b.wait_result().outcome, Outcome::Null);
    assert_eq!(
        a.ask(ClientMsg::AdminStopMatch {
            match_id: ha.match_id
        }),
        Err(Error::Forbidden)
    );
    assert_eq!(
        root.request(ClientMsg::AdminStopMatch {
            match_id: ha.match_id
        })
        .map_err(|e| match e {
            ClientError::Refused(error) => error,
            other => panic!("{other}"),
        }),
        Err(Error::UnknownMatch)
    );
}

#[test]
fn bans_remove_the_banned_from_the_queue_and_keep_them_out_until_lifted() {
    let env = start(rulesets("radiant"));
    env.account("root_user", AccountKind::Human);
    env.grant_admin("root_user");
    let mut root = env.connect();
    root.login("root_user", PASSWORD, ClientInfo::default())
        .expect("admin login");
    let solo = RulesetId("solo_mid".to_owned());

    let mut troll = env.bot("troll");
    assert_eq!(
        troll.ask(ClientMsg::QueueJoin {
            ruleset: solo.clone()
        }),
        Ok(Reply::Queued { waiting: 1 })
    );
    assert_eq!(
        troll.ask(ClientMsg::AdminBan {
            account: troll.me.id,
            reason: "x".to_owned(),
            until: None
        }),
        Err(Error::Forbidden)
    );
    root.request(ClientMsg::AdminBan {
        account: troll.me.id,
        reason: "spam".to_owned(),
        until: None,
    })
    .expect("ban");
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
        Err(ClientError::Refused(Error::Banned {
            reason,
            until: None,
        })) => assert_eq!(reason, "spam"),
        other => panic!("{other:?}"),
    }

    // The banned left the queue: the next account waits alone instead of being paired with it.
    let mut honest = env.bot("honest");
    assert_eq!(
        honest.ask(ClientMsg::QueueJoin { ruleset: solo }),
        Ok(Reply::Queued { waiting: 1 })
    );

    root.request(ClientMsg::AdminUnban {
        account: troll.me.id,
    })
    .expect("unban");
    again
        .login("troll", PASSWORD, ClientInfo::default())
        .expect("welcome back");
    root.request(ClientMsg::AdminSetKind {
        account: troll.me.id,
        kind: AccountKind::Human,
    })
    .expect("kind");
    let Ok(Reply::Account { account }) = honest.ask(ClientMsg::Account {
        account: troll.me.id,
    }) else {
        panic!("account");
    };
    assert_eq!(account.kind, AccountKind::Human);
    assert!(matches!(
        root.request(ClientMsg::AdminSetKind {
            account: honest.me.id,
            kind: AccountKind::Human
        }),
        Err(ClientError::Refused(Error::Busy { .. }))
    ));
}
