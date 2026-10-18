//! What can go wrong on the client's side of a connection.

use radovid_proto::Error;

#[derive(Clone, Debug, PartialEq)]
pub enum ClientError {
    /// The URL is not a `wss://host[:port][/control]` one.
    BadUrl(String),
    /// The socket, TLS or WebSocket failed.
    Io(String),
    /// The peer sent something that is no message of the protocol.
    Protocol(String),
    /// The service refused the request.
    Refused(Error),
    /// The connection ended.
    Closed,
    /// Nothing arrived in time.
    Timeout,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::BadUrl(url) => write!(f, "bad url {url}"),
            ClientError::Io(why) => write!(f, "connection: {why}"),
            ClientError::Protocol(why) => write!(f, "protocol: {why}"),
            ClientError::Refused(error) => write!(f, "refused: {error}"),
            ClientError::Closed => write!(f, "the connection ended"),
            ClientError::Timeout => write!(f, "timed out"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<ClientError> for std::io::Error {
    fn from(error: ClientError) -> Self {
        std::io::Error::other(error.to_string())
    }
}

impl From<std::io::Error> for ClientError {
    fn from(error: std::io::Error) -> Self {
        ClientError::Io(error.to_string())
    }
}
