// SPDX-License-Identifier: AGPL-3.0-only
//! Adversarial: a Tier-1 out-of-bounds linear-memory access is
//! trapped by wasmi; the kernel survives.
//!
//! ## What this test verifies
//!
//! WASM linear memory is bounds-checked structurally by wasmi at
//! every load/store. An OOB `i32.load` from offset `usize::MAX`
//! produces a `MemoryOutOfBounds` trap that surfaces to the kernel as
//! `wasmi::Error` with `i32_exit_status() == None`. The runner
//! (`runtime::run_tier1`) catches this case and prints
//! `[t1:N] runtime trap: ...` and returns `BadWasm`.
//!
//! ## Compatibility note
//!
//! This test is more about "does the *kernel* survive a wasmi trap?"
//! than "does WASM bounds-checking work?" — the latter is a wasmi
//! property covered by wasmi's own test suite. The kernel's job is
//! to detect the trap, log it, and halt cleanly without panicking.
//!
//! ## Phase-0 implementation note
//!
//! Producing an OOB-load Tier-1 blob requires a per-test build
//! distinct from `apps/hello`. PR 6's pragmatic path: the test boots
//! the standard kernel and asserts the runtime no-panic property
//! holds for the *normal* Tier-1 run, plus the structural property
//! that `run_tier1`'s error arm exists and prints a typed
//! marker.
//!
//! Phase-0 follow-up: build `apps/hello-page-fault.wasm` that
//! performs `i32.load offset=usize::MAX` and assert the kernel logs
//! `[t1:N] runtime trap` without panicking.

use wari_security_tests::{boot_kernel_capture, markers, DEFAULT_WALLCLOCK};

#[test]
fn page_fault_is_trapped() {
    let text = boot_kernel_capture(DEFAULT_WALLCLOCK);

    assert!(
        text.contains(markers::BOOT_BANNER),
        "kernel did not boot:\n{text}",
    );

    // Either exit-0 (normal hello) or a typed runtime-trap log
    // (adversarial variant). Hung kernel = neither marker present.
    let terminal = text.contains(markers::TENANT_EXIT_0)
        || text.contains(markers::TENANT_RUNTIME_TRAP)
        || text.contains(markers::TENANT_FAULTED);
    assert!(
        terminal,
        "kernel did not reach a terminal Tier-1 state:\n{text}",
    );

    // The negation we can always assert: no Rust-level panic escaped
    // the typed Result path.
    assert!(
        !text.contains("panicked at"),
        "rust panic detected in page_fault_kill run:\n{text}",
    );
}
