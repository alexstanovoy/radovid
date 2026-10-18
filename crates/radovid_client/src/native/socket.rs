//! Opening a WebSocket over TLS to a `wss://` URL.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use radovid_proto::{LEN_PREFIX, MAX_SERVER_PAYLOAD_LEN};
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Connector, Message, WebSocket};

use crate::{ClientError, TlsConfig};

pub(crate) type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Opens `url`, which must be `wss://`, with `io_timeout` on every read and
/// write of the socket.
pub(crate) fn connect(url: &str, tls: &TlsConfig, io_timeout: Duration) -> Result<Ws, ClientError> {
    let bad = || ClientError::BadUrl(url.to_owned());
    let request = url.into_client_request().map_err(|_| bad())?;
    let uri = request.uri();
    if uri.scheme_str() != Some("wss") {
        return Err(bad());
    }
    let host = uri
        .host()
        .ok_or_else(bad)?
        .trim_matches(['[', ']'])
        .to_owned();
    let port = uri.port_u16().unwrap_or(443);
    let stream = open_tcp(&host, port)?;
    stream.set_read_timeout(Some(io_timeout))?;
    stream.set_write_timeout(Some(io_timeout))?;
    stream.set_nodelay(true)?;
    let max = MAX_SERVER_PAYLOAD_LEN + LEN_PREFIX;
    let config = WebSocketConfig::default()
        .max_message_size(Some(max))
        .max_frame_size(Some(max));
    let (ws, _) = tungstenite::client_tls_with_config(
        request,
        stream,
        Some(config),
        Some(Connector::Rustls(tls.config.clone())),
    )
    .map_err(|e| ClientError::Io(e.to_string()))?;
    Ok(ws)
}

/// The first address of `host` that answers within the connect timeout.
fn open_tcp(host: &str, port: u16) -> Result<TcpStream, ClientError> {
    let mut last = ClientError::Io(format!("{host} resolves to no address"));
    for addr in (host, port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(error) => last = error.into(),
        }
    }
    Err(last)
}

/// Sets how long the next read may wait.
pub(crate) fn set_read_timeout(ws: &Ws, wait: Duration) -> Result<(), ClientError> {
    let wait = Some(wait.max(Duration::from_millis(1)));
    match ws.get_ref() {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(wait)?,
        MaybeTlsStream::Rustls(stream) => stream.get_ref().set_read_timeout(wait)?,
        _ => {}
    }
    Ok(())
}

/// One binary message, or none when the read timeout ran out first.
pub(crate) fn read_binary(ws: &mut Ws) -> Result<Option<Vec<u8>>, ClientError> {
    loop {
        match ws.read() {
            Ok(Message::Binary(bytes)) => return Ok(Some(bytes.into())),
            Ok(Message::Close(_)) => return Err(ClientError::Closed),
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None);
            }
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                return Err(ClientError::Closed);
            }
            Err(e) => return Err(ClientError::Io(e.to_string())),
        }
    }
}

pub(crate) fn send_binary(ws: &mut Ws, bytes: Vec<u8>) -> Result<(), ClientError> {
    ws.send(Message::Binary(bytes.into())).map_err(|e| match e {
        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => {
            ClientError::Closed
        }
        other => ClientError::Io(other.to_string()),
    })
}
