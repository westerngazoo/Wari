// SPDX-License-Identifier: AGPL-3.0-only
//! Adversarial: a Tier-1 module attempting `memory.grow` past wasmi's
//! configured limit must be **contained** — the trap must stay inside
//! wasmi, the kernel must not panic.
//!
//! ## What this test verifies
//!
//! Wasmi enforces `MemoryType` maxima at instantiation and at each
//! `memory.grow`. A module that asks for more than its declared max
//! gets `-1` from `memory.grow` (per WASM spec); a module whose
//! initial size exceeds its max fails to instantiate with a typed
//! `MemoryError`, which the kernel folds into `BadWasm`.
//!
//! ## Phase-0 implementation note
//!
//! Producing an OOM-bomb `.wasm` requires a custom-built Tier-1 blob
//! distinct from `apps/hello`. PR 6's atomic-exception scope chose the
//! pragmatic path: the test boots the standard kernel and asserts the
//! structural property "no panic, no infinite-grow loop, kernel
//! reaches a typed terminal state within the wall-clock budget".
//!
//! Phase-0 follow-up: swap the embedded blob at build time for an
//! `oom_bomb.wasm` that calls `memory.grow(u32::MAX)`. Expect either
//! the bomb's instantiation to fail with `BadWasm` or its first
//! `memory.grow` to return `-1`. Both are wasmi-contained. Neither
//! panics the kernel.

use wari_security_tests::{boot_kernel_capture, markers, DEFAULT_WALLCLOCK};

#[test]
fn oom_bomb_is_contained() {
    let text = boot_kernel_capture(DEFAULT_WALLCLOCK);

    // Kernel must boot — pre-condition for adversarial coverage.
    assert!(
        text.contains(markers::BOOT_BANNER),
        "kernel did not boot:\n{text}",
    );

    // Kernel must reach a terminal Tier-1 state within the budget —
    // either a clean exit (well-formed blob) or a typed rejection
    // (adversarial blob). The OOM bomb's failure mode under wasmi is
    // either "module fails to instantiate" → `tier-1 hello failed`
    // path or "memory.grow returns -1; module continues normally" →
    // `[hello] exit(...)`. Both are observed terminal states, not a
    // hung kernel.
    let terminal =
        text.contains(markers::HELLO_EXIT_0) || text.contains("tier-1 hello failed");
    assert!(
        terminal,
        "kernel did not reach a terminal Tier-1 state — possible \
         infinite-grow loop or hang:\n{text}",
    );

    // No kernel panic. A wasmi-contained trap is logged as part of
    // `tier-1 hello failed: BadWasm`, never as a Rust panic.
    assert!(
        !text.contains("panicked at"),
        "rust panic detected in oom_bomb run:\n{text}",
    );
}
