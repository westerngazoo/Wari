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
//! - The first instance always finds free sockets.
//! - The second finds one **only if** the kernel closed the first
//!   one's sockets when it faulted
//!   (`kernel/src/sched/mod.rs` → `cap::net_socket_close_all`).
//!
//! Without that cleanup the second instance prints
//! `[sockleak] no socket available`: the dead tenant still owns the
//! driver's sockets, and so would every later tenant find it.
//!
//! ## What it does not cover
//!
//! The cleanup walks the Socket caps the tenant still holds. A tenant
//! that drops a Socket cap with `cap_delete` or `cap_revoke` before
//! dying frees the cap without closing the driver socket; that leak
//! exists for a living tenant too and is tracked separately.
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

    // The kernel says it cleaned up after each of them.
    let cleaned = text.matches(markers::SOCKETS_CLOSED_AT_EXIT).count();
    assert!(
        cleaned >= 2,
        "expected a socket-cleanup line per probe, got {cleaned}:\n{text}",
    );

    assert!(!text.contains("panicked at"), "rust panic:\n{text}");
}
