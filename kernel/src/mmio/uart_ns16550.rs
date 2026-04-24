// SPDX-License-Identifier: AGPL-3.0-only
//! NS16550 UART — kernel-private putc for early boot + panic.
//!
//! This is **not** the customer UART driver. The Tier-2 WASM UART
//! driver lands in PR 5 with its own capability-gated register
//! interface. This file is the raw-MMIO printk path that lets the
//! kernel log during boot (before the runtime exists) and during a
//! panic (when the runtime may be gone).
//!
//! Target: QEMU `virt` machine emulated NS16550 at `0x1000_0000`.
//! QEMU's UART starts usable with no initialization — no divisor
//! latch, no FIFO config, no IER setup required to transmit.
//!
//! Reference: Texas Instruments TL16C550 datasheet (SLLS177I), §6
//! "Register summary". NS16550-compatible devices share the layout.
//!
//! Register map (byte offsets from base):
//!   +0  THR  transmit holding register (write)
//!   +5  LSR  line status register     (read; bit 5 = THR empty)
//!
//! Phase 0 scope: poll-write bytes, no interrupts, no reads. The
//! Tier-2 driver handles RX, IRQs, flow control, etc.

use super::volatile::VolatilePtr;

/// NS16550 base on QEMU `virt`. Fixed by QEMU's machine model.
const UART_BASE: usize = 0x1000_0000;

const THR_OFFSET: usize = 0;
const LSR_OFFSET: usize = 5;

/// Bit 5 of LSR: transmit holding register empty (ready to accept).
const LSR_THRE: u8 = 1 << 5;

/// Initialize the UART. No-op on QEMU's emulated NS16550 — the
/// call-site is kept for symmetry with real hardware (VF2, Phase 1+).
pub fn init() {
    // Intentionally empty. QEMU's virt UART is usable out of reset.
}

/// Write one byte to the UART, blocking until the THR is free.
///
/// # Contract
///
/// - Precondition: kernel is in S-mode with INV-3 (UART_BASE valid).
/// - Postcondition: the byte has been handed to the UART's THR. QEMU
///   flushes to stdout synchronously; real hardware has a shift
///   register latency we do not wait on.
/// - Panics: never.
pub fn putc(byte: u8) {
    // SAFETY: INV-3. UART_BASE + THR_OFFSET / LSR_OFFSET are fixed
    // NS16550 registers on QEMU virt. Both pointers are naturally
    // aligned (u8).
    let lsr: VolatilePtr<u8> =
        unsafe { VolatilePtr::new((UART_BASE + LSR_OFFSET) as *mut u8) };
    // SAFETY: INV-3 (same).
    let thr: VolatilePtr<u8> =
        unsafe { VolatilePtr::new((UART_BASE + THR_OFFSET) as *mut u8) };

    // Poll LSR until THRE is set. On QEMU this is effectively
    // always set, but the loop keeps the code correct on real
    // NS16550-compatible hardware.
    while lsr.read() & LSR_THRE == 0 {}
    thr.write(byte);
}
