//! The lease helper's own tests.

#[path = "common/port_lease.rs"]
mod port_lease;

use port_lease::{LEASE_PORTS, PortLease};
use std::collections::HashSet;
use std::net::TcpListener;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Tests that reason about which ports are held run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

#[test]
fn leases_taken_from_many_threads_are_distinct() {
    let _serial = serial();
    let handles: Vec<_> = (0..16)
        .map(|_| std::thread::spawn(|| (0..20).map(|_| PortLease::new()).collect::<Vec<_>>()))
        .collect();
    let leases: Vec<PortLease> = handles
        .into_iter()
        .flat_map(|handle| handle.join().expect("a thread"))
        .collect();
    let ports: HashSet<u16> = leases.iter().map(PortLease::port).collect();
    assert_eq!(ports.len(), leases.len());
}

#[test]
fn a_port_held_by_a_foreign_listener_is_skipped() {
    let _serial = serial();
    let (foreign, port) = (LEASE_PORTS.start..LEASE_PORTS.end)
        .rev()
        .find_map(|port| Some((TcpListener::bind(("127.0.0.1", port)).ok()?, port)))
        .expect("a free port in the range to hold");
    let lease = PortLease::block_from(port, 1);
    assert_ne!(lease.port(), port);
    drop(foreign);
}

#[test]
fn a_block_is_consecutive_and_holds_every_port() {
    let _serial = serial();
    let block = PortLease::block(4);
    assert_eq!(block.last() - block.first(), 3);
    let overlapping = PortLease::block_from(block.last(), 1);
    assert!(overlapping.port() > block.last() || overlapping.port() < block.first());
}

#[cfg(target_os = "linux")]
#[test]
fn a_leased_port_is_outside_the_ephemeral_range() {
    let _serial = serial();
    let lease = PortLease::new();
    assert!(
        !port_lease::ephemeral_ports().contains(&lease.port()),
        "{} is in the ephemeral range",
        lease.port()
    );
}

#[test]
fn a_leased_port_can_be_bound_by_its_holder() {
    let _serial = serial();
    let lease = PortLease::new();
    TcpListener::bind(("127.0.0.1", lease.port())).expect("the leased port is free to use");
}

#[test]
fn a_dropped_lease_is_taken_again_from_the_same_place() {
    let _serial = serial();
    let lease = PortLease::new();
    let port = lease.port();
    assert_ne!(
        PortLease::block_from(port, 1).port(),
        port,
        "held while the guard lives"
    );
    drop(lease);
    assert_eq!(
        PortLease::block_from(port, 1).port(),
        port,
        "released on drop"
    );
}
