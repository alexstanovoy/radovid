//! Pure control-service identities. Parsing never resolves or opens a destination.
pub const MAX_MATCHMAKER_BYTES: usize = 512;
/// The control URL of a service: `wss://host[:port]` with `/control`
/// appended unless it is there already.
pub fn control_url(url: &str) -> String {
    let url = url.trim_end_matches('/');
    if url.ends_with("/control") {
        url.to_owned()
    } else {
        format!("{url}/control")
    }
}

pub fn matchmaker_identity(value: &str) -> Option<String> {
    if value.len() > MAX_MATCHMAKER_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
        || value.contains(['@', '?', '#', '\\'])
    {
        return None;
    }
    let (scheme, rest) = value.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "ws" | "wss") {
        return None;
    }
    let (authority, path) = rest
        .split_once('/')
        .map_or((rest, ""), |(host, path)| (host, path));
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let address = host.parse::<std::net::Ipv6Addr>().ok()?;
        (
            format!("[{address}]"),
            if tail.is_empty() {
                None
            } else {
                Some(tail.strip_prefix(':')?)
            },
        )
    } else {
        let (host, port) = authority
            .split_once(':')
            .map_or((authority, None), |(host, port)| (host, Some(port)));
        if host.is_empty()
            || host.split('.').any(|label| {
                label.is_empty()
                    || !label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
        {
            return None;
        }
        if host
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        {
            host.parse::<std::net::Ipv4Addr>().ok()?;
        }
        (host.to_ascii_lowercase(), port)
    };
    let port = port.map(str::parse::<u16>).transpose().ok()?;
    if port == Some(0) {
        return None;
    }
    let authority = match port {
        Some(port) if port != if scheme == "wss" { 443 } else { 80 } => format!("{host}:{port}"),
        _ => host,
    };
    let identity = control_url(&format!(
        "{scheme}://{authority}/{}",
        path.trim_end_matches('/')
    ));
    (identity.len() <= MAX_MATCHMAKER_BYTES).then_some(identity)
}

/// Identity equality never resolves or opens an address.
pub fn same_matchmaker(left: &str, right: &str) -> bool {
    matchmaker_identity(left)
        .zip(matchmaker_identity(right))
        .is_some_and(|(left, right)| left == right)
}
pub fn shareable_matchmaker_identity(value: &str) -> Option<String> {
    let identity = matchmaker_identity(value)?;
    let authority = identity.split_once("://")?.1.split('/').next()?;
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split_once(']')?.0
    } else {
        authority.split(':').next()?
    };
    let public = match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => public_ipv4(ip),
        Ok(std::net::IpAddr::V6(ip)) => public_ipv6(ip),
        Err(_) => public_hostname(host),
    };
    public.then_some(identity)
}

fn public_ipv6(ip: std::net::Ipv6Addr) -> bool {
    if ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        || ip.is_multicast()
    {
        return false;
    }
    if let Some(ipv4) = ip.to_ipv4() {
        return public_ipv4(ipv4);
    }
    let bytes = ip.octets();
    let ipv4 = |start| {
        std::net::Ipv4Addr::new(
            bytes[start],
            bytes[start + 1],
            bytes[start + 2],
            bytes[start + 3],
        )
    };
    // RFC 6052 well-known NAT64 prefix and RFC 3056 6to4's embedded site address.
    if bytes[..12] == [0, 0x64, 0xff, 0x9b, 0, 0, 0, 0, 0, 0, 0, 0] {
        return public_ipv4(ipv4(12));
    }
    if bytes[..2] == [0x20, 0x02] {
        return public_ipv4(ipv4(2));
    }
    // RFC 4380 carries the server directly and the client's address complemented.
    if bytes[..4] == [0x20, 0x01, 0, 0] {
        return public_ipv4(ipv4(4))
            && public_ipv4(std::net::Ipv4Addr::new(
                !bytes[12], !bytes[13], !bytes[14], !bytes[15],
            ));
    }
    true
}

fn public_hostname(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    if host.len() > 253
        || !host.contains('.')
        || [".localhost", ".local", ".lan", ".internal"]
            .iter()
            .any(|suffix| host.ends_with(suffix))
    {
        return false;
    }
    let numeric = host.split('.').all(|label| {
        label.bytes().all(|byte| byte.is_ascii_digit())
            || label
                .strip_prefix("0x")
                .is_some_and(|digits| digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
    });
    !numeric
        && host.split('.').all(|label| {
            label.len() <= 63
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn public_ipv4(ip: std::net::Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    !ip.is_private()
        && !ip.is_loopback()
        && !ip.is_link_local()
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && !ip.is_broadcast()
        && a != 0
        && a < 240
        && !(a == 100 && (64..=127).contains(&b))
        && !(a == 198 && (18..=19).contains(&b))
}

/// A syntactically valid control-service address, including local addresses.
pub fn public_matchmaker(value: &str) -> bool {
    matchmaker_identity(value).is_some()
}
/// A control-service address suitable for publication in a shared file.
pub fn shareable_matchmaker(value: &str) -> bool {
    shareable_matchmaker_identity(value).is_some()
}

#[cfg(test)]
mod transition_tests {
    use super::*;
    #[test]
    fn embedded_private_ipv4_and_invalid_ports_are_not_shareable() {
        for uri in [
            "wss://[::7f00:1]",
            "wss://[::127.0.0.1]",
            "wss://[::10.0.0.1]",
            "wss://[::ffff:192.168.1.2]",
            "wss://[::ffff:169.254.1.2]",
            "wss://[64:ff9b::7f00:1]",
            "wss://[64:ff9b::a00:1]",
            "wss://[64:ff9b::a9fe:102]",
            "wss://[2002:7f00:1::]",
            "wss://[2002:c0a8:102::]",
            "wss://[2002:a9fe:102::]",
            "wss://[2001:0:808:808:0:0:80ff:fffe]",
            "wss://[2001:0:7f00:1:0:0:f7f7:f7f7]",
            "wss://[2001:0:808:808:0:0:3f57:fefd]",
            "wss://[::1]",
            "wss://[fc00::1]",
            "wss://[fe80::1]",
            "wss://example.org:99999",
            "wss://example.org:0",
        ] {
            assert_eq!(shareable_matchmaker_identity(uri), None, "{uri}");
        }
        assert_eq!(
            shareable_matchmaker_identity("WSS://EXAMPLE.org:443/"),
            Some("wss://example.org/control".into())
        );
        assert!(shareable_matchmaker_identity("wss://[2606:4700:4700::1111]:65535").is_some());
        assert!(
            shareable_matchmaker_identity("wss://127.0.0.1.nip.io").is_some(),
            "hostnames are not resolved"
        );
    }
}
