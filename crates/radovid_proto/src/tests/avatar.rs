use crate::*;

#[test]
fn nested_avatar_fields_require_protocol_20() {
    assert_eq!((PROTO_VERSION,MIN_PROTO_VERSION),(20,20));
}

#[test]
fn avatar_upload_and_download_are_bounded_before_allocation() {
    for size in [MAX_AVATAR_BYTES, MAX_AVATAR_BYTES+1] {
        let upload = ClientFrame { seq: 1, msg: ClientMsg::SetAvatar { png: vec![0;size] } };
        let encoded = encode_frame_to_vec(&upload).expect("upload");
        let result = decode_payload::<ClientFrame>(whole_frame(&encoded, MAX_CONTROL_PAYLOAD_LEN).expect("frame"));
        assert_eq!(result.is_ok(), size <= MAX_AVATAR_BYTES);
        let download = ServerMsg::Reply { seq:1, reply: Reply::Avatar { account:AccountId(1), avatar:Some(Avatar {version:AvatarVersion([0;32]),png:vec![0;size]}) } };
        let encoded = encode_frame_to_vec(&download).expect("download");
        let result = decode_payload::<ServerMsg>(whole_frame(&encoded,MAX_SERVER_PAYLOAD_LEN).expect("frame"));
        assert_eq!(result.is_ok(), size <= MAX_AVATAR_BYTES);
    }
}
