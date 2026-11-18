//! The checks of a process are bounded: the ones past the bound are told so, at once.

use std::time::Duration;

use radovid::test_support::impostor::{Behaviour, Impostor};
use radovid_client::{MAX_CHECKS_AT_ONCE, ServiceCheck, ServiceFault, TlsConfig};

#[test]
fn only_so_many_checks_run_at_once_and_the_rest_are_told_so() {
    let server = Impostor::start(Behaviour::Silent);
    let tls = TlsConfig::trust_pem(&server.pem).expect("the impostor's certificate");
    // A hang guard: the silent server holds every check that is let run until it is dropped.
    let patience = Duration::from_secs(120);
    let mut started: Vec<ServiceCheck> = (0..MAX_CHECKS_AT_ONCE + 4)
        .map(|_| ServiceCheck::start(&server.url, &tls, 1, patience))
        .collect();
    let told = started
        .iter_mut()
        .filter_map(|check| check.poll())
        .filter(|outcome| *outcome == Err(ServiceFault::TooManyChecks))
        .count();
    assert_eq!(told, 4, "the checks past the bound, and only they");
}
