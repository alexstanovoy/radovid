//! Checking a service before signing in to it, against a real service and against servers that
//! are something else: what each outcome is, and that nothing a server sends hangs or floods the
//! client.

mod common;

use std::net::TcpListener;
use std::time::Duration;

use common::*;
use radovid::test_support::impostor::{Behaviour, Impostor, SEND_CAP};
use radovid_client::proto::{Error, PROTO_VERSION};
use radovid_client::{MAX_ANSWER_BYTES, ServiceCheck, ServiceFault, ServiceFound, TlsConfig};

/// A hang guard for a check that has to end by itself: long enough for a loaded machine, and
/// never what ends a check that is expected to.
const PATIENCE: Duration = Duration::from_secs(120);

/// The checks of this file run one test at a time: a process holds [`MAX_CHECKS_AT_ONCE`] of
/// them at most, and tests side by side would take one another's places.
fn alone() -> std::sync::MutexGuard<'static, ()> {
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ONE.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn check(url: &str, tls: &TlsConfig) -> Result<ServiceFound, ServiceFault> {
    check_within(url, tls, PATIENCE)
}

fn check_within(
    url: &str,
    tls: &TlsConfig,
    patience: Duration,
) -> Result<ServiceFound, ServiceFault> {
    let mut check = ServiceCheck::start(url, tls, FINGERPRINT, patience);
    loop {
        if let Some(outcome) = check.poll() {
            return outcome;
        }
        std::thread::yield_now();
    }
}

fn trusting(impostor: &Impostor) -> TlsConfig {
    TlsConfig::trust_pem(&impostor.pem).expect("the impostor's certificate")
}

fn rules() -> Vec<radovid::config::RulesetConfig> {
    vec![ruleset("solo", true, "team0", "0.1")]
}

#[test]
fn a_running_service_passes_and_tells_its_name_without_any_sign_in() {
    let _alone = alone();
    let env = start(rules());
    assert_eq!(
        check(&env.url, &env.tls),
        Ok(ServiceFound {
            name: "radovid-test".to_owned()
        })
    );
    // The address as people type it, with the path the clients add themselves.
    assert!(check(&format!("{}/control", env.url), &env.tls).is_ok());
}

#[test]
fn a_check_leaves_no_account_and_no_session_behind() {
    let _alone = alone();
    let env = start(rules());
    check(&env.url, &env.tls).expect("passes");
    let mut probe = env.connect();
    let Ok(radovid_client::proto::Reply::Limits { .. }) =
        probe.request(radovid_client::proto::ClientMsg::Limits { ruleset: None })
    else {
        panic!("the service still answers");
    };
}

#[test]
fn a_wrong_path_on_a_running_service_is_no_service() {
    let _alone = alone();
    let env = start(rules());
    for path in ["/wrong", "/wrong/deeper", "/controls"] {
        assert_eq!(
            check(&format!("{}{path}", env.url), &env.tls),
            Err(ServiceFault::NotService),
            "{path}"
        );
    }
}

#[test]
fn a_port_nobody_listens_on_is_refused() {
    let _alone = alone();
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        listener.local_addr().expect("bound").port()
    };
    let tls = TlsConfig::webpki();
    assert_eq!(
        check(&format!("wss://127.0.0.1:{port}"), &tls),
        Err(ServiceFault::Refused)
    );
}

#[test]
fn a_check_sends_one_hello_and_nothing_that_says_who_is_asking() {
    let _alone = alone();
    let server = Impostor::start(Behaviour::Echo);
    assert_eq!(
        check(&server.url, &trusting(&server)),
        Err(ServiceFault::NotService)
    );
    let said = server.received();
    assert_eq!(said.len(), 1, "one message, and it is the Hello");
    let payload = radovid_client::proto::whole_frame(
        &said[0],
        radovid_client::proto::MAX_CONTROL_PAYLOAD_LEN,
    )
    .expect("a whole frame");
    let frame: radovid_client::proto::ClientFrame =
        radovid_client::proto::decode_payload(payload).expect("a message of the protocol");
    let radovid_client::proto::ClientMsg::Hello {
        proto,
        game_fingerprint,
        client,
    } = frame.msg
    else {
        panic!("the one message is not a Hello: {:?}", frame.msg);
    };
    // The protocol version, the game wire and the library's own name: no login, no token.
    assert_eq!((proto, game_fingerprint), (PROTO_VERSION, FINGERPRINT));
    assert!(client.starts_with("radovid_client/"), "{client:?}");
}

#[test]
fn a_host_that_does_not_exist_is_not_found() {
    let _alone = alone();
    // `.invalid` never resolves (RFC 6761).
    assert_eq!(
        check("wss://nothing-here.invalid", &TlsConfig::webpki()),
        Err(ServiceFault::HostNotFound)
    );
}

#[test]
fn addresses_that_are_no_wss_address_are_bad_before_any_connection() {
    let _alone = alone();
    let tls = TlsConfig::webpki();
    for url in ["ws://127.0.0.1:1", "http://127.0.0.1:1", "not a url", ""] {
        assert_eq!(check(url, &tls), Err(ServiceFault::BadAddress), "{url:?}");
    }
}

#[test]
fn a_certificate_the_client_does_not_trust_is_not_secure() {
    let _alone = alone();
    let env = start(rules());
    let other = Impostor::start(Behaviour::Silent);
    let stranger = trusting(&other);
    assert_eq!(check(&env.url, &stranger), Err(ServiceFault::Insecure));
}

#[test]
fn an_echo_server_a_web_server_and_a_mistaken_server_are_no_service() {
    let _alone = alone();
    for (behaviour, what) in [
        (Behaviour::Echo, "an echo"),
        (Behaviour::PlainHttp, "a plain web server"),
        (Behaviour::Mistaken, "a server that answers something else"),
    ] {
        let server = Impostor::start(behaviour);
        assert_eq!(
            check(&server.url, &trusting(&server)),
            Err(ServiceFault::NotService),
            "{what}"
        );
    }
}

#[test]
fn a_server_that_closes_the_connection_at_once_is_a_hangup() {
    let _alone = alone();
    let server = Impostor::start(Behaviour::HangUp);
    assert_eq!(
        check(&server.url, &trusting(&server)),
        Err(ServiceFault::Hangup)
    );
}

#[test]
fn a_refused_version_says_whether_the_service_or_the_client_is_older() {
    let _alone = alone();
    for (error, expected) in [
        (
            Error::ProtoMismatch {
                server: PROTO_VERSION + 1,
            },
            ServiceFault::Newer {
                server: PROTO_VERSION + 1,
            },
        ),
        (
            Error::ProtoMismatch {
                server: PROTO_VERSION - 1,
            },
            ServiceFault::Older {
                server: PROTO_VERSION - 1,
            },
        ),
        (Error::Malformed, ServiceFault::Incompatible),
        (Error::ServerStopping, ServiceFault::Busy),
    ] {
        let server = Impostor::start(Behaviour::Refuses(error.clone()));
        assert_eq!(
            check(&server.url, &trusting(&server)),
            Err(expected),
            "{error:?}"
        );
    }
}

#[test]
fn a_first_message_over_the_bound_is_cut_off_at_it() {
    let _alone = alone();
    let server = Impostor::start(Behaviour::Oversized(MAX_ANSWER_BYTES * 8));
    // The patience is a hang guard: only the bound can end this check with this fault.
    assert_eq!(
        check(&server.url, &trusting(&server)),
        Err(ServiceFault::NotService)
    );
}

#[test]
fn a_first_message_that_never_ends_is_cut_off_long_before_the_server_is_done() {
    let _alone = alone();
    let server = Impostor::start(Behaviour::Endless);
    assert_eq!(
        check(&server.url, &trusting(&server)),
        Err(ServiceFault::NotService)
    );
    server.wait_for_a_connection_to_end();
    assert!(
        server.sent() < SEND_CAP / 2,
        "the client read on for {} of {SEND_CAP} bytes",
        server.sent()
    );
}

#[test]
fn a_check_whose_time_is_spent_ends_before_it_connects() {
    let _alone = alone();
    let server = Impostor::start(Behaviour::Silent);
    assert_eq!(
        check_within(&server.url, &trusting(&server), Duration::ZERO),
        Err(ServiceFault::TimedOut)
    );
}

#[test]
fn a_check_dropped_while_it_waits_lets_go_of_the_connection() {
    let _alone = alone();
    let server = Impostor::start(Behaviour::Silent);
    let mut running = ServiceCheck::start(&server.url, &trusting(&server), FINGERPRINT, PATIENCE);
    // The server is silent: the check only ends when it is dropped, once it holds a connection.
    server.wait_for_a_connection();
    assert!(running.poll().is_none());
    drop(running);
    server.wait_for_a_connection_to_end();
}
