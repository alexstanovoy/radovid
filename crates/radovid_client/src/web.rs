//! A control connection over the browser's WebSocket (wasm32). The browser
//! does TLS and the WebSocket framing; nothing here blocks: requests are sent
//! (or held until the socket opens) and what arrives is polled.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use radovid_proto::ClientMsg;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{BinaryType, CloseEvent, Event, MessageEvent, WebSocket};

use crate::{ClientError, Control, Incoming, control_url};

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
            })
        };
        let on_message = {
            let (shared, ws) = (Rc::clone(&shared), ws.clone());
            Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                let mut shared = shared.borrow_mut();
                let Ok(buffer) = event.data().dyn_into::<js_sys::ArrayBuffer>() else {
                    shared.ended = Some(ClientError::Protocol("a text message".to_owned()));
                    let _ = ws.close();
                    return;
                };
                if shared.inbox.len() >= MAX_UNPOLLED {
                    shared.ended = Some(ClientError::Protocol("messages not polled".to_owned()));
                    let _ = ws.close();
                    return;
                }
                shared
                    .inbox
                    .push_back(js_sys::Uint8Array::new(&buffer).to_vec());
            })
        };
        let on_close = {
            let shared = Rc::clone(&shared);
            Closure::<dyn FnMut(CloseEvent)>::new(move |_: CloseEvent| {
                let mut shared = shared.borrow_mut();
                shared.open = false;
                shared.ended.get_or_insert(ClientError::Closed);
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
