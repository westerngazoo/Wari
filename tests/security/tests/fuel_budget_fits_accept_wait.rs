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
//! This boots with a NIC so the demo reaches its accept, lets it wait
//! well past the point where that budget ran out, and asserts that no
//! tenant other than the fuel probe ran out of fuel.
//!
//! Network waits now block the tenant (`BlockReason::NetWait`) and the
//! budget is back at ten million: a wait is no longer a step, so it
//! costs no fuel however long it lasts. The probe (`apps/hog`) now
//! runs while the demo waits and is faulted on fuel by design, which
//! is why the check names it rather than forbidding every fault.

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
    let probe =
        fuel_probe_pid(&text).unwrap_or_else(|| panic!("the fuel probe was not spawned:\n{text}"));
    let probe_line = format!(
        "{}{probe}] {}",
        markers::TIER1_PREFIX,
        markers::T1_OUT_OF_FUEL
    );
    assert!(
        text.lines()
            .filter(|l| l.starts_with(markers::TIER1_PREFIX) && l.contains(markers::T1_OUT_OF_FUEL))
            .all(|l| l.starts_with(&probe_line)),
        "a tenant other than the fuel probe (pid {probe}) was faulted on fuel: \
         waiting in accept is being charged as computation:\n{text}",
    );
    assert!(
        !text.contains("panicked at"),
        "rust panic detected:\n{text}",
    );
}

/// Pid of the fuel probe, from `[modreg] fuel probe: pid <n> will spin…`
/// (`kernel/src/runtime/modreg.rs`).
fn fuel_probe_pid(text: &str) -> Option<u32> {
    let rest = text.split("[modreg] fuel probe: pid ").nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}
