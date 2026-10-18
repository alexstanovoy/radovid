//! A control connection on a thread of its own, for a caller that must never
//! block (a GUI's frame loop): requests go out through a channel, and what
//! arrives is polled.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError};
use std::time::Duration;

use radovid_proto::ClientMsg;

use super::socket::{Ws, connect, read_binary, send_binary, set_read_timeout};
use crate::{ClientError, Control, Incoming, TlsConfig, control_url};

/// How long the thread waits for a message before it looks for requests.
const POLL: Duration = Duration::from_millis(20);
/// How long a write may block.
const IO_TIMEOUT: Duration = Duration::from_secs(30);
/// Messages held for a caller that has not polled them; past it the thread
/// waits, and the service in time drops the connection.
const MAX_UNPOLLED: usize = 1024;
/// Bytes of messages held for such a caller at most, past which the thread
/// waits as well: a service's message may be 4 MiB, and a thousand of them
/// would be gigabytes.
const MAX_UNPOLLED_BYTES: usize = 16 * 1024 * 1024;

/// What the connection thread reports.
enum Report {
    Message(Vec<u8>),
    Ended(ClientError),
}

/// A control connection whose calls never block. Dropping it hangs up the
/// request channel, which ends the thread within one poll (or once a
/// connection attempt under way gives up).
pub struct Background {
    control: Control,
    requests: Sender<Vec<u8>>,
    reports: Receiver<Report>,
    /// Bytes of the messages reported and not yet polled.
    unpolled: Arc<AtomicUsize>,
    ended: Option<ClientError>,
}

impl Background {
    /// Starts connecting to `url` (`wss://host:port`, with or without
    /// `/control`) and queues the Hello; its Welcome arrives as the reply to
    /// sequence number 1.
    pub fn open(
        url: &str,
        tls: &TlsConfig,
        game_fingerprint: u64,
    ) -> Result<Background, ClientError> {
        let mut control = Control::new();
        let hello = control.hello(game_fingerprint)?;
        let (requests, outgoing) = mpsc::channel();
        let (report, reports) = mpsc::sync_channel(MAX_UNPOLLED);
        requests
            .send(hello.bytes)
            .map_err(|_| ClientError::Closed)?;
        let url = control_url(url);
        let tls = tls.clone();
        let unpolled = Arc::new(AtomicUsize::new(0));
        let held = Arc::clone(&unpolled);
        std::thread::Builder::new()
            .name("radovid-control".to_owned())
            .spawn(move || {
                let end = match connect(&url, &tls, IO_TIMEOUT) {
                    Ok(ws) => pump(ws, &outgoing, &report, &held),
                    Err(error) => error,
                };
                let _ = report.send(Report::Ended(end));
            })?;
        Ok(Background {
            control,
            requests,
            reports,
            unpolled,
            ended: None,
        })
    }

    /// Queues a request; its answer carries the sequence number returned.
    pub fn request(&mut self, msg: ClientMsg) -> Result<u32, ClientError> {
        if let Some(error) = &self.ended {
            return Err(error.clone());
        }
        let out = self.control.request(msg)?;
        self.requests
            .send(out.bytes)
            .map_err(|_| ClientError::Closed)?;
        Ok(out.seq)
    }

    /// The next message that arrived, if any. Once the connection has ended,
    /// every call returns why.
    pub fn poll(&mut self) -> Result<Option<Incoming>, ClientError> {
        if let Some(error) = &self.ended {
            return Err(error.clone());
        }
        match self.reports.try_recv() {
            Ok(Report::Message(bytes)) => {
                self.unpolled.fetch_sub(bytes.len(), Ordering::Relaxed);
                self.control.receive(&bytes).map(Some)
            }
            Ok(Report::Ended(error)) => {
                self.ended = Some(error.clone());
                Err(error)
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                self.ended = Some(ClientError::Closed);
                Err(ClientError::Closed)
            }
        }
    }

    /// What the service knows about itself and the account.
    pub fn control(&self) -> &Control {
        &self.control
    }
}

/// Carries messages both ways until either side ends; returns why it ended.
fn pump(
    mut ws: Ws,
    outgoing: &Receiver<Vec<u8>>,
    report: &SyncSender<Report>,
    unpolled: &AtomicUsize,
) -> ClientError {
    if let Err(error) = set_read_timeout(&ws, POLL) {
        return error;
    }
    loop {
        loop {
            match outgoing.try_recv() {
                Ok(bytes) => {
                    if let Err(error) = send_binary(&mut ws, bytes) {
                        return error;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    return ClientError::Closed;
                }
            }
        }
        if unpolled.load(Ordering::Relaxed) >= MAX_UNPOLLED_BYTES {
            std::thread::sleep(POLL);
            continue;
        }
        match read_binary(&mut ws) {
            Ok(Some(bytes)) => {
                unpolled.fetch_add(bytes.len(), Ordering::Relaxed);
                if report.send(Report::Message(bytes)).is_err() {
                    return ClientError::Closed;
                }
            }
            Ok(None) => {}
            Err(error) => return error,
        }
    }
}
