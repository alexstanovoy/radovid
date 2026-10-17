//! The wire protocol of radovid: what a client and the service say to each
//! other on `/control`, plus the limits both sides hold.
//!
//! A message travels as one binary WebSocket message holding one frame as
//! `bota_proto` defines it (`u32 LE length | postcard payload`), so one codec
//! serves the control path, the game path and replay files. The crate does no
//! I/O and builds for `wasm32` as it is.

mod error;
mod frame;
mod ids;
mod limits;
mod message;
mod model;
mod validate;

#[cfg(test)]
mod tests;

pub use bota_proto::{
    CodecError, FrameReader, HeroId, LEN_PREFIX, MatchStats, SeatKey, SlotId, Team, decode_payload,
    encode_frame, encode_frame_to_vec,
};
pub use error::*;
pub use frame::whole_frame;
pub use ids::*;
pub use limits::*;
pub use message::*;
pub use model::*;
pub use validate::*;
