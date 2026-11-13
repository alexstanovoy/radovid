mod common;
use common::*;
use radovid_client::proto::*;

struct World {
    env: Env,
    alice: Account,
    bob: Account,
    root: Account,
}
fn world(rated: bool) -> World {
    let env = start_configured(
        vec![ruleset("solo", rated, "team0", "0.02")],
        listen_on_both_families,
    );
    env.account("root_user", AccountKind::Human);
    env.grant_admin("root_user");
    let mut alice = env.human("alice");
    let mut bob = env.human("bob");
    pair_up(&mut alice, &mut bob, &solo());
    alice.wait_result();
    bob.wait_result();
    let root = env.login("root_user");
    World {
        env,
        alice,
        bob,
        root,
    }
}
fn hide(env: &Env, id: AccountId) {
    rusqlite::Connection::open(env.db_path())
        .expect("database")
        .execute(
            "UPDATE ratings SET hidden=1 WHERE account=?1",
            [id.0 as i64],
        )
        .expect("hide rating");
}
fn assert_no_peer_identity(world: &World, peer: AccountId, name: &str) {
    let mut observer = world.env.human("observer");
    let Reply::Matches { list } = observer
        .ask(ClientMsg::RecentMatches {
            account: world.alice.me.id,
            limit: 50,
        })
        .expect("public list")
    else {
        panic!("matches")
    };
    assert_eq!(list.len(), 1);
    assert!(list[0].game_report.is_empty());
    assert!(
        list[0]
            .seats
            .iter()
            .any(|seat| seat.account == world.alice.me.id),
        "visible seat is retained"
    );
    assert!(
        !list[0]
            .seats
            .iter()
            .any(|seat| seat.account == peer || seat.nickname == name),
        "invisible identity leaked: {:?}",
        list[0].seats
    );
    let anonymous = list[0]
        .seats
        .iter()
        .find(|seat| seat.account == AccountId(0))
        .expect("unbound seat remains");
    assert!(anonymous.nickname.is_empty());
    assert!(anonymous.before.is_none());
    assert!(anonymous.after.is_none());
}
#[test]
fn banned_players_have_no_identity_inside_another_players_public_list() {
    let mut w = world(true);
    let bob = w.bob.me.id;
    w.root.expect_done(ClientMsg::AdminBan {
        account: bob,
        reason: "fixture".into(),
        until: None,
    });
    assert_no_peer_identity(&w, bob, "bob");
}
#[test]
fn hidden_players_have_no_identity_inside_another_players_public_list() {
    let w = world(true);
    let bob = w.bob.me.id;
    hide(&w.env, bob);
    assert_no_peer_identity(&w, bob, "bob");
}
#[test]
fn a_banned_player_without_any_ratings_is_not_public() {
    let mut w = world(true);
    let bob = w.bob.me.id;
    rusqlite::Connection::open(w.env.db_path())
        .expect("database")
        .execute("DELETE FROM ratings WHERE account=?1", [bob.0 as i64])
        .expect("remove rating rows");
    let count: i64 = rusqlite::Connection::open(w.env.db_path())
        .expect("database")
        .query_row(
            "SELECT COUNT(*) FROM ratings WHERE account=?1",
            [bob.0 as i64],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(count, 0, "this case must exercise the no-rating branch");
    w.root.expect_done(ClientMsg::AdminBan {
        account: bob,
        reason: "fixture".into(),
        until: None,
    });
    let mut observer = w.env.human("ban_observer");
    assert_eq!(
        observer.ask(ClientMsg::Account { account: bob }),
        Err(Error::UnknownAccount)
    );
    assert_eq!(
        observer.ask(ClientMsg::RecentMatches {
            account: bob,
            limit: 50
        }),
        Err(Error::UnknownAccount)
    );
    assert_no_peer_identity(&w, bob, "bob");
}
#[test]
fn a_deleted_only_account_has_no_public_profile_history_or_peer_identity() {
    let mut w = world(true);
    let bob = w.bob.me.id;
    w.bob.expect_done(ClientMsg::DeleteAccount {
        password: Secret::new(PASSWORD),
    });
    let mut observer = w.env.human("deleted_observer");
    assert_eq!(
        observer.ask(ClientMsg::Account { account: bob }),
        Err(Error::UnknownAccount)
    );
    assert_eq!(
        observer.ask(ClientMsg::RecentMatches {
            account: bob,
            limit: 50
        }),
        Err(Error::UnknownAccount)
    );
    assert_no_peer_identity(&w, bob, "bob");
}
#[test]
fn avatar_reads_do_not_distinguish_hidden_banned_deleted_and_nonexistent_accounts() {
    let mut w = world(true);
    hide(&w.env, w.bob.me.id);
    let banned = w.env.human("banned");
    w.root.expect_done(ClientMsg::AdminBan {
        account: banned.me.id,
        reason: "fixture".into(),
        until: None,
    });
    let mut deleted = w.env.human("deleted");
    let deleted_id = deleted.me.id;
    deleted.expect_done(ClientMsg::DeleteAccount {
        password: Secret::new(PASSWORD),
    });
    for (index, target) in [w.bob.me.id, banned.me.id, deleted_id, AccountId(999_999)]
        .into_iter()
        .enumerate()
    {
        let mut observer = w.env.human(&format!("avatar_observer{index}"));
        assert_eq!(
            observer.ask(ClientMsg::GetAvatar { account: target }),
            Err(Error::UnknownAccount)
        );
        assert_eq!(
            observer.ask(ClientMsg::GetAvatars {
                accounts: vec![AvatarQuery {
                    account: target,
                    version: None
                }]
            }),
            Ok(Reply::Avatars {
                changes: Vec::new()
            })
        );
        assert_eq!(
            observer.ask(ClientMsg::GetAvatars {
                accounts: vec![AvatarQuery {
                    account: target,
                    version: Some(AvatarVersion([7; 32]))
                }]
            }),
            Ok(Reply::Avatars {
                changes: vec![AvatarChange {
                    account: target,
                    avatar: None
                }]
            })
        );
    }
}

#[test]
fn a_launch_omits_hidden_account_bindings_but_keeps_the_seat() {
    let mut rule = ruleset("solo", true, "draw", "0.02");
    rule.launcher.args.push("{accounts}".into());
    let env = start_configured(vec![rule], listen_on_both_families);
    let mut alice = env.human("alice");
    let mut bob = env.human("bob");
    pair_up(&mut alice, &mut bob, &solo());
    let first = alice.wait_result();
    bob.wait_result();
    let original = String::from_utf8(first.game_report).expect("first report");
    let bindings = original.splitn(4, '|').nth(3).expect("bindings");
    assert!(
        bindings.split(',').any(|id| id == bob.me.id.0.to_string()),
        "visible profiles are published"
    );
    hide(&env, bob.me.id);
    let (handoff, _) = pair_up(&mut alice, &mut bob, &solo());
    let result = alice.wait_result();
    bob.wait_result();
    let report = String::from_utf8(result.game_report).expect("report");
    let published: Vec<_> = report
        .splitn(4, '|')
        .nth(3)
        .expect("account words")
        .split(',')
        .collect();
    assert_eq!(published.len(), 2, "the hidden seat remains");
    for seat in handoff.seats {
        assert_eq!(
            published[usize::from(seat.slot)],
            if seat.account == bob.me.id {
                "-".into()
            } else {
                seat.account.0.to_string()
            }
        );
    }
}
