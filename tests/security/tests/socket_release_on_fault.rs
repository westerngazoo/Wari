// SPDX-License-Identifier: AGPL-3.0-only
//! Adversarial: a tenant that dies holding sockets does not keep them.
//!
//! ## What this test verifies
//!
//! The net driver has four sockets for the whole board. The kernel
//! bakes two instances of `apps/sockleak` and runs them in order at
//! every boot. Each one takes every socket the driver has left and
//! then traps without closing any.
//!
//! - The first instance always finds free sockets, and ends by
//!   trapping with them open.
//! - The second finds one **only if** the kernel closed the first
//!   one's sockets when it faulted
//!   (`kernel/src/sched/mod.rs` → `cap::net_socket_close_all`).
//!
//! Without that cleanup the second instance prints
//! `[sockleak] no socket available`: the dead tenant still owns the
//! driver's sockets, and so would every later tenant find it.
//!
//! The other ways a tenant can part with a Socket cap (`cap_delete`,
//! `cap_copy`, `cap_revoke`) are the same probe's earlier steps,
//! checked in `socket_cap_lifetime.rs`.
//!
//! The run needs a NIC: without one the net driver never initialises
//! and no socket can be created at all.

use std::time::Duration;

use wari_security_tests::{boot_kernel_capture_with_net, markers};

/// Both probes run within the first second of boot; the budget covers
/// a slow TCG host.
const WALLCLOCK: Duration = Duration::from_secs(10);

#[test]
fn dead_tenant_does_not_keep_its_sockets() {
    let text = boot_kernel_capture_with_net(WALLCLOCK);

    assert!(
        text.contains(markers::BOOT_BANNER),
        "kernel did not boot:\n{text}",
    );

    // The attack ran, twice: each probe held sockets and died.
    let held = text.matches(markers::SOCKLEAK_HOLDING).count();
    assert_eq!(
        held, 2,
        "expected both socket-leak probes to obtain sockets, got {held}:\n{text}",
    );

    // The defect's signature: a probe found the driver empty.
    assert!(
        !text.contains(markers::SOCKLEAK_NONE_LEFT),
        "a dead tenant kept the driver's sockets:\n{text}",
    );

    // The kernel says it cleaned up after the one that died with
    // sockets open (the other revokes them itself before dying).
    assert!(
        text.contains(markers::SOCKETS_CLOSED_AT_EXIT),
        "no socket-cleanup line for the probe that died holding sockets:\n{text}",
    );

    assert!(!text.contains("panicked at"), "rust panic:\n{text}");
}
