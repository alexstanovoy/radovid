//! The wire protocol of radovid: what a client and the service say to each
//! other on `/control`, plus the limits both sides hold.
//!
//! radovid knows no game. A ruleset names its game and the fingerprint of the
//! game's own wire; a match is handed to its players as a URL and a ticket
//! (`Handoff`), and its result comes back as the game's opaque report.
//!
//! A message travels as one binary WebSocket message holding one frame
//! (`u32 LE length | postcard payload`). The crate does no I/O and builds for
//! `wasm32` as it is.

mod error;
mod frame;
mod ids;
mod limits;
mod message;
mod model;
mod validate;

#[cfg(test)]
mod tests;

pub use error::*;
pub use frame::*;
pub use ids::*;
pub use limits::*;
pub use message::*;
pub use model::*;
pub use validate::*;
