// SPDX-License-Identifier: AGPL-3.0-only
//! Cap-fastpath submission-ring drain (Lane B / B1, PR-2b).
//!
//! Consumes the pure wire format (`wari_abi::ring`) and the soundness
//! predicate (`wari_abi::reg::validate_handle`) to make the registered-
//! capability fast path *functional*: a module sets up an SQ/CQ ring in
//! its own linear memory and rings the doorbell once; the kernel drains
//! the SQ, validates each entry against the per-process `RegTable`, and
//! delegates to the existing capability-checked host fn by resolved
//! handle. See `docs/cap-registered-fastpath-design.md` §5.
//!
//! v1 ops **reuse existing operations** (`notification_wait`/`ack`)
//! reached by a registered handle — the ring mints no new authority. The
//! generation re-check (INV-γ) means a revoked cap's handle fails on the
//! next drain, and every SQE is copied out of linear memory before use
//! (INV-β), so a mid-drain mutation can't change the decision the kernel
//! acts on.
//!
//! Two `unsafe`-free impls (the `static mut` access is encapsulated in
//! `cap::storage`); linear-memory bounds are enforced by wasmi's
//! `Memory::{read,write}`.

use wasmi::Caller;

use wari_abi::reg::{validate_handle, RegCheck, REG_SLOTS};
use wari_abi::ring::{
    decode_sqe, encode_cqe, is_known_op, Sqe, CQE_SIZE, RING_OP_NOTIFY_ACK,
    RING_OP_NOTIFY_WAIT, SQE_SIZE,
};

use super::cspace::MAX_PROCS;
use super::storage::{cspaces, reg_tables, ring_descriptors};
use super::syscall::{notification_ack_impl, notification_wait_impl, E_INVAL, E_NOMEM, E_PERM};
use super::types::ObjectKind;

/// Upper bound on ring entries, to keep a single `ring_submit` drain
/// bounded regardless of the descriptor the module claims. The real
/// per-access bound is enforced by wasmi at read/write time.
pub const MAX_RING_ENTRIES: u32 = 1024;

/// Per-process ring descriptor: where the SQ/CQ live in the module's
/// linear memory, recorded by `ring_setup`. `active == false` marks a
/// process with no ring configured.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RingDesc {
    /// Submission-queue base offset in the module's linear memory.
    pub sq_ptr: u32,
    /// Completion-queue base offset in the module's linear memory.
    pub cq_ptr: u32,
    /// Number of entries the ring can hold.
    pub entries: u32,
    /// Has this process configured a ring?
    pub active: bool,
}

impl RingDesc {
    /// A process with no ring configured.
    pub const fn empty() -> Self {
        RingDesc {
            sq_ptr: 0,
            cq_ptr: 0,
            entries: 0,
            active: false,
        }
    }
}

/// Is `op` permitted for a registered capability of `kind`? The v1 op set
/// reuses existing ops: notify wait/ack require a `Notification` cap. This
/// is proposed INV-α clause 4 (op vs kind), computed by the kernel and
/// fed to `validate_handle`.
#[inline]
fn op_permitted_for(kind: ObjectKind, op: u32) -> bool {
    match op {
        RING_OP_NOTIFY_WAIT | RING_OP_NOTIFY_ACK => {
            matches!(kind, ObjectKind::Notification)
        }
        _ => false,
    }
}

/// `wari::ring_setup(sq_ptr, cq_ptr, entries) -> i32`.
///
/// Record the caller's SQ/CQ ring location. Returns `0` on success,
/// `E_INVAL` on a bad proc / zero or oversized entry count. The linear-
/// memory bounds of `sq_ptr`/`cq_ptr` are checked per-access during the
/// drain (wasmi enforces them), so setup only sanity-bounds `entries`.
pub fn ring_setup_impl(proc_id: u8, sq_ptr: u32, cq_ptr: u32, entries: u32) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if entries == 0 || entries > MAX_RING_ENTRIES {
        return E_INVAL;
    }
    let rd = ring_descriptors();
    rd[proc_id as usize] = RingDesc {
        sq_ptr,
        cq_ptr,
        entries,
        active: true,
    };
    0
}

/// `wari::ring_submit(n) -> i32`.
///
/// Drain up to `n` submission entries (bounded by the configured ring
/// size). For each entry: copy the SQE out of linear memory (INV-β),
/// validate the registered handle (INV-α/γ via `validate_handle`),
/// delegate to the matching op, and post a completion entry. Returns the
/// number of entries processed (`>= 0`), or a negative errno if the ring
/// is unconfigured / memory is unavailable.
///
/// Generic over the host state `T` so the one impl serves both Tier-1 and
/// Tier-2 linkers (like `cap_lookup_impl`).
pub fn ring_submit_impl<T>(caller: &mut Caller<'_, T>, proc_id: u8, n: u32) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    let desc = {
        let rd = ring_descriptors();
        rd[proc_id as usize]
    };
    if !desc.active {
        return E_INVAL;
    }
    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
        Some(m) => m,
        None => return E_NOMEM,
    };

    let count = if n < desc.entries { n } else { desc.entries };
    let mut processed: i32 = 0;
    let mut i: u32 = 0;
    while i < count {
        // Copy-before-use (INV-β): read the whole SQE into kernel memory
        // before any validation. A later mutation of linear memory by the
        // module cannot affect the decision below.
        let sq_off = desc.sq_ptr as usize + (i as usize) * SQE_SIZE;
        let mut sqe_buf = [0u8; SQE_SIZE];
        if memory.read(&*caller, sq_off, &mut sqe_buf).is_err() {
            break; // OOB submission region — stop draining.
        }
        let sqe = match decode_sqe(&sqe_buf, 0) {
            Some(s) => s,
            None => break,
        };

        let result = dispatch(proc_id, &sqe);

        let cqe = encode_cqe(sqe.user_data, result as i64);
        let cq_off = desc.cq_ptr as usize + (i as usize) * CQE_SIZE;
        if memory.write(&mut *caller, cq_off, &cqe).is_err() {
            break; // OOB completion region — stop.
        }
        processed += 1;
        i += 1;
    }
    processed
}

/// Validate one decoded SQE against the registered-handle table and
/// delegate to the underlying op. Returns the op's `i32` result (or a
/// negative errno for a rejected handle).
fn dispatch(proc_id: u8, sqe: &Sqe) -> i32 {
    if !is_known_op(sqe.op) {
        return E_INVAL;
    }
    if sqe.reg_idx >= REG_SLOTS {
        return E_INVAL;
    }
    // Snapshot the registered entry (Copy) so the reg-table borrow does
    // not overlap the cspaces borrow below.
    let entry = {
        let rt = reg_tables();
        rt[proc_id as usize].slots[sqe.reg_idx as usize]
    };
    let live = !entry.is_empty();
    let cur_gen = if live {
        let cs = cspaces();
        cs[proc_id as usize].generations[entry.cspace_slot as usize]
    } else {
        0
    };

    let op_ok = op_permitted_for(entry.kind, sqe.op);
    match validate_handle(sqe.reg_idx, live, entry.reg_generation, cur_gen, op_ok) {
        RegCheck::Ok => {}
        RegCheck::NotPermitted => return E_PERM,
        // OutOfRange / Empty / Stale — a forged, dropped, or revoked
        // handle. Fail closed.
        _ => return E_INVAL,
    }

    // Delegate to the existing capability-checked op by the resolved
    // CSpace slot. The op re-checks the cap; the ring adds batching +
    // the generation guard, not new authority.
    match sqe.op {
        RING_OP_NOTIFY_WAIT => notification_wait_impl(proc_id, entry.cspace_slot as u32),
        RING_OP_NOTIFY_ACK => notification_ack_impl(proc_id, entry.cspace_slot as u32),
        _ => E_INVAL,
    }
}
