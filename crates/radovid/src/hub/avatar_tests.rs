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

#[tokio::test]
async fn one_batch_fetches_a_hundred_avatars_for_one_lookup_token_and_honors_cached_versions() {
    let mut fixture=Fixture::new();
    let (_viewer,session)=fixture.account("viewer").await;
    fixture.hub.lookup_clock=Some(std::time::Instant::now());
    let png=crate::avatar::tests::image(32,32);
    let mut accounts=Vec::new();
    for index in 0..100 {
        let id=fixture.hub.db.create_account(format!("row{index}"),"hash".into(),AccountKind::Human,None).await.expect("account");
        fixture.hub.db.change_avatar(id,Some(png.clone()),UnixMs(1)).await.expect("storage").expect("avatar");
        accounts.push((id,None::<AvatarVersion>));
    }
    #[derive(serde::Serialize)]
    struct Batch {tag:u32,accounts:Vec<(AccountId,Option<AvatarVersion>)>}
    let bytes=postcard::to_allocvec(&Batch{tag:45,accounts:accounts.clone()}).expect("future batch wire");
    let request=radovid_proto::decode_payload::<ClientMsg>(&bytes).expect("bounded batch request");
    let reply=fixture.hub.request(session,request).await.expect("batch");
    #[derive(serde::Deserialize)]
    struct Images {tag:u32,changes:Vec<(AccountId,Option<Avatar>)>}
    let response:Images=postcard::from_bytes(&postcard::to_allocvec(&reply).expect("wire")).expect("batch reply");
    assert_eq!(response.tag,18);
    assert_eq!(response.changes.len(),100);
    let cached:Vec<_>=response.changes.iter().map(|(id,image)|(*id,image.as_ref().map(|image|image.version))).collect();
    assert!(response.changes.iter().all(|(_,image)|image.is_some()));
    let bytes=postcard::to_allocvec(&Batch{tag:45,accounts:cached}).expect("cached wire");
    let request=radovid_proto::decode_payload::<ClientMsg>(&bytes).expect("batch");
    let reply=fixture.hub.request(session,request).await.expect("cached batch");
    let response:Images=postcard::from_bytes(&postcard::to_allocvec(&reply).expect("wire")).expect("cached reply");
    assert!(response.changes.is_empty(),"unchanged images cost no response bytes");
    for _ in 0..2 {fixture.hub.request(session,ClientMsg::Account{account:accounts[0].0}).await.expect("remaining lookup tokens");}
    assert_eq!(fixture.hub.request(session,ClientMsg::Account{account:accounts[0].0}).await,Err(Error::Throttled{retry_after_ms:250}));
}

#[tokio::test]
async fn invalid_avatar_batches_are_refused_before_charging_lookup_quota() {
    let mut fixture=Fixture::new();
    let (account,session)=fixture.account("owner").await;
    fixture.hub.lookup_clock=Some(std::time::Instant::now());
    let query=AvatarQuery{account,version:None};
    for accounts in [vec![],vec![query;2],vec![query;101],vec![AvatarQuery{account:AccountId(0),version:None}]] {
        assert_eq!(fixture.hub.request(session,ClientMsg::GetAvatars{accounts}).await,Err(Error::BadAvatarBatch));
    }
    for _ in 0..4 {fixture.hub.request(session,ClientMsg::Account{account}).await.expect("invalid batches do not spend quota");}
}
