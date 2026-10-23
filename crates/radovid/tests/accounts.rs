//! Accounts over real WebSockets: registration, login, tokens, deletion, the
//! brute-force delay, versions, hostile input and certificate reloads.

mod common;

use std::time::{Duration, Instant};

use common::*;
use radovid_client::proto::*;
use radovid_client::{Client, ClientError, Incoming, TlsConfig};

fn rules() -> Vec<radovid::config::RulesetConfig> {
    vec![ruleset("solo", true, "team0", "0.1")]
}

fn refused(result: Result<impl std::fmt::Debug, ClientError>) -> Error {
    match result {
        Err(ClientError::Refused(error)) => error,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn accounts_register_log_in_resume_and_change_their_data() {
    let env = start(rules());
    let mut alice = env.account("alice", AccountKind::Human);
    assert_eq!(alice.me.login, "alice");
    assert_eq!(alice.me.nickname, "alice");
    assert!(!alice.me.admin);

    // The login is unique whatever the case; the rules on strings hold.
    let mut other = env.connect();
    let info = ClientInfo::default();
    assert_eq!(
        refused(other.register("ALICE", PASSWORD, AccountKind::Human, info.clone())),
        Error::LoginTaken
    );
    assert_eq!(
        refused(other.register("a", PASSWORD, AccountKind::Human, info.clone())),
        Error::BadLogin
    );
    assert_eq!(
        refused(other.register("bob", "short", AccountKind::Human, info.clone())),
        Error::WeakPassword
    );
    let bot_label = ClientInfo {
        bot_version: Some("v1".to_owned()),
    };
    assert_eq!(
        refused(other.register("bob", PASSWORD, AccountKind::Human, bot_label)),
        Error::BadBotVersion
    );
    // The catalog and the limits are public; everything else needs a login.
    let Ok(Reply::Rulesets { list }) = other.request(ClientMsg::Rulesets) else {
        panic!("rulesets");
    };
    assert_eq!(
        (
            list[0].game.as_str(),
            list[0].teams.as_slice(),
            list[0].games_per_account
        ),
        ("testgame", &[1u8, 1][..], 2)
    );
    let Ok(Reply::Limits { limits }) = other.request(ClientMsg::Limits { ruleset: None }) else {
        panic!("limits");
    };
    assert_eq!(
        (
            limits.games_per_account,
            limits.decline_cooldown_ms,
            limits.accept_timeout_ms
        ),
        (2, 120_000, 30_000)
    );
    assert_eq!(
        refused(other.request(ClientMsg::QueueJoin { ruleset: solo() })),
        Error::NotLoggedIn
    );

    // Log in again with the password, then resume with the token.
    let mut again = env.connect();
    let second = again.login("Alice", PASSWORD, info.clone()).expect("login");
    assert_eq!(second.me.id, alice.me.id);
    let mut resumed = env.connect();
    let logged = resumed.resume(alice.token, info.clone()).expect("resume");
    assert_eq!(logged.me.id, alice.me.id);
    assert_eq!(logged.activities, Activities::default());
    assert_eq!(
        refused(env.connect().resume(SessionToken([7; 32]), info.clone())),
        Error::BadSession
    );

    // A nickname is free-form and not unique; the account shows it.
    alice.expect_done(ClientMsg::ChangeNickname {
        nickname: "Queen_A".to_owned(),
    });
    assert_eq!(
        alice.ask(ClientMsg::ChangeNickname {
            nickname: "x".to_owned()
        }),
        Err(Error::BadNickname)
    );
    let mut bob = env.account("bobby", AccountKind::Human);
    bob.expect_done(ClientMsg::ChangeNickname {
        nickname: "Queen_A".to_owned(),
    });
    match bob
        .ask(ClientMsg::Account {
            account: alice.me.id,
        })
        .expect("account")
    {
        Reply::Account { account } => {
            assert_eq!(account.id, alice.me.id);
            assert_eq!(account.nickname, "Queen_A");
            assert!(account.online);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        bob.ask(ClientMsg::Account {
            account: AccountId(9999)
        }),
        Err(Error::UnknownAccount)
    );

    // A new password revokes every other token and the old password.
    let new_password = "another long passphrase";
    alice.expect_done(ClientMsg::ChangePassword {
        current: Secret::new(PASSWORD),
        new: Secret::new(new_password),
    });
    assert_eq!(
        refused(env.connect().resume(second.session, info.clone())),
        Error::BadSession
    );
    // The connection logged in with the old password is put out too; the
    // one on the token that changed it stays.
    let told = again.next_event(Duration::from_secs(5));
    assert!(
        matches!(
            told,
            Ok(Some(Event::Bye {
                error: Error::BadSession
            }))
        ),
        "{told:?}"
    );
    assert!(resumed.request(ClientMsg::Rulesets).is_ok());
    env.connect()
        .resume(alice.token, info.clone())
        .expect("the session that changed it stays");
    assert_eq!(
        refused(env.connect().login("alice", PASSWORD, info.clone())),
        Error::BadCredentials
    );
    // Three failures from this address in a row have earned a one second delay.
    std::thread::sleep(Duration::from_millis(1100));
    env.connect()
        .login("alice", new_password, info)
        .expect("the new password works");
    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(
        alice.ask(ClientMsg::ChangePassword {
            current: Secret::new("wrong password"),
            new: Secret::new(new_password)
        }),
        Err(Error::BadCredentials)
    );
}

#[test]
fn failed_logins_wait_longer_and_a_right_password_always_gets_in() {
    let env = start(rules());
    env.account("carol", AccountKind::Human);
    let info = ClientInfo::default();
    let mut client = env.connect();
    for _ in 0..3 {
        assert_eq!(
            refused(client.login("carol", "wrong password!", info.clone())),
            Error::BadCredentials
        );
    }
    // The fourth attempt comes inside the delay: not even evaluated.
    let started = Instant::now();
    let wait = match refused(client.login("carol", PASSWORD, info.clone())) {
        Error::Throttled { retry_after_ms } => retry_after_ms,
        other => panic!("expected a delay, got {other:?}"),
    };
    assert!((1..=1000).contains(&wait), "{wait}");
    // Another address-wide key: a different login is held up by the address too.
    assert!(matches!(
        refused(client.login("nobody_here", PASSWORD, info.clone())),
        Error::Throttled { .. }
    ));
    std::thread::sleep(
        Duration::from_millis(u64::from(wait) + 100).saturating_sub(started.elapsed()),
    );
    client
        .login("carol", PASSWORD, info)
        .expect("after the delay the right password works");
}

#[test]
fn a_stranger_failing_at_a_login_does_not_delay_its_owner() {
    let env = start_configured(rules(), listen_on_both_families);
    env.account("dora", AccountKind::Human);
    let info = ClientInfo::default();
    let mut attacker = env.connect_from_second_source();
    for _ in 0..3 {
        assert_eq!(
            refused(attacker.login("dora", "wrong password!", info.clone())),
            Error::BadCredentials
        );
    }
    assert!(matches!(
        refused(attacker.login("dora", "wrong password!", info.clone())),
        Error::Throttled { .. }
    ));
    // The owner's own address has no failures at that login.
    env.login("dora");
}

/// Twelve logins at once from the first source against a pool of `max_waiting`
/// places. Returns how many were turned away, and whether a login from the
/// second source got in before the flood was over.
fn flood_and_bystander(max_waiting: usize) -> (usize, Option<bool>) {
    const FLOOD: usize = 12;
    let env = start_configured(rules(), |config| {
        listen_on_both_families(config);
        config.password = radovid::password::PasswordCost {
            memory_kib: 256 * 1024,
            iterations: 3,
            max_waiting,
        };
    });
    env.account("erin", AccountKind::Human);
    env.account("finn", AccountKind::Human);
    let ordering = std::sync::atomic::Ordering::SeqCst;
    let finished = std::sync::atomic::AtomicUsize::new(0);
    let throttled = std::sync::atomic::AtomicUsize::new(0);
    let mut bystander = None;
    std::thread::scope(|scope| {
        let flood: Vec<_> = (0..FLOOD)
            .map(|_| {
                scope.spawn(|| {
                    let result = env.connect().login("erin", PASSWORD, ClientInfo::default());
                    if matches!(result, Err(ClientError::Refused(Error::Throttled { .. }))) {
                        throttled.fetch_add(1, ordering);
                    }
                    finished.fetch_add(1, ordering);
                })
            })
            .collect();
        std::thread::sleep(Duration::from_millis(150));
        let result =
            env.connect_from_second_source()
                .login("finn", PASSWORD, ClientInfo::default());
        if result.is_ok() {
            bystander = Some(finished.load(ordering) < FLOOD);
        }
        for handle in flood {
            handle.join().expect("flood thread");
        }
    });
    (throttled.load(ordering), bystander)
}

#[test]
fn one_source_hashes_one_password_at_a_time() {
    let (throttled, bystander) = flood_and_bystander(64);
    assert_eq!(throttled, 0, "a roomy line turns nobody away");
    assert_eq!(
        bystander,
        Some(true),
        "the other source waited for the whole flood"
    );
}

#[test]
fn a_full_hashing_line_answers_throttled() {
    let (throttled, _) = flood_and_bystander(4);
    assert!(throttled >= 6, "{throttled} turned away");
}

#[test]
fn failed_resumes_hold_up_nobody() {
    let env = start(rules());
    let gina = env.account("gina", AccountKind::Human);
    let info = ClientInfo::default();
    for byte in 1..=8 {
        assert_eq!(
            refused(env.connect().resume(SessionToken([byte; 32]), info.clone())),
            Error::BadSession
        );
    }
    // Neither the account's own token nor a password waits for the guesser.
    env.connect()
        .resume(gina.token, info.clone())
        .expect("a valid token gets in at once");
    env.connect()
        .login("gina", PASSWORD, info)
        .expect("a password gets in at once");
}

#[test]
fn connections_past_the_limit_are_closed_and_their_places_come_back() {
    let env = start_configured(rules(), |config| config.max_connections = 2);
    let first = env.connect();
    let _second = env.connect();
    assert!(
        Client::connect(&env.url, &env.tls, FINGERPRINT).is_err(),
        "a third connection was served"
    );
    drop(first);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Client::connect(&env.url, &env.tls, FINGERPRINT).is_err() {
        assert!(Instant::now() < deadline, "the place never came back");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn another_protocol_version_is_refused_and_another_game_wire_cannot_play() {
    let env = start(rules());
    let error = refused(Client::connect_as(
        &env.url,
        &env.tls,
        PROTO_VERSION + 1,
        FINGERPRINT,
    ));
    assert_eq!(
        error,
        Error::ProtoMismatch {
            server: PROTO_VERSION
        }
    );

    let mut client = Client::connect(&env.url, &env.tls, 12345).expect("accounts still work");
    client
        .register("dave", PASSWORD, AccountKind::Human, ClientInfo::default())
        .expect("register");
    let stale = Error::GameVersionMismatch {
        server: FINGERPRINT,
    };
    assert_eq!(
        refused(client.request(ClientMsg::QueueJoin { ruleset: solo() })),
        stale
    );
    assert_eq!(
        refused(client.request(ClientMsg::LobbyCreate {
            ruleset: solo(),
            name: "x".to_owned(),
            audience: Audience::Everyone,
            password: None,
        })),
        stale
    );
    assert!(client.request(ClientMsg::Rulesets).is_ok());
}

#[test]
fn a_deleted_account_is_gone_but_its_matches_stay_anonymous() {
    let env = start(rules());
    let mut erin = env.account("erin", AccountKind::Human);
    let mut fred = env.account("fred", AccountKind::Human);
    let mut second = env.login("erin");
    let (handoff, _) = pair_up(&mut erin, &mut fred, &solo());
    erin.wait_result();
    fred.wait_result();

    assert_eq!(
        erin.ask(ClientMsg::DeleteAccount {
            password: Secret::new("wrong password")
        }),
        Err(Error::BadCredentials)
    );
    erin.expect_done(ClientMsg::DeleteAccount {
        password: Secret::new(PASSWORD),
    });
    // Every session ends; the password and the tokens are gone.
    let told = second.wait_for(5, |event| match event {
        Event::Bye { error } => Some(error),
        _ => None,
    });
    assert_eq!(told, Error::BadSession);
    assert_eq!(
        refused(env.connect().login("erin", PASSWORD, ClientInfo::default())),
        Error::BadCredentials
    );
    assert_eq!(
        refused(env.connect().resume(erin.token, ClientInfo::default())),
        Error::BadSession
    );
    // The login is free again; the history shows an anonymous seat and no rating.
    env.account("erin", AccountKind::Human);
    let Ok(Reply::Matches { list }) = fred.ask(ClientMsg::RecentMatches {
        account: fred.me.id,
        limit: 5,
    }) else {
        panic!("matches");
    };
    assert_eq!(list[0].match_id, handoff.match_id);
    assert_eq!(list[0].seats[0].nickname, "deleted");
    let Ok(Reply::LadderPage { total, rows }) = fred.ask(ClientMsg::Ladder {
        ruleset: solo(),
        offset: 0,
        limit: 10,
    }) else {
        panic!("ladder");
    };
    assert_eq!((total, rows[0].account), (1, fred.me.id));
}

#[test]
fn a_reloaded_certificate_serves_new_connections() {
    let env = start(rules());
    let renewed = self_signed();
    env.reload_tls(&renewed);
    let trusting_new = TlsConfig::trust_pem(&renewed.cert_pem).expect("trust");
    Client::connect(&env.url, &trusting_new, FINGERPRINT).expect("the new certificate");
    assert!(
        Client::connect(&env.url, &env.tls, FINGERPRINT).is_err(),
        "the old certificate is still served"
    );
}

#[test]
fn hostile_input_closes_that_connection_and_nothing_else() {
    let env = start(rules());
    let healthy = env.account("erin", AccountKind::Human);

    // Bytes that are no frame.
    let mut client = env.connect();
    client.send_raw(vec![1, 2, 3]).expect("sent");
    let told = read_raw(&mut client, 5);
    assert!(
        matches!(
            told,
            Ok(Some(Incoming::Event(Event::Bye {
                error: Error::Malformed
            })))
        ),
        "{told:?}"
    );

    // A frame whose prefix lies.
    let mut client = env.connect();
    client.send_raw(vec![200, 0, 0, 0, 1]).expect("sent");
    let told = read_raw(&mut client, 5);
    assert!(
        matches!(
            told,
            Ok(Some(Incoming::Event(Event::Bye {
                error: Error::Malformed
            })))
        ),
        "{told:?}"
    );

    // A message far over the limit is refused as its header comes in: the
    // connection just ends.
    let mut client = env.connect();
    let _ = client.send_raw(vec![0; MAX_CONTROL_PAYLOAD_LEN * 4]);
    let told = read_raw(&mut client, 5);
    assert!(matches!(told, Err(ClientError::Closed)), "{told:?}");

    // A flood is slowed, then ended.
    let mut flooder = env.connect();
    let mut limited = 0;
    let mut closed = false;
    for _ in 0..400 {
        match flooder.request(ClientMsg::LobbyList) {
            Err(ClientError::Refused(Error::RateLimited { .. })) => limited += 1,
            Err(ClientError::Refused(Error::NotLoggedIn)) => {}
            Err(_) => {
                closed = true;
                break;
            }
            Ok(_) => {}
        }
    }
    assert!(limited > 0 && closed, "limited {limited}, closed {closed}");

    // The service goes on.
    drop(healthy);
    env.account("frank", AccountKind::Human);
}
