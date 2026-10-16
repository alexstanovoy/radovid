//! Limits both sides hold.

/// The version of every wire type here. Bumped on every change of any of them;
/// a server accepts exactly its own.
pub const PROTO_VERSION: u16 = 1;

/// Largest control frame payload, in bytes.
pub const MAX_PAYLOAD_LEN: usize = 64 * 1024;

/// Seconds a connection has from accept to its first valid `Login`, `Register`
/// or `Resume`.
pub const AUTH_DEADLINE_SECS: u64 = 10;

/// Seconds a game connection has to send its `Hello`.
pub const GAME_HELLO_DEADLINE_SECS: u64 = 10;

/// Seconds between the service's WebSocket pings.
pub const PING_INTERVAL_SECS: u64 = 30;

/// Seconds without a pong after which a connection is closed.
pub const PONG_DEADLINE_SECS: u64 = 60;

/// Control messages per second a connection sustains.
pub const MESSAGES_PER_SEC: u32 = 20;

/// Control messages a connection may burst.
pub const MESSAGE_BURST: u32 = 60;

/// Breaches of the rate limit within a minute after which a connection closes.
pub const RATE_BREACHES_BEFORE_CLOSE: u32 = 3;

/// Unauthenticated control connections per source address.
pub const MAX_UNAUTHENTICATED_PER_IP: usize = 16;

/// Control sessions per account.
pub const MAX_SESSIONS_PER_ACCOUNT: usize = 4;

/// Connections per seat key on the game path: one player and seven spectators.
pub const MAX_PLAYERS_PER_KEY: usize = 1;
/// Spectators per seat key on the game path.
pub const MAX_SPECTATORS_PER_KEY: usize = 7;

/// Rows of a ladder page, lobby list and recent matches at most.
pub const MAX_LADDER_PAGE: u8 = 100;
/// Lobbies listed at most.
pub const MAX_LOBBY_LIST: usize = 100;
/// Recent matches returned at most.
pub const MAX_RECENT_MATCHES: u8 = 50;
/// Lobbies alive at once at most.
pub const MAX_LOBBIES: usize = 256;
/// Join requests pending per lobby at most.
pub const MAX_PENDING_PER_LOBBY: usize = 16;

/// Bounds of the strings.
pub const MIN_LOGIN_LEN: usize = 3;
pub const MAX_LOGIN_LEN: usize = 20;
pub const MIN_NICKNAME_LEN: usize = 3;
pub const MAX_NICKNAME_LEN: usize = 20;
pub const MIN_PASSWORD_LEN: usize = 8;
pub const MAX_PASSWORD_LEN: usize = 128;
pub const MAX_BOT_VERSION_LEN: usize = 32;
pub const MAX_LOBBY_NAME_LEN: usize = 32;
pub const MAX_CLIENT_LABEL_LEN: usize = 64;
pub const MAX_BAN_REASON_LEN: usize = 200;
pub const MAX_RULESET_ID_LEN: usize = 32;

/// Milliseconds of the lobby countdown. Nobody can cancel it.
pub const LOBBY_COUNTDOWN_MS: u32 = 5000;
