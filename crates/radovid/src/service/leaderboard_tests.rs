//! The public lookup contract through authentication, TLS and real WebSockets.
#[path = "../../tests/common/mod.rs"]
mod common;
use common::*;
use radovid_client::proto::*;

#[test]
fn leaderboard_mode_permissions_and_profile_throttling_cross_the_wire() {
    let ranked = ruleset("ranked", true, "team0", "0.1");
    let mut casual = ruleset("casual", false, "team0", "0.1");
    casual.rating_ruleset = Some("ranked".into());
    let mut robots = ruleset("robots", true, "team0", "0.1");
    robots.audience = crate::config::AudienceConfig::BotsOnly;
    let env = start(vec![ranked, casual, robots]);
    let mut human = env.account("human", AccountKind::Human);
    env.block_on(
        env.service()
            .freeze_lookup_clock_for_test(std::time::Instant::now()),
    );
    for (mode, error) in [
        ("casual", Error::NoQueue),
        ("robots", Error::AudienceMismatch),
    ] {
        assert_eq!(
            human.ask(ClientMsg::Leaderboard {
                ruleset: RulesetId(mode.into()),
                kind: AccountKind::Human,
                prefix: String::new()
            }),
            Err(error)
        );
    }
    for kind in [
        AccountKind::Human,
        AccountKind::Bot,
        AccountKind::Human,
        AccountKind::Bot,
    ] {
        let Reply::LeaderboardPage { page } = human
            .ask(ClientMsg::Leaderboard {
                ruleset: RulesetId("ranked".into()),
                kind,
                prefix: String::new(),
            })
            .expect("board")
        else {
            panic!("board reply");
        };
        assert_eq!(page.kind, kind);
        assert_eq!(page.limit, 100);
        assert!(page.rows.is_empty());
        assert!(page.own.is_none());
    }
    assert_eq!(
        human.ask(ClientMsg::Leaderboard {
            ruleset: RulesetId("ranked".into()),
            kind: AccountKind::Human,
            prefix: String::new()
        }),
        Err(Error::Throttled {
            retry_after_ms: 250
        })
    );
    assert_eq!(
        human.ask(ClientMsg::Account {
            account: human.me.id
        }),
        Err(Error::Throttled {
            retry_after_ms: 250
        }),
        "profiles share the caller's lookup quota"
    );
    assert_eq!(
        human.ask(ClientMsg::Leaderboard {
            ruleset: RulesetId("ranked".into()),
            kind: AccountKind::Human,
            prefix: "%".into()
        }),
        Err(Error::BadLeaderboardSearch)
    );
}
