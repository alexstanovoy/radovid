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
/// whitespace other than the plain space, and not in `HIDDEN_CHARACTERS`.
pub fn valid_display_character(character: char) -> bool {
    !character.is_control()
        && (character == ' ' || !character.is_whitespace())
        && !HIDDEN_CHARACTERS
            .iter()
            .any(|&(first, last)| (first..=last).contains(&character))
}

/// A piece of displayable text, such as a lobby search query: every character is one that
/// `valid_display_text` admits, but the edges, the spaces and the combining marks are free,
/// because a fragment is not shown as it is.
pub fn valid_display_fragment(fragment: &str) -> bool {
    fragment.chars().all(valid_display_character)
}

/// The one rule for free text that other players read (lobby names, ban reasons, version names):
/// non-empty, every character passes `valid_display_character`, no space at either end and no two
/// spaces in a row, no combining mark (`is_combining_mark`) at the start, after a space or more
/// than `MAX_COMBINING_MARKS` in a row, so that no text looks empty, padded, reordered, smeared
/// over its neighbours or like another text.
///
/// It admits every script and the emoji that draw as emoji by themselves. It refuses every
/// invisible character, so an emoji that needs the emoji presentation selector (U+FE0F, such as
/// a heart or a sun) is refused, as are the emoji sequences made with a zero-width joiner
/// (families, professions) and the keycaps. The zero-width joiner and non-joiner are refused
/// everywhere, which also refuses the orthography that spells words with them (Persian,
/// Kurdish and Urdu, some Indic scripts): they are also how text is hidden, and a name may be
/// spelled without them. Lengths are the caller's rule. The tables follow Unicode 16.0.
pub fn valid_display_text(text: &str) -> bool {
    if text.starts_with(' ') || text.ends_with(' ') || text.contains("  ") {
        return false;
    }
    let mut previous = ' ';
    let mut marks_in_a_row = 0;
    for character in text.chars() {
        if !valid_display_character(character) {
            return false;
        }
        if is_combining_mark(character) {
            marks_in_a_row += 1;
            if previous == ' ' || marks_in_a_row > MAX_COMBINING_MARKS {
                return false;
            }
        } else {
            marks_in_a_row = 0;
        }
        previous = character;
    }
    !text.is_empty()
}

/// The most combining marks one character may carry; Vietnamese and Hebrew with vowel points
/// need two or three.
const MAX_COMBINING_MARKS: usize = 3;

/// The combining diacritics blocks: marks that draw over the character before them. Other
/// scripts' combining marks are left alone: telling them apart takes general-category tables.
fn is_combining_mark(character: char) -> bool {
    matches!(character,
        '\u{300}'..='\u{36f}'
        | '\u{1ab0}'..='\u{1aff}'
        | '\u{1dc0}'..='\u{1dff}'
        | '\u{20d0}'..='\u{20ff}'
        | '\u{fe20}'..='\u{fe2f}')
}

/// What no displayed text may hold besides control characters and whitespace, which `char`
/// itself classifies (so U+0085, U+00A0, U+1680, U+2000..U+200A, U+2028 LINE SEPARATOR,
/// U+2029 PARAGRAPH SEPARATOR, U+202F, U+205F and U+3000 are refused as whitespace). Sorted and
/// disjoint, inclusive on both ends. It is the Unicode 16.0 format characters (category Cf,
/// among them every bidirectional control), private use (Co), noncharacters, and the
/// `Default_Ignorable_Code_Point` ones of other categories; the standard library has no table
/// for them and a dependency is not worth one. Other unassigned code points are not listed: the
/// table would age with every Unicode release, and they draw as a visible box, not as nothing.
/// `exactly_the_invisible_characters_of_unicode_are_refused_over_every_scalar_value` checks it
/// against a separate description by category.
const HIDDEN_CHARACTERS: &[(char, char)] = &[
    ('\u{ad}', '\u{ad}'),         // SOFT HYPHEN
    ('\u{34f}', '\u{34f}'),       // COMBINING GRAPHEME JOINER
    ('\u{600}', '\u{605}'), // ARABIC NUMBER SIGN..SUBTENDING MARK, which draw over the text that follows
    ('\u{61c}', '\u{61c}'), // ARABIC LETTER MARK
    ('\u{6dd}', '\u{6dd}'), // ARABIC END OF AYAH
    ('\u{70f}', '\u{70f}'), // SYRIAC ABBREVIATION MARK
    ('\u{890}', '\u{891}'), // ARABIC POUND MARK ABOVE, PIASTRE MARK ABOVE
    ('\u{8e2}', '\u{8e2}'), // ARABIC DISPUTED END OF AYAH
    ('\u{115f}', '\u{1160}'), // HANGUL CHOSEONG FILLER, JUNGSEONG FILLER
    ('\u{17b4}', '\u{17b5}'), // KHMER VOWEL INHERENT AQ, AA
    ('\u{180b}', '\u{180f}'), // MONGOLIAN FREE VARIATION SELECTOR ONE..THREE, VOWEL SEPARATOR, FOUR
    ('\u{200b}', '\u{200f}'), // ZERO WIDTH SPACE, NON-JOINER, JOINER, LEFT-TO-RIGHT MARK, RIGHT-TO-LEFT MARK
    ('\u{202a}', '\u{202e}'), // LRE, RLE, PDF, LRO, RLO
    ('\u{2060}', '\u{206f}'), // WORD JOINER, invisible operators, isolates, deprecated format characters
    ('\u{2800}', '\u{2800}'), // BRAILLE PATTERN BLANK, draws as an empty cell
    ('\u{3164}', '\u{3164}'), // HANGUL FILLER
    ('\u{e000}', '\u{f8ff}'), // PRIVATE USE AREA
    ('\u{fdd0}', '\u{fdef}'), // NONCHARACTERS
    ('\u{fe00}', '\u{fe0f}'), // VARIATION SELECTOR-1..16
    ('\u{feff}', '\u{feff}'), // ZERO WIDTH NO-BREAK SPACE, BYTE ORDER MARK
    ('\u{ffa0}', '\u{ffa0}'), // HALFWIDTH HANGUL FILLER
    ('\u{fff0}', '\u{fffb}'), // unassigned default ignorables, INTERLINEAR ANNOTATION ANCHOR..TERMINATOR
    ('\u{fffe}', '\u{ffff}'), // NONCHARACTERS
    ('\u{110bd}', '\u{110bd}'), // KAITHI NUMBER SIGN
    ('\u{110cd}', '\u{110cd}'), // KAITHI NUMBER SIGN ABOVE
    ('\u{13430}', '\u{1343f}'), // EGYPTIAN HIEROGLYPH VERTICAL JOINER..END WALLED ENCLOSURE
    ('\u{16fe4}', '\u{16fe4}'), // KHITAN SMALL SCRIPT FILLER, draws as nothing
    ('\u{1bca0}', '\u{1bca3}'), // SHORTHAND FORMAT LETTER OVERLAP..UP STEP
    ('\u{1d173}', '\u{1d17a}'), // MUSICAL SYMBOL BEGIN BEAM..END PHRASE
    ('\u{1fffe}', '\u{1ffff}'), // NONCHARACTERS
    ('\u{2fffe}', '\u{2ffff}'), // NONCHARACTERS
    ('\u{3fffe}', '\u{3ffff}'), // NONCHARACTERS
    ('\u{4fffe}', '\u{4ffff}'), // NONCHARACTERS
    ('\u{5fffe}', '\u{5ffff}'), // NONCHARACTERS
    ('\u{6fffe}', '\u{6ffff}'), // NONCHARACTERS
    ('\u{7fffe}', '\u{7ffff}'), // NONCHARACTERS
    ('\u{8fffe}', '\u{8ffff}'), // NONCHARACTERS
    ('\u{9fffe}', '\u{9ffff}'), // NONCHARACTERS
    ('\u{afffe}', '\u{affff}'), // NONCHARACTERS
    ('\u{bfffe}', '\u{bffff}'), // NONCHARACTERS
    ('\u{cfffe}', '\u{cffff}'), // NONCHARACTERS
    ('\u{dfffe}', '\u{dffff}'), // NONCHARACTERS
    ('\u{e0000}', '\u{e0fff}'), // TAG CHARACTERS, VARIATION SELECTOR-17..256, unassigned default ignorables
    ('\u{efffe}', '\u{effff}'), // NONCHARACTERS
    ('\u{f0000}', '\u{fffff}'), // SUPPLEMENTARY PRIVATE USE AREA-A, with its NONCHARACTERS
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
mod display_text_tests {
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
            (
                "emoji needing the presentation selector",
                "I \u{2764}\u{fe0f} it",
            ),
            ("keycap", "1\u{fe0f}\u{20e3}"),
            ("emoji variation selector after a letter", "a\u{fe0f}"),
            ("emoji variation selector after cyrillic", "Пятница\u{fe0f}"),
            ("emoji variation selector only", "\u{fe0f}"),
            ("emoji variation selector after a space", "a \u{fe0f}"),
            ("emoji variation selector twice", "\u{2764}\u{fe0f}\u{fe0f}"),
            (
                "family emoji joined by zero width joiner",
                "👨\u{200d}👩\u{200d}👧",
            ),
            ("egyptian hieroglyph format control", "fri\u{13430}day"),
            ("interlinear annotation anchor", "fri\u{fff9}day"),
            ("arabic number sign", "fri\u{600}day"),
            ("combining mark alone", "\u{301}"),
            ("combining mark first", "\u{323}Friday"),
            ("combining mark after a space", "Friday \u{301}night"),
            (
                "four combining marks on one letter",
                "a\u{301}\u{302}\u{303}\u{304}",
            ),
            (
                "many combining marks",
                &format!("x{}", "\u{301}".repeat(31)),
            ),
            (
                "four marks of the extended block",
                "a\u{1ab0}\u{1ab1}\u{1ab2}\u{1ab3}",
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
            ("emoji with skin tone", "👍🏽"),
            ("emoji drawn as emoji by themselves", "😀🚀🐱"),
            ("single character", "x"),
            ("vietnamese with stacked marks", "Vie\u{323}\u{302}t Nam"),
            (
                "three combining marks on one letter",
                "a\u{301}\u{302}\u{303}",
            ),
            ("exactly the longest", &"я".repeat(MAX_LOBBY_NAME_LEN)),
        ];
        for (script, name) in admitted {
            assert!(valid_lobby_name(name), "{script}: {name:?}");
        }
    }

    #[test]
    fn hidden_character_table_is_sorted_and_disjoint() {
        for pair in HIDDEN_CHARACTERS.windows(2) {
            assert!(
                pair[0].1 < pair[1].0,
                "{pair:?} out of order or overlapping"
            );
        }
        for &(first, last) in HIDDEN_CHARACTERS {
            assert!(first <= last, "{first:?}..{last:?}");
        }
    }

    /// Unicode 16.0 as `unicodedata` reports it: every code point of general category Cf or Co,
    /// listed here as ranges, and the `Default_Ignorable_Code_Point` ones of other categories.
    /// Written apart from `HIDDEN_CHARACTERS` so that a gap in either shows up as a difference.
    const FORMAT_CATEGORY: &[(u32, u32)] = &[
        (0xad, 0xad),
        (0x600, 0x605),
        (0x61c, 0x61c),
        (0x6dd, 0x6dd),
        (0x70f, 0x70f),
        (0x890, 0x891),
        (0x8e2, 0x8e2),
        (0x180e, 0x180e),
        (0x200b, 0x200f),
        (0x202a, 0x202e),
        (0x2060, 0x2064),
        (0x2066, 0x206f),
        (0xfeff, 0xfeff),
        (0xfff9, 0xfffb),
        (0x110bd, 0x110bd),
        (0x110cd, 0x110cd),
        (0x13430, 0x1343f),
        (0x1bca0, 0x1bca3),
        (0x1d173, 0x1d17a),
        (0xe0001, 0xe0001),
        (0xe0020, 0xe007f),
    ];
    const PRIVATE_USE_CATEGORY: &[(u32, u32)] =
        &[(0xe000, 0xf8ff), (0xf0000, 0xffffd), (0x10_0000, 0x10_fffd)];
    const DEFAULT_IGNORABLE_OF_OTHER_CATEGORIES: &[(u32, u32)] = &[
        (0x34f, 0x34f),
        (0x115f, 0x1160),
        (0x17b4, 0x17b5),
        (0x180b, 0x180d),
        (0x180f, 0x180f),
        (0x2065, 0x2065),
        (0x3164, 0x3164),
        (0xfe00, 0xfe0f),
        (0xffa0, 0xffa0),
        (0xfff0, 0xfff8),
        (0xe0000, 0xe0fff),
    ];
    /// Not in any Unicode category of invisible characters, but draws as an empty cell.
    const DRAWN_EMPTY: &[(u32, u32)] = &[(0x2800, 0x2800), (0x16fe4, 0x16fe4)];

    fn is_noncharacter(code: u32) -> bool {
        (0xfdd0..=0xfdef).contains(&code) || code & 0xfffe == 0xfffe
    }

    #[test]
    fn exactly_the_invisible_characters_of_unicode_are_refused_over_every_scalar_value() {
        let listed = |ranges: &[(u32, u32)], code: u32| {
            ranges
                .iter()
                .any(|&(first, last)| (first..=last).contains(&code))
        };
        for character in (0..=0x10_ffff_u32).filter_map(char::from_u32) {
            let code = character as u32;
            let invisible = character.is_control()
                || (character.is_whitespace() && character != ' ')
                || is_noncharacter(code)
                || [
                    FORMAT_CATEGORY,
                    PRIVATE_USE_CATEGORY,
                    DEFAULT_IGNORABLE_OF_OTHER_CATEGORIES,
                    DRAWN_EMPTY,
                ]
                .iter()
                .any(|ranges| listed(ranges, code));
            assert_eq!(
                valid_display_character(character),
                !invisible,
                "U+{code:04X} {}",
                if invisible {
                    "is invisible and admitted"
                } else {
                    "is shown and refused"
                }
            );
        }
    }

    #[test]
    fn a_character_is_admitted_in_a_name_exactly_when_it_is_admitted_in_a_search_query() {
        for character in (0..=0x10_ffff_u32).filter_map(char::from_u32) {
            assert_eq!(
                valid_display_text(&format!("a{character}b")),
                valid_display_fragment(&character.to_string()),
                "U+{:04X}",
                character as u32
            );
        }
    }

    #[test]
    fn a_fragment_may_have_edge_spaces_double_spaces_and_a_leading_mark_but_no_hidden_character() {
        for fragment in [
            "",
            " ",
            "fri ",
            "  night",
            "a  b",
            "\u{301}",
            "\u{301}\u{302}\u{303}\u{304}",
        ] {
            assert!(valid_display_fragment(fragment), "{fragment:?}");
        }
        for fragment in ["\u{202e}", "a\u{fe0f}", "a\tb", "a\u{a0}"] {
            assert!(!valid_display_fragment(fragment), "{fragment:?}");
        }
    }

    #[test]
    fn ban_reasons_and_versions_follow_the_displayed_text_rule() {
        assert!(valid_ban_reason(""), "no reason given");
        assert!(valid_ban_reason("спам 垃圾邮件"));
        assert!(!valid_ban_reason(&"x".repeat(MAX_BAN_REASON_LEN + 1)));
        for reason in [
            "spam\u{202e}",
            "\u{200b}",
            " spam",
            "two  spaces",
            "a\nb",
            "\u{301}",
        ] {
            assert!(!valid_ban_reason(reason), "{reason:?}");
        }
        assert!(!valid_version("v1  beta"), "two spaces in a row");
    }
}
