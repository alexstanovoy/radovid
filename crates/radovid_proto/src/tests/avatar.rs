use crate::*;

#[test]
fn the_batch_reply_with_its_answered_count_requires_protocol_25() {
    // A reply that says how many queries it answers cannot be read by an older client, so the
    // floor was raised to the protocol that appended the count.
    assert_eq!(MIN_PROTO_VERSION, 25);
    assert_eq!(PROTO_VERSION, 25);
}

#[test]
fn the_avatar_limits_fit_the_frames_they_travel_in() {
    // An upload is one control frame; a batch reply is one server frame holding up to the budget.
    const { assert!(MAX_AVATAR_BYTES + 1024 <= MAX_CONTROL_PAYLOAD_LEN) };
    const { assert!(MAX_AVATAR_BYTES <= MAX_AVATAR_REPLY_BYTES) };
    const { assert!(MAX_AVATAR_REPLY_BYTES + 100 * 64 <= MAX_SERVER_PAYLOAD_LEN) };
    const { assert!(AVATAR_BYTES_BURST as usize >= MAX_AVATAR_REPLY_BYTES) };
}

#[test]
fn the_fullest_batch_reply_encodes_within_the_server_frame_and_decodes_back() {
    let full = MAX_AVATAR_REPLY_BYTES / MAX_AVATAR_BYTES;
    let reply = ServerMsg::Reply {
        seq: 1,
        reply: Reply::Avatars {
            changes: (1..=full as u64)
                .map(|account| AvatarChange {
                    account: AccountId(account),
                    avatar: Some(Avatar {
                        version: AvatarVersion([account as u8; 32]),
                        png: vec![7; MAX_AVATAR_BYTES],
                    }),
                })
                .collect(),
            answered: MAX_AVATAR_BATCH as u8,
        },
    };
    let encoded = encode_frame_to_vec(&reply).expect("fullest reply");
    assert!(encoded.len() <= LEN_PREFIX + MAX_SERVER_PAYLOAD_LEN);
    let payload = whole_frame(&encoded, MAX_SERVER_PAYLOAD_LEN).expect("frame");
    assert_eq!(decode_payload::<ServerMsg>(payload), Ok(reply));
}

#[test]
fn avatar_upload_is_bounded_before_allocation() {
    for size in [MAX_AVATAR_BYTES, MAX_AVATAR_BYTES + 1] {
        let upload = ClientFrame {
            seq: 1,
            msg: ClientMsg::SetAvatar { png: vec![0; size] },
        };
        let encoded = encode_frame_to_vec(&upload).expect("upload");
        let result = decode_payload::<ClientFrame>(
            whole_frame(&encoded, MAX_CONTROL_PAYLOAD_LEN).expect("frame"),
        );
        assert_eq!(result.is_ok(), size <= MAX_AVATAR_BYTES);
    }
}

#[test]
fn avatar_download_is_bounded_before_allocation() {
    for size in [MAX_AVATAR_BYTES, MAX_AVATAR_BYTES + 1] {
        let download = ServerMsg::Reply {
            seq: 1,
            reply: Reply::Avatar {
                account: AccountId(1),
                avatar: Some(Avatar {
                    version: AvatarVersion([0; 32]),
                    png: vec![0; size],
                }),
            },
        };
        let encoded = encode_frame_to_vec(&download).expect("download");
        let result = decode_payload::<ServerMsg>(
            whole_frame(&encoded, MAX_SERVER_PAYLOAD_LEN).expect("frame"),
        );
        assert_eq!(result.is_ok(), size <= MAX_AVATAR_BYTES);
    }
}

#[test]
fn avatar_batches_bound_both_vectors_and_nested_images_before_allocation() {
    for size in [MAX_AVATAR_BATCH, MAX_AVATAR_BATCH + 1] {
        let request = ClientFrame {
            seq: 1,
            msg: ClientMsg::GetAvatars {
                accounts: vec![
                    AvatarQuery {
                        account: AccountId(1),
                        version: None
                    };
                    size
                ],
            },
        };
        let frame = encode_frame_to_vec(&request).expect("wire");
        assert_eq!(
            decode_payload::<ClientFrame>(
                whole_frame(&frame, MAX_CONTROL_PAYLOAD_LEN).expect("frame")
            )
            .is_ok(),
            size <= MAX_AVATAR_BATCH
        );
        let response = ServerMsg::Reply {
            seq: 1,
            reply: Reply::Avatars {
                changes: vec![
                    AvatarChange {
                        account: AccountId(1),
                        avatar: None
                    };
                    size
                ],
                answered: 1,
            },
        };
        let frame = encode_frame_to_vec(&response).expect("wire");
        assert_eq!(
            decode_payload::<ServerMsg>(
                whole_frame(&frame, MAX_SERVER_PAYLOAD_LEN).expect("frame")
            )
            .is_ok(),
            size <= MAX_AVATAR_BATCH
        );
    }
    let response = ServerMsg::Reply {
        seq: 1,
        reply: Reply::Avatars {
            changes: vec![AvatarChange {
                account: AccountId(1),
                avatar: Some(Avatar {
                    version: AvatarVersion([0; 32]),
                    png: vec![0; MAX_AVATAR_BYTES + 1],
                }),
            }],
            answered: 1,
        },
    };
    let frame = encode_frame_to_vec(&response).expect("wire");
    assert!(
        decode_payload::<ServerMsg>(whole_frame(&frame, MAX_SERVER_PAYLOAD_LEN).expect("frame"))
            .is_err()
    );
}
