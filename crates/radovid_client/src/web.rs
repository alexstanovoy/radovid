//! A control connection over the browser's WebSocket (wasm32). The browser
//! does TLS and the WebSocket framing; nothing here blocks: requests are sent
//! (or held until the socket opens) and what arrives is polled.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::task::Waker;

use radovid_proto::ClientMsg;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{BinaryType, CloseEvent, Event, MessageEvent, WebSocket};

use crate::check::{MAX_ANSWER_BYTES, judge};
use crate::{ClientError, Control, Incoming, ServiceFault, ServiceFound, control_url};

/// Messages held for a caller that has not polled them; past it the
/// connection is closed.
const MAX_UNPOLLED: usize = 1024;

/// What the socket's callbacks leave for `poll`.
#[derive(Default)]
struct Shared {
    open: bool,
    /// Requests made before the socket opened.
    outbox: Vec<Vec<u8>>,
    inbox: VecDeque<Vec<u8>>,
    ended: Option<ClientError>,
    waker: Option<Waker>,
}

fn notify(shared: std::cell::RefMut<'_, Shared>) {
    let waker = shared.waker.clone();
    drop(shared);
    if let Some(waker) = waker {
        waker.wake();
    }
}

/// A control connection in the browser.
pub struct WebClient {
    ws: WebSocket,
    control: Control,
    shared: Rc<RefCell<Shared>>,
    _on_open: Closure<dyn FnMut(Event)>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_close: Closure<dyn FnMut(CloseEvent)>,
}

impl WebClient {
    /// Starts connecting to `url` (`wss://host:port`, with or without
    /// `/control`) and queues the Hello; its Welcome arrives as the reply to
    /// sequence number 1.
    pub fn open(url: &str, game_fingerprint: u64) -> Result<WebClient, ClientError> {
        Self::open_with_limits(url, game_fingerprint, MAX_UNPOLLED, usize::MAX)
    }

    fn open_with_limits(
        url: &str,
        game_fingerprint: u64,
        max_unpolled: usize,
        max_bytes: usize,
    ) -> Result<WebClient, ClientError> {
        let url = control_url(url);
        if !url.starts_with("wss://") {
            return Err(ClientError::BadUrl(url));
        }
        let ws = WebSocket::new(&url).map_err(|e| ClientError::Io(format!("{e:?}")))?;
        ws.set_binary_type(BinaryType::Arraybuffer);
        let shared = Rc::new(RefCell::new(Shared::default()));
        let on_open = {
            let (shared, ws) = (Rc::clone(&shared), ws.clone());
            Closure::<dyn FnMut(Event)>::new(move |_: Event| {
                let mut shared = shared.borrow_mut();
                shared.open = true;
                for bytes in std::mem::take(&mut shared.outbox) {
                    if ws.send_with_u8_array(&bytes).is_err() {
                        shared.ended = Some(ClientError::Closed);
                    }
                }
                if shared.ended.is_some() {
                    notify(shared);
                }
            })
        };
        let on_message = {
            let (shared, ws) = (Rc::clone(&shared), ws.clone());
            Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                let mut shared = shared.borrow_mut();
                if shared.ended.is_some() {
                    return;
                }
                let Ok(buffer) = event.data().dyn_into::<js_sys::ArrayBuffer>() else {
                    shared.ended = Some(ClientError::Protocol("a text message".to_owned()));
                    let _ = ws.close();
                    notify(shared);
                    return;
                };
                if buffer.byte_length() as usize > max_bytes {
                    shared.ended =
                        Some(ClientError::Protocol("service answer too large".to_owned()));
                    let _ = ws.close();
                    return;
                }
                if shared.inbox.len() >= max_unpolled {
                    shared.ended = Some(ClientError::Protocol("messages not polled".to_owned()));
                    let _ = ws.close();
                    notify(shared);
                    return;
                }
                shared
                    .inbox
                    .push_back(js_sys::Uint8Array::new(&buffer).to_vec());
                notify(shared);
            })
        };
        let on_close = {
            let shared = Rc::clone(&shared);
            Closure::<dyn FnMut(CloseEvent)>::new(move |_: CloseEvent| {
                let mut shared = shared.borrow_mut();
                shared.open = false;
                shared.ended.get_or_insert(ClientError::Closed);
                notify(shared);
            })
        };
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        let mut client = WebClient {
            ws,
            control: Control::new(),
            shared,
            _on_open: on_open,
            _on_message: on_message,
            _on_close: on_close,
        };
        let hello = client.control.hello(game_fingerprint)?;
        client.send(hello.bytes)?;
        Ok(client)
    }

    /// Replace the notification target. Wakes once at registration and whenever a message
    /// or a connection error becomes available. After a wake, drain [`Self::poll`].
    pub fn set_waker(&mut self, waker: Waker) {
        self.shared.borrow_mut().waker = Some(waker.clone());
        waker.wake();
    }

    /// Sends a request, or holds it until the socket opens; its answer
    /// carries the sequence number returned.
    pub fn request(&mut self, msg: ClientMsg) -> Result<u32, ClientError> {
        let out = self.control.request(msg)?;
        self.send(out.bytes)?;
        Ok(out.seq)
    }

    fn send(&mut self, bytes: Vec<u8>) -> Result<(), ClientError> {
        let mut shared = self.shared.borrow_mut();
        if let Some(error) = &shared.ended {
            return Err(error.clone());
        }
        if shared.open {
            self.ws
                .send_with_u8_array(&bytes)
                .map_err(|e| ClientError::Io(format!("{e:?}")))
        } else {
            shared.outbox.push(bytes);
            Ok(())
        }
    }

    /// The next message that arrived, if any. Once the connection has ended
    /// and everything that came before was polled, every call returns why.
    pub fn poll(&mut self) -> Result<Option<Incoming>, ClientError> {
        let next = {
            let mut shared = self.shared.borrow_mut();
            match shared.inbox.pop_front() {
                Some(bytes) => bytes,
                None => {
                    return match &shared.ended {
                        Some(error) => Err(error.clone()),
                        None => Ok(None),
                    };
                }
            }
        };
        self.control.receive(&next).map(Some)
    }

    /// What the service knows about itself and the account.
    pub fn control(&self) -> &Control {
        &self.control
    }
}

impl Drop for WebClient {
    fn drop(&mut self) {
        self.ws.set_onopen(None);
        self.ws.set_onmessage(None);
        self.ws.set_onclose(None);
        let _ = self.ws.close();
    }
}

/// A check of a service under way in the browser: the `Hello` over the browser's WebSocket,
/// judged when the answer arrives. A page learns no more of a failed connection than that it
/// failed, so that is [`ServiceFault::CannotConnect`]. Dropping it closes the socket.
pub struct ServiceCheck {
    client: Option<WebClient>,
    hello: u32,
    deadline: f64,
    early: Option<ServiceFault>,
}

impl ServiceCheck {
    /// Starts checking `url` (`wss://host[:port][/path]`) for a client of the game wire
    /// `game_fingerprint`, giving up after `patience`.
    pub fn start(url: &str, game_fingerprint: u64, patience: std::time::Duration) -> ServiceCheck {
        let deadline = js_sys::Date::now() + patience.as_secs_f64() * 1000.0;
        match WebClient::open_with_limits(url, game_fingerprint, 1, MAX_ANSWER_BYTES) {
            Ok(client) => ServiceCheck {
                client: Some(client),
                hello: 1,
                deadline,
                early: None,
            },
            Err(error) => ServiceCheck {
                client: None,
                hello: 1,
                deadline,
                early: Some(match error {
                    ClientError::BadUrl(_) => ServiceFault::BadAddress,
                    _ => ServiceFault::CannotConnect,
                }),
            },
        }
    }

    /// The outcome, once, when it is there; nothing while the check runs and nothing after it
    /// was returned.
    pub fn poll(&mut self) -> Option<Result<ServiceFound, ServiceFault>> {
        if let Some(fault) = self.early.take() {
            return Some(Err(fault));
        }
        let client = self.client.as_mut()?;
        let outcome = match client.poll() {
            Ok(Some(incoming)) => judge(self.hello, incoming),
            Ok(None) if js_sys::Date::now() >= self.deadline => Err(ServiceFault::TimedOut),
            Ok(None) => return None,
            Err(ClientError::Protocol(_)) => Err(ServiceFault::NotService),
            Err(_) => Err(ServiceFault::CannotConnect),
        };
        self.client = None;
        Some(outcome)
    }
}
