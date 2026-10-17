//! The service against the real `bota_server`: reserved seats, the referee and
//! the hero draw of a match nobody joined, and two bots playing a whole match
//! through the game path.

mod common;

use std::path::PathBuf;

use bota_proto::{HeroId, Order, Target, Team, Vec2};
use common::*;
use radovid::config::BackendConfig;
use radovid_client::proto::*;
use radovid_client::{play, take_seat};

/// The game server binary: `BOTA_SERVER_BIN`, built beforehand with
/// `cargo build -p bota_server --bin bota_server`; else the one beside the tests.
fn real_game_server() -> Option<PathBuf> {
    let path = match std::env::var_os("BOTA_SERVER_BIN") {
        Some(path) => PathBuf::from(path),
        None => {
            let exe = std::env::current_exe().expect("test executable");
            exe.parent()
                .and_then(|deps| deps.parent())
                .expect("target dir")
                .join("bota_server")
        }
    };
    path.is_file().then_some(path)
}

fn mid_only(start_after_secs: u64) -> Vec<radovid::config::RulesetConfig> {
    let mut solo = ruleset("solo_mid", true, &["--map", "2", "--mode", "lockstep"]);
    solo.start_after_secs = start_after_secs;
    vec![solo]
}

fn real(rulesets: Vec<radovid::config::RulesetConfig>) -> Env {
    let binary = real_game_server()
        .expect("build the game server first: cargo build -p bota_server --bin bota_server");
    start_with(
        BackendConfig::Local {
            bota_server: binary,
        },
        rulesets,
    )
}

/// A draw comes at the time cap; a side wins before it, when a tower falls.
fn assert_end_is_consistent(result: &MatchResult) {
    let cap = bota_core::MAP2_TICK_CAP;
    let duration = result.stats.as_ref().expect("stats").duration;
    assert_eq!(duration, result.duration_ticks);
    match result.outcome {
        Outcome::Draw => assert_eq!(
            (result.reason, duration),
            (EndReason::TimeCap, cap),
            "a draw comes at the time cap"
        ),
        Outcome::Radiant | Outcome::Dire => {
            assert_eq!(result.reason, EndReason::Decided);
            assert!(duration < cap, "a side wins before the cap");
        }
        Outcome::Null => panic!("the match was lost: {result:?}"),
    }
}

/// Nobody plays, so the waves fight on alone: on most seeds the clock runs out,
/// on some a wave takes a tower first. The service draws the seed from the OS,
/// so the test takes either end.
#[test]
fn a_match_nobody_joins_starts_with_drawn_heroes_and_plays_to_its_own_end() {
    let env = real(mid_only(1));
    let mut a = env.bot("alpha");
    let mut b = env.bot("beta");
    pair_up(&mut a, &mut b, "solo_mid");
    let result = a.wait_result();
    assert_end_is_consistent(&result);
    assert!(
        result.seats.iter().all(|s| s.hero.is_some()),
        "the server drew a hero for each seat"
    );
    for seat in &result.seats {
        let after = seat.after.expect("rated");
        assert!(after.deviation < 350.0);
        let won = match result.outcome {
            Outcome::Radiant => seat.team == Team::Radiant,
            Outcome::Dire => seat.team == Team::Dire,
            _ => {
                assert!((after.rating - 1500.0).abs() < 1e-6, "a draw of equals");
                continue;
            }
        };
        assert_eq!(
            after.rating > 1500.0,
            won,
            "the winner gains, the loser loses"
        );
    }
}

#[test]
fn two_bots_play_a_whole_match_through_the_game_path() {
    let env = real(mid_only(30));
    let mut a = env.bot("alpha");
    let mut b = env.bot("beta");
    let (ha, hb) = pair_up(&mut a, &mut b, "solo_mid");
    let games: Vec<_> = [ha, hb]
        .into_iter()
        .map(|handoff| {
            let tls = env.tls.clone();
            std::thread::spawn(move || {
                let (mut link, seated) =
                    take_seat(&handoff, HeroId::SHADOW_FIEND, &tls).expect("seat");
                let middle = Order::Move {
                    target: Target::Pos(Vec2::from_ints(9216, 9216)),
                };
                // In lockstep an acknowledgement can race the server's exit at the very end; the
                // service's own record of the result is what counts.
                play(&mut link, &seated, |_| Some(middle)).ok()
            })
        })
        .collect();
    let ends: Vec<_> = games
        .into_iter()
        .filter_map(|g| g.join().expect("game"))
        .collect();
    if let [first, second] = ends.as_slice() {
        assert_eq!(first.winner, second.winner, "both seats saw the same end");
    }
    let result = a.wait_result();
    assert_ne!(result.outcome, Outcome::Null);
    assert!(ends.iter().all(|o| match o.winner {
        Team::Radiant => result.outcome == Outcome::Radiant,
        Team::Dire => result.outcome == Outcome::Dire,
        Team::Neutral => result.outcome == Outcome::Draw,
    }));
    assert!(
        result
            .seats
            .iter()
            .all(|s| s.hero == Some(HeroId::SHADOW_FIEND))
    );
}

/// The docker backend: set `RADOVID_DOCKER_IMAGE` to an image of `bota_server`
/// (docker/Dockerfile.bota_server) and `RADOVID_DOCKER_NETWORK` to a network; the
/// daemon is reached on a loopback port, which also suits a rootless one.
#[test]
fn a_container_runs_the_match_and_is_gone_after_it() {
    let (Some(image), Some(network)) = (
        std::env::var_os("RADOVID_DOCKER_IMAGE"),
        std::env::var_os("RADOVID_DOCKER_NETWORK"),
    ) else {
        eprintln!("skipped: RADOVID_DOCKER_IMAGE and RADOVID_DOCKER_NETWORK are not set");
        return;
    };
    let backend = BackendConfig::Docker {
        image: image.to_string_lossy().into_owned(),
        network: network.to_string_lossy().into_owned(),
        docker: "docker".to_owned(),
        memory: "512m".to_owned(),
        cpus: "1".to_owned(),
        publish_loopback: true,
    };
    let env = start_with(backend, mid_only(1));
    let mut a = env.bot("alpha");
    let mut b = env.bot("beta");
    pair_up(&mut a, &mut b, "solo_mid");
    let result = a.wait_result();
    assert_end_is_consistent(&result);
    let left = std::process::Command::new("docker")
        .args(["ps", "-aq", "--filter", "label=radovid.match"])
        .output()
        .expect("docker");
    std::thread::sleep(std::time::Duration::from_secs(3));
    let left_later = std::process::Command::new("docker")
        .args(["ps", "-aq", "--filter", "label=radovid.match"])
        .output()
        .expect("docker");
    drop(left);
    assert!(left_later.stdout.is_empty(), "the container was removed");
}
