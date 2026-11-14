// SPDX-License-Identifier: AGPL-3.0-only
//! Adversarial: a socket lives exactly as long as a cap names it
//! (INV-26), whichever way the tenant parts with the cap.
//!
//! ## What this test verifies
//!
//! Every socket call reaches the driver through a Socket cap, and a
//! tenant can drop or duplicate that cap with the generic cap
//! operations. `apps/sockleak` (two baked instances, every boot)
//! tries each and reports what the driver then has available:
//!
//! - **`cap_delete`** of every Socket cap: the sockets must come back.
//!   Before the fix the caps were freed and the driver sockets were
//!   not, so one tenant could empty the board for good.
//! - **`cap_copy`** then closing the original: the copy must keep the
//!   socket. Before the fix the driver socket was closed under the
//!   copy and handed out again, leaving the stale copy naming a
//!   socket that could by then belong to another tenant.
//! - **`cap_revoke`** of the Net cap (second instance): the Socket
//!   caps derived from it go with it, and the kernel must close their
//!   sockets.
//!
//! `socket_release_on_fault.rs` covers the remaining way: dying with
//! the caps still held.
//!
//! The run needs a NIC: without one no socket can be created.

use std::time::Duration;

use wari_security_tests::{boot_kernel_capture_with_net, markers};

/// Both probes run within the first second of boot; the budget covers
/// a slow TCG host.
const WALLCLOCK: Duration = Duration::from_secs(10);

#[test]
fn socket_is_closed_when_its_last_cap_goes_and_not_before() {
    let text = boot_kernel_capture_with_net(WALLCLOCK);

    assert!(
        text.contains(markers::BOOT_BANNER),
        "kernel did not boot:\n{text}",
    );

    // cap_delete: both instances got every socket back.
    assert!(
        !text.contains(markers::SOCKLEAK_DELETE_LEAK),
        "cap_delete left driver sockets allocated:\n{text}",
    );
    let delete_ok = text.matches(markers::SOCKLEAK_DELETE_OK).count();
    assert_eq!(
        delete_ok, 2,
        "expected the cap_delete case to pass in both probes, got {delete_ok}:\n{text}",
    );

    // cap_copy: a socket still named by a cap was never handed out.
    assert!(
        !text.contains(markers::SOCKLEAK_COPY_ALIAS),
        "a socket still held through a copied cap was handed out again:\n{text}",
    );
    let copy_ok = text.matches(markers::SOCKLEAK_COPY_OK).count();
    assert_eq!(
        copy_ok, 2,
        "expected the cap_copy case to pass in both probes, got {copy_ok}:\n{text}",
    );

    // cap_revoke: the kernel closed the sockets the cascade orphaned.
    assert!(
        text.contains(markers::SOCKETS_CLOSED_AT_REVOKE),
        "cap_revoke of the Net cap did not close its sockets:\n{text}",
    );

    assert!(!text.contains("panicked at"), "rust panic:\n{text}");
}
