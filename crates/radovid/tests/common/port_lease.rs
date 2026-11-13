//! Loopback ports for tests that must hand a port NUMBER to another process or a config (a
//! service address, a game port range, an address that has to stay closed). A test that owns its
//! socket binds port 0 itself and needs none of this.
//!
//! A port found by "bind 0, read it, drop the listener" is free only until the next bind(0) or
//! outgoing connection of any process takes it, so a lease holds one for as long as the guard
//! lives, from a range outside the kernel's ephemeral range. The hold is an abstract unix socket
//! named after the port: machine-wide, released by the kernel when the process dies, and no
//! file to clean up. Other projects whose tests share a machine with this one carry the same
//! helper, name scheme and range (they share no crate), so their tests never lease the same port.

use std::ops::Range;
use std::sync::atomic::{AtomicU16, Ordering};

/// Never overlaps the kernel's ephemeral range (checked on every lease) and stays clear of the
/// fixed ports of our tooling: 22000-22020 (the local stand), 20000-20015 (the example config).
pub const LEASE_PORTS: Range<u16> = 24_000..26_000;

/// Ports held for the lifetime of the value: `first()..=last()`.
#[derive(Debug)]
pub struct PortLease {
    first: u16,
    count: u16,
    #[cfg(target_os = "linux")]
    _claims: Vec<std::os::unix::net::UnixListener>,
}

impl PortLease {
    /// One leased port.
    pub fn new() -> PortLease {
        PortLease::block(1)
    }

    /// `count` consecutive leased ports, searched from a start that differs between processes.
    pub fn block(count: u16) -> PortLease {
        static NEXT: AtomicU16 = AtomicU16::new(0);
        let step = NEXT.fetch_add(count, Ordering::Relaxed);
        let size = LEASE_PORTS.len();
        let from = (std::process::id() as usize + usize::from(step)) % size;
        PortLease::block_from(LEASE_PORTS.start + from as u16, count)
    }

    /// The first block of `count` ports at or after `from` (wrapping round the range) that no
    /// other lease holds and nobody listens on.
    pub fn block_from(from: u16, count: u16) -> PortLease {
        let size = LEASE_PORTS.len();
        assert!(
            LEASE_PORTS.contains(&from),
            "{from} is outside {LEASE_PORTS:?}"
        );
        assert!(
            (1..=size).contains(&usize::from(count)),
            "a block of {count} ports"
        );
        assert_outside_ephemeral_range();
        let begin = usize::from(from - LEASE_PORTS.start);
        for step in 0..size {
            let offset = (begin + step) % size;
            if offset + usize::from(count) > size {
                continue;
            }
            let first = LEASE_PORTS.start + offset as u16;
            if let Some(lease) = PortLease::try_take(first, count) {
                return lease;
            }
        }
        panic!(
            "no block of {count} free ports among the {size} test-port lease ports {LEASE_PORTS:?}: \
             leaked test processes or foreign listeners hold them"
        );
    }

    #[cfg(target_os = "linux")]
    fn try_take(first: u16, count: u16) -> Option<PortLease> {
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::{SocketAddr, UnixListener};
        let mut claims = Vec::new();
        for port in first..first + count {
            let name = format!("test-port-lease-{port}");
            let address = SocketAddr::from_abstract_name(name).expect("an abstract name");
            match UnixListener::bind_addr(&address) {
                Ok(claim) => claims.push(claim),
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => return None,
                Err(error) => panic!("claim of test port {port} failed: {error}"),
            }
        }
        // A process outside the tests may hold the port; the probe is dropped at once.
        let free = (first..first + count)
            .all(|port| std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
        free.then_some(PortLease {
            first,
            count,
            _claims: claims,
        })
    }

    /// Without abstract sockets the old behaviour stays: a port is free until someone takes it.
    #[cfg(not(target_os = "linux"))]
    fn try_take(first: u16, count: u16) -> Option<PortLease> {
        let free = (first..first + count)
            .all(|port| std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
        free.then_some(PortLease { first, count })
    }

    /// The port of a one-port lease.
    pub fn port(&self) -> u16 {
        assert_eq!(self.count, 1, "port() of a block of {} ports", self.count);
        self.first
    }

    pub fn first(&self) -> u16 {
        self.first
    }

    pub fn last(&self) -> u16 {
        self.first + self.count - 1
    }
}

impl Default for PortLease {
    fn default() -> PortLease {
        PortLease::new()
    }
}

/// The kernel's `ip_local_port_range`: every outgoing connection and bind(0) draws from it.
#[cfg(target_os = "linux")]
pub fn ephemeral_ports() -> std::ops::RangeInclusive<u16> {
    let text = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .expect("the kernel's ephemeral port range");
    let mut ports = text.split_whitespace().map(|word| word.parse::<u16>());
    match (ports.next(), ports.next()) {
        (Some(Ok(low)), Some(Ok(high))) => low..=high,
        _ => panic!("unreadable ip_local_port_range {text:?}"),
    }
}

#[cfg(target_os = "linux")]
fn assert_outside_ephemeral_range() {
    let ephemeral = ephemeral_ports();
    assert!(
        LEASE_PORTS.end <= *ephemeral.start() || LEASE_PORTS.start > *ephemeral.end(),
        "the test-port lease range {LEASE_PORTS:?} overlaps the ephemeral range {ephemeral:?}"
    );
}

#[cfg(not(target_os = "linux"))]
fn assert_outside_ephemeral_range() {}
