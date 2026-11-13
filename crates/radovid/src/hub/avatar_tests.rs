use super::matching_tests::Fixture;
use super::*;

#[tokio::test]
async fn avatar_upload_get_and_remove_use_authenticated_account_and_shared_read_quota() {
    let mut fixture = Fixture::new();
    let (account, session) = fixture.account("alpha").await;
    let (other, second) = fixture.account("beta").await;
    fixture.hub.lookup_clock = Some(std::time::Instant::now());
    let png = crate::avatar::tests::image(32, 32);
    let Reply::Avatar {
        account: owner,
        avatar: Some(avatar),
    } = fixture
        .hub
        .request(session, ClientMsg::SetAvatar { png: png.clone() })
        .await
        .expect("upload")
    else {
        panic!("avatar reply");
    };
    assert_eq!(owner, account);
    assert_eq!(avatar.png, png);
    assert_eq!(
        fixture
            .hub
            .request(second, ClientMsg::GetAvatar { account })
            .await,
        Ok(Reply::Avatar {
            account,
            avatar: Some(avatar.clone())
        })
    );
    assert_eq!(
        fixture
            .hub
            .request(session, ClientMsg::GetAvatar { account: other })
            .await,
        Ok(Reply::Avatar {
            account: other,
            avatar: None
        })
    );
    assert!(matches!(
        fixture.hub.request(session, ClientMsg::RemoveAvatar).await,
        Err(Error::Throttled { .. })
    ));
    // A new account's read burst is independent of uploads and other accounts.
    for _ in 0..3 {
        fixture
            .hub
            .request(second, ClientMsg::GetAvatar { account })
            .await
            .expect("remaining reads");
    }
    assert_eq!(
        fixture
            .hub
            .request(second, ClientMsg::GetAvatar { account })
            .await,
        Err(Error::Throttled {
            retry_after_ms: 250
        })
    );
}

async fn avatar_page(database: &crate::db::Db) -> Vec<AvatarQuery> {
    let png = crate::avatar::tests::image(32, 32);
    let mut queries = Vec::new();
    for index in 0..100 {
        let account = database
            .create_account(
                format!("row{index}"),
                "hash".into(),
                AccountKind::Human,
                None,
            )
            .await
            .expect("account");
        database
            .change_avatar(account, Some(png.clone()), UnixMs(1))
            .await
            .expect("storage")
            .expect("avatar");
        queries.push(AvatarQuery {
            account,
            version: None,
        });
    }
    queries
}

#[tokio::test]
async fn one_batch_fetches_a_hundred_avatars_for_one_lookup_token_and_honors_cached_versions() {
    let mut fixture = Fixture::new();
    let (_viewer, session) = fixture.account("viewer").await;
    fixture.hub.lookup_clock = Some(std::time::Instant::now());
    let accounts = avatar_page(&fixture.hub.db).await;
    let first = accounts[0].account;
    let Reply::Avatars { changes } = fixture
        .hub
        .request(session, ClientMsg::GetAvatars { accounts })
        .await
        .expect("batch")
    else {
        panic!("batch reply");
    };
    assert_eq!(changes.len(), 100);
    let accounts = changes
        .iter()
        .map(|change| AvatarQuery {
            account: change.account,
            version: Some(change.avatar.as_ref().expect("uncached image").version),
        })
        .collect();
    assert_eq!(
        fixture
            .hub
            .request(session, ClientMsg::GetAvatars { accounts })
            .await,
        Ok(Reply::Avatars {
            changes: Vec::new()
        })
    );
    for _ in 0..2 {
        fixture
            .hub
            .request(session, ClientMsg::Account { account: first })
            .await
            .expect("remaining lookup tokens");
    }
    assert_eq!(
        fixture
            .hub
            .request(session, ClientMsg::Account { account: first })
            .await,
        Err(Error::Throttled {
            retry_after_ms: 250
        })
    );
}

#[tokio::test]
async fn invalid_avatar_batches_are_refused_before_charging_lookup_quota() {
    let mut fixture = Fixture::new();
    let (account, session) = fixture.account("owner").await;
    fixture.hub.lookup_clock = Some(std::time::Instant::now());
    let query = AvatarQuery {
        account,
        version: None,
    };
    for accounts in [
        vec![],
        vec![query; 2],
        vec![query; 101],
        vec![AvatarQuery {
            account: AccountId(0),
            version: None,
        }],
    ] {
        assert_eq!(
            fixture
                .hub
                .request(session, ClientMsg::GetAvatars { accounts })
                .await,
            Err(Error::BadAvatarBatch)
        );
    }
    for _ in 0..4 {
        fixture
            .hub
            .request(session, ClientMsg::Account { account })
            .await
            .expect("invalid batches do not spend quota");
    }
}

#[tokio::test]
async fn another_players_recent_matches_share_the_profile_read_quota() {
    let mut fixture = Fixture::new();
    let (owner, _) = fixture.account("owner").await;
    let (_, viewer) = fixture.account("viewer").await;
    fixture.hub.lookup_clock = Some(std::time::Instant::now());
    for _ in 0..4 {
        assert_eq!(
            fixture
                .hub
                .request(
                    viewer,
                    ClientMsg::RecentMatches {
                        account: owner,
                        limit: 255
                    }
                )
                .await,
            Ok(Reply::Matches { list: Vec::new() })
        );
    }
    assert_eq!(
        fixture
            .hub
            .request(
                viewer,
                ClientMsg::RecentMatches {
                    account: owner,
                    limit: 255
                }
            )
            .await,
        Err(Error::Throttled {
            retry_after_ms: 250
        })
    );
}

#[tokio::test]
async fn another_players_public_match_list_omits_the_participants_opaque_report() {
    let mut fixture = Fixture::new();
    let (owner, owner_session) = fixture.account("owner").await;
    let (_, viewer) = fixture.account("viewer").await;
    let seats = vec![crate::db::NewSeat {
        account: owner,
        slot: 0,
        team: 0,
    }];
    let match_id = fixture
        .hub
        .db
        .create_match(RulesetId("casual".into()), false, seats.clone())
        .await
        .expect("match");
    fixture
        .hub
        .db
        .finish_match(crate::db::Finish {
            match_id,
            ruleset: RulesetId("casual".into()),
            rate: false,
            outcome: Outcome::Winner { team: 0 },
            reason: EndReason::Decided,
            duration_ms: 1000,
            report: vec![7, 8, 9],
            seats,
            finished_at: UnixMs(1000),
        })
        .await
        .expect("finish");
    let Reply::Matches { list } = fixture
        .hub
        .request(
            viewer,
            ClientMsg::RecentMatches {
                account: owner,
                limit: 255,
            },
        )
        .await
        .expect("public match list")
    else {
        panic!("match list");
    };
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].match_id, match_id);
    assert_eq!(list[0].seats[0].account, owner);
    assert!(
        list[0].game_report.is_empty(),
        "a public profile exposes the match list, not an opaque participant report"
    );
    let Reply::Matches { list } = fixture
        .hub
        .request(
            owner_session,
            ClientMsg::RecentMatches {
                account: owner,
                limit: 50,
            },
        )
        .await
        .expect("participant history")
    else {
        panic!("match list");
    };
    assert_eq!(
        list[0].game_report,
        vec![7, 8, 9],
        "the participant keeps their own report"
    );
}
