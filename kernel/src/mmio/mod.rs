// SPDX-License-Identifier: AGPL-3.0-only
//! Typed volatile MMIO wrappers — CLAUDE R3.
//!
//! Raw `ptr::read_volatile`/`write_volatile` is **banned outside this
//! module**. Drivers use `VolatilePtr<T>` or device-specific typed
//! register definitions.
//!
//! Why: compiler optimization + MMIO = silent correctness bugs. A
//! typed wrapper makes the intent explicit at every call site, and
//! a future lint or audit can verify R3 mechanically by checking
//! that no other file imports `core::ptr::{read,write}_volatile`.
//!
//! Submodules:
//!   - `volatile`     — `VolatilePtr<T>` generic wrapper
//!   - `uart_ns16550` — kernel-private NS16550 putc (early printk /
//!     panic path). NOT the customer UART driver; the Tier-2 WASM
//!     driver lands in PR 5.

pub mod dma_mem;

/// Order every earlier store to memory ahead of the device write that
/// follows (`fence w,o`).
///
/// A device reads descriptors from ordinary memory and is told to
/// look by a register write. RISC-V's memory model does not order a
/// store to memory before a later store to a device, so without this
/// the doorbell can reach the device first: it reads a stale
/// descriptor. That is the RX wedge build 155 exposed on the VF2
/// (INV-25, "Ordering"). Same barrier as Linux's `__io_bw()` before
/// `writel`. Call it immediately before a doorbell write.
#[inline]
pub fn barrier_before_device_write() {
    // SAFETY: INV-7 — `fence` is unprivileged and always legal.
    unsafe {
        core::arch::asm!("fence w,o", options(nostack, preserves_flags));
    }
}
pub mod plic;
pub mod uart_ns16550;
pub mod volatile;
