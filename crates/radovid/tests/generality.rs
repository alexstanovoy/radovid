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
