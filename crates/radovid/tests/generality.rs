//! Contracts that do not depend on any particular game's rules.
mod common;

use common::*;
use radovid::config::{Config, LimitsConfig, PerKindOrBoth};
use radovid_client::proto::*;

#[test]
fn default_lobbies_have_one_seat_per_side() {
    let config: Config = toml::from_str(include_str!("../../../examples/tic_tac_toe.toml"))
        .expect("example configuration with no explicit lobby layout");
    assert_eq!(config.lobby_teams, [1, 1]);
}

#[test]
fn unlimited_account_default_is_queryable_and_explicit_limits_still_apply() {
    let mut limited = ruleset("limited", true, "team0", "0");
    limited.games_per_account = Some(PerKindOrBoth::Both(2));
    let env = start_configured(vec![ruleset("solo", true, "team0", "0"), limited], |c| {
        c.limits = LimitsConfig {
            games_per_source: 200,
            ..LimitsConfig::default()
        };
    });
    let mut account = env.human("player");
    let Reply::Limits { limits } = account
        .ask(ClientMsg::Limits { ruleset: None })
        .expect("limits")
    else {
        panic!("limits reply")
    };
    assert_eq!(limits.games_per_account, AccountGameLimit::Unlimited);
    assert_eq!(limits.retired_games_per_account, 0);
    let Reply::Rulesets { list } = account.ask(ClientMsg::Rulesets).expect("catalog") else {
        panic!("catalog")
    };
    assert_eq!(list[0].games_per_account, AccountGameLimit::Unlimited);
    for _ in 0..12 {
        account.queue(&solo());
    }
    let limited = RulesetId("limited".into());
    let Reply::Limits { limits } = account
        .ask(ClientMsg::Limits {
            ruleset: Some(limited.clone()),
        })
        .expect("limits")
    else {
        panic!("limits reply")
    };
    assert_eq!(limits.games_per_account, AccountGameLimit::Limited(2));
    assert_eq!(
        account.ask(ClientMsg::QueueJoin { ruleset: limited }),
        Err(Error::TooManyGames { limit: 2 })
    );
}

#[test]
fn a_lobby_leaver_cannot_win_on_its_actual_side() {
    let mut rules = ruleset("fun", false, "leave1", "0");
    rules.lobby_min_players = Some(2);
    let env = start_configured(vec![rules], |c| c.lobby_teams = vec![2, 2]);
    let mut owner = env.human("owner");
    let mut guest = env.human("guest");
    let Reply::Lobby { state } = owner
        .ask(ClientMsg::LobbyCreate {
            ruleset: RulesetId("fun".into()),
            name: "Small lobby".into(),
            audience: Audience::Everyone,
            password: None,
        })
        .expect("create")
    else {
        panic!("lobby")
    };
    assert!(matches!(
        guest.ask(ClientMsg::LobbyJoin {
            lobby: state.id,
            password: None
        }),
        Ok(Reply::Lobby { .. })
    ));
    owner.expect_done(ClientMsg::LobbySlot {
        lobby: state.id,
        account: Some(guest.me.id),
        slot: 1,
    });
    guest.expect_done(ClientMsg::LobbyReady {
        lobby: state.id,
        ready: true,
    });
    owner.expect_done(ClientMsg::LobbyReady {
        lobby: state.id,
        ready: true,
    });
    let result = owner.wait_result();
    assert_eq!(result.outcome, Outcome::Null);
    assert_eq!(result.reason, EndReason::Crashed);
    assert_eq!(guest.wait_result(), result);
}

#[test]
fn an_unoccupied_configured_opponent_can_win_a_solo_game() {
    let mut rules = ruleset("practice", false, "team1", "0");
    rules.instant_solo = true;
    let env = start(vec![rules]);
    let mut player = env.human("player");
    player.expect_done(ClientMsg::StartSolo {
        ruleset: RulesetId("practice".into()),
    });
    let result = player.wait_result();
    assert_eq!(result.seats.len(), 1);
    assert_eq!(result.outcome, Outcome::Winner { team: 1 });
}

#[test]
fn loading_a_ruleset_that_does_not_fit_lobbies_names_the_configuration_error() {
    let text = include_str!("../../../radovid.example.toml");
    let config: Config = toml::from_str(text).expect("configuration syntax");
    let mut config = config;
    config.lobby_teams = vec![1, 1];
    config.rulesets[0].teams = vec![5, 5];
    assert_eq!(
        config
            .validate()
            .expect_err("5v5 must not silently become 1v1"),
        "ruleset tic_tac_toe: teams [5, 5] do not fit lobby_teams [1, 1]; configure lobby_teams explicitly"
    );
}

#[test]
fn every_shipped_example_configuration_loads() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut paths = vec![root.join("radovid.example.toml")];
    for entry in std::fs::read_dir(root.join("examples")).expect("examples") {
        let path = entry.expect("entry").path();
        if path
            .extension()
            .is_some_and(|extension| extension == "toml")
        {
            paths.push(path);
        }
    }
    paths.sort();
    for path in paths {
        Config::load(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    }
}

#[test]
fn version_twelve_clients_are_refused_before_receiving_typed_limits() {
    let env = start(vec![ruleset("solo", true, "team0", "0")]);
    let result = radovid_client::Client::connect_as(&env.url, &env.tls, 12, FINGERPRINT);
    let Err(radovid_client::ClientError::Refused(error)) = result else {
        panic!("old client must be refused")
    };
    assert_eq!(
        error,
        Error::ProtoMismatch {
            server: PROTO_VERSION
        }
    );
}
