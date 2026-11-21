// SPDX-License-Identifier: AGPL-3.0-only
//! Ring lifecycle for the Tier-2 GMAC driver: attach, kick, rewind
//! (ADR-002 step 1, INV-25).
//!
//! One concern: decide when the kernel programs the DWMAC4 ring
//! registers on a driver's behalf. Descriptors themselves go through
//! `cap::dma` (`nic_dma_set_buf`, `nic_dma_status`); the register
//! accesses are `mmio::dwmac`.
//!
//! ## WASM-side ABI
//!
//! ```text
//! (import "wari" "nic_attach_ring" (func (param i32) (result i32)))
//! (import "wari" "nic_ring_kick"   (func (param i32 i32) (result i32)))
//! (import "wari" "nic_ring_rewind" (func (param i32) (result i32)))
//! ```
//!
//! Ring ids as in `cap::dma`: 0 = receive, 1 = transmit. All three
//! return 0 on success, `E_PERM` without `Net + WRITE` at slot 0, and
//! `E_INVAL` on a board without a DWMAC4 NIC or a ring id the kernel
//! does not hold, with nothing written. Further:
//!
//! - `nic_attach_ring(ring)` claims ring `ring` (16 descriptors, all
//!   cleared) and points the channel at it. `E_INVAL` if the ring is
//!   already claimed — by anyone, the caller included: re-pointing a
//!   live ring is what `nic_ring_rewind` is for — or if the channel
//!   is running (start bit set).
//! - `nic_ring_kick(ring, idx)` sets the tail pointer to descriptor
//!   `idx` ("everything before `idx` is ready"; `idx == 16` is the
//!   end of the ring). `E_INVAL` unless the caller holds the ring and
//!   `idx <= 16`. Ordered after every earlier descriptor store.
//! - `nic_ring_rewind(ring)` re-points a stopped channel at the head
//!   of its ring, for the receive-wedge recovery. `E_INVAL` unless
//!   the caller holds the ring and the channel's start bit is clear
//!   (see `mmio::dwmac::program_ring` on what that does not wait for).

use wari_validate::board::DmaEngine;
use wari_validate::dma::{dwmac_ring_regs, dwmac_tail_offset, DwmacRingRegs, KERNEL_RING_ENTRIES};

use super::dma::{has_net_right, RING_RX};
use super::syscall::{E_INVAL, E_PERM};
use super::types::CAP_RIGHT_WRITE;
use crate::mmio::{barrier_before_device_write, dma_mem, dwmac};

/// The GMAC base and ring registers for `ring`, or `None` on a board
/// whose NIC is not a DWMAC4 or for a ring id the kernel does not
/// hold.
fn gmac_ring(ring: u32) -> Option<(usize, DwmacRingRegs)> {
    match crate::board::BOARD.dma_engine {
        DmaEngine::Dwmac4 { base } => dwmac_ring_regs(ring).map(|regs| (base, regs)),
        DmaEngine::VirtioSplit | DmaEngine::None => None,
    }
}

/// `wari::nic_attach_ring(ring) -> i32`. See the module docs.
pub fn nic_attach_ring_impl(proc_id: u8, ring: u32) -> i32 {
    if !has_net_right(proc_id, CAP_RIGHT_WRITE) {
        return E_PERM;
    }
    let Some((base, regs)) = gmac_ring(ring) else {
        return E_INVAL;
    };
    if dma_mem::is_claimed(ring as usize) || dwmac::channel_running(base, &regs) {
        return E_INVAL;
    }
    let Some(table_pa) = dma_mem::claim(ring as usize, proc_id, KERNEL_RING_ENTRIES) else {
        return E_INVAL;
    };
    dwmac::program_ring(base, &regs, ring == RING_RX, table_pa, KERNEL_RING_ENTRIES);
    0
}

/// `wari::nic_ring_kick(ring, idx) -> i32`. See the module docs.
pub fn nic_ring_kick_impl(proc_id: u8, ring: u32, idx: u32) -> i32 {
    if !has_net_right(proc_id, CAP_RIGHT_WRITE) {
        return E_PERM;
    }
    let Some((base, regs)) = gmac_ring(ring) else {
        return E_INVAL;
    };
    let Some((table_pa, entries)) = dma_mem::owned(ring as usize, proc_id) else {
        return E_INVAL;
    };
    let Some(off) = dwmac_tail_offset(entries, idx) else {
        return E_INVAL;
    };
    // The descriptors this tail announces were stored by earlier
    // host calls; order them before the device sees the tail (R6).
    barrier_before_device_write();
    dwmac::write_tail(base, &regs, table_pa + off);
    0
}

/// `wari::nic_ring_rewind(ring) -> i32`. See the module docs.
pub fn nic_ring_rewind_impl(proc_id: u8, ring: u32) -> i32 {
    if !has_net_right(proc_id, CAP_RIGHT_WRITE) {
        return E_PERM;
    }
    let Some((base, regs)) = gmac_ring(ring) else {
        return E_INVAL;
    };
    let Some((table_pa, entries)) = dma_mem::owned(ring as usize, proc_id) else {
        return E_INVAL;
    };
    if dwmac::channel_running(base, &regs) {
        return E_INVAL;
    }
    dwmac::program_ring(base, &regs, ring == RING_RX, table_pa, entries);
    0
}
