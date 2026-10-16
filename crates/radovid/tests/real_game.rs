//! The service against the real `bota_server`: reserved seats, the referee and
//! the hero draw of a match nobody joined, and two bots playing a whole match
//! through the game path.

mod common;

use std::path::PathBuf;
use std::sync::OnceLock;

use bota_proto::{HeroId, Order, Target, Team, Vec2};
use common::*;
use radovid::config::BackendConfig;
use radovid_client::proto::*;
use radovid_client::{play, take_seat};

/// The game server, built once from the bota the service is built against.
fn real_game_server() -> PathBuf {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
            let status = std::process::Command::new(cargo)
                .args(["build", "--quiet", "-p", "bota_server", "--bin", "bota_server"])
                .current_dir(&root)
                .status()
                .expect("cargo runs");
            assert!(status.success(), "the game server builds");
            let exe = std::env::current_exe().expect("test executable");
            exe.parent().and_then(|deps| deps.parent()).expect("target dir").join("bota_server")
        })
        .clone()
}

fn mid_only(start_after_secs: u64) -> Vec<radovid::config::RulesetConfig> {
    let mut solo = ruleset("solo_mid", true, &["--map", "2", "--mode", "lockstep"]);
    solo.start_after_secs = start_after_secs;
    vec![solo]
}

fn real(rulesets: Vec<radovid::config::RulesetConfig>) -> Env {
    start_with(BackendConfig::Local { bota_server: real_game_server() }, rulesets)
}

#[test]
fn a_match_nobody_joins_starts_with_drawn_heroes_and_ends_in_a_drawn_clock() {
    let env = real(mid_only(1));
    let mut a = env.bot("alpha");
    let mut b = env.bot("beta");
    pair_up(&mut a, &mut b, "solo_mid");
    let result = a.wait_result();
    assert_eq!((result.outcome, result.reason), (Outcome::Draw, EndReason::TimeCap));
    assert!(result.stats.as_ref().is_some_and(|s| s.duration > 0));
    assert!(result.seats.iter().all(|s| s.hero.is_some()), "the server drew a hero for each seat");
    let after = result.seats[0].after.expect("rated");
    assert!((after.rating - 1500.0).abs() < 1e-6 && after.deviation < 350.0);
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
                let (mut link, seated) = take_seat(&handoff, HeroId::SHADOW_FIEND, &tls).expect("seat");
                let middle = Order::Move { target: Target::Pos(Vec2::from_ints(9216, 9216)) };
                play(&mut link, &seated, HeroId::SHADOW_FIEND, |_| Some(middle)).expect("a whole match")
            })
        })
        .collect();
    for game in games {
        let (winner, stats) = game.join().expect("game");
        assert_eq!(winner, Team::Neutral);
        assert_eq!(stats.slots.len(), 2);
    }
    let result = a.wait_result();
    assert_eq!(result.outcome, Outcome::Draw);
    assert!(result.seats.iter().all(|s| s.hero == Some(HeroId::SHADOW_FIEND)));
}
