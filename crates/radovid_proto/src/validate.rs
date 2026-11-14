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

/// Empty lists the board; underscores are literal nickname characters, never wildcards.
pub fn valid_nickname_prefix(prefix: &str) -> bool {
    prefix.len() <= MAX_NICKNAME_LEN
        && prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
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

/// An opaque version name: 1 to 32 printable ASCII bytes, with no edge whitespace.
pub fn valid_version(version: &str) -> bool {
    (1..=MAX_VERSION_LEN).contains(&version.len())
        && version == version.trim()
        && version
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
}

/// A client label: at most 64 bytes without control characters.
pub fn valid_client_label(label: &str) -> bool {
    label.len() <= MAX_CLIENT_LABEL_LEN && !label.chars().any(char::is_control)
}

/// A ban reason: at most 200 bytes.
pub fn valid_ban_reason(reason: &str) -> bool {
    reason.len() <= MAX_BAN_REASON_LEN
}

#[cfg(test)]
mod version_tests {
    use super::*;
    #[test]
    fn version_names_keep_the_exact_printable_string_and_reject_every_control_byte() {
        assert!(valid_version("v1.0.0 (buster)"));
        assert!(valid_version(&"x".repeat(MAX_VERSION_LEN)));
        assert!(!valid_version(&"x".repeat(MAX_VERSION_LEN + 1)));
        for byte in 0..=u8::MAX {
            let candidate = format!("a{}b", char::from(byte));
            assert_eq!(
                valid_version(&candidate),
                (32..=126).contains(&byte),
                "byte {byte}"
            );
        }
        for text in ["", " ", " version", "version ", "\u{200b}version", "версия"] {
            assert!(!valid_version(text), "{text:?}");
        }
    }
}
