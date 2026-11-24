// SPDX-License-Identifier: AGPL-3.0-only
//! One process cannot reach another's memory (ADR-004 step 2, INV-28).
//!
//! The kernel's boot self-test (`kernel/src/umode.rs`) builds two
//! address spaces, A and B. B maps a page at a user VA and holds a
//! known value there. As a control, B's stub reads that VA and exits
//! normally; then A's stub reads the same VA, which A does not map,
//! and must die of a load page fault while B's page keeps its value.
//! Destroying both spaces must return every page to the allocator.
//!
//! This file asserts those outcomes from the console of a normal QEMU
//! boot, and that the boot carried on to the end.

use wari_security_tests::{boot_kernel_capture, markers, DEFAULT_WALLCLOCK};

/// The kernel's line once every Tier-1 tenant has ended.
const BOOT_FINISHED: &str = "[sched] all tenants exited";

#[test]
fn process_reading_another_spaces_page_is_killed() {
    let text = boot_kernel_capture(DEFAULT_WALLCLOCK);
    assert!(
        text.contains(markers::UMODE_CROSS_SPACE_KILLED),
        "cross-space read was not stopped by a load page fault:\n{text}"
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

#[test]
fn destroying_address_spaces_returns_every_page() {
    let text = boot_kernel_capture(DEFAULT_WALLCLOCK);
    assert!(
        text.contains(markers::UMODE_SPACES_FREED),
        "address-space teardown leaked pages:\n{text}"
    );
}
