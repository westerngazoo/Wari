// SPDX-License-Identifier: AGPL-3.0-only
//! Minimal S-mode trap dispatcher.
//!
//! Cherry-picked and adapted from goose-os/kernel/src/trap.rs at
//! 69d9908b6956315684c567fb95cec542062a61a5 under the "only copy what
//! makes sense" discipline.
//!
//! Phase 0 scope: no userspace, no syscall surface (Q1=A: ecall is not
//! a normal control path), no scheduler, no IPC. The trap vector exists
//! to (a) catch kernel exceptions and dump diagnostics, and (b) ack
//! timer interrupts cleanly if any fire. Anything else halts the hart.
//!
//! Differences from goose-os:
//!   - No `handle_syscall`, no IPC dispatch, no scheduler hooks
//!   - No PLIC/external-interrupt path (no PLIC in Phase 0)
//!   - `kprintln!` instead of `println!`/`kdebug!`/`kdump_csrs!`
//!   - `TrapFrame` carries `scause`/`stval` (saved by `trap.S`) rather
//!     than re-reading them in the handler — keeps the Rust side pure
//!     dispatch and matches the spec for this PR

use core::arch::{asm, global_asm};

use crate::kprintln;

// Pull in the assembly trap entry. `_trap_entry` is the symbol `stvec`
// will point at.
global_asm!(include_str!("trap.S"));

/// Trap frame layout — must match `trap.S` byte-for-byte.
///
/// 32 GP registers (x0 stored as 0 padding for index symmetry with
/// xN-indexed offsets), then sepc, sstatus, scause, stval. Total
/// 36 × 8 = 288 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TrapFrame {
    pub zero: usize,    // x0   offset 0x00 (always 0, kept for index symmetry)
    pub ra: usize,      // x1   offset 0x08
    pub sp: usize,      // x2   offset 0x10
    pub gp: usize,      // x3   offset 0x18
    pub tp: usize,      // x4   offset 0x20
    pub t0: usize,      // x5   offset 0x28
    pub t1: usize,      // x6   offset 0x30
    pub t2: usize,      // x7   offset 0x38
    pub s0: usize,      // x8   offset 0x40
    pub s1: usize,      // x9   offset 0x48
    pub a0: usize,      // x10  offset 0x50
    pub a1: usize,      // x11  offset 0x58
    pub a2: usize,      // x12  offset 0x60
    pub a3: usize,      // x13  offset 0x68
    pub a4: usize,      // x14  offset 0x70
    pub a5: usize,      // x15  offset 0x78
    pub a6: usize,      // x16  offset 0x80
    pub a7: usize,      // x17  offset 0x88
    pub s2: usize,      // x18  offset 0x90
    pub s3: usize,      // x19  offset 0x98
    pub s4: usize,      // x20  offset 0xA0
    pub s5: usize,      // x21  offset 0xA8
    pub s6: usize,      // x22  offset 0xB0
    pub s7: usize,      // x23  offset 0xB8
    pub s8: usize,      // x24  offset 0xC0
    pub s9: usize,      // x25  offset 0xC8
    pub s10: usize,     // x26  offset 0xD0
    pub s11: usize,     // x27  offset 0xD8
    pub t3: usize,      // x28  offset 0xE0
    pub t4: usize,      // x29  offset 0xE8
    pub t5: usize,      // x30  offset 0xF0
    pub t6: usize,      // x31  offset 0xF8
    pub sepc: usize,    //      offset 0x100
    pub sstatus: usize, //      offset 0x108
    pub scause: usize,  //      offset 0x110
    pub stval: usize,   //      offset 0x118
}

// `TrapFrame::zero()` constructor intentionally not implemented in PR 3.
// The trap path constructs a frame from saved register memory in
// `trap.S`, not from a Rust constructor. A `zero()` ctor will land
// when a future PR (likely PR 4 or PR 5) needs one for context-switch
// or wasmi-instance setup. Adding it now would be speculative
// abstraction (Simplicity First).

/// Install the trap vector by writing `_trap_entry` to `stvec`.
///
/// Direct mode (low two bits of stvec = 00): every trap (interrupt or
/// exception) jumps to `_trap_entry`.
pub fn install() {
    extern "C" {
        fn _trap_entry();
    }
    // First cast to a pointer, then to usize — direct fn-to-int casts
    // are denied by clippy's `function_casts_as_integer` lint.
    let entry = _trap_entry as *const () as usize;
    // SAFETY: INV-7. `csrw stvec` is an S-mode privileged CSR write;
    // we are in S-mode. `entry` is the address of an `extern "C"` symbol
    // exported by `trap.S`, naturally aligned to 4 bytes.
    unsafe {
        asm!("csrw stvec, {0}", in(reg) entry);
    }
}

/// scause MSB = 1 means "interrupt"; MSB = 0 means "exception".
const SCAUSE_INTERRUPT_BIT: usize = 1 << 63;

/// scause code for supervisor timer interrupt.
const SCAUSE_S_TIMER: usize = 5;

/// scause code for supervisor external interrupt (PLIC-routed).
const SCAUSE_S_EXT: usize = 9;

/// Rust-side trap dispatcher, called from `_trap_entry` with
/// `a0 = &mut TrapFrame` pointing at the saved frame on the stack.
///
/// Phase 0 policy:
///   - Timer interrupt: ack and return (the kernel doesn't arm timers
///     in Phase 0, but if OpenSBI delivers a stray one we don't want
///     to halt).
///   - Anything else: print scause/sepc/stval and park the hart.
#[no_mangle]
pub extern "C" fn handle_trap(frame: &mut TrapFrame) {
    // SAFETY (R1): INV-2. `frame` is exclusively owned by this trap
    // service — `trap.S` saves into a fresh frame on the kernel stack
    // and S-mode interrupts are masked while we run, so no other
    // execution touches it until sret.
    let scause = frame.scause;
    let is_interrupt = (scause & SCAUSE_INTERRUPT_BIT) != 0;
    let code = scause & !SCAUSE_INTERRUPT_BIT;

    if is_interrupt {
        match code {
            SCAUSE_S_TIMER => {
                // Phase 0 has no scheduler — just clear the pending bit
                // so the timer doesn't re-fire immediately, then return.
                ack_timer();
            }
            SCAUSE_S_EXT => {
                // PLIC-routed external interrupt (Phase 1b PR Net-1).
                // Claim → signal-bound-notification → complete.
                crate::mmio::plic::dispatch();
            }
            _ => {
                kprintln!(
                    "[trap] unhandled interrupt code={} sepc={:#x}",
                    code,
                    frame.sepc,
                );
                halt();
            }
        }
    } else {
        // Exception. With no userspace in Phase 0 every exception is a
        // kernel bug; print and halt.
        kprintln!(
            "[trap] exception code={} sepc={:#x} stval={:#x}",
            code,
            frame.sepc,
            frame.stval,
        );
        halt();
    }
}

/// Clear `sip.STIP` so the timer interrupt does not immediately re-fire.
///
/// Real timer scheduling is a Phase 1 concern; here we just dismiss the
/// signal.
fn ack_timer() {
    // SAFETY: INV-7. `csrc sip` is S-mode privileged.
    unsafe {
        asm!("csrc sip, {0}", in(reg) 1usize << 5);
    }
}

/// Park the hart in a WFI loop. Used after an unrecoverable trap.
fn halt() -> ! {
    loop {
        // SAFETY: INV-7. `wfi` is permitted in S-mode.
        unsafe {
            asm!("wfi");
        }
    }
}
