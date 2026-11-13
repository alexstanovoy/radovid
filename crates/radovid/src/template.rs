//! The placeholders of a launcher's arguments, environment and public URL.
//!
//! `{match_id}`, `{port}`, `{seats}` (`ticket_hex:team:name:kind` per slot in
//! slot order, comma separated; kind is `bot` or `human`, the account's own),
//! `{ticket_<slot>}`, `{team_<slot>}`, `{name_<slot>}`, `{kind_<slot>}`; `{{` and `}}` are literal braces. Nothing else is
//! interpreted. `{accounts}` and `{account_<slot>}` are public account ids in slot order. Tickets may only go into the environment: an argument shows
//! in every process listing.

use radovid_proto::{AccountId, AccountKind, MatchId, Ticket};

/// One seat as the game is told about it.
#[derive(Clone, Debug)]
pub struct SeatSpec {
    pub account: AccountId,
    pub ticket: Ticket,
    pub team: u8,
    /// `[A-Za-z0-9_]`, unique within the match.
    pub name: String,
    /// What the account is; only an administrator changes it.
    pub kind: AccountKind,
}

impl SeatSpec {
    fn kind_word(&self) -> &'static str {
        match self.kind {
            AccountKind::Human => "human",
            AccountKind::Bot => "bot",
        }
    }
}

/// What the placeholders stand for in one launch.
pub struct Vars<'a> {
    pub match_id: MatchId,
    pub port: u16,
    pub seats: &'a [SeatSpec],
    /// Where the game writes its artifact, as the game sees the path.
    pub artifact: Option<&'a str>,
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
        "team" | "name" | "kind" | "account" => Some((slot, false)),
        _ => None,
    }
}

/// Refuses a template with an unknown placeholder, a slot past `seats`, or a
/// ticket where `secrets_allowed` is false.
pub fn check(template: &str, seats: usize, secrets_allowed: bool) -> Result<(), String> {
    check_with(template, seats, secrets_allowed, false)
}

/// As [`check`], also allowing `{artifact}` (the file the game leaves for the match) when
/// `artifact` is set.
pub fn check_with(
    template: &str,
    seats: usize,
    secrets_allowed: bool,
    artifact: bool,
) -> Result<(), String> {
    for piece in pieces(template)? {
        let Piece::Name(name) = piece else {
            continue;
        };
        let secret = match name {
            "match_id" | "port" | "accounts" => false,
            "artifact" if artifact => false,
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
            Piece::Name("artifact") => out.push_str(vars.artifact.unwrap_or_default()),
            Piece::Name("accounts") => {
                out.push_str(
                    &vars
                        .seats
                        .iter()
                        .map(|seat| seat.account.0.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                );
            }
            Piece::Name("seats") => {
                let seats: Vec<String> = vars
                    .seats
                    .iter()
                    .map(|s| {
                        format!(
                            "{}:{}:{}:{}",
                            s.ticket.to_hex(),
                            s.team,
                            s.name,
                            s.kind_word()
                        )
                    })
                    .collect();
                out.push_str(&seats.join(","));
            }
            Piece::Name(name) => {
                let Some(seat) = per_seat(name).and_then(|(slot, _)| vars.seats.get(slot)) else {
                    continue;
                };
                match name.split('_').next() {
                    Some("account") => out.push_str(&seat.account.0.to_string()),
                    Some("ticket") => out.push_str(&seat.ticket.to_hex()),
                    Some("team") => out.push_str(&seat.team.to_string()),
                    Some("kind") => out.push_str(seat.kind_word()),
                    _ => out.push_str(&seat.name),
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod profile_tests {
    use super::*;
    #[test]
    fn public_account_placeholders_are_allowed_without_revealing_seat_tickets() {
        assert!(
            check("{accounts}/{account_0}", 2, false).is_ok(),
            "public identifiers belong in public launch context"
        );
        let seats = [
            SeatSpec {
                account: AccountId(42),
                ticket: Ticket([7; 32]),
                team: 0,
                name: "player".into(),
                kind: AccountKind::Human,
            },
            SeatSpec {
                account: AccountId(99),
                ticket: Ticket([8; 32]),
                team: 1,
                name: "bot".into(),
                kind: AccountKind::Bot,
            },
        ];
        let vars = Vars {
            match_id: MatchId(1),
            port: 1,
            seats: &seats,
            artifact: None,
        };
        assert_eq!(expand("{accounts}/{account_1}", &vars), "42,99/99");
        assert!(check("{account_2}", 2, false).is_err());
        assert!(
            check("{seats}", 2, false).is_err(),
            "existing ticket-bearing format remains secret"
        );
    }
}
