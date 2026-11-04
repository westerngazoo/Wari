// SPDX-License-Identifier: AGPL-3.0-only
//! Regression: a tenant waiting for a TCP connection is not faulted on
//! fuel.
//!
//! ## What this test verifies
//!
//! The HTTP demo waits for a client by polling `net_socket_accept` in
//! a loop for up to the kernel's 60 s accept window. A host fn call is
//! not a yield, so the whole wait is one scheduling step and is charged
//! against `wari_sched::TIER1_FUEL_PER_STEP`. Build 171 shipped a
//! budget of ten million units, and both demo tenants were faulted on
//! fuel after ~10 s of waiting, on QEMU and on the VF2; the 8 s
//! `fuel_exhaustion` run ended before that could show.
//!
//! This boots with a NIC so the demo reaches its accept loop, lets it
//! wait well past the point where the old budget ran out, and asserts
//! that nobody ran out of fuel. The hog probe cannot interfere: it is
//! scheduled after the demo tenants, which are still waiting when the
//! run ends.
//!
//! The real fix is network waits that yield (tracked separately); when
//! it lands this test keeps guarding the same property.

use std::time::Duration;

use wari_security_tests::{boot_kernel_capture_with_net, markers};

/// Long enough to cross the ~10 s mark where the old budget faulted
/// the waiting tenant, short of the 60 s window, so the demo is still
/// in its first wait when the run ends.
const WALLCLOCK: Duration = Duration::from_secs(25);

/// Printed by the kernel when a tenant first polls `accept`.
const ACCEPT_WINDOW_OPEN: &str = "[accept] window open";

#[test]
fn tenant_waiting_in_accept_is_not_faulted_on_fuel() {
    let text = boot_kernel_capture_with_net(WALLCLOCK);

    assert!(
        text.contains(markers::BOOT_BANNER),
        "kernel did not boot:\n{text}",
    );
    assert!(
        text.contains(ACCEPT_WINDOW_OPEN),
        "the demo never reached its accept loop, so this run proves nothing:\n{text}",
    );
    assert!(
        !markers::tier1_event(&text, markers::T1_OUT_OF_FUEL),
        "a tenant waiting in accept was faulted on fuel: \
         TIER1_FUEL_PER_STEP is too small for the accept window:\n{text}",
    );
    assert!(
        !text.contains("panicked at"),
        "rust panic detected:\n{text}",
    );
}
