//! Named bot identities authenticate through their owner and keep independent records.
mod common;

use common::*;
use radovid_client::{ClientError, proto::*};

fn start_wide(rated: bool) -> Env {
    let mut mode = ruleset("ranked", rated, "team0", "0");
    let mut modes = Vec::new();
    if !rated {
        mode.rating_ruleset = Some("rating".into());
        modes.push(ruleset("rating", true, "team0", "0"));
    }
    modes.push(mode);
    start_configured(modes, |config| {
        config.limits.ranked_window.initial = Some(10_000);
        config.limits.ranked_window.maximum = Some(10_000);
        config.limits.unranked_window.initial = Some(10_000);
        config.limits.unranked_window.maximum = Some(10_000);
    })
}
fn version(name: &str) -> ClientInfo {
    ClientInfo {
        retired_bot_version: None,
        version: Some(name.into()),
    }
}
fn identity(env: &Env, login: &str, name: &str) -> Account {
    let mut client = env.connect();
    let logged = client
        .login(login, PASSWORD, version(name))
        .expect("select bot version");
    Account {
        client,
        me: logged.me,
        token: logged.session,
    }
}
fn assert_waiting(env: &Env, token: &SessionToken, name: &str, count: usize) {
    let mut client = env.connect();
    let logged = client
        .resume(*token, version(name))
        .expect("snapshot barrier");
    assert!(
        logged.activities.proposals.is_empty(),
        "related versions cannot be proposed together"
    );
    assert_eq!(logged.activities.queued.len(), count);
}
fn board(account: &mut Account) -> Vec<LadderRow> {
    match account
        .ask(ClientMsg::Leaderboard {
            ruleset: RulesetId("ranked".into()),
            kind: AccountKind::Bot,
            prefix: String::new(),
        })
        .expect("leaderboard")
    {
        Reply::LeaderboardPage { page } => page.rows,
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn versions_have_distinct_ratings_profiles_history_and_survive_restart() {
    let mut env = start_wide(true);
    let mut first = env.bot("engine");
    let mut second = identity(&env, "engine", "v2 (sample)");
    let mut opponent = env.human("opponent");
    assert_ne!(
        first.me.id, second.me.id,
        "each version needs its own profile and history identity"
    );
    assert_eq!(first.me.main_account, second.me.main_account);
    let ranked = RulesetId("ranked".into());
    pair_up(&mut first, &mut opponent, &ranked);
    let result = first.wait_result();
    opponent.wait_result();
    let first_result = result
        .seats
        .iter()
        .find(|seat| seat.account == first.me.id)
        .expect("first version");
    assert_eq!(
        first_result.before,
        Some(Rating {
            rating: 1500.0,
            deviation: 350.0
        })
    );
    let first_after = first_result.after.expect("rated");
    let first_finished = result.finished_at;
    pair_up(&mut second, &mut opponent, &ranked);
    let result = second.wait_result();
    opponent.wait_result();
    assert_eq!(
        result
            .seats
            .iter()
            .find(|seat| seat.account == second.me.id)
            .expect("second version")
            .before,
        first_result.before
    );
    // The ladder grows a deviation with the time since a player last played: read it as of the
    // moment the first version's game ended, so the shown value is the stored one.
    env.service().freeze_now(first_finished);
    let rows = board(&mut opponent);
    assert_eq!(rows.len(), 2);
    let row = rows
        .iter()
        .find(|row| row.account == first.me.id)
        .expect("first row");
    assert_eq!(row.rating, first_after);
    assert_eq!(row.games, 1);
    assert_eq!(row.version.as_deref(), Some("test-1"));
    assert!(
        rows.iter()
            .all(|row| row.nickname == "engine" && row.rating.deviation < 350.0)
    );
    for account in [&mut first, &mut second] {
        match account
            .ask(ClientMsg::RecentMatches {
                account: account.me.id,
                limit: 10,
            })
            .expect("history")
        {
            Reply::Matches { list } => assert_eq!(list.len(), 1),
            other => panic!("unexpected {other:?}"),
        }
    }
    let id = second.me.id;
    drop((first, second, opponent));
    env.restart();
    env.service().freeze_now(first_finished);
    let mut selected = identity(&env, "engine", "v2 (sample)");
    assert_eq!(selected.me.id, id);
    assert_eq!(board(&mut selected), rows);
}

#[test]
fn versions_are_mandatory_for_bots_and_forbidden_for_humans() {
    let env = start(vec![ruleset("ranked", true, "team0", "0")]);
    let _bot = env.bot("engine");
    let _human = env.human("person");
    for (login, info, error, message) in [
        (
            "engine",
            ClientInfo::default(),
            Error::VersionRequired,
            "a bot account must select a version",
        ),
        (
            "person",
            version("v1"),
            Error::VersionNotAllowed,
            "human accounts cannot select a version",
        ),
    ] {
        let mut client = env.connect();
        match client
            .login(login, PASSWORD, info)
            .expect_err("must refuse")
        {
            ClientError::Refused(actual) => {
                assert_eq!(actual, error);
                assert_eq!(actual.to_string(), message);
            }
            other => panic!("unexpected {other}"),
        }
    }
}

#[test]
fn versions_reject_invalid_names_and_bound_creation_without_blocking_existing_versions() {
    let env = start(vec![ruleset("ranked", true, "team0", "0")]);
    let mut first = env.bot("engine");
    for name in [
        String::new(),
        "x".repeat(MAX_VERSION_LEN + 1),
        "a\nb".into(),
        " a".into(),
        "test-1 ".into(),
        "\u{7f}".into(),
        "\u{200b}".into(),
    ] {
        let mut client = env.connect();
        assert!(
            matches!(
                client.login("engine", PASSWORD, version(&name)),
                Err(ClientError::Refused(Error::BadVersion))
            ),
            "invalid {name:?}"
        );
    }
    let token = first.token;
    // Reuse one authenticated token; every resume states the selected version explicitly.
    for index in 1..MAX_VERSIONS_PER_ACCOUNT {
        let mut client = env.connect();
        client
            .resume(token, version(&format!("v{index}")))
            .expect("new version");
    }
    let mut client = env.connect();
    assert!(matches!(
        client.resume(token, version("overflow")),
        Err(ClientError::Refused(Error::VersionLimit))
    ));
    let selected = client
        .resume(token, version("test-1"))
        .expect("existing version at limit");
    assert_eq!(selected.me.id, first.me.id);
    assert!(
        board(&mut first).is_empty(),
        "credential account never has a ladder row"
    );
}

#[test]
fn versions_of_one_owner_and_repeated_searches_never_match_in_ranked_or_unranked_queues() {
    for rated in [true, false] {
        let env = start_wide(rated);
        let mut first = env.bot("engine");
        let mut second = identity(&env, "engine", "v2");
        let mut third = env.human("third");
        let mode = RulesetId("ranked".into());
        first.queue(&mode);
        second.queue(&mode);
        assert_waiting(&env, &first.token, "test-1", 1);
        assert_waiting(&env, &second.token, "v2", 1);
        third.queue(&mode);
        let proposal = first.wait_proposal();
        let other = third.wait_proposal();
        assert_eq!(proposal.proposal, other.proposal);
        first.expect_done(ClientMsg::MatchAccept {
            proposal: proposal.proposal,
        });
        third.expect_done(ClientMsg::MatchAccept {
            proposal: other.proposal,
        });
        let found = first.wait_match_found();
        third.wait_match_found();
        assert!(found.seats.iter().all(|seat| seat.account != second.me.id));
        first.wait_result();
        third.wait_result();
        // A fresh third-party search also releases the still-waiting second version.
        third.queue(&mode);
        second.accept();
        third.accept();
        let found = second.wait_match_found();
        third.wait_match_found();
        assert!(found.seats.iter().any(|seat| seat.account == second.me.id));
        second.wait_result();
        third.wait_result();
        let mut same_version = identity(&env, "engine", "test-1");
        first.queue(&mode);
        same_version.queue(&mode);
        assert_waiting(&env, &first.token, "test-1", 2);
        third.queue(&mode);
        first.accept();
        third.accept();
        let found = first.wait_match_found();
        third.wait_match_found();
        assert_eq!(
            found
                .seats
                .iter()
                .filter(|seat| seat.account == first.me.id)
                .count(),
            1
        );
    }
}

#[test]
fn versions_share_owner_activity_limits_and_password_controls() {
    let env = start_configured(vec![ruleset("ranked", true, "team0", "0")], |config| {
        config.limits.games_per_account.bot = 2
    });
    let mut first = env.bot("engine");
    let mut second = identity(&env, "engine", "v2");
    let mode = RulesetId("ranked".into());
    first.queue(&mode);
    second.queue(&mode);
    assert_eq!(
        first.ask(ClientMsg::QueueJoin {
            ruleset: mode.clone()
        }),
        Err(Error::TooManyGames { limit: 2 })
    );
    first.expect_done(ClientMsg::ChangeNickname {
        nickname: "renamed".into(),
    });
    let mut reader = env.human("reader");
    let Reply::Account { account } = reader
        .ask(ClientMsg::Account {
            account: second.me.id,
        })
        .expect("profile")
    else {
        panic!("profile")
    };
    assert_eq!(account.nickname, "renamed");
    assert_eq!(account.version.as_deref(), Some("v2"));
    let old = second.token;
    first.expect_done(ClientMsg::ChangePassword {
        current: Secret(PASSWORD.into()),
        new: Secret("another long password".into()),
    });
    let error = second.wait_for(10, |event| match event {
        Event::Bye { error } => Some(error),
        _ => None,
    });
    assert_eq!(error, Error::BadSession);
    let mut client = env.connect();
    assert!(matches!(
        client.resume(old, version("v2")),
        Err(ClientError::Refused(Error::BadSession))
    ));
    let selected = client
        .resume(first.token, version("v2"))
        .expect("kept token still authenticates the owner");
    assert_eq!(selected.me.id, second.me.id);
    assert_eq!(selected.me.login, "engine");
    assert_eq!(selected.me.nickname, "renamed");
}
