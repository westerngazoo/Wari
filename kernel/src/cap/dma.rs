// SPDX-License-Identifier: AGPL-3.0-only
//! Kernel-owned DMA addressing for Tier-2 NIC drivers (ADR-002 step 1).
//!
//! One concern: the Tier-2 driver names a buffer by its *offset* in
//! its own linear memory, and the kernel — never the driver — turns
//! that into the physical address the DMA engine will read. This is
//! the enforcement half of INV-25 ("every physical address handed to
//! a DMA engine lies inside the driver's own linear memory"), which
//! `docs/invariants.md` recorded as enforced by nothing.
//!
//! The pure decisions live in `wari_validate::dma` and are host-
//! tested there; this file is glue: capability check, linear-memory
//! resolution, the one `Memory::write`, and the per-process ring
//! registry in `cap::storage`.
//!
//! ## WASM-side ABI
//!
//! ```text
//! (import "wari" "nic_dma_set_buf" (func (param i32 i32 i32 i32) (result i32)))
//! ```
//!
//! `nic_dma_set_buf(ring, idx, buf_off, len) -> i32` points
//! descriptor `idx` of ring `ring` (0 = RX, 1 = TX) at
//! `[buf_off, buf_off + len)` of the caller's linear memory. Returns
//! 0 on success, `E_PERM` without `Net + WRITE`, `E_INVAL` if the
//! ring is not configured, `idx` is past its end, or the buffer span
//! is not inside linear memory, `E_NOMEM` if the module exports no
//! memory. On any error nothing is written.
//!
//! A ring becomes known to the kernel when the driver attaches it:
//! `nic_attach_queue` (VirtIO) records it via [`record_ring`]; the
//! GMAC path gains its own attach host fn in the VF2 follow-up.

use wari_validate::dma::{desc_addr_offset, ring_fits, span_in_linmem, DmaRing};

use super::cspace::MAX_PROCS;
use super::storage::{cspaces, dma_rings};
use super::syscall::{E_INVAL, E_NOMEM, E_PERM};
use super::types::{ObjectKind, CAP_RIGHT_WRITE};

/// Rings per driver the kernel tracks: RX and TX.
pub const RINGS_PER_PROC: usize = 2;

/// Record `ring` (0 = RX, 1 = TX) of process `proc_id` as the
/// descriptor table at `desc_off` with `entries` descriptors, after
/// the caller has validated the table against the module's linear
/// memory of `mem_len` bytes.
///
/// Returns `false` (and records nothing) if `ring` or `proc_id` is
/// out of range or the table does not fit (`wari_validate::dma::
/// ring_fits`). Re-recording an active ring replaces it: a driver
/// that re-initialises its device re-attaches its queues.
pub fn record_ring(proc_id: u8, ring: u32, desc_off: u32, entries: u32, mem_len: usize) -> bool {
    if (proc_id as usize) >= MAX_PROCS || (ring as usize) >= RINGS_PER_PROC {
        return false;
    }
    if !ring_fits(desc_off, entries, mem_len) {
        return false;
    }
    let rings = dma_rings();
    rings[proc_id as usize][ring as usize] = DmaRing { desc_off, entries };
    true
}

/// `wari::nic_dma_set_buf(ring, idx, buf_off, len) -> i32`. See the
/// module docs for the contract.
pub fn nic_dma_set_buf_impl<T>(
    caller: &mut wasmi::Caller<'_, T>,
    proc_id: u8,
    ring: u32,
    idx: u32,
    buf_off: u32,
    len: u32,
) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_PERM;
    }
    // Same gate as every other device-facing host fn: the driver's
    // root cap at slot 0 must be Net with WRITE.
    let cap = {
        let cs = cspaces();
        cs[proc_id as usize].slots[0]
    };
    if cap.is_empty() || !matches!(cap.kind, ObjectKind::Net) || cap.rights & CAP_RIGHT_WRITE == 0 {
        return E_PERM;
    }
    if (ring as usize) >= RINGS_PER_PROC {
        return E_INVAL;
    }
    let ring_desc = {
        let rings = dma_rings();
        rings[proc_id as usize][ring as usize]
    };
    let Some(addr_off) = desc_addr_offset(&ring_desc, idx) else {
        return E_INVAL;
    };

    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
        Some(m) => m,
        None => return E_NOMEM,
    };
    let (mem_base, mem_len) = {
        let data = memory.data(&*caller);
        (data.as_ptr() as u64, data.len())
    };
    // Both spans — the buffer the device will DMA to or from, and
    // the 8 descriptor bytes we are about to write — must lie inside
    // the module's own memory. This is INV-25's address containment.
    if !span_in_linmem(buf_off, len, mem_len) || !span_in_linmem(addr_off, 8, mem_len) {
        return E_INVAL;
    }

    // Linear memory is identity-mapped (kernel-virtual == physical)
    // for everything wasmi allocates from the bump arena — the same
    // assumption `nic_attach_queue` relies on for the ring addresses.
    let pa = mem_base + buf_off as u64;
    // VirtIO `virtq_desc.addr` and DWMAC4 TDES0/1 + RDES0/1 are both
    // a little-endian u64 in the descriptor's first 8 bytes.
    if memory
        .write(&mut *caller, addr_off as usize, &pa.to_le_bytes())
        .is_err()
    {
        return E_INVAL;
    }
    0
}
