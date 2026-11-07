use super::*;
use super::matching_tests::Fixture;

#[tokio::test]
async fn avatar_upload_get_and_remove_use_authenticated_account_and_shared_read_quota() {
    let mut fixture = Fixture::new();
    let (account,session) = fixture.account("alpha").await;
    let (other,second) = fixture.account("beta").await;
    fixture.hub.lookup_clock=Some(std::time::Instant::now());
    let png = crate::avatar::tests::image(32,32);
    let Reply::Avatar { account: owner, avatar: Some(avatar) } = fixture.hub.request(session,ClientMsg::SetAvatar{png:png.clone()}).await.expect("upload") else { panic!("avatar reply"); };
    assert_eq!(owner,account);
    assert_eq!(avatar.png,png);
    assert_eq!(fixture.hub.request(second,ClientMsg::GetAvatar{account}).await,Ok(Reply::Avatar{account,avatar:Some(avatar.clone())}));
    assert_eq!(fixture.hub.request(session,ClientMsg::GetAvatar{account:other}).await,Ok(Reply::Avatar{account:other,avatar:None}));
    assert!(matches!(fixture.hub.request(session,ClientMsg::RemoveAvatar).await,Err(Error::Throttled{..})));
    // A new account's read burst is independent of uploads and other accounts.
    for _ in 0..3 { fixture.hub.request(second,ClientMsg::GetAvatar{account}).await.expect("remaining reads"); }
    assert_eq!(fixture.hub.request(second,ClientMsg::GetAvatar{account}).await,Err(Error::Throttled{retry_after_ms:250}));
}
