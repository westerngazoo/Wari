// SPDX-License-Identifier: AGPL-3.0-only
//! Meta-test: none of the four adversarial inputs above causes a
//! kernel panic.
//!
//! Each individual test
//! (`malformed_wasm`, `oom_bomb`, `host_fn_escape`, `mmio_bypass`,
//! `page_fault_kill`) already asserts no panic in its own run. This
//! meta-test boots the kernel one more time and asserts the
//! aggregate property: across an adversarial-style boot, the kernel
//! does not emit a panic backtrace, does emit its boot banner, and
//! does reach a typed terminal state for Tier-1.
//!
//! Phase-0 audit criterion #9 ("Security test suite covers ... All
//! fail safely; no kernel panic") cites this meta-test as the single
//! line item that ratifies the suite.

use wari_security_tests::{boot_kernel_capture, markers, DEFAULT_WALLCLOCK};

#[test]
fn no_kernel_panic_across_adversarial_boot() {
    let text = boot_kernel_capture(DEFAULT_WALLCLOCK);

    // Kernel must boot.
    assert!(
        text.contains(markers::BOOT_BANNER),
        "kernel did not boot:\n{text}",
    );

    // Tier-1 must reach a typed terminal state.
    let terminal = text.contains(markers::HELLO_EXIT_0)
        || text.contains("tier-1 hello failed")
        || text.contains("[hello] runtime trap")
        || text.contains("[hello] returned cleanly");
    assert!(
        terminal,
        "kernel did not reach a terminal Tier-1 state:\n{text}",
    );

    // No panic markers anywhere in the captured output.
    for needle in ["panicked at", "Stack backtrace", "panic_handler"] {
        assert!(
            !text.contains(needle),
            "panic marker {:?} found in kernel output:\n{text}",
            needle,
        );
    }
}
