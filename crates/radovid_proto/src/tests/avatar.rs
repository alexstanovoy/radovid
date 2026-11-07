use crate::*;

#[test]
fn nested_avatar_fields_require_protocol_20() {
    assert_eq!(MIN_PROTO_VERSION,20);
    assert_eq!(PROTO_VERSION,21);
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
    for size in [MAX_AVATAR_BATCH,MAX_AVATAR_BATCH+1] {
        let request=ClientFrame{seq:1,msg:ClientMsg::GetAvatars{accounts:vec![AvatarQuery{account:AccountId(1),version:None};size]}};
        let frame=encode_frame_to_vec(&request).expect("wire");
        assert_eq!(decode_payload::<ClientFrame>(whole_frame(&frame,MAX_CONTROL_PAYLOAD_LEN).expect("frame")).is_ok(),size<=MAX_AVATAR_BATCH);
        let response=ServerMsg::Reply{seq:1,reply:Reply::Avatars{changes:vec![AvatarChange{account:AccountId(1),avatar:None};size]}};
        let frame=encode_frame_to_vec(&response).expect("wire");
        assert_eq!(decode_payload::<ServerMsg>(whole_frame(&frame,MAX_SERVER_PAYLOAD_LEN).expect("frame")).is_ok(),size<=MAX_AVATAR_BATCH);
    }
    let response=ServerMsg::Reply{seq:1,reply:Reply::Avatars{changes:vec![AvatarChange{account:AccountId(1),avatar:Some(Avatar{version:AvatarVersion([0;32]),png:vec![0;MAX_AVATAR_BYTES+1]})}]}};
    let frame=encode_frame_to_vec(&response).expect("wire");
    assert!(decode_payload::<ServerMsg>(whole_frame(&frame,MAX_SERVER_PAYLOAD_LEN).expect("frame")).is_err());
}
