//! Frames inside WebSocket messages.

use crate::LEN_PREFIX;

/// The payload of a binary message that holds exactly one frame (`u32 LE length
/// | payload`) whose payload is at most `max` bytes.
pub fn whole_frame(bytes: &[u8], max: usize) -> Option<&[u8]> {
    let payload = bytes.get(LEN_PREFIX..)?;
    let declared = u32::from_le_bytes(bytes.get(..LEN_PREFIX)?.try_into().ok()?) as usize;
    (declared == payload.len() && payload.len() <= max).then_some(payload)
}
