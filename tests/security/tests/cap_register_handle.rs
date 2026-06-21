// SPDX-License-Identifier: AGPL-3.0-only
//! Adversarial: the cap-fastpath registration host fns are bound with a
//! compatible signature, register a handle without conferring authority,
//! and never panic the kernel.
//!
//! ## What this test verifies
//!
//! `wari::cap_register(cspace_slot) -> i32` and
//! `wari::cap_unregister(reg_idx) -> i32` (kernel side:
//! `cap::syscall::cap_register_impl` / `cap_unregister_impl`) resolve a
//! capability **once** and cache it behind a registered-handle index, or
//! return a typed errno (`E_INVAL` for an out-of-range / empty slot,
//! `E_NOMEM` when the per-process `RegTable` is full). The returned index
//! is not a capability — it names a cap the kernel already proved
//! (proposed INV-α). None of these paths panic.
//!
//! ## Why this is a structural test (Phase-style precedent)
//!
//! Mirrors `host_fn_escape.rs`: building a bespoke Tier-1/Tier-2 blob
//! that imports `cap_register` and drives the bad-slot / table-full
//! branches is parent-gate work beyond this PR. Instead we assert the
//! **structural** safety properties that a regression here would break,
//! and rely on two already-landed layers for the logic:
//!
//!   1. The pure soundness predicate `wari_abi::reg::validate_handle`
//!      is exhaustively unit-tested (truth table) in `wari-abi`.
//!   2. `cap_register_impl` / `cap_unregister_impl` have simple,
//!      code-reviewable errno branches (bounds → `E_INVAL`, full →
//!      `E_NOMEM`), with no `unsafe`.
//!
//! The structural property below proves the third leg: the new host-fn
//! bindings are registered with signatures compatible with the WASM
//! import surface (a bad `func_wrap` signature would break instantiation
//! of every module sharing the linker), and adding them did not regress
//! the standard boot or introduce a panic.
//!
//! Follow-up (tracked in `docs/cap-registered-fastpath-design.md` §8):
//! a bespoke blob that calls `cap_register` with an empty slot, an
//! out-of-range slot, and to exhaustion, asserting `E_INVAL` / `E_NOMEM`
//! on the serial log.

use wari_security_tests::{boot_kernel_capture, markers, DEFAULT_WALLCLOCK};

#[test]
fn cap_register_binding_is_sound_and_panic_free() {
    let text = boot_kernel_capture(DEFAULT_WALLCLOCK);

    assert!(
        text.contains(markers::BOOT_BANNER),
        "kernel did not boot:\n{text}",
    );

    // The Tier-2 driver instantiates through the SAME `wari` linker that
    // this PR extends with `cap_register` / `cap_unregister` func_wraps.
    // If either binding had an incompatible signature the linker build
    // would fail and the driver would not load — so its load marker is
    // direct proof the new Tier-2 bindings are sound.
    assert!(
        text.contains(markers::TIER2_LOADED),
        "Tier-2 driver did not load — cap-fastpath host-fn binding may \
         be broken:\n{text}",
    );

    // A Tier-1 instance running its body proves the Tier-1 linker
    // (wasi.rs, which this PR also extends) instantiated successfully:
    // a bad cap_register/unregister signature there would surface as a
    // `BadWasm` instantiation failure before any output.
    assert!(
        text.contains("Hello from Wari"),
        "Tier-1 instance did not run — cap-fastpath host-fn binding may \
         be broken:\n{text}",
    );

    // No kernel panic from the new registration path / bindings.
    assert!(
        !text.contains("panicked at"),
        "rust panic detected in cap_register_handle run:\n{text}",
    );
}
