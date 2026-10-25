//! Limits both sides hold.

/// The version of every wire type here. Bumped on every change of any of them;
/// a server accepts exactly its own.
pub const PROTO_VERSION: u16 = 5;

/// Bytes of a match artifact (a game's replay) one `Reply::Artifact` carries at most: well under
/// `MAX_SERVER_PAYLOAD_LEN`.
pub const MAX_ARTIFACT_CHUNK: usize = 1024 * 1024;

/// Largest payload of a frame a client sends, in bytes.
pub const MAX_CONTROL_PAYLOAD_LEN: usize = 64 * 1024;

/// Largest payload of a frame the service sends, in bytes: a list of match
/// results carries their game reports.
pub const MAX_SERVER_PAYLOAD_LEN: usize = 4 * 1024 * 1024;

/// Seconds a connection has from accept to its first valid `Login`, `Register`
/// or `Resume`.
pub const AUTH_DEADLINE_SECS: u64 = 10;

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

/// Control sessions per account. A bot with more games than this runs them
/// all from one session.
pub const MAX_SESSIONS_PER_ACCOUNT: usize = 4;

/// Rows of a ladder page at most.
pub const MAX_LADDER_PAGE: u8 = 100;
/// Lobbies listed at most.
pub const MAX_LOBBY_LIST: usize = 100;
/// Recent matches returned at most.
pub const MAX_RECENT_MATCHES: u8 = 50;
/// Lobbies alive at once at most.
pub const MAX_LOBBIES: usize = 256;
/// Join requests pending per lobby at most.
pub const MAX_PENDING_PER_LOBBY: usize = 16;
/// Seats of a match at most.
pub const MAX_SEATS: usize = 10;

/// Bounds of the strings and byte strings.
pub const MIN_LOGIN_LEN: usize = 3;
pub const MAX_LOGIN_LEN: usize = 20;
pub const MIN_NICKNAME_LEN: usize = 3;
pub const MAX_NICKNAME_LEN: usize = 20;
pub const MIN_PASSWORD_LEN: usize = 8;
pub const MAX_PASSWORD_LEN: usize = 128;
pub const MAX_BOT_VERSION_LEN: usize = 32;
pub const MAX_LOBBY_NAME_LEN: usize = 32;
pub const MAX_LOBBY_PASSWORD_LEN: usize = 32;
pub const MAX_CLIENT_LABEL_LEN: usize = 64;
pub const MAX_BAN_REASON_LEN: usize = 200;
pub const MAX_RULESET_ID_LEN: usize = 32;
/// A game's report of one match, forwarded untouched.
pub const MAX_GAME_REPORT_LEN: usize = 64 * 1024;
/// A ruleset's opaque description for clients.
pub const MAX_RULESET_INFO_LEN: usize = 4 * 1024;

/// Milliseconds of the lobby countdown. Nobody can cancel it.
pub const LOBBY_COUNTDOWN_MS: u32 = 5000;

/// Milliseconds between two `QueueStatus` events of one queue entry at least.
pub const QUEUE_STATUS_INTERVAL_MS: u32 = 5000;
