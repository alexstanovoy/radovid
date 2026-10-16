//! A client library for radovid: log in, queue or join a lobby, and play the
//! match through the service's game path.
//!
//! Everything is synchronous (plain threads, `tungstenite` over rustls), like
//! `bota_client_core`, whose `Transport` the [`GameLink`] implements.

mod control;
mod error;
mod game;
mod tls;
mod url;

pub use bota_client_core::{ClientSession, Seated};
pub use control::{Client, LoggedIn};
pub use error::ClientError;
pub use game::{GameLink, play, spectate, take_seat};
pub use radovid_proto as proto;
pub use tls::TlsConfig;
