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
//! attached by the caller, `idx` is past its end, or the span is not
//! inside linear memory, `E_NOMEM` if the module exports no memory.
//! On any error nothing is written.
//!
//! A ring comes into being when the driver attaches its queue:
//! `nic_attach_queue` (VirtIO) claims it. The GMAC path gains its own
//! operations in the VF2 follow-up.

use wari_validate::dma::{span_in_linmem, virtq_desc, RingDir};

use super::cspace::MAX_PROCS;
use super::storage::cspaces;
use super::syscall::{E_INVAL, E_NOMEM, E_PERM};
use super::types::{ObjectKind, CAP_RIGHT_WRITE};
use crate::mmio::dma_mem;

/// Ring id of the receive queue (VirtIO-net §5.1.6.1: receiveq 0).
pub const RING_RX: u32 = 0;

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
    if dma_mem::write_desc(ring as usize, proc_id, idx, virtq_desc(pa, len, dir)) {
        0
    } else {
        E_INVAL
    }
}
