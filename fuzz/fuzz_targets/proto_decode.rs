//! Every message either side decodes: no input panics the service or a
//! client, and whatever decodes encodes back to the same message.
#![no_main]

use libfuzzer_sys::fuzz_target;
use radovid_client::Control;
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
    round_trip::<ClientFrame>(data);
    round_trip::<ServerMsg>(data);
    let mut control = Control::new();
    let _ = control.receive(data);
    let mut framed = (data.len() as u32).to_le_bytes().to_vec();
    framed.extend_from_slice(data);
    let _ = control.receive(&framed);
});
