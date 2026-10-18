//! The placeholders of a launcher's arguments, environment and public URL.
//!
//! `{match_id}`, `{port}`, `{seats}` (`ticket_hex:team:name` per slot in slot
//! order, comma separated), `{ticket_<slot>}`, `{team_<slot>}`,
//! `{name_<slot>}`; `{{` and `}}` are literal braces. Nothing else is
//! interpreted. Tickets may only go into the environment: an argument shows
//! in every process listing.

use radovid_proto::{MatchId, Ticket};

/// One seat as the game is told about it.
#[derive(Clone, Debug)]
pub struct SeatSpec {
    pub ticket: Ticket,
    pub team: u8,
    /// `[A-Za-z0-9_]`, unique within the match.
    pub name: String,
}

/// What the placeholders stand for in one launch.
pub struct Vars<'a> {
    pub match_id: MatchId,
    pub port: u16,
    pub seats: &'a [SeatSpec],
}

enum Piece<'t> {
    Text(&'t str),
    Name(&'t str),
}

/// Splits a template into text and placeholder names.
fn pieces(template: &str) -> Result<Vec<Piece<'_>>, String> {
    let mut pieces = Vec::new();
    let mut rest = template;
    while let Some(at) = rest.find(['{', '}']) {
        pieces.push(Piece::Text(&rest[..at]));
        let tail = &rest[at..];
        if let Some(after) = tail.strip_prefix("{{") {
            pieces.push(Piece::Text("{"));
            rest = after;
        } else if let Some(after) = tail.strip_prefix("}}") {
            pieces.push(Piece::Text("}"));
            rest = after;
        } else if tail.starts_with('{') {
            let end = tail
                .find('}')
                .ok_or_else(|| format!("an unclosed {{ in {template:?}"))?;
            pieces.push(Piece::Name(&tail[1..end]));
            rest = &tail[end + 1..];
        } else {
            return Err(format!("a lone }} in {template:?}"));
        }
    }
    pieces.push(Piece::Text(rest));
    Ok(pieces)
}

/// The slot a per-seat placeholder names, and whether it is a ticket.
fn per_seat(name: &str) -> Option<(usize, bool)> {
    let (kind, slot) = name.rsplit_once('_')?;
    let slot = slot.parse().ok()?;
    match kind {
        "ticket" => Some((slot, true)),
        "team" | "name" => Some((slot, false)),
        _ => None,
    }
}

/// Refuses a template with an unknown placeholder, a slot past `seats`, or a
/// ticket where `secrets_allowed` is false.
pub fn check(template: &str, seats: usize, secrets_allowed: bool) -> Result<(), String> {
    for piece in pieces(template)? {
        let Piece::Name(name) = piece else {
            continue;
        };
        let secret = match name {
            "match_id" | "port" => false,
            "seats" => true,
            _ => match per_seat(name) {
                Some((slot, _)) if slot >= seats => {
                    return Err(format!("{{{name}}}: the ruleset has {seats} seats"));
                }
                Some((_, ticket)) => ticket,
                None => return Err(format!("unknown placeholder {{{name}}}")),
            },
        };
        if secret && !secrets_allowed {
            return Err(format!(
                "{{{name}}} carries tickets: put it in env, where no process listing shows it"
            ));
        }
    }
    Ok(())
}

/// The template with its placeholders filled; `check` passed on it before.
pub fn expand(template: &str, vars: &Vars<'_>) -> String {
    let Ok(pieces) = pieces(template) else {
        return template.to_owned();
    };
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Text(text) => out.push_str(text),
            Piece::Name("match_id") => out.push_str(&vars.match_id.0.to_string()),
            Piece::Name("port") => out.push_str(&vars.port.to_string()),
            Piece::Name("seats") => {
                let seats: Vec<String> = vars
                    .seats
                    .iter()
                    .map(|s| format!("{}:{}:{}", s.ticket.to_hex(), s.team, s.name))
                    .collect();
                out.push_str(&seats.join(","));
            }
            Piece::Name(name) => {
                let Some(seat) = per_seat(name).and_then(|(slot, _)| vars.seats.get(slot)) else {
                    continue;
                };
                match name.split('_').next() {
                    Some("ticket") => out.push_str(&seat.ticket.to_hex()),
                    Some("team") => out.push_str(&seat.team.to_string()),
                    _ => out.push_str(&seat.name),
                }
            }
        }
    }
    out
}
