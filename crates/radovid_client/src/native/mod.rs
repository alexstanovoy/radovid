//! The native backends: tungstenite over rustls on a plain socket.

mod background;
mod blocking;
mod check;
mod socket;
mod tls;

pub use background::Background;
pub use blocking::{Client, LoggedIn};
pub use check::ServiceCheck;
pub use tls::TlsConfig;
