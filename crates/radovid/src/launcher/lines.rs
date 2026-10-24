//! What a game server says on stdout: `READY`, then one
//! `RESULT winner=<team>|draw duration_ms=<n> report=<hex> [left=<slot>]`. Every other line
//! is ignored; a line longer than any `RESULT` can be is skipped whole.

use radovid_proto::MAX_GAME_REPORT_LEN;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use crate::config::decode_hex;

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
    let left = match fields.next() {
        None => None,
        Some(field) => Some(field.strip_prefix("left=")?.parse().ok()?),
    };
    if fields.next().is_some() || report.len() > MAX_GAME_REPORT_LEN {
        return None;
    }
    Some(GameResult {
        winner,
        duration_ms,
        report,
        left,
    })
}
