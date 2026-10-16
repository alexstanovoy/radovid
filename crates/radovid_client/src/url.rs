//! Connecting a WebSocket over TLS to a `wss://` URL.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Connector, WebSocket};

use crate::{ClientError, TlsConfig};

pub(crate) type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_ADDRESSES: usize = 8;

/// Opens `url` (which must be `wss://`), with a read and write timeout on the socket.
pub(crate) fn connect(url: &str, tls: &TlsConfig, max_message: usize, io_timeout: Duration) -> Result<Ws, ClientError> {
    let request = url.into_client_request().map_err(|_| ClientError::BadUrl(url.to_owned()))?;
    let uri = request.uri();
    if uri.scheme_str() != Some("wss") {
        return Err(ClientError::BadUrl(url.to_owned()));
    }
    let host = uri.host().ok_or_else(|| ClientError::BadUrl(url.to_owned()))?.to_owned();
    let port = uri.port_u16().unwrap_or(443);
    let addresses = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| ClientError::Io(e.to_string()))?;
    let mut last = ClientError::Io("the host resolves to nothing".to_owned());
    let mut connected = None;
    for address in addresses.take(MAX_ADDRESSES) {
        match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
            Ok(stream) => {
                connected = Some(stream);
                break;
            }
            Err(e) => last = ClientError::Io(e.to_string()),
        }
    }
    let stream = connected.ok_or(last)?;
    let _ = stream.set_nodelay(true);
    stream.set_read_timeout(Some(io_timeout)).map_err(|e| ClientError::Io(e.to_string()))?;
    stream.set_write_timeout(Some(io_timeout)).map_err(|e| ClientError::Io(e.to_string()))?;
    let config = WebSocketConfig::default()
        .max_message_size(Some(max_message))
        .max_frame_size(Some(max_message));
    let (ws, _) = tungstenite::client_tls_with_config(
        request,
        stream,
        Some(config),
        Some(Connector::Rustls(tls.config.clone())),
    )
    .map_err(|e| ClientError::Io(e.to_string()))?;
    Ok(ws)
}

/// The socket under a connection, to set its timeouts.
pub(crate) fn socket(ws: &Ws) -> Option<&TcpStream> {
    match ws.get_ref() {
        MaybeTlsStream::Plain(stream) => Some(stream),
        MaybeTlsStream::Rustls(stream) => Some(stream.get_ref()),
        _ => None,
    }
}
