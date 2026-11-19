// SPDX-License-Identifier: AGPL-3.0-only
//! U-mode is a real boundary (ADR-004 step 1).
//!
//! The kernel's boot self-test (`kernel/src/umode.rs`) runs a
//! ten-instruction stub in U-mode twice. This file asserts both runs
//! from the console of a normal QEMU boot:
//!
//! 1. Round trip: the stub's `ecall` reaches the kernel, the kernel's
//!    answer reaches the stub, and the stub's exit brings control back
//!    to the kernel.
//! 2. Adversarial: the stub reads a kernel address. The page has U=0,
//!    so the load faults in U-mode; the kernel ends the stub and keeps
//!    booting — no halt, no panic, and the rest of the boot (Tier-2
//!    drivers, Tier-1 tenants) still runs.

use wari_security_tests::{boot_kernel_capture, markers, DEFAULT_WALLCLOCK};

/// The kernel's line once every Tier-1 tenant has ended — proof that
/// the boot went all the way through after the self-test.
const BOOT_FINISHED: &str = "[sched] all tenants exited";

#[test]
fn umode_stub_ecall_returns_and_exit_comes_back() {
    let text = boot_kernel_capture(DEFAULT_WALLCLOCK);
    assert!(
        text.contains(markers::BOOT_BANNER),
        "kernel did not boot:\n{text}"
    );
    assert!(
        text.contains(markers::UMODE_STUB_OK),
        "U-mode stub round trip missing:\n{text}"
    );
}

#[test]
fn umode_read_of_kernel_memory_kills_only_the_stub() {
    let text = boot_kernel_capture(DEFAULT_WALLCLOCK);
    assert!(
        text.contains(markers::UMODE_HOSTILE_KILLED),
        "hostile stub was not killed by a load page fault:\n{text}"
    );
    assert!(
        !text.contains(markers::UMODE_FAILED),
        "a U-mode self-test did not end as expected:\n{text}"
    );

    let continues = text
        .find(markers::UMODE_KERNEL_CONTINUES)
        .unwrap_or_else(|| panic!("kernel did not come back from U-mode:\n{text}"));
    assert!(
        text[continues..].contains(BOOT_FINISHED),
        "boot did not finish after the U-mode self-test:\n{text}"
    );
    for needle in ["panicked at", "[trap] exception"] {
        assert!(!text.contains(needle), "kernel failure `{needle}`:\n{text}");
    }
}
