//! One `/control` connection: the handshake, the login, and then requests and
//! events until either side ends it.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use radovid_proto::*;
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::config::Config;
use crate::db::{AccountRow, DbError, now_ms};
use crate::hub::{Attach, Cmd, SessionId, event_channel};
use crate::server::{Shared, Socket, Unauthenticated};
use crate::throttle::TokenBucket;

/// A request the hub takes longer than this over is answered `Internal`.
const HUB_TIMEOUT: Duration = Duration::from_secs(30);
/// A peer that does not take a message in this long is dropped.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// Window in which rate-limit breaches are counted.
const BREACH_WINDOW: Duration = Duration::from_secs(60);

struct Live {
    id: SessionId,
    account: AccountInfo,
    token_hash: [u8; 32],
    events: mpsc::Receiver<ServerMsg>,
}

struct Conn {
    ws: Socket,
    ip: IpAddr,
    shared: Arc<Shared>,
    bucket: TokenBucket,
    breaches: Vec<Instant>,
    hello: Option<u64>,
    live: Option<Live>,
}

/// The connection is over.
struct Closed;

pub async fn run(ws: Socket, ip: IpAddr, shared: Arc<Shared>, mut guard: Unauthenticated) {
    let mut conn = Conn {
        ws,
        ip,
        shared,
        bucket: TokenBucket::new(MESSAGES_PER_SEC, MESSAGE_BURST, Instant::now()),
        breaches: Vec::new(),
        hello: None,
        live: None,
    };
    let _ = conn.serve(&mut guard).await;
    if let Some(live) = conn.live.take() {
        let _ = conn
            .shared
            .hub
            .tx
            .send(Cmd::Detach { session: live.id })
            .await;
    }
    let _ = conn.ws.close(None).await;
}

impl Conn {
    async fn serve(&mut self, guard: &mut Unauthenticated) -> Result<(), Closed> {
        let accepted = tokio::time::Instant::now();
        let mut ping = tokio::time::interval(Duration::from_secs(PING_INTERVAL_SECS));
        ping.tick().await;
        let mut last_heard = Instant::now();
        loop {
            let auth_left =
                Duration::from_secs(AUTH_DEADLINE_SECS).saturating_sub(accepted.elapsed());
            tokio::select! {
                message = self.ws.next() => {
                    let message = message.ok_or(Closed)?.map_err(|_| Closed)?;
                    last_heard = Instant::now();
                    match message {
                        Message::Binary(bytes) => self.frame(&bytes, guard).await?,
                        Message::Ping(_) | Message::Pong(_) => {}
                        _ => return Err(Closed),
                    }
                }
                event = next_event(&mut self.live) => match event {
                    Some(message) => {
                        let over = matches!(message, ServerMsg::Event(Event::Bye { .. }));
                        self.send(&message).await?;
                        if over {
                            return Err(Closed);
                        }
                    }
                    None => return Err(Closed),
                },
                _ = ping.tick() => {
                    if last_heard.elapsed() > Duration::from_secs(PONG_DEADLINE_SECS) {
                        return Err(Closed);
                    }
                    self.send_raw(Message::Ping(Vec::new().into())).await?;
                }
                _ = tokio::time::sleep(auth_left), if self.live.is_none() => {
                    return Err(Closed);
                }
            }
        }
    }

    async fn send_raw(&mut self, message: Message) -> Result<(), Closed> {
        match tokio::time::timeout(SEND_TIMEOUT, self.ws.send(message)).await {
            Ok(Ok(())) => Ok(()),
            _ => Err(Closed),
        }
    }

    async fn send(&mut self, message: &ServerMsg) -> Result<(), Closed> {
        let bytes = encode_frame_to_vec(message).map_err(|_| Closed)?;
        self.send_raw(Message::Binary(bytes.into())).await
    }

    async fn refuse(&mut self, seq: u32, error: Error) -> Result<(), Closed> {
        self.send(&ServerMsg::Refused { seq, error }).await
    }

    async fn reply(&mut self, seq: u32, result: Result<Reply, Error>) -> Result<(), Closed> {
        match result {
            Ok(reply) => self.send(&ServerMsg::Reply { seq, reply }).await,
            Err(error) => self.refuse(seq, error).await,
        }
    }

    /// Says goodbye and ends the connection.
    async fn bye(&mut self, error: Error) -> Result<(), Closed> {
        let _ = self.send(&ServerMsg::Event(Event::Bye { error })).await;
        Err(Closed)
    }

    async fn frame(&mut self, bytes: &[u8], guard: &mut Unauthenticated) -> Result<(), Closed> {
        let frame = match decode_control_frame(bytes) {
            Some(frame) => frame,
            None => return self.bye(Error::Malformed).await,
        };
        if let Err(retry_after_ms) = self.bucket.take(Instant::now()) {
            let now = Instant::now();
            self.breaches
                .retain(|at| now.duration_since(*at) < BREACH_WINDOW);
            self.breaches.push(now);
            if self.breaches.len() >= RATE_BREACHES_BEFORE_CLOSE as usize {
                return self.bye(Error::RateLimited { retry_after_ms }).await;
            }
            return self
                .refuse(frame.seq, Error::RateLimited { retry_after_ms })
                .await;
        }
        let ClientFrame { seq, msg } = frame;
        if self.hello.is_none() {
            return self.hello(seq, msg).await;
        }
        match (&self.live, msg) {
            (_, ClientMsg::Hello { .. }) => self.bye(Error::Malformed).await,
            (
                None,
                ClientMsg::Register {
                    login,
                    password,
                    kind,
                    client,
                },
            ) => {
                let result = self.register(login, password, kind, client, guard).await;
                self.reply(seq, result).await
            }
            (
                None,
                ClientMsg::Login {
                    login,
                    password,
                    client,
                },
            ) => {
                let result = self.login(login, password, client, guard).await;
                self.reply(seq, result).await
            }
            (None, ClientMsg::Resume { session, client }) => {
                let result = self.resume(session, client, guard).await;
                self.reply(seq, result).await
            }
            (None, ClientMsg::Rulesets) => {
                let list = self.shared.config.ruleset_infos(AccountKind::Human);
                self.reply(seq, Ok(Reply::Rulesets { list })).await
            }
            (None, ClientMsg::Limits { ruleset }) => {
                let result = human_limits(&self.shared.config, ruleset);
                self.reply(seq, result).await
            }
            (None, _) => self.refuse(seq, Error::NotLoggedIn).await,
            (
                Some(_),
                ClientMsg::Register { .. } | ClientMsg::Login { .. } | ClientMsg::Resume { .. },
            ) => self.refuse(seq, Error::AlreadyLoggedIn).await,
            (Some(live), ClientMsg::Logout) => {
                let hash = live.token_hash;
                let _ = self.shared.db.revoke_session(hash).await;
                self.send(&ServerMsg::Reply {
                    seq,
                    reply: Reply::Done,
                })
                .await?;
                Err(Closed)
            }
            (Some(_), ClientMsg::ChangePassword { current, new }) => {
                let result = self.change_password(current, new).await;
                self.reply(seq, result).await
            }
            (Some(_), ClientMsg::DeleteAccount { password }) => {
                let result = self.delete_account(password).await;
                let deleted = result.is_ok();
                self.reply(seq, result).await?;
                if deleted { Err(Closed) } else { Ok(()) }
            }
            (
                Some(live),
                msg @ (ClientMsg::LobbyAsk {
                    password: Some(_), ..
                }
                | ClientMsg::LobbyJoinByName {
                    password: Some(_), ..
                }),
            ) => {
                // A lobby password is guessed like a login: the same delay per source.
                let session = live.id;
                let result = match self.throttled("") {
                    Some(error) => Err(error),
                    None => self.ask_hub(session, msg).await,
                };
                if result == Err(Error::BadLobbyPassword) {
                    self.failed("");
                }
                self.reply(seq, result).await
            }
            (Some(live), msg) => {
                let result = self.ask_hub(live.id, msg).await;
                self.reply(seq, result).await
            }
        }
    }

    async fn ask_hub(&self, session: SessionId, msg: ClientMsg) -> Result<Reply, Error> {
        let (reply, answer) = oneshot::channel();
        self.shared
            .hub
            .tx
            .send(Cmd::Request {
                session,
                msg,
                reply,
            })
            .await
            .map_err(|_| Error::ServerStopping)?;
        match tokio::time::timeout(HUB_TIMEOUT, answer).await {
            Ok(Ok(result)) => result,
            _ => Err(Error::Internal),
        }
    }

    async fn hello(&mut self, seq: u32, msg: ClientMsg) -> Result<(), Closed> {
        let ClientMsg::Hello {
            proto,
            game_fingerprint,
            client,
        } = msg
        else {
            return self.bye(Error::Malformed).await;
        };
        if !(MIN_PROTO_VERSION..=PROTO_VERSION).contains(&proto) {
            self.send(&ServerMsg::Refused {
                seq,
                error: Error::ProtoMismatch {
                    server: PROTO_VERSION,
                },
            })
            .await?;
            return Err(Closed);
        }
        if !valid_client_label(&client) {
            return self.bye(Error::Malformed).await;
        }
        self.hello = Some(game_fingerprint);
        let config = &self.shared.config;
        // The client is answered in its own version, which it checks.
        let welcome = Reply::Welcome {
            proto,
            server: config.server_name.clone(),
            rulesets: config.ruleset_infos(AccountKind::Human),
        };
        self.reply(seq, Ok(welcome)).await
    }

    fn throttled(&self, login: &str) -> Option<Error> {
        self.shared.login_delay(self.ip, login)
    }

    fn failed(&self, login: &str) {
        self.shared.login_failed(self.ip, login);
    }

    async fn register(
        &mut self,
        login: String,
        Secret(password): Secret,
        kind: AccountKind,
        client: ClientInfo,
        guard: &mut Unauthenticated,
    ) -> Result<Reply, Error> {
        let login = login.to_ascii_lowercase();
        if !valid_login(&login) {
            return Err(Error::BadLogin);
        }
        if !valid_password(&password) {
            return Err(Error::WeakPassword);
        }
        let bot_version = check_bot_version(kind, client)?;
        self.shared.register_allowed(self.ip)?;
        let hash = self.shared.passwords.hash(self.ip, password).await?;
        let id = match self
            .shared
            .db
            .create_account(login.clone(), hash, kind, bot_version)
            .await
        {
            Ok(id) => id,
            Err(DbError::Taken) => return Err(Error::LoginTaken),
            Err(error) => return Err(crate::hub::internal(error)),
        };
        let row = self
            .shared
            .db
            .account_by_id(id)
            .await
            .map_err(crate::hub::internal)?
            .ok_or(Error::Internal)?;
        self.finish_login(row, None, guard).await
    }

    async fn login(
        &mut self,
        login: String,
        Secret(password): Secret,
        client: ClientInfo,
        guard: &mut Unauthenticated,
    ) -> Result<Reply, Error> {
        let login = login.to_ascii_lowercase();
        if let Some(error) = self.throttled(&login) {
            return Err(error);
        }
        let row = if valid_login(&login) && valid_password(&password) {
            self.shared
                .db
                .account_by_login(login.clone())
                .await
                .map_err(crate::hub::internal)?
        } else {
            None
        };
        let stored = row.as_ref().map(|r| r.hash.clone());
        if !self
            .shared
            .passwords
            .verify(self.ip, password, stored)
            .await?
        {
            self.failed(&login);
            return Err(Error::BadCredentials);
        }
        let row = row.ok_or(Error::BadCredentials)?;
        self.shared.login_succeeded(self.ip, &login);
        let row = self.admit(row, client).await?;
        self.finish_login(row, None, guard).await
    }

    async fn resume(
        &mut self,
        session: SessionToken,
        client: ClientInfo,
        guard: &mut Unauthenticated,
    ) -> Result<Reply, Error> {
        // No delay and no count: a token has 256 random bits, so guessing
        // gains nothing, and a count shared by every resume would let anyone
        // hold up everyone's.
        let hash = token_hash(&session);
        let Some(id) = self
            .shared
            .db
            .resume_session(hash)
            .await
            .map_err(crate::hub::internal)?
        else {
            return Err(Error::BadSession);
        };
        let row = self
            .shared
            .db
            .account_by_id(id)
            .await
            .map_err(crate::hub::internal)?
            .ok_or(Error::BadSession)?;
        let row = self.admit(row, client).await?;
        self.finish_login(row, Some(session), guard).await
    }

    /// The checks every way in shares: no ban in force, a bot version fit for
    /// the account's kind (recorded when it changed).
    async fn admit(&self, row: AccountRow, client: ClientInfo) -> Result<AccountRow, Error> {
        if let Some(ban) = row.active_ban(now_ms()) {
            return Err(Error::Banned {
                reason: ban.reason.clone(),
                until: ban.until,
            });
        }
        let bot_version = check_bot_version(row.kind, client)?;
        if bot_version.is_some() && bot_version != row.bot_version {
            self.shared
                .db
                .set_bot_version(row.id, bot_version.clone())
                .await
                .map_err(crate::hub::internal)?;
        }
        Ok(AccountRow {
            bot_version: bot_version.or(row.bot_version.clone()),
            ..row
        })
    }

    /// Opens the session at the hub and answers the login.
    async fn finish_login(
        &mut self,
        row: AccountRow,
        token: Option<SessionToken>,
        guard: &mut Unauthenticated,
    ) -> Result<Reply, Error> {
        let token = match token {
            Some(token) => token,
            None => {
                let mut bytes = [0u8; 32];
                getrandom::fill(&mut bytes).map_err(crate::hub::internal)?;
                let token = SessionToken(bytes);
                self.shared
                    .db
                    .create_session(row.id, token_hash(&token))
                    .await
                    .map_err(crate::hub::internal)?;
                token
            }
        };
        let (events_tx, events) = event_channel();
        let (reply, answer) = oneshot::channel();
        let me = row.info();
        self.shared
            .hub
            .tx
            .send(Cmd::Attach(Box::new(Attach {
                account: row,
                token_hash: token_hash(&token),
                game_fingerprint: self.hello.unwrap_or(0),
                source: self.ip,
                events: events_tx,
                reply,
            })))
            .await
            .map_err(|_| Error::ServerStopping)?;
        let (id, activities) = answer.await.map_err(|_| Error::ServerStopping)??;
        guard.release();
        self.live = Some(Live {
            id,
            account: me.clone(),
            token_hash: token_hash(&token),
            events,
        });
        Ok(Reply::LoggedIn {
            me,
            session: token,
            activities,
        })
    }

    /// Checks the account's own password again, under the login delay.
    async fn reverify(&self, password: String) -> Result<(AccountId, [u8; 32]), Error> {
        let (id, login, keep) = match &self.live {
            Some(live) => (live.account.id, live.account.login.clone(), live.token_hash),
            None => return Err(Error::NotLoggedIn),
        };
        if let Some(error) = self.throttled(&login) {
            return Err(error);
        }
        let row = self
            .shared
            .db
            .account_by_id(id)
            .await
            .map_err(crate::hub::internal)?
            .ok_or(Error::Internal)?;
        if !self
            .shared
            .passwords
            .verify(self.ip, password, Some(row.hash))
            .await?
        {
            self.failed(&login);
            return Err(Error::BadCredentials);
        }
        Ok((id, keep))
    }

    async fn delete_account(&mut self, Secret(password): Secret) -> Result<Reply, Error> {
        let (id, _) = self.reverify(password).await?;
        self.shared
            .db
            .delete_account(id)
            .await
            .map_err(crate::hub::internal)?;
        let (done, deleted) = oneshot::channel();
        self.shared
            .hub
            .tx
            .send(Cmd::AccountDeleted { account: id, done })
            .await
            .map_err(|_| Error::ServerStopping)?;
        deleted.await.map_err(|_| Error::ServerStopping)?;
        Ok(Reply::Done)
    }

    async fn change_password(
        &mut self,
        Secret(current): Secret,
        Secret(new): Secret,
    ) -> Result<Reply, Error> {
        if !valid_password(&new) {
            return Err(Error::WeakPassword);
        }
        let (id, keep) = self.reverify(current).await?;
        let hash = self.shared.passwords.hash(self.ip, new).await?;
        self.shared
            .db
            .set_password(id, hash)
            .await
            .map_err(crate::hub::internal)?;
        self.shared
            .db
            .revoke_other_sessions(id, keep)
            .await
            .map_err(crate::hub::internal)?;
        self.shared
            .hub
            .tx
            .send(Cmd::EndOtherSessions { account: id, keep })
            .await
            .map_err(|_| Error::ServerStopping)?;
        Ok(Reply::Done)
    }
}

fn check_bot_version(kind: AccountKind, client: ClientInfo) -> Result<Option<String>, Error> {
    match (kind, client.bot_version) {
        (AccountKind::Human, None) => Ok(None),
        (AccountKind::Human, Some(_)) => Err(Error::BadBotVersion),
        (AccountKind::Bot, None) => Ok(None),
        (AccountKind::Bot, Some(version)) if valid_bot_version(&version) => Ok(Some(version)),
        (AccountKind::Bot, Some(_)) => Err(Error::BadBotVersion),
    }
}

fn token_hash(token: &SessionToken) -> [u8; 32] {
    Sha256::digest(token.0).into()
}

fn decode_control_frame(bytes: &[u8]) -> Option<ClientFrame> {
    decode_payload(whole_frame(bytes, MAX_CONTROL_PAYLOAD_LEN)?).ok()
}

/// The next event for the live session, or never while there is none.
async fn next_event(live: &mut Option<Live>) -> Option<ServerMsg> {
    match live {
        Some(live) => live.events.recv().await,
        None => std::future::pending().await,
    }
}

/// The limits of a human account, for a client not logged in yet.
fn human_limits(config: &Config, ruleset: Option<RulesetId>) -> Result<Reply, Error> {
    let ruleset = match &ruleset {
        Some(ruleset) => Some(config.ruleset(ruleset).ok_or(Error::UnknownRuleset)?),
        None => None,
    };
    Ok(Reply::Limits {
        limits: config.limits_for(AccountKind::Human, ruleset),
    })
}
