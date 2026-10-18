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

mod control;
mod error;
#[cfg(feature = "native")]
mod native;
#[cfg(all(feature = "web", target_arch = "wasm32"))]
mod web;

pub use control::{Control, Incoming, Outgoing, control_url};
pub use error::ClientError;
#[cfg(feature = "native")]
pub use native::{Background, Client, LoggedIn, TlsConfig};
pub use radovid_proto as proto;
#[cfg(all(feature = "web", target_arch = "wasm32"))]
pub use web::WebClient;
