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

/// A lobby name: 1 to 32 characters of displayable text.
pub fn valid_lobby_name(name: &str) -> bool {
    (1..=MAX_LOBBY_NAME_LEN).contains(&name.chars().count()) && valid_display_text(name)
}

/// Whether `character` may appear in text shown to other players: not a control character, not
/// whitespace other than the plain space, and not in `HIDDEN_CHARACTERS`. A lobby search holds
/// to this too, since it can only ever name what a lobby name may hold.
pub fn valid_display_character(character: char) -> bool {
    !character.is_control()
        && (character == ' ' || !character.is_whitespace())
        && !HIDDEN_CHARACTERS
            .iter()
            .any(|&(first, last)| (first..=last).contains(&character))
}

/// The one rule for free text that other players read (lobby names, ban reasons, version names):
/// non-empty, every character passes `valid_display_character`, no space at either end and no two
/// spaces in a row, so that no text looks empty, padded, reordered or like another text.
///
/// It admits every script and ordinary emoji, including the emoji presentation selector
/// (U+FE0F) that single-code-point emoji such as a heart need, but only straight after a
/// non-ASCII character and never twice in a row. Emoji sequences made with a zero-width joiner
/// (families, professions, flags of people) are refused: the joiner is also how text is hidden.
/// Lengths are the caller's rule.
pub fn valid_display_text(text: &str) -> bool {
    if text.starts_with(' ') || text.ends_with(' ') || text.contains("  ") {
        return false;
    }
    let mut previous = None;
    for character in text.chars() {
        let admitted = if character == EMOJI_PRESENTATION_SELECTOR {
            previous.is_some_and(|before: char| {
                !before.is_ascii() && before != EMOJI_PRESENTATION_SELECTOR
            })
        } else {
            valid_display_character(character)
        };
        if !admitted {
            return false;
        }
        previous = Some(character);
    }
    previous.is_some()
}

/// VARIATION SELECTOR-16: the only variation selector admitted, see `valid_display_text`.
const EMOJI_PRESENTATION_SELECTOR: char = '\u{fe0f}';

/// What no displayed text may hold besides control characters and whitespace, which `char`
/// itself classifies (so U+0085, U+00A0, U+1680, U+2000..U+200A, U+2028 LINE SEPARATOR,
/// U+2029 PARAGRAPH SEPARATOR, U+202F, U+205F and U+3000 are refused as whitespace). Sorted and
/// disjoint, inclusive on both ends. Most of it is the Unicode `Default_Ignorable_Code_Point`
/// property; the standard library has no table for it and a dependency is not worth one.
/// Unassigned code points other than noncharacters are not listed: the table would age with
/// every Unicode release, and they draw as a visible box rather than nothing.
const HIDDEN_CHARACTERS: &[(char, char)] = &[
    ('\u{ad}', '\u{ad}'),         // SOFT HYPHEN
    ('\u{34f}', '\u{34f}'),       // COMBINING GRAPHEME JOINER
    ('\u{61c}', '\u{61c}'),       // ARABIC LETTER MARK
    ('\u{115f}', '\u{1160}'),     // HANGUL CHOSEONG FILLER, HANGUL JUNGSEONG FILLER
    ('\u{17b4}', '\u{17b5}'),     // KHMER VOWEL INHERENT AQ, KHMER VOWEL INHERENT AA
    ('\u{180b}', '\u{180f}'),     // MONGOLIAN FREE VARIATION SELECTOR ONE..FOUR, VOWEL SEPARATOR
    ('\u{200b}', '\u{200f}'),     // ZERO WIDTH SPACE, NON-JOINER, JOINER, LRM, RLM
    ('\u{202a}', '\u{202e}'),     // LRE, RLE, PDF, LRO, RLO
    ('\u{2060}', '\u{206f}'),     // WORD JOINER, invisible operators, isolates, deprecated formats
    ('\u{2800}', '\u{2800}'),     // BRAILLE PATTERN BLANK, draws as an empty cell
    ('\u{3164}', '\u{3164}'),     // HANGUL FILLER
    ('\u{e000}', '\u{f8ff}'),     // PRIVATE USE AREA
    ('\u{fdd0}', '\u{fdef}'),     // NONCHARACTERS
    ('\u{fe00}', '\u{fe0f}'),     // VARIATION SELECTOR-1..16 (`valid_display_text` re-admits -16)
    ('\u{feff}', '\u{feff}'),     // ZERO WIDTH NO-BREAK SPACE, BYTE ORDER MARK
    ('\u{ffa0}', '\u{ffa0}'),     // HALFWIDTH HANGUL FILLER
    ('\u{fff0}', '\u{fff8}'),     // unassigned default ignorables
    ('\u{fffe}', '\u{ffff}'),     // NONCHARACTERS
    ('\u{1bca0}', '\u{1bca3}'),   // SHORTHAND FORMAT LETTER OVERLAP..UP STEP
    ('\u{1d173}', '\u{1d17a}'),   // MUSICAL SYMBOL BEGIN BEAM..END PHRASE
    ('\u{1fffe}', '\u{1ffff}'),   // NONCHARACTERS
    ('\u{2fffe}', '\u{2ffff}'),   // NONCHARACTERS
    ('\u{3fffe}', '\u{3ffff}'),   // NONCHARACTERS
    ('\u{4fffe}', '\u{4ffff}'),   // NONCHARACTERS
    ('\u{5fffe}', '\u{5ffff}'),   // NONCHARACTERS
    ('\u{6fffe}', '\u{6ffff}'),   // NONCHARACTERS
    ('\u{7fffe}', '\u{7ffff}'),   // NONCHARACTERS
    ('\u{8fffe}', '\u{8ffff}'),   // NONCHARACTERS
    ('\u{9fffe}', '\u{9ffff}'),   // NONCHARACTERS
    ('\u{afffe}', '\u{affff}'),   // NONCHARACTERS
    ('\u{bfffe}', '\u{bffff}'),   // NONCHARACTERS
    ('\u{cfffe}', '\u{cffff}'),   // NONCHARACTERS
    ('\u{dfffe}', '\u{dffff}'),   // NONCHARACTERS
    ('\u{e0000}', '\u{e0fff}'),   // TAG CHARACTERS, VARIATION SELECTOR-17..256, unassigned
    ('\u{efffe}', '\u{effff}'),   // NONCHARACTERS
    ('\u{f0000}', '\u{fffff}'),   // SUPPLEMENTARY PRIVATE USE AREA-A, with its NONCHARACTERS
    ('\u{100000}', '\u{10ffff}'), // SUPPLEMENTARY PRIVATE USE AREA-B, with its NONCHARACTERS
];

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

/// An opaque version name: 1 to 32 printable ASCII bytes of displayable text.
pub fn valid_version(version: &str) -> bool {
    (1..=MAX_VERSION_LEN).contains(&version.len())
        && valid_display_text(version)
        && version
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
}

/// A client label: at most 64 bytes without control characters.
pub fn valid_client_label(label: &str) -> bool {
    label.len() <= MAX_CLIENT_LABEL_LEN && !label.chars().any(char::is_control)
}

/// A ban reason, which the banned account reads: empty, or at most 200 bytes of displayable text.
pub fn valid_ban_reason(reason: &str) -> bool {
    reason.len() <= MAX_BAN_REASON_LEN && (reason.is_empty() || valid_display_text(reason))
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

#[cfg(test)]
mod lobby_name_tests {
    use super::*;

    #[test]
    fn lobby_names_refuse_hidden_and_deceptive_text_and_admit_every_script() {
        let refused = [
            ("empty", ""),
            ("control character", "fri\u{7}day"),
            ("next line", "fri\u{85}day"),
            ("right-to-left override", "gnp.\u{202e}exe"),
            ("left-to-right embedding", "\u{202a}night"),
            ("pop directional formatting", "night\u{202c}"),
            ("right-to-left mark", "night\u{200f}"),
            ("arabic letter mark", "night\u{61c}"),
            ("first strong isolate", "\u{2068}night\u{2069}"),
            ("zero width space only", "\u{200b}"),
            ("zero width space inside", "fri\u{200b}day"),
            ("zero width joiner", "fri\u{200d}day"),
            ("zero width non-joiner", "fri\u{200c}day"),
            ("word joiner", "fri\u{2060}day"),
            ("byte order mark", "\u{feff}friday"),
            ("soft hyphen", "fri\u{ad}day"),
            ("line separator", "fri\u{2028}day"),
            ("paragraph separator", "fri\u{2029}day"),
            ("hangul filler only", "\u{3164}"),
            ("combining grapheme joiner", "fri\u{34f}day"),
            ("mongolian free variation selector", "fri\u{180b}day"),
            ("braille pattern blank only", "\u{2800}"),
            ("private use", "fri\u{e000}day"),
            ("supplementary private use", "fri\u{f0000}day"),
            ("noncharacter in arabic block", "fri\u{fdd0}day"),
            ("noncharacter at the plane end", "fri\u{1fffe}day"),
            ("tag character", "fri\u{e0041}day"),
            ("text variation selector", "fri\u{fe0e}day"),
            ("ideographic variation selector", "\u{845b}\u{e0100}"),
            ("emoji variation selector after a letter", "a\u{fe0f}"),
            ("emoji variation selector only", "\u{fe0f}"),
            ("emoji variation selector after a space", "a \u{fe0f}"),
            ("emoji variation selector twice", "\u{2764}\u{fe0f}\u{fe0f}"),
            (
                "family emoji joined by zero width joiner",
                "👨\u{200d}👩\u{200d}👧",
            ),
            ("leading space", " friday"),
            ("trailing space", "friday "),
            ("spaces only", "   "),
            ("no-break space", "fri\u{a0}day"),
            ("ideographic space", "fri\u{3000}day"),
            ("en space", "fri\u{2002}day"),
            ("tab", "fri\tday"),
            ("two spaces in a row", "fri  day"),
            (
                "one character too many",
                &"x".repeat(MAX_LOBBY_NAME_LEN + 1),
            ),
        ];
        for (class, name) in refused {
            assert!(!valid_lobby_name(name), "{class}: {name:?}");
        }
        let admitted = [
            ("latin", "Friday Night"),
            ("accented latin", "Café Zoë Straße"),
            ("cyrillic", "Пятничная ночь"),
            ("greek", "ΟΔΟΣ"),
            ("cjk", "金曜日の夜 周五"),
            ("hangul", "금요일 밤"),
            ("arabic", "ليلة الجمعة"),
            ("hebrew", "ליל שישי"),
            ("devanagari with combining marks", "शुक्रवार रात"),
            ("emoji", "party 🎉🔥"),
            ("emoji with presentation selector", "I \u{2764}\u{fe0f} it"),
            ("emoji with skin tone", "👍🏽"),
            ("single character", "x"),
            ("exactly the longest", &"я".repeat(MAX_LOBBY_NAME_LEN)),
        ];
        for (script, name) in admitted {
            assert!(valid_lobby_name(name), "{script}: {name:?}");
        }
    }

    #[test]
    fn hidden_character_table_is_sorted_disjoint_and_refused_to_its_edges() {
        for pair in HIDDEN_CHARACTERS.windows(2) {
            assert!(
                pair[0].1 < pair[1].0,
                "{pair:?} out of order or overlapping"
            );
        }
        for &(first, last) in HIDDEN_CHARACTERS {
            assert!(first <= last, "{first:?}..{last:?}");
            for code in (first as u32)..=(last as u32) {
                if let Some(character) = char::from_u32(code) {
                    assert!(!valid_display_character(character), "U+{code:04X}");
                }
            }
        }
    }

    #[test]
    fn every_noncharacter_is_refused() {
        let planes = (0..=0x10_u32).flat_map(|plane| [plane << 16 | 0xfffe, plane << 16 | 0xffff]);
        for code in planes.chain(0xfdd0..=0xfdef) {
            let character = char::from_u32(code).expect("noncharacters are scalar values");
            assert!(!valid_display_character(character), "U+{code:04X}");
        }
    }

    #[test]
    fn every_whitespace_character_but_the_plain_space_is_refused() {
        for character in (0..=0x10ffff_u32).filter_map(char::from_u32) {
            if character.is_whitespace() && character != ' ' {
                assert!(!valid_display_character(character), "{character:?}");
            }
        }
        assert!(valid_display_character(' '));
    }

    #[test]
    fn ban_reasons_and_versions_follow_the_displayed_text_rule() {
        assert!(valid_ban_reason(""), "no reason given");
        assert!(valid_ban_reason("спам 垃圾邮件"));
        assert!(!valid_ban_reason(&"x".repeat(MAX_BAN_REASON_LEN + 1)));
        for reason in ["spam\u{202e}", "\u{200b}", " spam", "two  spaces", "a\nb"] {
            assert!(!valid_ban_reason(reason), "{reason:?}");
        }
        assert!(!valid_version("v1  beta"), "two spaces in a row");
    }
}
