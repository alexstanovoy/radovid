use crate::*;

#[test]
fn a_profile_cannot_make_a_client_cache_more_than_64_mode_ratings() {
    let rating = RatingSummary {
        rating: Rating {
            rating: 1500.0,
            deviation: 350.0,
        },
        games: 1,
        wins: 1,
        draws: 0,
        losses: 0,
        rank: Some(1),
    };
    let account = PublicAccount {
        avatar: None,
        id: AccountId(1),
        nickname: "alpha".into(),
        kind: AccountKind::Human,
        online: false,
        bot_version: None,
        ratings: (0..=MAX_RATED_MODES)
            .map(|index| (RulesetId(format!("mode_{index}")), rating))
            .collect(),
    };
    let frame = encode_frame_to_vec(&ServerMsg::Reply {
        seq: 1,
        reply: Reply::Account { account },
    })
    .expect("fixture");
    let decoded =
        decode_payload::<ServerMsg>(whole_frame(&frame, MAX_SERVER_PAYLOAD_LEN).expect("frame"));
    assert_eq!(
        decoded.err(),
        Some(CodecError::Malformed),
        "profile mode count must be bounded before caching"
    );
}

#[test]
fn a_leaderboard_reply_is_bounded_before_a_client_can_cache_it() {
    let row = LadderRow {
        avatar: None,
        rank: 1,
        account: AccountId(1),
        nickname: "alpha".into(),
        kind: AccountKind::Human,
        bot_version: None,
        rating: Rating {
            rating: 1500.0,
            deviation: 350.0,
        },
        games: 1,
        wins: 1,
        draws: 0,
        losses: 0,
        last_played: Some(UnixMs(0)),
    };
    for size in [
        usize::from(MAX_LEADERBOARD_ROWS),
        usize::from(MAX_LEADERBOARD_ROWS) + 1,
    ] {
        let page = LeaderboardPage {
            ruleset: RulesetId("duel".into()),
            kind: AccountKind::Human,
            prefix: String::new(),
            limit: MAX_LEADERBOARD_ROWS,
            total: size as u32,
            rows: vec![row.clone(); size],
            own: None,
        };
        let frame = encode_frame_to_vec(&ServerMsg::Reply {
            seq: 1,
            reply: Reply::LeaderboardPage { page },
        })
        .expect("encode fixture");
        let decoded = decode_payload::<ServerMsg>(
            whole_frame(&frame, MAX_SERVER_PAYLOAD_LEN).expect("frame"),
        );
        if size > usize::from(MAX_LEADERBOARD_ROWS) {
            assert_eq!(
                decoded.err(),
                Some(CodecError::Malformed),
                "oversized boards must be refused before caching"
            );
        } else {
            let ServerMsg::Reply {
                reply: Reply::LeaderboardPage { page },
                ..
            } = decoded.expect("maximum valid board")
            else {
                panic!("reply");
            };
            assert_eq!(page.rows.len(), size);
        }
    }
}
