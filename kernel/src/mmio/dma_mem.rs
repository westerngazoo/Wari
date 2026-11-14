// SPDX-License-Identifier: AGPL-3.0-only
//! Kernel-owned DMA descriptor memory (ADR-002 step 1, INV-25).
//!
//! One concern: hold the NIC's descriptor tables in kernel memory and
//! be the only code that stores to them.
//!
//! The tables are what a DMA engine reads to learn where in physical
//! memory to read and write. A Tier-2 driver must not be able to
//! store to them — it could aim the engine anywhere — so they do not
//! live in the driver's linear memory. They live here, the driver
//! never learns where, and it reaches them only through the
//! validated operations in `cap::dma`.
//!
//! This file is under `mmio/` because it is the same kind of memory
//! as a device register: hardware reads it behind the compiler's
//! back, so every store is volatile (R3 keeps raw volatile access in
//! this directory) and is followed by an explicit barrier (R6).
//!
//! The pure decisions (index bounds, descriptor encoding) are in
//! `wari_validate::dma`, host-tested there.
//!
//! // unsafe glue

use core::ptr::addr_of_mut;
use core::sync::atomic::{fence, Ordering};

use wari_validate::dma::{desc_index_ok, ring_entries_ok, DESC_SIZE, KERNEL_RING_ENTRIES};

use super::volatile::VolatilePtr;

/// Rings the kernel holds: receive (0) and transmit (1) of the one
/// NIC a board has.
pub const RINGS: usize = 2;

/// One descriptor table. Aligned to a cache line, which satisfies
/// both engines' requirements (VirtIO 1.2 §2.7: 16 bytes; DWMAC4:
/// bus width) and keeps the two rings off each other's lines.
#[repr(C, align(64))]
struct RingMem([[u8; DESC_SIZE]; KERNEL_RING_ENTRIES as usize]);

/// Who claimed a ring and how many of its descriptors are in use.
#[derive(Clone, Copy)]
struct RingState {
    /// Process id of the driver that claimed the ring; `None` until
    /// a driver attaches.
    owner: Option<u8>,
    /// Descriptors in use (`0` = not claimed).
    entries: u32,
}

static mut MEM: [RingMem; RINGS] = [
    RingMem([[0; DESC_SIZE]; KERNEL_RING_ENTRIES as usize]),
    RingMem([[0; DESC_SIZE]; KERNEL_RING_ENTRIES as usize]),
];

static mut STATE: [RingState; RINGS] = [RingState {
    owner: None,
    entries: 0,
}; RINGS];

/// Claim ring `ring` for the driver process `owner`, with `entries`
/// descriptors, and return the address of its descriptor table for
/// the kernel to program into the device.
///
/// # Contract
///
/// - Precondition: the caller has checked `owner`'s capability. This
///   function trusts `owner` to be the process it is acting for.
/// - Returns `None`, changing nothing, if `ring >= RINGS`, `entries`
///   is zero or more than the kernel reserves
///   (`wari_validate::dma::ring_entries_ok`), or another process
///   already owns the ring.
/// - Postcondition on `Some(addr)`: every descriptor of the table is
///   zero, the ring belongs to `owner`, and `addr` is the table's
///   kernel-virtual address, which equals its physical address (the
///   kernel image is identity-mapped; the same assumption the queue
///   attach code makes for wasmi memories).
/// - Re-claiming by the same owner is allowed and resets the table: a
///   driver that re-initialises its device attaches its queues again.
/// - Never panics.
pub fn claim(ring: usize, owner: u8, entries: u32) -> Option<u64> {
    if ring >= RINGS || !ring_entries_ok(entries) {
        return None;
    }
    // SAFETY: INV-1 (single hart, host-fn context) — no concurrent
    // access to STATE. `addr_of_mut!` avoids a reference to the whole
    // static.
    let state = unsafe { &mut (*addr_of_mut!(STATE))[ring] };
    if matches!(state.owner, Some(other) if other != owner) {
        return None;
    }
    state.owner = Some(owner);
    state.entries = entries;

    for idx in 0..KERNEL_RING_ENTRIES {
        store(ring, idx, [0; DESC_SIZE]);
    }
    // SAFETY: INV-1. Address-of only; nothing is read or written.
    let table = unsafe { addr_of_mut!((*addr_of_mut!(MEM))[ring]) };
    Some(table as u64)
}

/// Store `bytes` as descriptor `idx` of ring `ring`, on behalf of the
/// driver process `owner`.
///
/// # Contract
///
/// - Returns `false`, storing nothing, if `ring >= RINGS`, the ring
///   is not claimed by `owner`, or `idx` is not one of its
///   descriptors (`wari_validate::dma::desc_index_ok`).
/// - Postcondition on `true`: the 16 bytes are in the table and the
///   store is ordered before whatever the caller does next (see
///   [`store`]). The descriptor is not live until the driver publishes
///   its index to the device, which is a later, separate step.
/// - Never panics.
pub fn write_desc(ring: usize, owner: u8, idx: u32, bytes: [u8; DESC_SIZE]) -> bool {
    if ring >= RINGS {
        return false;
    }
    // SAFETY: INV-1 — single hart; read of a Copy value.
    let state = unsafe { (*addr_of_mut!(STATE))[ring] };
    if state.owner != Some(owner) || !desc_index_ok(state.entries, idx) {
        return false;
    }
    store(ring, idx, bytes);
    true
}

/// The one store into descriptor memory. `ring < RINGS` and
/// `idx < KERNEL_RING_ENTRIES` are the callers' checks.
fn store(ring: usize, idx: u32, bytes: [u8; DESC_SIZE]) {
    // SAFETY: INV-1 (single hart) and INV-25: MEM is kernel-owned
    // descriptor memory that no Rust reference aliases; the pointer
    // is in bounds by the callers' checks and aligned (u8 array
    // inside a 64-byte-aligned struct). The device may be reading
    // this memory, which is why the write is volatile.
    let slot =
        unsafe { VolatilePtr::new(addr_of_mut!((*addr_of_mut!(MEM))[ring].0[idx as usize])) };
    slot.write(bytes);
    // Barrier (R6): the descriptor must be in memory before the
    // device is told about it. The driver publishes the index and
    // notifies the device through later host calls; `fence rw,rw`
    // here orders this store before any of those MMIO writes
    // regardless of how the calls are inlined or reordered.
    fence(Ordering::SeqCst);
}
