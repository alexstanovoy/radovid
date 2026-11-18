//! The avatar codec both the service and every client run on bytes a stranger chose: whatever is
//! no avatar is refused, whatever is one decodes to exactly the avatar's pixels and encodes to
//! one stable byte sequence within the limit, and no input is slow or large.
#![no_main]

use libfuzzer_sys::fuzz_target;
use radovid_proto::{
    AVATAR_SIDE, MAX_AVATAR_BYTES, canonical_avatar, decode_avatar, encode_avatar_rgba,
};

const PIXELS: usize = (AVATAR_SIDE as usize) * (AVATAR_SIDE as usize);

fuzz_target!(|data: &[u8]| {
    if let Ok(image) = decode_avatar(data) {
        let rgba = image.to_rgba();
        assert_eq!(rgba.len(), PIXELS * 4);
        let canonical = canonical_avatar(data).expect("what decodes has a canonical form");
        assert!(canonical.len() <= MAX_AVATAR_BYTES);
        assert_eq!(
            canonical_avatar(&canonical).expect("canonical bytes decode"),
            canonical,
            "canonical bytes are stable"
        );
        assert_eq!(
            decode_avatar(&canonical).expect("canonical bytes decode").to_rgba(),
            rgba,
            "the canonical form shows the same picture"
        );
    }
    // Short inputs repeat into flat pictures, which encode in milliseconds.
    if (4..=256).contains(&data.len()) {
        let rgba: Vec<u8> = data.iter().copied().cycle().take(PIXELS * 4).collect();
        if let Ok(png) = encode_avatar_rgba(&rgba) {
            assert!(png.len() <= MAX_AVATAR_BYTES);
            assert_eq!(
                canonical_avatar(&png).expect("an encoded avatar is canonical"),
                png
            );
        }
    }
});
