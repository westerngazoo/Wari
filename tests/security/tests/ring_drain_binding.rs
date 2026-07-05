// SPDX-License-Identifier: AGPL-3.0-only
//! Adversarial: the cap-fastpath ring host fns (`ring_setup` /
//! `ring_submit`) are bound with compatible signatures, and the drain
//! path never panics the kernel.
//!
//! ## What this verifies
//!
//! `wari::ring_setup(sq, cq, entries) -> i32` and
//! `wari::ring_submit(n) -> i32` (kernel:
//! `cap::ring_drain::{ring_setup_impl, ring_submit_impl}`) drain a
//! submission ring by: copying each SQE out of linear memory
//! (copy-before-use, INV-β), validating the registered handle
//! (`validate_handle`, INV-α/γ), and delegating to the underlying op —
//! or returning a typed errno. wasmi enforces the linear-memory bounds on
//! every SQE read / CQE write, so an out-of-range ring stops the drain
//! rather than escaping.
//!
//! ## Why structural (Phase-style precedent)
//!
//! Mirrors `host_fn_escape.rs` / `cap_register_handle.rs`: a bespoke blob
//! that drives the forged-handle / stale-generation / OOB-ring branches
//! is parent-gate work beyond this PR. The logic is covered by two
//! already-landed pure layers — the SQE decoder + `validate_handle` truth
//! tables in `wari-abi` — plus code review of the drain's fail-closed
//! branches. This test proves the third leg: the ring bindings are
//! registered with signatures compatible with the WASM import surface (a
//! bad `func_wrap` breaks instantiation of every module sharing the
//! linker), and adding them did not regress the boot or add a panic.
//!
//! Follow-up (docs/cap-registered-fastpath-design.md §8): a bespoke blob
//! that registers a Notification cap, sets up a ring, submits
//! notify-wait/ack, and asserts the CQ results + a revoked-handle
//! rejection.

use wari_security_tests::{boot_kernel_capture, markers, DEFAULT_WALLCLOCK};

#[test]
fn ring_binding_is_sound_and_panic_free() {
    let text = boot_kernel_capture(DEFAULT_WALLCLOCK);

    assert!(
        text.contains(markers::BOOT_BANNER),
        "kernel did not boot:\n{text}",
    );

    // The Tier-2 driver instantiates through the same `wari` linker this
    // PR extends with ring_setup/ring_submit. A signature-incompatible
    // binding would fail the linker build and the driver would not load.
    assert!(
        text.contains(markers::TIER2_LOADED),
        "Tier-2 driver did not load — ring host-fn binding may be \
         broken:\n{text}",
    );

    // A Tier-1 instance running its body proves the Tier-1 linker
    // (wasi.rs, also extended here) instantiated successfully.
    assert!(
        text.contains("Hello from Wari"),
        "Tier-1 instance did not run — ring host-fn binding may be \
         broken:\n{text}",
    );

    // No kernel panic from the new ring binding / drain path.
    assert!(
        !text.contains("panicked at"),
        "rust panic detected in ring_drain_binding run:\n{text}",
    );
}
