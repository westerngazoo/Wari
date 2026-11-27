// SPDX-License-Identifier: AGPL-3.0-only
//! A U-mode process hands the kernel data only through its IPC buffer,
//! and the kernel never reads past it (ADR-004 step 3a, INV-31).
//!
//! The kernel's boot self-test (`kernel/src/umode.rs`) runs a stub that
//! copies a message into its IPC buffer and writes it with `SYS_WRITE`,
//! which must return the length. The same stub then tries a length one
//! byte beyond the buffer, a descriptor other than stdout and an
//! unknown syscall number: each must come back as an error value — the
//! kernel neither obeys nor kills — or the stub exits non-zero and the
//! self-test prints `FAILED`. A second stub runs in a space with no
//! IPC buffer, then in one with code at the IPC-buffer address: its
//! `SYS_WRITE` must come back as `NotMapped`, with nothing read.
//!
//! This file asserts those outcomes from the console of a normal QEMU
//! boot, and that the boot carried on to the end.

use wari_security_tests::{boot_kernel_capture, markers, DEFAULT_WALLCLOCK};

/// The kernel's line once every Tier-1 tenant has ended.
const BOOT_FINISHED: &str = "[sched] all tenants exited";

#[test]
fn ipc_buffer_write_is_bounded_and_malformed_calls_are_refused() {
    let text = boot_kernel_capture(DEFAULT_WALLCLOCK);
    assert!(
        text.contains(markers::UMODE_IPC_WRITE_OUTPUT),
        "the U-mode stub's SYS_WRITE did not reach the console:\n{text}"
    );
    assert!(
        text.contains(markers::UMODE_IPC_WRITE_REFUSALS),
        "SYS_WRITE returned the wrong count or a malformed call was not refused:\n{text}"
    );
    assert!(
        text.contains(markers::UMODE_IPC_MISSING_REFUSED),
        "SYS_WRITE without a valid IPC buffer was not answered with NotMapped:\n{text}"
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
