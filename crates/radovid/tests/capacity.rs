//! Admission bounds apply even when individual accounts have no cap.
mod common;

use common::*;
use radovid::config::{LimitsConfig, PerKind};
use radovid_client::proto::*;

#[test]
fn custom_matches_have_a_global_cap_before_launch_and_lobby_countdown() {
    for running_slots in [0, 1] {
    let limits: LimitsConfig = toml::from_str("custom_match_capacity = 1").expect("custom capacity config");
    let mut practice = ruleset("practice", false, "hang", "0");
    practice.instant_solo = true;
    let env = start_configured(vec![practice], |c| {
        c.limits = limits;
        c.limits.games_per_account = PerKind { human: 0, bot: 0 };
        c.limits.game_capacity = 1;
        c.limits.lobby_games_max = running_slots;
    });
    let mut player = env.human("player");
    player.expect_done(ClientMsg::StartSolo { ruleset: RulesetId("practice".into()) });
    let match_id = if running_slots == 1 { player.wait_match_found().match_id } else {
        player.wait_for(10, |event| match event { Event::MatchQueued { match_id, .. } => Some(match_id), _ => None })
    };
    let Reply::Limits { limits } = player.ask(ClientMsg::Limits { ruleset: None }).expect("limits") else { panic!("limits") };
    assert_eq!(limits.custom_match_capacity, 1);
    let refused = player.ask(ClientMsg::StartSolo { ruleset: RulesetId("practice".into()) }).expect_err("bounded pending solos");
    assert_eq!(refused.to_string(), "CustomMatchCapacity { limit: 1 }");
    let Reply::Lobby { state } = player.ask(ClientMsg::LobbyCreate { ruleset: RulesetId("practice".into()), name: "Waiting".into(), audience: Audience::Everyone, password: None }).expect("lobby") else { panic!("lobby") };
    let refused = player.ask(ClientMsg::LobbyReady { lobby: state.id, ready: true }).expect_err("bounded lobby starts");
    assert_eq!(refused.to_string(), "CustomMatchCapacity { limit: 1 }");
    env.grant_admin("player");
    let mut admin = env.login("player");
    admin.expect_done(ClientMsg::AdminStopMatch { match_id });
    assert_eq!(player.wait_result().outcome, Outcome::Null);
    player.expect_done(ClientMsg::StartSolo { ruleset: RulesetId("practice".into()) });
    }
}


#[test]
fn a_lobby_countdown_reserves_capacity_before_its_match_is_formed() {
    let env = start_configured(vec![ruleset("fun", false, "hang", "0")], |c| {
        c.limits.games_per_account = PerKind { human: 0, bot: 0 };
        c.limits.custom_match_capacity = 1;
    });
    let mut player = env.human("player");
    for (index, accepted) in [(0, true), (1, false)] {
        let Reply::Lobby { state } = player.ask(ClientMsg::LobbyCreate {
            ruleset: RulesetId("fun".into()), name: format!("Lobby {index}"), audience: Audience::Everyone, password: None,
        }).expect("lobby") else { panic!("lobby") };
        let result = player.ask(ClientMsg::LobbyReady { lobby: state.id, ready: true });
        if accepted { assert_eq!(result, Ok(Reply::Done)); }
        else { assert_eq!(result, Err(Error::CustomMatchCapacity { limit: 1 })); }
    }
}
