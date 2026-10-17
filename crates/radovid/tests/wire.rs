//! A game server built from another bota is refused when the service starts,
//! not discovered match by match. Its own test binary: the stand-in is a
//! script written here, and no other test may fork while it is open.

mod common;

use std::os::unix::fs::PermissionsExt;

use common::*;
use radovid::config::BackendConfig;

#[test]
fn the_service_will_not_start_with_game_servers_of_another_wire() {
    let dir = test_dir();
    let other = dir.join("bota_server");
    std::fs::write(&other, "#!/bin/sh\necho 0123456789abcdef\n").expect("script");
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let refused = try_start_with(
        BackendConfig::Local { bota_server: other },
        vec![ruleset("solo_mid", true, &[])],
    );
    let _ = std::fs::remove_dir_all(&dir);
    let error = refused.err().expect("the service refuses to start");
    assert!(
        error.contains("0123456789abcdef")
            && error.contains(&format!("{:016x}", bota_proto::WIRE_FINGERPRINT)),
        "{error}"
    );
}
