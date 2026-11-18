//! What a person types or pastes for the address of a service, and the one form it is kept in.

/// Longest address typed, and longest canonical address: what a text field takes, well under
/// [`crate::MAX_MATCHMAKER_BYTES`].
pub const MAX_SERVICE_ADDRESS_BYTES: usize = 200;

/// The part of a typed address that cannot be one. Every case names what to change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressFault {
    /// Nothing typed.
    Empty,
    /// More than [`MAX_SERVICE_ADDRESS_BYTES`].
    TooLong,
    /// A space or a line break inside the address.
    Spaces,
    /// A control character.
    Characters,
    /// A scheme that is no way to reach a service (`ftp://`, `tcp://`).
    Scheme,
    /// `ws://` or `http://`: services are reached over a secure connection only.
    Plain,
    /// A login in front of the host (`user@host`).
    Credentials,
    /// A `?` or `#` in a `wss://` address.
    QueryOrFragment,
    /// The host is empty or is no host name or number.
    Host,
    /// The port is not a number from 1 to 65535.
    Port,
    /// The path holds an empty part or a backslash.
    Path,
}

/// The canonical form of the address of a service, `wss://host[:port][/path]`: the scheme and
/// host in lower case, the port left out when it is 443, no trailing slash, and no `/control`
/// (every client adds it, see [`crate::control_url`]).
///
/// Accepted: `host`, `host:port`, `wss://host`, an address with spaces around it or a trailing
/// slash, and the address of a page the service serves (`https://host/anything`: only its origin
/// is the service's). Refused with the reason: anything that cannot be an address.
///
/// The result is always a valid [`crate::matchmaker_identity`] and its own canonical form.
pub fn service_address(typed: &str) -> Result<String, AddressFault> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Err(AddressFault::Empty);
    }
    if typed.len() > MAX_SERVICE_ADDRESS_BYTES {
        return Err(AddressFault::TooLong);
    }
    if typed.chars().any(char::is_whitespace) {
        return Err(AddressFault::Spaces);
    }
    if typed.chars().any(char::is_control) {
        return Err(AddressFault::Characters);
    }
    let (page, rest) = match typed.split_once("://") {
        None => (false, typed),
        Some((scheme, rest)) => match scheme.to_ascii_lowercase().as_str() {
            "wss" => (false, rest),
            "https" => (true, rest),
            "ws" | "http" => return Err(AddressFault::Plain),
            _ => return Err(AddressFault::Scheme),
        },
    };
    let at = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(at);
    // A page address carries the page's own path, query and fragment: none is the service's.
    let tail = if page { "" } else { tail };
    if tail.contains(['?', '#']) {
        return Err(AddressFault::QueryOrFragment);
    }
    if authority.contains('@') {
        return Err(AddressFault::Credentials);
    }
    let (host, port) = split_authority(authority)?;
    let path = canonical_path(tail)?;
    let port = match port {
        Some(443) | None => String::new(),
        Some(port) => format!(":{port}"),
    };
    let canonical = format!("wss://{host}{port}{path}");
    // The canonical form is read again, so it is within the limit it is read by.
    if canonical.len() > MAX_SERVICE_ADDRESS_BYTES {
        return Err(AddressFault::TooLong);
    }
    Ok(canonical)
}

/// The host in canonical form and the port, if one is given.
fn split_authority(authority: &str) -> Result<(String, Option<u16>), AddressFault> {
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest.split_once(']').ok_or(AddressFault::Host)?;
        let address = host
            .parse::<std::net::Ipv6Addr>()
            .map_err(|_| AddressFault::Host)?;
        let port = match tail {
            "" => None,
            tail => Some(tail.strip_prefix(':').ok_or(AddressFault::Host)?),
        };
        (format!("[{address}]"), port)
    } else {
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        (valid_host(host)?, port)
    };
    let port = port
        .map(|port| match port.parse::<u16>() {
            Ok(0) | Err(_) => Err(AddressFault::Port),
            Ok(port) => Ok(port),
        })
        .transpose()?;
    Ok((host, port))
}

/// A host name or an IPv4 address, in lower case.
fn valid_host(host: &str) -> Result<String, AddressFault> {
    let label = |label: &str| {
        (1..=63).contains(&label.len())
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    if host.is_empty() || host.len() > 253 || !host.split('.').all(label) {
        return Err(AddressFault::Host);
    }
    let numeric = host
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.');
    if numeric && host.parse::<std::net::Ipv4Addr>().is_err() {
        return Err(AddressFault::Host);
    }
    Ok(host.to_ascii_lowercase())
}

/// The path of a `wss://` address: `""` for none, else `/part/part` without a trailing slash and
/// without the `/control` every client appends.
fn canonical_path(tail: &str) -> Result<String, AddressFault> {
    if tail.contains("//") {
        return Err(AddressFault::Path);
    }
    let path = tail.trim_end_matches('/');
    let path = path.strip_suffix("/control").unwrap_or(path);
    if path.is_empty() {
        return Ok(String::new());
    }
    let graphic = path.bytes().all(|byte| byte.is_ascii_graphic());
    // `@` is what an address keeps for a login (and no identity holds it).
    if !graphic || path.contains(['\\', '@']) {
        return Err(AddressFault::Path);
    }
    Ok(path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matchmaker_identity;

    #[test]
    fn what_people_type_becomes_the_one_canonical_address() {
        for typed in [
            "play.example.org",
            "wss://play.example.org",
            "WSS://Play.Example.Org/",
            "  play.example.org  ",
            "wss://play.example.org:443",
            "wss://play.example.org/control",
            "wss://play.example.org/control/",
            "https://play.example.org",
            "https://play.example.org/play/index.html?room=3#top",
            "HTTPS://play.example.org:443/",
        ] {
            assert_eq!(
                service_address(typed).as_deref(),
                Ok("wss://play.example.org"),
                "{typed:?}"
            );
        }
    }

    #[test]
    fn ports_paths_and_numbers_are_kept_where_they_belong_to_the_service() {
        for (typed, canonical) in [
            ("example.org:4460", "wss://example.org:4460"),
            ("https://example.org:8443/page", "wss://example.org:8443"),
            (
                "wss://example.org/matchmaking",
                "wss://example.org/matchmaking",
            ),
            ("wss://example.org/a/b/control", "wss://example.org/a/b"),
            ("127.0.0.1:4460", "wss://127.0.0.1:4460"),
            ("[::1]:4460", "wss://[::1]:4460"),
            ("[2001:DB8::1]", "wss://[2001:db8::1]"),
            ("localhost", "wss://localhost"),
        ] {
            assert_eq!(
                service_address(typed).as_deref(),
                Ok(canonical),
                "{typed:?}"
            );
        }
    }

    #[test]
    fn what_cannot_be_an_address_is_refused_with_the_part_at_fault() {
        for (typed, fault) in [
            ("", AddressFault::Empty),
            ("   ", AddressFault::Empty),
            ("play example.org", AddressFault::Spaces),
            ("wss://play.example.org/a b", AddressFault::Spaces),
            ("wss://play.example.org\u{7}", AddressFault::Characters),
            ("ws://example.org", AddressFault::Plain),
            ("http://example.org/page", AddressFault::Plain),
            ("ftp://example.org", AddressFault::Scheme),
            ("tcp://example.org:4455", AddressFault::Scheme),
            ("wss://user:secret@example.org", AddressFault::Credentials),
            (
                "wss://example.org/control?token=1",
                AddressFault::QueryOrFragment,
            ),
            ("wss://example.org#top", AddressFault::QueryOrFragment),
            ("wss://", AddressFault::Host),
            ("wss://ошибочный_путь", AddressFault::Host),
            ("wss://wrong_host", AddressFault::Host),
            ("wss://-example.org", AddressFault::Host),
            ("wss://example..org", AddressFault::Host),
            ("wss://999.1.1.1", AddressFault::Host),
            ("wss://[::1", AddressFault::Host),
            ("wss://[nope]", AddressFault::Host),
            ("example.org:0", AddressFault::Port),
            ("example.org:65536", AddressFault::Port),
            ("example.org:port", AddressFault::Port),
            ("example.org:", AddressFault::Port),
            ("wss://example.org//control", AddressFault::Path),
            ("wss://example.org/a\\b", AddressFault::Path),
            // Found by the fuzz target: a path with an @ is no identity of a service.
            ("0j/@", AddressFault::Path),
            ("wss://example.org/a@b", AddressFault::Path),
        ] {
            assert_eq!(service_address(typed), Err(fault), "{typed:?}");
        }
        let long = format!("{}.org", "a".repeat(MAX_SERVICE_ADDRESS_BYTES));
        assert_eq!(service_address(&long), Err(AddressFault::TooLong));
        // Found by the fuzz target: the canonical form is longer than what was typed (`wss://`).
        let host = "a".repeat(60);
        let almost = format!(
            "{host}.{host}.{host}/{}",
            "p".repeat(MAX_SERVICE_ADDRESS_BYTES - 183)
        );
        assert_eq!(almost.len(), MAX_SERVICE_ADDRESS_BYTES);
        assert_eq!(service_address(&almost), Err(AddressFault::TooLong));
    }

    #[test]
    fn a_canonical_address_is_its_own_canonical_form_and_an_identity_of_the_protocol() {
        for typed in [
            "play.example.org",
            "example.org:4460",
            "https://example.org:8443/x",
            "wss://example.org/prefix/control",
            "[2001:DB8::1]:4460",
            "10.0.0.7:1",
        ] {
            let canonical = service_address(typed).expect("an address");
            assert_eq!(
                service_address(&canonical).as_deref(),
                Ok(canonical.as_str())
            );
            assert!(
                matchmaker_identity(&canonical).is_some(),
                "{canonical} is a control service identity"
            );
        }
    }
}
