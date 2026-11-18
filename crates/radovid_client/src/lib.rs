//! A client library for radovid: log in, queue (several entries at once),
//! answer proposals, sit in lobbies and receive the handoffs of matches. What
//! a client does with a handoff (connect to the game server with its ticket)
//! is the game's business.
//!
//! [`Control`] is the protocol without I/O: it numbers requests, encodes
//! them and decodes what the service sends. Two backends carry it:
//! - `native` (default): [`Client`], blocking, for bots and tools, and
//!   [`Background`], a thread with channels, for a GUI's frame loop;
//! - `web` (wasm32): [`WebClient`] over the browser's WebSocket.
//!
//! [`ServiceCheck`] asks a service, before anyone signs in to it, whether it is a radovid this
//! client can use (on both backends).

mod check;
mod control;
mod error;
#[cfg(feature = "native")]
mod native;
#[cfg(all(feature = "web", target_arch = "wasm32"))]
mod web;

pub use check::{
    MAX_ANSWER_BYTES, MAX_CHECKS_AT_ONCE, MAX_NAME_CHARS, ServiceFault, ServiceFound, judge_message,
};
pub use control::{Control, Incoming, Outgoing, control_url};
pub use error::ClientError;
#[cfg(feature = "native")]
pub use native::{Background, Client, LoggedIn, ServiceCheck, TlsConfig};
pub use radovid_proto as proto;
pub use radovid_proto::{matchmaker_identity, same_matchmaker, shareable_matchmaker_identity};
#[cfg(all(feature = "web", target_arch = "wasm32"))]
pub use web::{ServiceCheck, WebClient};
