//! Contracts that do not depend on any particular game's rules.
mod common;

use common::*;
use radovid::config::{Config, LimitsConfig, PerKindOrBoth};
use radovid_client::proto::*;

#[test]
fn default_lobbies_have_one_seat_per_side() {
    let config: Config = toml::from_str(include_str!("../../../radovid.example.toml"))
        .expect("example configuration");
    assert_eq!(config.lobby_teams, [1, 1]);
}

#[test]
fn unlimited_account_default_is_queryable_and_explicit_limits_still_apply() {
    let mut limited = ruleset("limited", true, "team0", "0");
    limited.games_per_account = Some(PerKindOrBoth::Both(2));
    let env = start_configured(vec![ruleset("solo", true, "team0", "0"), limited], |c| {
        c.limits = LimitsConfig { games_per_source: 200, ..LimitsConfig::default() };
    });
    let mut account = env.human("player");
    let Reply::Limits { limits } = account.ask(ClientMsg::Limits { ruleset: None }).expect("limits") else {
        panic!("limits reply")
    };
    assert_eq!(limits.games_per_account, 0, "zero means unlimited");
    for _ in 0..12 {
        account.queue(&solo());
    }
    let limited = RulesetId("limited".into());
    let Reply::Limits { limits } = account.ask(ClientMsg::Limits { ruleset: Some(limited.clone()) }).expect("limits") else {
        panic!("limits reply")
    };
    assert_eq!(limits.games_per_account, 2);
    assert_eq!(account.ask(ClientMsg::QueueJoin { ruleset: limited }), Err(Error::TooManyGames { limit: 2 }));
}

#[test]
fn a_lobby_leaver_is_checked_against_the_actual_side_not_the_queue_slot() {
    let mut rules = ruleset("fun", false, "leave1", "0");
    rules.teams = vec![2, 2];
    rules.lobby_min_players = Some(2);
    let env = start_configured(vec![rules], |c| c.lobby_teams = vec![1, 1]);
    let mut owner = env.human("owner");
    let mut guest = env.human("guest");
    let Reply::Lobby { state } = owner.ask(ClientMsg::LobbyCreate {
        ruleset: RulesetId("fun".into()), name: "Small lobby".into(),
        audience: Audience::Everyone, password: None,
    }).expect("create") else { panic!("lobby") };
    assert!(matches!(guest.ask(ClientMsg::LobbyJoin { lobby: state.id, password: None }), Ok(Reply::Lobby { .. })));
    guest.expect_done(ClientMsg::LobbyReady { lobby: state.id, ready: true });
    owner.expect_done(ClientMsg::LobbyReady { lobby: state.id, ready: true });
    let result = owner.wait_result();
    assert_eq!(result.outcome, Outcome::Winner { team: 0 });
    assert_eq!(result.reason, EndReason::Left);
    assert_eq!(guest.wait_result(), result);
}

#[test]
fn an_unoccupied_configured_opponent_can_win_a_solo_game() {
    let mut rules = ruleset("practice", false, "team1", "0");
    rules.instant_solo = true;
    let env = start(vec![rules]);
    let mut player = env.human("player");
    player.expect_done(ClientMsg::StartSolo { ruleset: RulesetId("practice".into()) });
    let result = player.wait_result();
    assert_eq!(result.seats.len(), 1);
    assert_eq!(result.outcome, Outcome::Winner { team: 1 });
}
