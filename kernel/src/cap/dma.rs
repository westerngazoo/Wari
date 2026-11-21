// SPDX-License-Identifier: AGPL-3.0-only
//! Kernel-owned DMA descriptors for Tier-2 NIC drivers (ADR-002
//! step 1, INV-25).
//!
//! One concern: turn a driver's request "aim descriptor `idx` of ring
//! `ring` at this span of my memory" into a descriptor the device
//! reads, after checking the span.
//!
//! The descriptor tables live in kernel memory (`mmio::dma_mem`); the
//! driver holds no copy and cannot store to them. It names a buffer
//! by its *offset* in its own linear memory, and the kernel — never
//! the driver — forms the physical address, the length and the flags
//! the DMA engine will act on. Why the whole descriptor and not just
//! the address is in `wari_validate::dma`'s module docs.
//!
//! The pure decisions live in `wari_validate::dma` and are host-
//! tested there; this file is glue: capability check, linear-memory
//! resolution, and the call into the descriptor memory.
//!
//! ## WASM-side ABI
//!
//! ```text
//! (import "wari" "nic_dma_set_buf" (func (param i32 i32 i32 i32) (result i32)))
//! ```
//!
//! `nic_dma_set_buf(ring, idx, buf_off, len) -> i32` aims descriptor
//! `idx` of ring `ring` (0 = RX, 1 = TX) at `[buf_off, buf_off + len)`
//! of the caller's linear memory, as one unchained buffer. The
//! direction follows from the ring: RX descriptors are
//! device-writable, TX descriptors are not. Returns 0 on success,
//! `E_PERM` without `Net + WRITE`, `E_INVAL` if the ring is not
//! attached by the caller, `idx` is past its end, the span is not
//! inside linear memory, or re-aiming a descriptor could let the
//! device pair its old address with the new length (or the reverse)
//! outside linear memory; `E_NOMEM` if the module exports no memory.
//! On any error nothing is written.
//!
//! A ring comes into being when the driver attaches it:
//! `nic_attach_queue` (VirtIO) or `nic_attach_ring` (DWMAC4,
//! `cap::dwmac`) claims it.
//!
//! ### DWMAC4 (the VF2 GMAC)
//!
//! Same call, different descriptor. A receive buffer is always
//! `DWMAC_RBSZ` bytes — the engine fills that much whatever the
//! descriptor says — so `len` must equal it; a transmit length must
//! fit `TDES2`. The descriptor is handed to the engine (OWN set) by
//! the store itself. A descriptor the engine still owns is **refused**:
//! the engine may be reading it, and on this engine the driver never
//! needs to take one back (it waits for the writeback that clears
//! OWN).
//!
//! That refusal does not by itself make every read of a rewrite
//! safe. The engine also reads descriptors that are *not* owned (it
//! fetches the one at its current position and suspends on OWN=0),
//! and if it read the two halves of one at different times it could
//! pair the new OWN bit with the old words 0-1 — which after a receive
//! writeback hold frame metadata, partly chosen by a remote sender.
//! What closes that is a **per-SoC assumption**: the engine fetches a
//! descriptor in one coherent read of its cache line (tables are
//! 64-byte aligned, a descriptor is 16 bytes, so it never straddles a
//! line). It holds for the JH7110 GMAC as far as we know and is
//! recorded in INV-25 beside `dma_coherent`; a third SoC re-decides
//! it rather than inheriting it.
//!
//! ```text
//! (import "wari" "nic_dma_status" (func (param i32 i32 i32) (result i32)))
//! ```
//!
//! `nic_dma_status(ring, idx, out_off) -> i32` stores descriptor
//! `idx`'s status word (`RDES3`/`TDES3`: OWN, and after a receive the
//! frame length and error bits) as 4 little-endian bytes at `out_off`
//! in the caller's memory. Only that word: in a descriptor the engine
//! has not written back, words 0-1 are the physical address, which
//! the driver must not learn. Returns 0, `E_PERM` without `Net +
//! READ`, `E_INVAL` on a board without a DWMAC4 NIC, a ring or index
//! the caller does not hold, or an `out_off` whose 4 bytes are not
//! inside its memory; `E_NOMEM` without exported memory.

use wari_validate::board::DmaEngine;
use wari_validate::dma::{
    dwmac_desc_status, dwmac_rx_buf_aligned, dwmac_rx_desc, dwmac_tx_desc, dwmac_tx_len_ok,
    reaim_keeps_mixtures_inside, span_in_linmem, virtq_desc, virtq_desc_span, RingDir,
    DWMAC_DESC_OWN, DWMAC_RBSZ,
};

use super::cspace::MAX_PROCS;
use super::storage::cspaces;
use super::syscall::{E_INVAL, E_NOMEM, E_PERM};
use super::types::{ObjectKind, CAP_RIGHT_READ, CAP_RIGHT_WRITE};
use crate::mmio::dma_mem;

/// Ring id of the receive queue (VirtIO-net §5.1.6.1: receiveq 0).
pub const RING_RX: u32 = 0;

/// Does `proc_id`'s root cap (slot 0) grant `Net` with `right`? The
/// gate every device-facing host fn applies.
pub(super) fn has_net_right(proc_id: u8, right: u8) -> bool {
    if (proc_id as usize) >= MAX_PROCS {
        return false;
    }
    let cap = {
        let cs = cspaces();
        cs[proc_id as usize].slots[0]
    };
    !cap.is_empty() && matches!(cap.kind, ObjectKind::Net) && cap.rights & right != 0
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
    if !has_net_right(proc_id, CAP_RIGHT_WRITE) {
        return E_PERM;
    }
    if (ring as usize) >= dma_mem::RINGS {
        return E_INVAL;
    }

    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
        Some(m) => m,
        None => return E_NOMEM,
    };
    let (mem_base, mem_len) = {
        let data = memory.data(&*caller);
        (data.as_ptr() as u64, data.len())
    };
    // The span the device will DMA to or from must lie inside the
    // module's own memory. This is INV-25's containment, for the
    // address and the length together.
    if !span_in_linmem(buf_off, len, mem_len) {
        return E_INVAL;
    }

    // Linear memory is identity-mapped (kernel-virtual == physical)
    // for everything wasmi allocates from the bump arena — the same
    // assumption `nic_attach_queue` relies on for the ring addresses.
    let pa = mem_base + buf_off as u64;
    let dir = if ring == RING_RX {
        RingDir::DeviceWrites
    } else {
        RingDir::DeviceReads
    };
    // `None` here is the ring/owner/index refusal `write_desc` would
    // give anyway.
    let Some(old) = dma_mem::read_desc(ring as usize, proc_id, idx) else {
        return E_INVAL;
    };
    let desc = match crate::board::BOARD.dma_engine {
        DmaEngine::VirtioSplit => {
            // The descriptor may be live, and the device may pair the
            // address of one version with the length of the other.
            // Refuse a re-aim whose mixtures would leave this module's
            // memory.
            let (old_pa, old_len) = virtq_desc_span(&old);
            if !reaim_keeps_mixtures_inside(old_pa, old_len, pa, len, mem_base, mem_len) {
                return E_INVAL;
            }
            virtq_desc(pa, len, dir)
        }
        DmaEngine::Dwmac4 { .. } => {
            // Never rewrite a descriptor the engine owns.
            if dwmac_desc_status(&old) & DWMAC_DESC_OWN != 0 {
                return E_INVAL;
            }
            match dir {
                // The engine fills RBSZ bytes; the span checked above
                // must be exactly that, starting on a bus-width
                // boundary so no burst beat spills past either end.
                RingDir::DeviceWrites if len == DWMAC_RBSZ && dwmac_rx_buf_aligned(buf_off) => {
                    dwmac_rx_desc(pa)
                }
                RingDir::DeviceReads if dwmac_tx_len_ok(len) => dwmac_tx_desc(pa, len),
                _ => return E_INVAL,
            }
        }
        DmaEngine::None => return E_INVAL,
    };
    if dma_mem::write_desc(ring as usize, proc_id, idx, desc) {
        0
    } else {
        E_INVAL
    }
}

/// `wari::nic_dma_status(ring, idx, out_off) -> i32`. See the module
/// docs for the contract.
pub fn nic_dma_status_impl<T>(
    caller: &mut wasmi::Caller<'_, T>,
    proc_id: u8,
    ring: u32,
    idx: u32,
    out_off: u32,
) -> i32 {
    /// Bytes of the status word stored at `out_off`.
    const STATUS_LEN: u32 = 4;
    if !has_net_right(proc_id, CAP_RIGHT_READ) {
        return E_PERM;
    }
    if !matches!(crate::board::BOARD.dma_engine, DmaEngine::Dwmac4 { .. }) {
        return E_INVAL;
    }
    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
        Some(m) => m,
        None => return E_NOMEM,
    };
    if !span_in_linmem(out_off, STATUS_LEN, memory.data(&*caller).len()) {
        return E_INVAL;
    }
    let Some(desc) = dma_mem::read_desc(ring as usize, proc_id, idx) else {
        return E_INVAL;
    };
    // Order the status read before anything the driver reads next:
    // once it sees OWN clear it reads the buffer the engine wrote
    // (Linux `dma_rmb()` after the OWN check). R6.
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    let status = dwmac_desc_status(&desc).to_le_bytes();
    match memory.write(&mut *caller, out_off as usize, &status) {
        Ok(()) => 0,
        Err(_) => E_INVAL,
    }
}
