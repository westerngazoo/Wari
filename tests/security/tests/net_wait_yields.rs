// SPDX-License-Identifier: AGPL-3.0-only
//! A tenant waiting for a client does not hold the hart.
//!
//! The HTTP demo runs in two tenants that both call
//! `net_socket_accept` on port 7000. With no client yet the kernel
//! blocks each one (`BlockReason::NetWait`) instead of letting it spin
//! in the host fn, so both wait at the same time and every other
//! tenant (guard, modreg spawns, the `apps/hog` fuel probe) runs
//! meanwhile.
//!
//! Before the fix the first tenant busy-polled accept for its whole
//! 60 s window: the second never opened its window and nothing else
//! ran until the first gave up.
//!
//! A client connecting late in the window is checked on the VF2, not
//! here: TCP through QEMU's user-mode forward gets no reply from the
//! demo on any build tried (back to build 157, before this change), so
//! a QEMU client would test that gap rather than this change.

use std::time::Duration;

use wari_security_tests::{boot_kernel_capture_with_net, markers};

/// Long enough for both windows to open and the other tenants to run,
/// far short of the 60 s window, so both demo tenants are still
/// waiting when the run ends.
const WALLCLOCK: Duration = Duration::from_secs(10);

/// Printed by the kernel when a tenant first calls accept on a socket.
const ACCEPT_WINDOW_OPEN: &str = "[accept] window open";

/// Printed by the fuel probe right before it spins; it is one of the
/// tenants that must run while the demo waits.
const HOG_BANNER: &str = "[hog] spinning without yielding";

#[test]
fn waiting_tenants_yield_the_hart() {
    let text = boot_kernel_capture_with_net(WALLCLOCK);

    assert!(
        text.contains(markers::BOOT_BANNER),
        "kernel did not boot:\n{text}"
    );

    let opens: Vec<usize> = text
        .match_indices(ACCEPT_WINDOW_OPEN)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        opens.len(),
        2,
        "both demo tenants must open an accept window:\n{text}",
    );
    let hog = text
        .find(HOG_BANNER)
        .unwrap_or_else(|| panic!("the fuel probe never ran:\n{text}"));
    assert!(
        opens[1] < hog,
        "the second window opened only after other tenants ran: \
         the first waiter held the hart:\n{text}",
    );
    assert!(
        markers::sched_tier1_faulted(&text[hog..]),
        "the fuel probe ran during the wait but was not faulted:\n{text}",
    );

    assert!(
        !text.contains("panicked at"),
        "rust panic detected:\n{text}"
    );
}
