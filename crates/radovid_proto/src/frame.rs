//! Frames: a little endian `u32` length followed by that many bytes of
//! postcard payload, one frame per WebSocket message.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::MAX_SERVER_PAYLOAD_LEN;

/// Bytes in the length prefix.
pub const LEN_PREFIX: usize = 4;

/// Something went wrong turning bytes into a message or back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecError {
    /// A message would produce a payload above [`MAX_SERVER_PAYLOAD_LEN`].
    TooLarge { len: usize },
    /// The payload did not decode into the expected message.
    Malformed,
}

impl core::fmt::Display for CodecError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CodecError::TooLarge { len } => {
                write!(
                    f,
                    "a payload of {len} bytes exceeds {MAX_SERVER_PAYLOAD_LEN}"
                )
            }
            CodecError::Malformed => write!(f, "a payload did not decode"),
        }
    }
}

impl std::error::Error for CodecError {}

impl From<postcard::Error> for CodecError {
    fn from(_: postcard::Error) -> Self {
        CodecError::Malformed
    }
}

/// Encodes one message and appends the whole frame to `out`.
pub fn encode_frame<T: Serialize + ?Sized>(msg: &T, out: &mut Vec<u8>) -> Result<(), CodecError> {
    let payload = postcard::to_allocvec(msg)?;
    if payload.len() > MAX_SERVER_PAYLOAD_LEN {
        return Err(CodecError::TooLarge { len: payload.len() });
    }
    out.reserve(LEN_PREFIX + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(())
}

/// Encodes one message into a frame of its own.
pub fn encode_frame_to_vec<T: Serialize + ?Sized>(msg: &T) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::new();
    encode_frame(msg, &mut out)?;
    Ok(out)
}

/// Decodes one message from a frame payload, the length prefix removed.
pub fn decode_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T, CodecError> {
    Ok(postcard::from_bytes(payload)?)
}

/// The payload of a binary message that holds exactly one frame whose payload
/// is at most `max` bytes.
pub fn whole_frame(bytes: &[u8], max: usize) -> Option<&[u8]> {
    let payload = bytes.get(LEN_PREFIX..)?;
    let declared = u32::from_le_bytes(bytes.get(..LEN_PREFIX)?.try_into().ok()?) as usize;
    (declared == payload.len() && payload.len() <= max).then_some(payload)
}
