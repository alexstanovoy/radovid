//! What the service refuses to start with, and the files it leaves behind:
//! secrets are its owner's alone, and tickets never cross a network in clear.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use common::*;
use radovid_client::proto::AccountKind;

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .permissions()
        .mode()
        & 0o777
}

#[test]
fn the_database_and_its_journal_are_the_owners_alone() {
    let env = start(vec![ruleset("solo", true, "draw", "0")]);
    env.account("someone", AccountKind::Human);
    assert_eq!(mode(&env.db_path()), 0o600);
    assert_eq!(mode(&env.db_path().with_extension("db-wal")), 0o600);
}

#[test]
fn a_database_every_user_may_read_is_refused() {
    let error = try_start(vec![ruleset("solo", true, "draw", "0")], |config| {
        std::fs::write(&config.database, b"").expect("database");
        let open = std::fs::Permissions::from_mode(0o644);
        std::fs::set_permissions(&config.database, open).expect("mode");
    })
    .map(Env::stop)
    .expect_err("refused");
    assert!(error.contains("chmod o-rwx"), "{error}");
}

#[test]
fn a_tls_key_every_user_may_read_is_refused_and_dev_cert_writes_none() {
    let dir = test_dir();
    let status = Command::new(env!("CARGO_BIN_EXE_radovid"))
        .arg("dev-cert")
        .arg(&dir)
        .status()
        .expect("radovid runs");
    assert!(status.success());
    let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
    assert_eq!(mode(&key), 0o600);
    assert!(radovid::tls::load(&cert, &key).is_ok());

    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).expect("mode");
    let error = radovid::tls::load(&cert, &key)
        .map(|_| ())
        .expect_err("refused");
    assert!(error.contains("chmod o-rwx"), "{error}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_game_url_in_clear_must_stay_on_this_machine_unless_allowed() {
    let with_url = |url: &str, insecure: bool| {
        let mut rules = ruleset("solo", true, "draw", "0");
        rules.launcher.public_url = url.to_owned();
        rules.launcher.insecure_plaintext = insecure;
        try_start(vec![rules], |_| {}).map(|env| env.stop())
    };
    for url in [
        "ws://game.example.org:{port}",
        "game.example.org:{port}",
        "ws://10.0.0.1:{port}",
    ] {
        let error = with_url(url, false).expect_err("refused");
        assert!(error.contains("insecure_plaintext"), "{url}: {error}");
        assert!(with_url(url, true).is_ok(), "{url}");
    }
    for url in [
        "ws://127.0.0.1:{port}",
        "ws://localhost:{port}",
        "ws://[::1]:{port}",
        "127.0.0.1:{port}",
    ] {
        assert!(with_url(url, false).is_ok(), "{url}");
    }
}
