//! Every message either side decodes: no input panics the service or a
//! client, and whatever decodes encodes back to the same message.
#![no_main]

use libfuzzer_sys::fuzz_target;
use radovid_client::Control;
use sha2::{Digest,Sha256};
use radovid_proto::{ClientFrame, LEN_PREFIX, ServerMsg, decode_payload, encode_frame_to_vec};

/// Whatever decodes encodes, and decodes again to what encodes the same
/// (bytes, not values: a float may be NaN).
fn round_trip<T>(data: &[u8])
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    if let Ok(msg) = decode_payload::<T>(data) {
        let frame = encode_frame_to_vec(&msg).expect("a decoded message encodes");
        let again = decode_payload::<T>(&frame[LEN_PREFIX..]).expect("and decodes");
        assert_eq!(frame, encode_frame_to_vec(&again).expect("and encodes"));
    }
}

fuzz_target!(|data: &[u8]| {
    let _=radovid::db::decode_pending_result(data);
    if data.len()<=radovid_proto::MAX_GAME_REPORT_LEN+4096 {
        let mut digest=Sha256::new();digest.update(0u64.to_le_bytes());digest.update(data);
        let mut record=b"RDVRES1\0".to_vec();record.extend_from_slice(&0u64.to_le_bytes());record.extend_from_slice(&digest.finalize());record.extend_from_slice(data);
        let _=radovid::db::decode_pending_result(&record);
    }
    round_trip::<ClientFrame>(data);
    round_trip::<ServerMsg>(data);
    let mut control = Control::new();
    let _ = control.receive(data);
    let mut framed = (data.len() as u32).to_le_bytes().to_vec();
    framed.extend_from_slice(data);
    let _ = control.receive(&framed);
});
