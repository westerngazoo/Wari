//! Wari kernel — Tier 0 entry point.
//!
//! This file is the kernel crate's root. It declares modules, sets up
//! the `no_std` / `no_main` environment, and provides the panic handler.
//!
//! The actual boot sequence lives in `boot.rs` as a list of named stages
//! with documented pre- and post-conditions (goose-os pattern; see
//! book Part 1, Ch 4 "Inheritance from Goose").
//!
//! Phase 0 scaffold: `_start` is a stub. The execution agent populates
//! the modules below per the lean Phase 0 plan in CLAUDE.md — build
//! from scratch, copy only what makes sense from predecessor code
//! (pure logic: page_alloc, page_table, validators, invariants).
//!
//! No IPC, no scheduler, no multi-process in Phase 0: a single WASM
//! module loaded at boot runs to completion and the kernel halts.
//! Multi-tenant pieces arrive in Phase 1 alongside the capability
//! system.

#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

use core::panic::PanicInfo;

// Module skeleton — populated Phase 0a onward.
mod abi;
mod boot;
mod error;
mod mem;
mod mmio;
mod trap;
mod validate;

/// Kernel entry point.
///
/// Called from `boot.S` (not yet in tree) after OpenSBI hands control
/// to S-mode and the boot stack is set up. Never returns.
///
/// # Safety
/// This function is the first Rust code to run after OpenSBI. It runs
/// with interrupts disabled, MMU off, and only the kernel image mapped.
/// See `boot.rs` staged invariants.
#[no_mangle]
pub extern "C" fn _start(_hart_id: usize, _dtb_addr: usize) -> ! {
    // Phase-0 placeholder. The agent's first PR rewires this to
    // `boot::run(hart_id, dtb_addr)` per the staged-boot pattern.
    loop {
        // SAFETY: wfi is an S-mode instruction; we are in S-mode.
        // See docs/invariants.md INV-7 (privileged ASM in S-mode).
        unsafe { core::arch::asm!("wfi"); }
    }
}

/// Kernel panic handler.
///
/// Per CLAUDE R5, panics in the kernel are last-resort assertions
/// only. When one fires, we disable interrupts and halt — the system
/// is in an undefined state and attempting recovery is worse than
/// stopping.
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        // SAFETY: wfi is an S-mode instruction; see INV-7.
        unsafe { core::arch::asm!("wfi"); }
    }
}
