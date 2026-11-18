use super::matching_tests::Fixture;
use super::*;
use crate::rating::RatingShare;

#[tokio::test]
async fn avatar_upload_get_and_remove_use_authenticated_account_and_shared_read_quota() {
    let mut fixture = Fixture::new();
    let (account, session) = fixture.account("alpha").await;
    let (other, second) = fixture.account("beta").await;
    fixture.hub.lookup_clock = Some(std::time::Instant::now());
    let png = crate::avatar::tests::avatar();
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

/// A picture whose avatar is nearly as large as the format allows: noise cannot be compressed.
fn heavy_avatar() -> &'static [u8] {
    static IMAGE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    IMAGE.get_or_init(|| {
        let mut state = 1u64;
        let rgba: Vec<u8> = (0..256 * 256 * 4)
            .map(|index| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                if index % 4 == 3 {
                    255
                } else {
                    (state >> 56) as u8
                }
            })
            .collect();
        let png =
            radovid_proto::encode_avatar_rgba(&rgba).expect("noise fits after reducing colours");
        assert!(
            png.len() > MAX_AVATAR_BYTES / 2,
            "the fixture is heavy: {}",
            png.len()
        );
        png
    })
}

async fn avatar_page(database: &crate::db::Db, png: &[u8]) -> Vec<AvatarQuery> {
    let mut queries = Vec::new();
    for index in 0..100 {
        let account = database
            .create_account(format!("row{index}"), "hash".into(), AccountKind::Human)
            .await
            .expect("account");
        database.put_avatar_bytes(account, png).await;
        queries.push(AvatarQuery {
            account,
            version: None,
        });
    }
    queries
}

#[tokio::test]
async fn one_batch_of_cached_versions_costs_one_lookup_token_and_no_image_bytes() {
    let mut fixture = Fixture::new();
    let (_viewer, session) = fixture.account("viewer").await;
    fixture.hub.lookup_clock = Some(std::time::Instant::now());
    let accounts = avatar_page(&fixture.hub.db, &crate::avatar::tests::avatar()).await;
    let first = accounts[0].account;
    let Reply::Avatars { changes, answered } = fixture
        .hub
        .request(session, ClientMsg::GetAvatars { accounts })
        .await
        .expect("batch")
    else {
        panic!("batch reply");
    };
    assert_eq!((changes.len(), answered), (100, 100));
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
            changes: Vec::new(),
            answered: 100
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

/// Asks until every query is answered, waiting out the throttle on the lookup clock; returns the
/// replies' image bytes, one entry a reply, and every account's image.
async fn fetch_everything(
    fixture: &mut Fixture,
    session: SessionId,
    mut queries: Vec<AvatarQuery>,
) -> (Vec<usize>, Vec<AvatarChange>) {
    let (mut sizes, mut all) = (Vec::new(), Vec::new());
    for _ in 0..200 {
        if queries.is_empty() {
            return (sizes, all);
        }
        match fixture
            .hub
            .request(
                session,
                ClientMsg::GetAvatars {
                    accounts: queries.clone(),
                },
            )
            .await
        {
            Ok(Reply::Avatars { changes, answered }) => {
                assert!(answered >= 1, "a reply makes progress");
                sizes.push(
                    changes
                        .iter()
                        .filter_map(|change| change.avatar.as_ref())
                        .map(|avatar| avatar.png.len())
                        .sum(),
                );
                all.extend(changes);
                queries.drain(..usize::from(answered));
            }
            Err(Error::Throttled { retry_after_ms }) => {
                let now = fixture.hub.lookup_clock.expect("virtual clock");
                fixture.hub.lookup_clock =
                    Some(now + std::time::Duration::from_millis(u64::from(retry_after_ms)));
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    panic!("the batch was not answered in 200 asks");
}

#[tokio::test]
async fn a_batch_of_heavy_avatars_is_split_by_the_reply_byte_budget_and_returns_everything() {
    let mut fixture = Fixture::new();
    let (_viewer, session) = fixture.account("viewer").await;
    fixture.hub.lookup_clock = Some(std::time::Instant::now());
    let accounts = avatar_page(&fixture.hub.db, heavy_avatar()).await;
    let (sizes, all) = fetch_everything(&mut fixture, session, accounts.clone()).await;
    assert!(
        sizes.iter().all(|bytes| *bytes <= MAX_AVATAR_REPLY_BYTES),
        "{sizes:?}"
    );
    assert!(
        sizes.len() >= 100 * heavy_avatar().len() / MAX_AVATAR_REPLY_BYTES,
        "{sizes:?}"
    );
    let got: Vec<AccountId> = all.iter().map(|change| change.account).collect();
    let wanted: Vec<AccountId> = accounts.iter().map(|query| query.account).collect();
    assert_eq!(got, wanted, "every account once, in the order asked");
    assert!(all.iter().all(|change| {
        change
            .avatar
            .as_ref()
            .is_some_and(|a| a.png == heavy_avatar())
    }));
}

#[tokio::test]
async fn images_sent_are_charged_to_the_account_and_the_next_avatar_read_waits_for_the_debt() {
    let mut fixture = Fixture::new();
    let (_viewer, session) = fixture.account("viewer").await;
    let start = std::time::Instant::now();
    fixture.hub.lookup_clock = Some(start);
    let accounts = avatar_page(&fixture.hub.db, heavy_avatar()).await;
    let image = heavy_avatar().len();
    let rate = AVATAR_BYTES_PER_SEC as usize;
    // A new ask every quarter second keeps the lookup tokens out of the way; each reply carries
    // about five images, more than a quarter second repays, so the debt rises.
    let (mut sent, mut elapsed_ms, mut next) = (0usize, 0usize, 0usize);
    while sent.saturating_sub(rate * elapsed_ms / 1000) < AVATAR_BYTES_BURST as usize {
        let Reply::Avatars { changes, answered } = fixture
            .hub
            .request(
                session,
                ClientMsg::GetAvatars {
                    accounts: accounts[next..].to_vec(),
                },
            )
            .await
            .expect("admitted while the debt is under the burst")
        else {
            panic!("batch reply");
        };
        sent += changes.len() * image;
        next += usize::from(answered);
        elapsed_ms += 250;
        fixture.hub.lookup_clock =
            Some(start + std::time::Duration::from_millis(elapsed_ms as u64));
    }
    let owed = sent - rate * elapsed_ms / 1000;
    let rest = accounts[next..].to_vec();
    let Err(Error::Throttled { retry_after_ms }) = fixture
        .hub
        .request(
            session,
            ClientMsg::GetAvatars {
                accounts: rest.clone(),
            },
        )
        .await
    else {
        panic!("the debt is past the burst: {owed} owed");
    };
    let expected = (owed - AVATAR_BYTES_BURST as usize) * 1000 / rate;
    assert!(
        retry_after_ms as usize >= expected && retry_after_ms as usize <= expected + 250,
        "{retry_after_ms} vs {expected}"
    );
    assert!(
        fixture
            .hub
            .request(
                session,
                ClientMsg::Account {
                    account: accounts[0].account
                }
            )
            .await
            .is_ok(),
        "other lookups do not wait for avatar bytes"
    );
    let now = fixture.hub.lookup_clock.expect("clock");
    fixture.hub.lookup_clock =
        Some(now + std::time::Duration::from_millis(u64::from(retry_after_ms)));
    assert!(matches!(
        fixture
            .hub
            .request(session, ClientMsg::GetAvatars { accounts: rest })
            .await,
        Ok(Reply::Avatars { .. })
    ));
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
            bot_share: RatingShare::FULL,
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

#[tokio::test]
async fn public_match_history_follows_ban_hidden_and_deleted_profile_visibility() {
    let mut fixture = Fixture::new();
    let (hidden, _) = fixture.account("hidden").await;
    let (other, _) = fixture.account("other").await;
    let (_, viewer) = fixture.account("viewer").await;
    let seats = vec![
        crate::db::NewSeat {
            account: hidden,
            slot: 0,
            team: 0,
        },
        crate::db::NewSeat {
            account: other,
            slot: 1,
            team: 1,
        },
    ];
    let game = fixture
        .hub
        .db
        .create_match(RulesetId("ranked".into()), true, seats.clone())
        .await
        .expect("match");
    fixture
        .hub
        .db
        .finish_match(crate::db::Finish {
            match_id: game,
            ruleset: RulesetId("ranked".into()),
            rate: true,
            outcome: Outcome::Winner { team: 0 },
            reason: EndReason::Decided,
            duration_ms: 1000,
            report: vec![7, 8, 9],
            seats,
            finished_at: UnixMs(1000),
            bot_share: RatingShare::FULL,
        })
        .await
        .expect("finish");
    fixture
        .hub
        .db
        .set_ban(
            hidden,
            Some(crate::db::Ban {
                reason: "fixture".into(),
                until: None,
            }),
        )
        .await
        .expect("ban");
    assert_eq!(
        fixture
            .hub
            .request(viewer, ClientMsg::Account { account: hidden })
            .await,
        Err(Error::UnknownAccount)
    );
    assert_eq!(
        fixture
            .hub
            .request(
                viewer,
                ClientMsg::RecentMatches {
                    account: hidden,
                    limit: 50
                }
            )
            .await,
        Err(Error::UnknownAccount),
        "a banned profile must not be discoverable through another endpoint"
    );
    let Reply::Matches { list } = fixture
        .hub
        .request(
            viewer,
            ClientMsg::RecentMatches {
                account: other,
                limit: 50,
            },
        )
        .await
        .expect("other public history")
    else {
        panic!("list")
    };
    let seat = &list[0].seats[0];
    assert_eq!(seat.account, AccountId(0));
    assert!(seat.nickname.is_empty());
    assert!(seat.before.is_none());
    assert!(seat.after.is_none());
    fixture.hub.db.delete_account(hidden).await.expect("delete");
    // A different requester keeps the visibility checks independent of quota exhaustion.
    let (_, fresh) = fixture.account("fresh").await;
    assert_eq!(
        fixture
            .hub
            .request(fresh, ClientMsg::Account { account: hidden })
            .await,
        Err(Error::UnknownAccount)
    );
    assert_eq!(
        fixture
            .hub
            .request(
                fresh,
                ClientMsg::RecentMatches {
                    account: hidden,
                    limit: 50
                }
            )
            .await,
        Err(Error::UnknownAccount)
    );
    let Reply::Matches { list } = fixture
        .hub
        .request(
            fresh,
            ClientMsg::RecentMatches {
                account: other,
                limit: 50,
            },
        )
        .await
        .expect("retained match")
    else {
        panic!("list")
    };
    assert_eq!(list[0].seats[0].account, AccountId(0));
    assert!(list[0].seats[0].nickname.is_empty());
    assert!(list[0].seats[0].before.is_none());
    assert!(list[0].seats[0].after.is_none());
}

#[tokio::test]
async fn public_match_reads_and_profile_avatar_reads_spend_one_mixed_quota() {
    let mut fixture = Fixture::new();
    let (target, _) = fixture.account("target").await;
    let (_, viewer) = fixture.account("viewer").await;
    fixture.hub.lookup_clock = Some(std::time::Instant::now());
    for message in [
        ClientMsg::Account { account: target },
        ClientMsg::RecentMatches {
            account: target,
            limit: 255,
        },
        ClientMsg::GetAvatar { account: target },
        ClientMsg::RecentMatches {
            account: target,
            limit: 1,
        },
    ] {
        fixture
            .hub
            .request(viewer, message)
            .await
            .expect("four mixed reads");
    }
    assert_eq!(
        fixture
            .hub
            .request(viewer, ClientMsg::Account { account: target })
            .await,
        Err(Error::Throttled {
            retry_after_ms: 250
        })
    );
}

#[tokio::test]
async fn own_match_history_spends_the_same_lookup_quota() {
    let mut fixture = Fixture::new();
    let (account, session) = fixture.account("owner").await;
    fixture.hub.lookup_clock = Some(std::time::Instant::now());
    for _ in 0..4 {
        assert_eq!(
            fixture
                .hub
                .request(
                    session,
                    ClientMsg::RecentMatches {
                        account,
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
                session,
                ClientMsg::RecentMatches {
                    account,
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
async fn own_match_history_masks_a_banned_opponent_but_keeps_own_seat_and_report() {
    let mut fixture = Fixture::new();
    let (banned, _) = fixture.account("banned").await;
    let (me, session) = fixture.account("me").await;
    let seats = vec![
        crate::db::NewSeat {
            account: banned,
            slot: 0,
            team: 0,
        },
        crate::db::NewSeat {
            account: me,
            slot: 1,
            team: 1,
        },
    ];
    let game = fixture
        .hub
        .db
        .create_match(RulesetId("ranked".into()), true, seats.clone())
        .await
        .expect("match");
    fixture
        .hub
        .db
        .finish_match(crate::db::Finish {
            match_id: game,
            ruleset: RulesetId("ranked".into()),
            rate: true,
            outcome: Outcome::Winner { team: 1 },
            reason: EndReason::Decided,
            duration_ms: 1000,
            report: vec![7, 8, 9],
            seats,
            finished_at: UnixMs(1000),
            bot_share: RatingShare::FULL,
        })
        .await
        .expect("finish");
    fixture
        .hub
        .db
        .set_ban(
            banned,
            Some(crate::db::Ban {
                reason: "fixture".into(),
                until: None,
            }),
        )
        .await
        .expect("ban");
    let Reply::Matches { list } = fixture
        .hub
        .request(
            session,
            ClientMsg::RecentMatches {
                account: me,
                limit: 50,
            },
        )
        .await
        .expect("own history")
    else {
        panic!("list")
    };
    let [first, second] = &list[0].seats[..] else {
        panic!("two seats")
    };
    assert_eq!(first.account, AccountId(0));
    assert!(first.nickname.is_empty());
    assert!(first.before.is_none());
    assert!(first.after.is_none());
    assert_eq!(second.account, me);
    assert!(second.before.is_some());
    assert!(second.after.is_some());
    assert_eq!(list[0].game_report, vec![7, 8, 9]);
}

#[tokio::test]
async fn public_history_is_capped_at_fifty_matches_for_a_limit_of_255() {
    let mut fixture = Fixture::new();
    let (first, _) = fixture.account("first").await;
    let (second, viewer) = fixture.account("viewer").await;
    for _ in 0..57 {
        let seats = vec![
            crate::db::NewSeat {
                account: first,
                slot: 0,
                team: 0,
            },
            crate::db::NewSeat {
                account: second,
                slot: 1,
                team: 1,
            },
        ];
        let id = fixture
            .hub
            .db
            .create_match(RulesetId("ranked".into()), false, seats.clone())
            .await
            .expect("create");
        fixture
            .hub
            .db
            .finish_match(crate::db::Finish {
                match_id: id,
                ruleset: RulesetId("ranked".into()),
                rate: false,
                outcome: Outcome::Draw,
                reason: EndReason::Decided,
                duration_ms: 1000,
                report: vec![1],
                seats,
                finished_at: UnixMs(1000),
                bot_share: RatingShare::FULL,
            })
            .await
            .expect("finish");
    }
    let Reply::Matches { list } = fixture
        .hub
        .request(
            viewer,
            ClientMsg::RecentMatches {
                account: first,
                limit: 255,
            },
        )
        .await
        .expect("public list")
    else {
        panic!("matches")
    };
    assert_eq!(list.len(), 50);
    assert!(list.iter().all(|result| result.game_report.is_empty()));
}
