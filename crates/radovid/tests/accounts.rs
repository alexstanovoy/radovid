//! Accounts over real WebSockets: registration, login, tokens, the brute-force
//! delay, versions and hostile input.

mod common;

use std::time::{Duration, Instant};

use common::*;
use radovid_client::proto::*;
use radovid_client::{Client, ClientError};

fn solo() -> Vec<radovid::config::RulesetConfig> {
    vec![ruleset("solo_mid", true, &[])]
}

fn refused(result: Result<impl std::fmt::Debug, ClientError>) -> Error {
    match result {
        Err(ClientError::Refused(error)) => error,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn accounts_register_log_in_resume_and_change_their_data() {
    let env = start(solo());
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
    assert_eq!(
        refused(other.request(ClientMsg::Rulesets)),
        Error::NotLoggedIn
    );

    // Log in again with the password, then resume with the token.
    let mut again = env.connect();
    let second = again.login("Alice", PASSWORD, info.clone()).expect("login");
    assert_eq!(second.me.id, alice.me.id);
    let mut resumed = env.connect();
    let logged = resumed.resume(alice.token, info.clone()).expect("resume");
    assert_eq!(logged.me.id, alice.me.id);
    assert_eq!(logged.presence, Presence::Idle);
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
            assert_eq!(account.login, "alice");
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
        current: PASSWORD.to_owned(),
        new: new_password.to_owned(),
    });
    assert_eq!(
        refused(env.connect().resume(second.session, info.clone())),
        Error::BadSession
    );
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
            current: "wrong password".to_owned(),
            new: new_password.to_owned()
        }),
        Err(Error::BadCredentials)
    );
}

#[test]
fn failed_logins_wait_longer_and_a_right_password_always_gets_in() {
    let env = start(solo());
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
fn failed_resumes_hold_up_nobody() {
    let env = start(solo());
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
    let env = try_start_configured(
        radovid::config::BackendConfig::Local {
            bota_server: fake_game_server(),
        },
        solo(),
        |config| config.max_connections = 2,
    )
    .expect("service");
    let first = env.connect();
    let _second = env.connect();
    assert!(
        Client::connect(&env.url, &env.tls).is_err(),
        "a third connection was served"
    );
    drop(first);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Client::connect(&env.url, &env.tls).is_err() {
        assert!(Instant::now() < deadline, "the place never came back");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn another_protocol_version_is_refused_and_a_stale_game_wire_cannot_queue() {
    let env = start(solo());
    let error = refused(Client::connect_as(
        &env.url,
        &env.tls,
        PROTO_VERSION + 1,
        bota_proto::WIRE_FINGERPRINT,
    ));
    assert_eq!(
        error,
        Error::ProtoMismatch {
            server: PROTO_VERSION
        }
    );

    let mut client =
        Client::connect_as(&env.url, &env.tls, PROTO_VERSION, 12345).expect("accounts still work");
    client
        .register("dave", PASSWORD, AccountKind::Human, ClientInfo::default())
        .expect("register");
    let error = refused(client.request(ClientMsg::QueueJoin {
        ruleset: RulesetId("solo_mid".to_owned()),
    }));
    assert_eq!(
        error,
        Error::BotaWireMismatch {
            server: bota_proto::WIRE_FINGERPRINT
        }
    );
    assert!(client.request(ClientMsg::Rulesets).is_ok());
}

#[test]
fn hostile_input_closes_that_connection_and_nothing_else() {
    let env = start(solo());
    let healthy = env.account("erin", AccountKind::Human);

    // Bytes that are no frame.
    let mut client = env.connect();
    client.send_raw(vec![1, 2, 3]).expect("sent");
    let told = client.read_raw(Duration::from_secs(5));
    assert!(
        matches!(
            told,
            Ok(Some(radovid_client::proto::ServerMsg::Event(Event::Bye {
                error: Error::Malformed
            })))
        ),
        "{told:?}"
    );

    // A frame whose prefix lies.
    let mut client = env.connect();
    client.send_raw(vec![200, 0, 0, 0, 1]).expect("sent");
    let told = client.read_raw(Duration::from_secs(5));
    assert!(
        matches!(
            told,
            Ok(Some(radovid_client::proto::ServerMsg::Event(Event::Bye {
                error: Error::Malformed
            })))
        ),
        "{told:?}"
    );

    // A message far over the limit.
    let mut client = env.connect();
    let _ = client.send_raw(vec![0; MAX_CONTROL_PAYLOAD_LEN * 4]);
    let told = client.read_raw(Duration::from_secs(5));
    assert!(
        matches!(
            told,
            Ok(Some(radovid_client::proto::ServerMsg::Event(Event::Bye {
                error: Error::Malformed
            })))
        ),
        "{told:?}"
    );

    // A flood is slowed, then ended.
    let mut flooder = env.connect();
    let mut limited = 0;
    let mut closed = false;
    for _ in 0..400 {
        match flooder.request(ClientMsg::Rulesets) {
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
