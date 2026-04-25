//! Wari kernel — Tier 0 entry point.
//!
//! This file is the kernel crate's root. It declares modules, sets up
//! the `no_std` / `no_main` environment, and provides the panic handler.
//!
//! The actual boot sequence lives in `boot.rs` as a list of named stages
//! with documented pre- and post-conditions (goose-os pattern; see
//! book Part 1, Ch 4 "Inheritance from Goose").
//!
//! Phase 0 PR 1: boot.S lands hart 0 in `kmain`, which prints the
//! banner and halts. Paging, trap vector, wasmi, and everything else
//! lands in later PRs per the approved Phase-0 plan.

#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

use core::panic::PanicInfo;

// Assemble boot.S into the crate. Keeps the build single-step (no
// build.rs, no cc crate). The linker script's `KEEP(*(.text.entry))`
// places the resulting `_start` at the load address.
core::arch::global_asm!(include_str!("boot.S"));

// Module skeleton — populated per the approved Phase-0 plan.
mod abi;
mod boot;
mod cap;
mod error;
mod kputc;
mod mem;
mod mmio;
mod runtime;
mod trap;
mod validate;

/// Build identifier string — supplied by the Makefile via the
/// `WARI_BUILD` env var. Falls back to `"dev"` for ad-hoc builds
/// (e.g. `cargo build` without going through make).
const BUILD: &str = match option_env!("WARI_BUILD") {
    Some(s) => s,
    None => "dev",
};

/// Kernel entry point, called from `boot.S` after OpenSBI hands
/// control to S-mode and the boot stack is set up. Never returns.
///
/// # Safety
///
/// First Rust code to run after OpenSBI. Interrupts disabled, MMU
/// off, only the kernel image mapped. `.bss` has already been zeroed
/// by `boot.S`.
#[no_mangle]
pub extern "C" fn kmain(hart_id: usize, _dtb_addr: usize) -> ! {
    mmio::uart_ns16550::init();
    kprintln!("Wari v0 build {} boot OK, hart {}", BUILD, hart_id);

    if let Err(e) = mem::kvm::init() {
        kprintln!("MMU init failed: {:?}", e);
        loop {
            // SAFETY: INV-7 — wfi is an S-mode instruction in S-mode.
            unsafe { core::arch::asm!("wfi"); }
        }
    }
    trap::install();
    kprintln!("mmu OK, traps installed");

    if let Err(e) = runtime::run_tier2_uart() {
        kprintln!("wari runtime: tier-2 load failed: {:?}", e);
        loop {
            // SAFETY: INV-7 — wfi is an S-mode instruction in S-mode.
            unsafe { core::arch::asm!("wfi"); }
        }
    }
    kprintln!("tier-2 uart driver loaded");

    if let Err(e) = runtime::run_tier1_hello() {
        kprintln!("wari runtime: tier-1 hello failed: {:?}", e);
        loop {
            // SAFETY: INV-7 — wfi is an S-mode instruction in S-mode.
            unsafe { core::arch::asm!("wfi"); }
        }
    }

    loop {
        // SAFETY: INV-7 — wfi is an S-mode instruction; we are in S-mode.
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
        // SAFETY: INV-7 — wfi is an S-mode instruction in S-mode.
        unsafe { core::arch::asm!("wfi"); }
    }
}
