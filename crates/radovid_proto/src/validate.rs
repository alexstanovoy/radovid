//! The shape of the strings accounts and lobbies are made of.

use crate::*;

/// A login: 3 to 20 characters of `[a-z0-9_]`.
pub fn valid_login(login: &str) -> bool {
    (MIN_LOGIN_LEN..=MAX_LOGIN_LEN).contains(&login.len())
        && login
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A nickname: 3 to 20 characters of `[A-Za-z0-9_]`.
pub fn valid_nickname(nickname: &str) -> bool {
    (MIN_NICKNAME_LEN..=MAX_NICKNAME_LEN).contains(&nickname.len())
        && nickname
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// A password: 8 to 128 bytes of UTF-8 (the type already guarantees UTF-8).
pub fn valid_password(password: &str) -> bool {
    (MIN_PASSWORD_LEN..=MAX_PASSWORD_LEN).contains(&password.len())
}

/// A lobby name: 1 to 32 characters, none a control character.
pub fn valid_lobby_name(name: &str) -> bool {
    (1..=MAX_LOBBY_NAME_LEN).contains(&name.chars().count()) && !name.chars().any(char::is_control)
}

/// A lobby password: 1 to 32 bytes, no control character.
pub fn valid_lobby_password(password: &str) -> bool {
    (1..=MAX_LOBBY_PASSWORD_LEN).contains(&password.len())
        && !password.chars().any(char::is_control)
}

/// A ruleset id: 1 to 32 bytes of `[a-z0-9_]`.
pub fn valid_ruleset_id(id: &str) -> bool {
    (1..=MAX_RULESET_ID_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// The name of a match artifact: 1 to 32 bytes of `[a-z0-9_]`.
pub fn valid_artifact_name(name: &str) -> bool {
    (1..=MAX_ARTIFACT_NAME_LEN).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A bot version label: 1 to 32 bytes without control characters.
pub fn valid_bot_version(version: &str) -> bool {
    (1..=MAX_BOT_VERSION_LEN).contains(&version.len()) && !version.chars().any(char::is_control)
}

/// A client label: at most 64 bytes without control characters.
pub fn valid_client_label(label: &str) -> bool {
    label.len() <= MAX_CLIENT_LABEL_LEN && !label.chars().any(char::is_control)
}

/// A ban reason: at most 200 bytes.
pub fn valid_ban_reason(reason: &str) -> bool {
    reason.len() <= MAX_BAN_REASON_LEN
}
