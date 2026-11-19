// SPDX-License-Identifier: AGPL-3.0-only
//! Minimal S-mode trap dispatcher.
//!
//! Cherry-picked and adapted from goose-os/kernel/src/trap.rs at
//! 69d9908b6956315684c567fb95cec542062a61a5 under the "only copy what
//! makes sense" discipline.
//!
//! Two entry points, one per trapping mode (`trap.S` tells them apart
//! by `sscratch`, INV-2):
//!   - [`handle_trap`]: a trap from S-mode. Interrupts are serviced; an
//!     exception is a kernel bug, so it is printed and the hart halts.
//!   - [`handle_user_trap`]: a trap from U-mode (ADR-004 step 1). The
//!     decision is pure (`wari_sched::utrap`); an exception ends only
//!     the process, never the kernel.
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
use wari_sched::utrap::{cause_name, decide, UserExit, UserTrap, ECALL_LEN};

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
    // SAFETY: INV-7, INV-2. `csrw sscratch` / `csrw stvec` are S-mode
    // privileged CSR writes; we are in S-mode. sscratch = 0 marks "the
    // kernel is running" for `_trap_entry` (INV-2) and must hold before
    // the first trap can arrive. `entry` is the address of an
    // `extern "C"` symbol exported by `trap.S`, naturally aligned to 4.
    unsafe {
        asm!("csrw sscratch, zero", "csrw stvec, {0}", in(reg) entry);
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
        dispatch_interrupt(code, frame.sepc);
    } else {
        // Exception from S-mode: always a kernel bug (U-mode faults take
        // `handle_user_trap`); print and halt.
        kprintln!(
            "[trap] exception code={} sepc={:#x} stval={:#x}",
            code,
            frame.sepc,
            frame.stval,
        );
        halt();
    }
}

/// Service an interrupt, whichever mode it arrived in.
///
/// Timer: the kernel arms none yet, so a stray one is acknowledged.
/// External: PLIC claim → signal-bound-notification → complete (Phase
/// 1b PR Net-1). Anything else halts: no other source is enabled.
fn dispatch_interrupt(code: usize, sepc: usize) {
    match code {
        SCAUSE_S_TIMER => ack_timer(),
        SCAUSE_S_EXT => crate::mmio::plic::dispatch(),
        _ => {
            kprintln!("[trap] unhandled interrupt code={} sepc={:#x}", code, sepc);
            halt();
        }
    }
}

/// Rust-side handler for a trap taken from U-mode, called from
/// `_trap_entry`'s U-mode path with `a0 = &mut TrapFrame` on the
/// kernel stack.
///
/// Returns 0 to resume the process (the frame, possibly updated, is
/// restored and `sret` goes back to U-mode), or a non-zero
/// [`UserExit`] word to end it: the assembly then returns that word
/// from `__umode_enter`. Never halts the kernel on the process's
/// behalf — a U-mode fault ends only the process (ADR-004 step 1).
#[no_mangle]
pub extern "C" fn handle_user_trap(frame: &mut TrapFrame) -> u64 {
    // SAFETY (R1): INV-2. The frame was saved by `_trap_entry` on the
    // kernel stack sscratch named; interrupts are masked while we run,
    // so nothing else touches it until sret or the leave path.
    match decide(frame.scause as u64, frame.a7 as u64, frame.a0 as u64) {
        UserTrap::Interrupt { code } => {
            dispatch_interrupt(code as usize, frame.sepc);
            0
        }
        UserTrap::Return { a0 } => {
            frame.a0 = a0 as usize;
            frame.sepc += ECALL_LEN;
            0
        }
        UserTrap::Exit { code } => UserExit::Exited(code).encode(),
        UserTrap::Kill { cause } => {
            kprintln!(
                "[umode] process killed: {} sepc={:#x} stval={:#x}",
                cause_name(cause),
                frame.sepc,
                frame.stval,
            );
            UserExit::Killed(cause).encode()
        }
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
