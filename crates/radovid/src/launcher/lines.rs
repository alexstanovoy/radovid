//! What a game server says on stdout: `READY`, then one
//! `RESULT winner=<team>|draw duration_ms=<n> report=<hex> [<key>=<value>]...`. Every other line
//! is ignored; a line longer than any `RESULT` can be is skipped whole.
//!
//! The first three fields are fixed and in this order. The rest are `key=value` fields in any
//! order, each key once: `left=<slot>`, `replay=truncated` (version 2), `v=<n>`. A key radovid does not know
//! is skipped, so a game may add facts without breaking the radovid that reads it; a game that
//! changes what a field means says so with `v=<n>` (absent: 1), and a radovid that knows only
//! older versions refuses the line (README "Launcher contract").

use radovid_proto::MAX_GAME_REPORT_LEN;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use crate::config::decode_hex;

/// The newest `RESULT` version this radovid reads: 1 is the line of the first games, 2 adds
/// `replay=truncated`. Every game is told it (`RADOVID_RESULT_VERSION`), and prints the fields
/// of newer versions only when it is.
pub const RESULT_VERSION: u16 = 2;
/// The variable that tells a game [`RESULT_VERSION`].
pub const RESULT_VERSION_VAR: &str = "RADOVID_RESULT_VERSION";
/// The most fields after the report: a line cannot make the parser do unbounded work.
const MAX_EXTRA_FIELDS: usize = 16;

/// The longest line read: a `RESULT` with the largest report.
const MAX_LINE: usize = 2 * MAX_GAME_REPORT_LEN + 128;

/// A line of the contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    Ready,
    Result(GameResult),
    /// A `RESULT` line that does not parse.
    BadResult,
}

/// What the game reports at the end of a match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameResult {
    /// The winning team, or none for a draw.
    pub winner: Option<u8>,
    pub duration_ms: u32,
    pub report: Vec<u8>,
    /// The slot of a player who left the match for good: the other team won by it.
    pub left: Option<u8>,
    /// The game says its artifact was closed short of the match's end (a size limit).
    pub artifact_truncated: bool,
}

/// Reads lines of bounded length.
pub struct LineReader<R> {
    reader: BufReader<R>,
    line: Vec<u8>,
}

impl<R: AsyncRead + Unpin> LineReader<R> {
    pub fn new(reader: R) -> Self {
        LineReader {
            reader: BufReader::new(reader),
            line: Vec::new(),
        }
    }

    /// The next line of the contract; `None` at the end of the stream.
    pub async fn next(&mut self) -> Option<Line> {
        loop {
            let line = self.next_line().await?;
            if let Some(line) = parse(&line) {
                return Some(line);
            }
        }
    }

    /// The next line within the bound, without its end; longer ones are skipped.
    pub(super) async fn next_line(&mut self) -> Option<Vec<u8>> {
        self.line.clear();
        let mut too_long = false;
        loop {
            let buf = self.reader.fill_buf().await.ok()?;
            if buf.is_empty() {
                // The last line may lack its end.
                return (!self.line.is_empty() && !too_long)
                    .then(|| std::mem::take(&mut self.line));
            }
            let (chunk, done) = match buf.iter().position(|&b| b == b'\n') {
                Some(end) => (&buf[..end], Some(end + 1)),
                None => (buf, None),
            };
            if self.line.len() + chunk.len() > MAX_LINE {
                too_long = true;
                self.line.clear();
            } else if !too_long {
                self.line.extend_from_slice(chunk);
            }
            let used = done.unwrap_or(buf.len());
            self.reader.consume(used);
            if done.is_some() {
                if too_long {
                    too_long = false;
                    continue;
                }
                return Some(std::mem::take(&mut self.line));
            }
        }
    }
}

fn parse(line: &[u8]) -> Option<Line> {
    let line = std::str::from_utf8(line).ok()?.trim_end_matches('\r');
    if line == "READY" {
        return Some(Line::Ready);
    }
    let rest = line.strip_prefix("RESULT ")?;
    Some(parse_result(rest).map_or(Line::BadResult, Line::Result))
}

fn parse_result(rest: &str) -> Option<GameResult> {
    let mut fields = rest.split(' ');
    let winner = match fields.next()?.strip_prefix("winner=")? {
        "draw" => None,
        team => Some(team.parse().ok()?),
    };
    let duration_ms = fields.next()?.strip_prefix("duration_ms=")?.parse().ok()?;
    let report = decode_hex(fields.next()?.strip_prefix("report=")?)?;
    if report.len() > MAX_GAME_REPORT_LEN {
        return None;
    }
    let mut result = GameResult {
        winner,
        duration_ms,
        report,
        left: None,
        artifact_truncated: false,
    };
    let mut seen = Vec::new();
    for (count, field) in fields.enumerate() {
        let (key, value) = field.split_once('=').filter(|(key, _)| !key.is_empty())?;
        if count >= MAX_EXTRA_FIELDS || seen.contains(&key) {
            return None;
        }
        seen.push(key);
        match key {
            "left" => result.left = Some(value.parse().ok()?),
            "replay" => result.artifact_truncated = value == "truncated",
            "v" if value.parse::<u16>().ok()? > RESULT_VERSION => return None,
            _ => {}
        }
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(extra: &str) -> Option<GameResult> {
        parse_result(&format!("winner=1 duration_ms=5 report=0aff{extra}"))
    }

    /// The contract in README "Launcher contract": old lines parse, new fields are skipped,
    /// a newer version or a damaged field is refused.
    #[test]
    fn a_result_line_reads_old_and_future_games_and_refuses_what_it_cannot_know() {
        let old = parsed("").expect("a line of the first games");
        assert_eq!(
            (old.winner, old.report, old.left),
            (Some(1), vec![10, 255], None)
        );
        assert_eq!(parsed(" left=1").expect("left").left, Some(1));
        let newer = parsed(" replay=truncated future=whatever left=0 v=2").expect("skipped");
        assert!(newer.artifact_truncated && newer.left == Some(0));
        for bad in [
            " v=3",
            " left",
            " =x",
            " left=0 left=1",
            " left=x",
            " v=x",
            "  ",
        ] {
            assert!(parsed(bad).is_none(), "{bad:?} is refused");
        }
        let many = (0..=MAX_EXTRA_FIELDS).fold(String::new(), |s, i| format!("{s} k{i}=1"));
        assert!(parsed(&many).is_none(), "an endless tail is refused");
    }
}
