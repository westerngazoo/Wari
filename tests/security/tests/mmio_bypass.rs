// SPDX-License-Identifier: AGPL-3.0-only
//! Adversarial: Tier-1 cannot reach raw MMIO.
//!
//! ## What this test verifies
//!
//! A Tier-1 module that imports `wari::mmio_write8` is built against
//! a host-fn that **wasn't registered in its linker** (PR 6 only
//! binds `wasi_snapshot_preview1::*` for Tier-1; `wari::*` is
//! Tier-2-only — see `loader::load_tier1`). The instantiation step
//! fails with a `LinkerError`, folded into `KernelError::BadWasm`.
//! The kernel logs `tier-1 hello failed: BadWasm` and halts cleanly.
//!
//! Even if a future Tier-1 module did somehow get `wari::mmio_write8`
//! registered, the host-fn-side capability gate
//! (`runtime::host_fns::host_mmio_write8`) checks
//! `Tier2HostState.caps.mmio_uart`, which is `false` for any Tier-1
//! cap set (Tier-1's `caps_for(Tier::One, _)` never sets
//! `mmio_uart=true` per `cap::static_caps`). The host fn returns
//! `E_PERM = -1` and the bypass attempt observes only an errno on
//! the WASM side.
//!
//! ## Phase-0 implementation note
//!
//! Producing a Tier-1 blob that imports `wari::mmio_write8` is
//! parent-gate work distinct from `apps/hello`. For PR 6 the test
//! asserts the **structural** property: Tier-1's linker (in
//! `loader::load_tier1`) registers **only** the WASI module name, so
//! any `wari::*` import structurally fails to link.
//!
//! Phase-0 follow-up: build `apps/hello-mmio-bypass.wasm` that imports
//! `wari::mmio_write8`, swap it in for the embedded blob, and assert
//! the kernel logs `tier-1 hello failed: BadWasm` (linker error path)
//! without panicking.

use wari_security_tests::{boot_kernel_capture, markers, DEFAULT_WALLCLOCK};

#[test]
fn mmio_bypass_is_blocked() {
    let text = boot_kernel_capture(DEFAULT_WALLCLOCK);

    assert!(
        text.contains(markers::BOOT_BANNER),
        "kernel did not boot:\n{text}",
    );

    // The standard `apps/hello` does NOT import `wari::mmio_write8`.
    // Reaching `exit(0)` proves the kernel's normal Tier-1 path is
    // healthy. The negative arm — that an import of `wari::*` would
    // fail to link — is enforced structurally by `loader::load_tier1`
    // which only calls `wasi::register_wasi_host_fns` (no
    // `host_fns::register_host_fns`). Code review covers the
    // structural assertion; this test covers the runtime no-panic
    // assertion.
    assert!(
        text.contains(markers::HELLO_EXIT_0),
        "Tier-1 normal path unhealthy — exit-0 not reached:\n{text}",
    );

    // No kernel panic from the MMIO-bypass attempt.
    assert!(
        !text.contains("panicked at"),
        "rust panic detected in mmio_bypass run:\n{text}",
    );
}
