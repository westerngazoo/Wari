// SPDX-License-Identifier: AGPL-3.0-only
//! Adversarial: a Tier-1 instance that never yields is faulted on
//! fuel; the kernel and the other tenants carry on.
//!
//! ## What this test verifies
//!
//! `apps/hog` prints one line and then spins forever without calling
//! a host fn. The module registry spawns it at every boot
//! (`modreg::self_test_spawn`, after the demo tenants). With the
//! Tier-1 engine metered (`loader::load_tier1`) and the per-step
//! budget set by `tier1_pool`, wasmi traps the spin with
//! `TrapCode::OutOfFuel` after `wari_sched::TIER1_FUEL_PER_STEP`
//! units; `tier1_pool::settle` logs `[t1:<n>] out of fuel` and the
//! scheduler marks that one tenant faulted. Before this landed the
//! same module would have held the hart until reset: no other
//! tenant, no network poll, no Ctrl-R.
//!
//! The negative arm is real here (a genuine non-yielding blob runs),
//! unlike the structural Phase-0 tests in this directory.

use std::time::Duration;

use wari_security_tests::{boot_kernel_capture, markers};

/// The hog needs `TIER1_FUEL_PER_STEP` / ~24 M units/s (QEMU TCG,
/// measured) ≈ 6 s to exhaust its step after a ~1 s boot. Twice that,
/// so a slower host does not turn a working budget into a failure.
const WALLCLOCK: Duration = Duration::from_secs(15);

/// Printed by `apps/hog` before it spins — proves the probe ran.
const HOG_BANNER: &str = "[hog] spinning without yielding";

#[test]
fn non_yielding_tenant_is_faulted_on_fuel() {
    let text = boot_kernel_capture(WALLCLOCK);

    assert!(
        text.contains(markers::BOOT_BANNER),
        "kernel did not boot:\n{text}",
    );

    // The probe was spawned and started spinning.
    assert!(
        text.contains(HOG_BANNER),
        "fuel probe never ran — modreg did not spawn apps/hog:\n{text}",
    );

    // ...and the kernel ended its step on fuel, as a typed fault.
    assert!(
        markers::tier1_event(&text, markers::T1_OUT_OF_FUEL),
        "no `[t1:<n>] out of fuel` line — the hog was not faulted on fuel \
         (hart held until the wall-clock budget?):\n{text}",
    );
    assert!(
        markers::sched_tier1_faulted(&text),
        "scheduler did not record the faulted tenant:\n{text}",
    );

    // Containment: the well-behaved tenants still exit normally.
    assert!(
        markers::tier1_event(&text, markers::T1_EXIT_0),
        "no other tenant reached exit(0) — the hog starved them:\n{text}",
    );

    // Never a kernel panic.
    assert!(
        !text.contains("panicked at"),
        "rust panic detected in fuel_exhaustion run:\n{text}",
    );
}
