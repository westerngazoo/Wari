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
//! Register map (logical indices; multiply by `platform::UART_REG_STRIDE`
//! for the byte offset):
//!   index 0  THR  transmit holding register (write)
//!   index 5  LSR  line status register     (read; bit 5 = THR empty)
//!
//! On QEMU `virt` the stride is 1 (NS16550A, byte-spaced registers);
//! on JH7110 / VF2 the stride is 4 (DesignWare 8250, 32-bit-aligned
//! registers). Both expose 8-bit register *contents*; only the spacing
//! differs. We keep `VolatilePtr<u8>` for the access width and let the
//! stride pick the offset.
//!
//! Phase 0 scope: poll-write bytes, no interrupts, no reads. The
//! Tier-2 driver handles RX, IRQs, flow control, etc.

use super::volatile::VolatilePtr;
use crate::platform;

/// NS16550-style UART base — sourced from the active platform.
const UART_BASE: usize = platform::UART_BASE;

/// Bytes between consecutive registers (1 on QEMU, 4 on VF2).
const REG_STRIDE: usize = platform::UART_REG_STRIDE;

const THR_INDEX: usize = 0;
const LSR_INDEX: usize = 5;

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
    // SAFETY: INV-3. `UART_BASE + index * REG_STRIDE` is a fixed
    // hardware register address on the active platform (NS16550A on
    // QEMU virt, DesignWare 8250 on JH7110 / VF2). Both layouts use
    // 8-bit register *contents*; the stride only changes spacing.
    // u8 is naturally aligned at every byte address.
    let lsr: VolatilePtr<u8> = unsafe {
        VolatilePtr::new((UART_BASE + LSR_INDEX * REG_STRIDE) as *mut u8)
    };
    // SAFETY: INV-3 (same).
    let thr: VolatilePtr<u8> = unsafe {
        VolatilePtr::new((UART_BASE + THR_INDEX * REG_STRIDE) as *mut u8)
    };

    // Poll LSR until THRE is set. On QEMU this is effectively
    // always set, but the loop keeps the code correct on real
    // NS16550-compatible hardware.
    while lsr.read() & LSR_THRE == 0 {}
    thr.write(byte);
}
