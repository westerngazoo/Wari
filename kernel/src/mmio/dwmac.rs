// SPDX-License-Identifier: AGPL-3.0-only
//! DWMAC4 channel-0 ring registers, written by the kernel (ADR-002
//! step 1, INV-25).
//!
//! One concern: the register accesses that point the GMAC's DMA
//! engine at a kernel-owned descriptor table — list address, ring
//! length, receive buffer size, descriptor format, tail pointer.
//! The driver may not write any of them (`BoardDescriptor::
//! dma_addr_regs`, `write_masks`); `cap::dwmac` decides when the
//! kernel does, and this file only does it.
//!
//! Register offsets and bit meanings are pure data in
//! `wari_validate::dma::dwmac_reg`; nothing is spelled twice.
//!
//! // unsafe glue

use wari_validate::dma::{dwmac_reg, dwmac_rx_control_with_rbsz, DwmacRingRegs};

use super::volatile::VolatilePtr;

/// The 32-bit register at `base + off`.
fn reg(base: usize, off: usize) -> VolatilePtr<u32> {
    // SAFETY: INV-3 + INV-20. `base` is the board descriptor's GMAC
    // base (`DmaEngine::Dwmac4`), and every `off` passed in this file
    // is a fixed DWMAC4 register offset from `dwmac_reg`, inside the
    // GMAC window that `BoardDescriptor::mmio_regions` identity-maps
    // at boot (`board::tests::dma_addr_regs_sit_inside_net_windows`
    // pins the registers inside that window). 4-byte aligned by
    // construction.
    unsafe { VolatilePtr::new((base + off) as *mut u32) }
}

/// Is the ring's channel started (`ST`/`SR` set)?
pub fn channel_running(base: usize, regs: &DwmacRingRegs) -> bool {
    reg(base, regs.control).read() & dwmac_reg::CONTROL_START != 0
}

/// Point the ring's channel at the descriptor table at physical
/// address `table_pa`, `entries` descriptors long.
///
/// Writes, in this order: `DMA_CH0_CONTROL` to zero (no descriptor
/// skip, no split header: descriptors are contiguous 16-byte
/// entries, which is the table's layout); the list address, high
/// word first (the databook latches the low word against the high
/// one); the ring length (`entries − 1`); and, for the receive ring,
/// the buffer size field of `RX_CONTROL`, other bits kept.
///
/// # Contract
///
/// - Preconditions: the caller holds the ring (`dma_mem::owned`),
///   `1 <= entries <= KERNEL_RING_ENTRIES`, and the channel is
///   stopped ([`channel_running`] is false) — the databook allows
///   the list address to change only then.
/// - Postcondition: the engine, once started, walks exactly that
///   table and fills at most `DWMAC_RBSZ` bytes per receive buffer.
/// - Never panics. `entries == 0` cannot reach here (`dma_mem::claim`
///   refuses it); the subtraction saturates rather than wrap anyway.
pub fn program_ring(base: usize, regs: &DwmacRingRegs, is_rx: bool, table_pa: u64, entries: u32) {
    reg(base, dwmac_reg::CH0_CONTROL).write(0);
    reg(base, regs.list_hi).write((table_pa >> 32) as u32);
    reg(base, regs.list_lo).write(table_pa as u32);
    reg(base, regs.ring_len).write(entries.saturating_sub(1));
    if is_rx {
        let control = reg(base, dwmac_reg::RX_CONTROL);
        control.write(dwmac_rx_control_with_rbsz(control.read()));
    }
}

/// Write the ring's tail pointer: the engine may process
/// descriptors up to (not including) physical address `pa`.
///
/// The register holds the low 32 bits; the engine takes the high
/// bits from the list address. A 64-byte-aligned 256-byte table in
/// the kernel image cannot straddle a 4 GiB boundary on the VF2
/// (the image is linked at `0x4020_0000`).
///
/// Precondition: the caller has ordered the descriptor stores before
/// this device write (`mmio::barrier_before_device_write`) and
/// checked that `pa` is inside or one past the ring
/// (`wari_validate::dma::dwmac_tail_offset`). Never panics.
pub fn write_tail(base: usize, regs: &DwmacRingRegs, pa: u64) {
    reg(base, regs.tail).write(pa as u32);
}
