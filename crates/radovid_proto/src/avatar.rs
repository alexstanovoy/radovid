//! Bounded PNG framing shared by service validation and client decoder admission.
use crate::{AVATAR_SIDE, MAX_AVATAR_BYTES};

/// Checks framing and the fixed image shape, returning bounded IDAT bytes and channels.
/// Full pixel, zlib and checksum validation belongs to the service decoder.
pub fn avatar_image_data(bytes: &[u8]) -> Option<(Vec<u8>, u64)> {
    if bytes.len() > MAX_AVATAR_BYTES || bytes.get(..8)? != b"\x89PNG\r\n\x1a\n" { return None; }
    let mut offset = 8;
    let mut channels = None;
    let mut compressed = Vec::new();
    let (mut palette, mut transparency, mut data) = (false, false, false);
    while offset < bytes.len() {
        let length = u32::from_be_bytes(bytes.get(offset..offset+4)?.try_into().ok()?) as usize;
        let kind = bytes.get(offset+4..offset+8)?;
        let end = offset.checked_add(12)?.checked_add(length)?;
        let payload = bytes.get(offset+8..end.checked_sub(4)?)?;
        bytes.get(..end)?;
        match kind {
            b"IHDR" if offset == 8 && length == 13 => {
                if payload[..4] != AVATAR_SIDE.to_be_bytes() || payload[4..8] != AVATAR_SIDE.to_be_bytes()
                    || payload[8] != 8 || payload[10..13] != [0,0,0] { return None; }
                channels = Some(match payload[9] { 0|3 => 1, 2 => 3, 4 => 2, 6 => 4, _ => return None });
            }
            b"PLTE" if channels.is_some() && !palette && !transparency && !data => { palette = true; }
            b"tRNS" if channels.is_some() && !transparency && !data => { transparency = true; }
            b"IDAT" if channels.is_some() => { data = true; compressed.extend_from_slice(payload); }
            b"IEND" if data && length == 0 && end == bytes.len() => return Some((compressed, channels?)),
            _ => return None,
        }
        offset = end;
    }
    None
}
