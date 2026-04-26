// SPDX-License-Identifier: AGPL-3.0-only
//! Capability-management syscalls — the userspace-facing surface of
//! the cap system.
//!
//! Phase 1b ships these as **WASM host functions** registered with
//! the wasmi linker, *not* as RISC-V `ecall` syscalls. The Phase-0
//! kernel has no userspace ecall path (every Wari userspace module
//! is WASM, by R7), so the host-fn registration in
//! `runtime/{host_fns,wasi}.rs` is the actual ABI carrier. The
//! `SYS_CAP_*` constants in `wari-abi` document the same surface as
//! sysnums for the day a non-WASM userspace ever appears (it
//! shouldn't, but the design contract in `docs/cap-system-design.md`
//! references them).
//!
//! All five host fns return an `i32`:
//!   - `0` on success
//!   - `E_PERM` (`-1`) on permission denial
//!   - `E_INVAL` (`-2`) on bad arguments / out-of-bounds slot
//!   - `E_NOMEM` (`-3`) on pool exhaustion (cap_lookup OOB write)
//!
//! Errno values match the existing `runtime::host_fns` convention.
//!
//! ## Why these are pure functions of `(proc_id, args)`
//!
//! The host fn closure registered for Tier-1 is shaped:
//!
//! ```text
//! linker.func_wrap("wari", "cap_mint", |_caller, ps, ts, r, b| {
//!     cap_mint_impl(PROC_ID_TIER1_HELLO, ps, ts, r, b)
//! });
//! ```
//!
//! The `proc_id` is baked in at registration time. The
//! implementation here doesn't read the wasmi `Caller` at all
//! (except `cap_lookup_impl` which writes to caller's linear
//! memory). This keeps the impl testable without a wasmi context
//! and matches the goose-os pattern in similar IPC dispatchers.

#![allow(dead_code)]
#![allow(clippy::doc_lazy_continuation)]

use wasmi::Caller;

use super::cspace::{CSPACE_SLOTS, MAX_PROCS};
use super::revoke::{dec_refcount, inc_refcount, revoke};
use super::storage::{cspaces, object_pools};
use super::types::{
    Cap, CapId, ObjectKind, CAP_RIGHTS_PHASE_1B_MASK, CAP_RIGHT_READ,
    CAP_RIGHT_WRITE,
};

// ─────────────────────────────────────────────────────────────────
// Errno values — match runtime::host_fns convention
// ─────────────────────────────────────────────────────────────────

/// Returned to WASM when a capability check fails.
pub const E_PERM: i32 = -1;
/// Returned to WASM when an argument is malformed or out of bounds.
pub const E_INVAL: i32 = -2;
/// Returned to WASM when a pool is full or memory write fails.
pub const E_NOMEM: i32 = -3;
/// Returned to WASM when an operation would block (no IRQ pending,
/// recv buffer empty, etc.). Phase-1b polling primitive.
pub const E_AGAIN: i32 = -4;
/// Returned to WASM when a TCP socket op is attempted on a socket
/// that is not in the connected state. Added in PR Net-2 for the
/// upcoming socket host fns; consumed by PR Net-6.
pub const E_NOTCONN: i32 = -5;
/// Returned to WASM when a TCP connect attempt is rejected by the
/// peer (RST). Added in PR Net-2; consumed by PR Net-6.
pub const E_REFUSED: i32 = -6;

// ─────────────────────────────────────────────────────────────────
// check_cap — runtime permission gate
// ─────────────────────────────────────────────────────────────────

/// Verify that process `proc_id` holds a capability of `expected_kind`
/// at `slot` with **all** of the bits in `required_rights` set.
///
/// Used by host functions on the runtime fast-path (PR 3b
/// migration) to replace the legacy `host.caps.<bool>` pattern with
/// a real cap lookup. Returns `Ok(())` on success; `Err(E_PERM)` on
/// any failure. Bounds errors collapse into `E_PERM` so userspace
/// cannot distinguish "I don't have the cap" from "I asked for a
/// nonexistent slot" — both are caller errors with the same
/// remediation (don't do that).
///
/// # Invariants
///
/// - **INV-18** (CSpace Slot Index Bounds): bounds-checks `proc_id <
///   MAX_PROCS` and `slot < CSPACE_SLOTS` before any indexing.
/// - **INV-15** (Forgery Prevention): only reads the cap; never
///   constructs one.
pub fn check_cap(
    proc_id: u8,
    slot: u8,
    expected_kind: ObjectKind,
    required_rights: u8,
) -> Result<(), i32> {
    if (proc_id as usize) >= MAX_PROCS {
        return Err(E_PERM);
    }
    if (slot as usize) >= CSPACE_SLOTS {
        return Err(E_PERM);
    }
    let cs = cspaces();
    let cap = cs[proc_id as usize].slots[slot as usize];
    if cap.is_empty() {
        return Err(E_PERM);
    }
    if cap.kind != expected_kind {
        return Err(E_PERM);
    }
    if cap.rights & required_rights != required_rights {
        return Err(E_PERM);
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────
// cap_mint
// ─────────────────────────────────────────────────────────────────

/// `wari::cap_mint(parent_slot, target_slot, rights, badge) -> i32`.
///
/// Derive a child cap from the cap at `parent_slot` and install it
/// at `target_slot`, with the requested rights subset and badge.
///
/// Enforces INV-10 (rights monotonicity), INV-15 (reserved bits
/// rejected), INV-16 (kind/pool preservation), INV-18 (slot bounds).
pub fn cap_mint_impl(
    proc_id: u8,
    parent_slot: u32,
    target_slot: u32,
    rights: u32,
    badge: u32,
) -> i32 {
    // Bounds checks (INV-18).
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if parent_slot >= CSPACE_SLOTS as u32 || target_slot >= CSPACE_SLOTS as u32 {
        return E_INVAL;
    }
    if rights > 0xFF {
        // The full WASM i32 rights value must fit in a u8.
        return E_INVAL;
    }
    let parent_slot = parent_slot as u8;
    let target_slot = target_slot as u8;
    let rights = rights as u8;

    // Snapshot parent + parent_id while holding the cspaces borrow.
    let (parent_cap, parent_id) = {
        let cs = cspaces();
        let parent = cs[proc_id as usize].slots[parent_slot as usize];
        if parent.is_empty() {
            return E_INVAL;
        }
        if !cs[proc_id as usize].slots[target_slot as usize].is_empty() {
            return E_INVAL;
        }
        let gen = cs[proc_id as usize].generations[parent_slot as usize];
        let id = CapId::new(proc_id, parent_slot, gen);
        (parent, id)
    };

    // Pure-function derive (PR 1).
    let mut child = match Cap::derive(&parent_cap, parent_id, rights, badge) {
        Ok(c) => c,
        Err(crate::error::KernelError::PermissionDenied) => return E_PERM,
        Err(_) => return E_INVAL,
    };

    // The child carries the target slot's current generation so any
    // future revoke walk can detect ABA.
    let target_gen = {
        let cs = cspaces();
        cs[proc_id as usize].generations[target_slot as usize]
    };
    child.generation = target_gen as u32;

    // Install child + bump object refcount.
    {
        let cs = cspaces();
        cs[proc_id as usize].slots[target_slot as usize] = child;
    }
    inc_refcount(child.kind, child.pool_index);

    0
}

// ─────────────────────────────────────────────────────────────────
// cap_copy
// ─────────────────────────────────────────────────────────────────

/// `wari::cap_copy(src_slot, target_slot) -> i32`.
///
/// Same-rights duplicate of the cap at `src_slot` into `target_slot`.
/// The new cap shares the parent of the source (sibling, not child).
/// Used by callers who want two slots referencing the same cap.
pub fn cap_copy_impl(proc_id: u8, src_slot: u32, target_slot: u32) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if src_slot >= CSPACE_SLOTS as u32 || target_slot >= CSPACE_SLOTS as u32 {
        return E_INVAL;
    }
    let src_slot = src_slot as u8;
    let target_slot = target_slot as u8;

    let copied = {
        let cs = cspaces();
        let src = cs[proc_id as usize].slots[src_slot as usize];
        if src.is_empty() {
            return E_INVAL;
        }
        if !cs[proc_id as usize].slots[target_slot as usize].is_empty() {
            return E_INVAL;
        }
        let target_gen = cs[proc_id as usize].generations[target_slot as usize];
        let mut c = src;
        // Copy is a sibling — same parent as src, same rights — but
        // gets the target slot's generation.
        c.generation = target_gen as u32;
        cs[proc_id as usize].slots[target_slot as usize] = c;
        c
    };
    inc_refcount(copied.kind, copied.pool_index);

    0
}

// ─────────────────────────────────────────────────────────────────
// cap_revoke
// ─────────────────────────────────────────────────────────────────

/// `wari::cap_revoke(slot) -> i32`.
///
/// Revoke the cap at `slot` and every descendant. Requires the cap
/// to have `CAP_RIGHT_WRITE` (Phase 1b convention; PR 3 §10 Q6).
pub fn cap_revoke_impl(proc_id: u8, slot: u32) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if slot >= CSPACE_SLOTS as u32 {
        return E_INVAL;
    }
    let slot = slot as u8;

    // Permission check: caller must hold WRITE on the cap to revoke.
    {
        let cs = cspaces();
        let cap = cs[proc_id as usize].slots[slot as usize];
        if cap.is_empty() {
            return E_INVAL;
        }
        if cap.rights & CAP_RIGHT_WRITE == 0 {
            return E_PERM;
        }
    }

    match revoke(proc_id, slot) {
        Ok(()) => 0,
        Err(_) => E_INVAL,
    }
}

// ─────────────────────────────────────────────────────────────────
// cap_delete
// ─────────────────────────────────────────────────────────────────

/// `wari::cap_delete(slot) -> i32`.
///
/// Remove the cap at `slot` without cascading. The kernel object's
/// refcount is decremented; the object is freed if the count hits
/// zero. Descendants of the deleted cap are NOT affected — they
/// become orphaned via INV-17 (their parent slot's generation will
/// no longer match) and get cleaned up on the next revoke walk that
/// touches them, or when their containing process exits.
pub fn cap_delete_impl(proc_id: u8, slot: u32) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if slot >= CSPACE_SLOTS as u32 {
        return E_INVAL;
    }
    let slot = slot as u8;

    let (kind, pool_index) = {
        let cs = cspaces();
        let slot_ref = &mut cs[proc_id as usize].slots[slot as usize];
        if slot_ref.is_empty() {
            return E_INVAL;
        }
        let info = (slot_ref.kind, slot_ref.pool_index);
        *slot_ref = Cap::empty();
        let g = &mut cs[proc_id as usize].generations[slot as usize];
        *g = g.saturating_add(1);
        info
    };
    dec_refcount(kind, pool_index);

    0
}

// ─────────────────────────────────────────────────────────────────
// notification_wait / notification_ack
// ─────────────────────────────────────────────────────────────────

/// `wari::notification_wait(slot) -> i32`.
///
/// Phase-1b **polling** primitive: returns `0` immediately if any
/// signal bit is set on the Notification at `slot`, `E_AGAIN` if
/// the bitmap is zero, `E_PERM` if the slot doesn't hold a
/// Notification cap with READ rights.
///
/// Drivers that need IRQ-driven processing call this in a loop
/// (yielding via `cap_lookup` or arbitrary host fns until the
/// kernel's trap dispatcher signals the bound IRQ).
///
/// Phase 2+ extends this to a real blocking primitive backed by a
/// scheduler wait queue; for Phase 1b polling is acceptable
/// because (a) the only caller is the net driver which is
/// re-entered by every Tier-1 socket call anyway, (b) we have no
/// preemption so a busy-wait blocks the system — by design,
/// drivers must check this once per dispatch and return.
pub fn notification_wait_impl(proc_id: u8, slot: u32) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if slot >= CSPACE_SLOTS as u32 {
        return E_INVAL;
    }
    let slot = slot as u8;
    let cap = {
        let cs = cspaces();
        cs[proc_id as usize].slots[slot as usize]
    };
    if cap.is_empty() {
        return E_PERM;
    }
    if !matches!(cap.kind, ObjectKind::Notification) {
        return E_PERM;
    }
    if cap.rights & CAP_RIGHT_READ == 0 {
        return E_PERM;
    }
    let pools = object_pools();
    if let Some(notif) = pools.notifications.get(cap.pool_index) {
        if notif.signals != 0 {
            0
        } else {
            E_AGAIN
        }
    } else {
        E_INVAL
    }
}

/// `wari::notification_ack(slot) -> i32`.
///
/// Clears all signal bits on the Notification at `slot`. Used by
/// drivers after they have processed the IRQ work and want to
/// re-arm for the next signal.
///
/// Phase 1b clears all bits at once (doesn't accept a per-bit
/// mask); the only caller is the single-IRQ-per-driver pattern
/// where there's nothing finer to ack.
pub fn notification_ack_impl(proc_id: u8, slot: u32) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if slot >= CSPACE_SLOTS as u32 {
        return E_INVAL;
    }
    let slot = slot as u8;
    let cap = {
        let cs = cspaces();
        cs[proc_id as usize].slots[slot as usize]
    };
    if cap.is_empty() {
        return E_PERM;
    }
    if !matches!(cap.kind, ObjectKind::Notification) {
        return E_PERM;
    }
    if cap.rights & CAP_RIGHT_READ == 0 {
        return E_PERM;
    }
    let pools = object_pools();
    if let Some(notif) = pools.notifications.get_mut(cap.pool_index) {
        notif.signals = 0;
        0
    } else {
        E_INVAL
    }
}

// ─────────────────────────────────────────────────────────────────
// cap_lookup
// ─────────────────────────────────────────────────────────────────

/// In-memory layout of `CapInfo` written by `cap_lookup`.
///
/// 8 bytes total, repr(C), little-endian (RISC-V default):
///
/// ```text
///   offset  size  field
///   ──────  ────  ────────
///   0       1     kind   (ObjectKind discriminant)
///   1       1     rights
///   2-3     2     _padding
///   4-7     4     badge
/// ```
///
/// Note: parent CapId, pool_index, and slot generation are
/// **not** exposed to userspace (kernel-internal — INV-15 and
/// design-doc §10 Q5).
const CAP_INFO_SIZE: usize = 8;

/// `wari::cap_lookup(slot, out_buf) -> i32`.
///
/// Read metadata for the cap at `slot` and write `CapInfo` (8 bytes)
/// to `out_buf` in the caller's WASM linear memory. Returns 0 on
/// success even if the slot is empty (the written `CapInfo` will
/// have `kind = Empty = 0`); errors are reserved for OOB slot,
/// missing memory export, and OOB linear-memory write.
pub fn cap_lookup_impl<T>(
    caller: &mut Caller<'_, T>,
    proc_id: u8,
    slot: u32,
    out_buf: u32,
) -> i32 {
    if (proc_id as usize) >= MAX_PROCS {
        return E_INVAL;
    }
    if slot >= CSPACE_SLOTS as u32 {
        return E_INVAL;
    }
    let slot = slot as u8;

    let (kind_disc, rights, badge) = {
        let cs = cspaces();
        let cap = cs[proc_id as usize].slots[slot as usize];
        (cap.kind as u8, cap.rights, cap.badge)
    };

    // Build the 8-byte CapInfo on the stack.
    let mut buf = [0u8; CAP_INFO_SIZE];
    buf[0] = kind_disc;
    buf[1] = rights;
    // bytes 2..4 are reserved padding, left zeroed
    buf[4..8].copy_from_slice(&badge.to_le_bytes());

    // Resolve the caller's linear memory and write the buffer.
    let memory = match caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
    {
        Some(m) => m,
        None => return E_NOMEM,
    };
    if memory
        .write(&mut *caller, out_buf as usize, &buf)
        .is_err()
    {
        return E_NOMEM;
    }
    0
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cap::types::{CAP_RIGHT_READ, CAP_RIGHT_WRITE};

    // These tests exercise the bounds-checking and rights paths.
    // Setup of populated CSpaces in tests requires touching the
    // global statics, which is awkward — full integration coverage
    // lives in the QEMU smoke test after PR 3a lands and in
    // `tests/security/cap_*.rs` (a follow-up PR).

    #[test]
    fn errno_values_distinct() {
        assert_ne!(E_PERM, E_INVAL);
        assert_ne!(E_PERM, E_NOMEM);
        assert_ne!(E_INVAL, E_NOMEM);
    }

    #[test]
    fn errno_values_are_negative() {
        assert!(E_PERM < 0);
        assert!(E_INVAL < 0);
        assert!(E_NOMEM < 0);
    }

    #[test]
    fn cap_mint_rejects_oob_proc_id() {
        let r = cap_mint_impl(MAX_PROCS as u8, 0, 1, CAP_RIGHT_READ as u32, 0);
        assert_eq!(r, E_INVAL);
    }

    #[test]
    fn cap_mint_rejects_oob_parent_slot() {
        let r = cap_mint_impl(0, CSPACE_SLOTS as u32, 1, CAP_RIGHT_READ as u32, 0);
        assert_eq!(r, E_INVAL);
    }

    #[test]
    fn cap_mint_rejects_oob_target_slot() {
        let r = cap_mint_impl(0, 0, CSPACE_SLOTS as u32, CAP_RIGHT_READ as u32, 0);
        assert_eq!(r, E_INVAL);
    }

    #[test]
    fn cap_mint_rejects_oversize_rights() {
        let r = cap_mint_impl(0, 0, 1, 0xDEAD_BEEF, 0);
        assert_eq!(r, E_INVAL);
    }

    #[test]
    fn cap_copy_rejects_oob_proc_id() {
        let r = cap_copy_impl(MAX_PROCS as u8, 0, 1);
        assert_eq!(r, E_INVAL);
    }

    #[test]
    fn cap_revoke_rejects_oob_slot() {
        let r = cap_revoke_impl(0, CSPACE_SLOTS as u32);
        assert_eq!(r, E_INVAL);
    }

    #[test]
    fn cap_delete_rejects_oob_proc_id() {
        let r = cap_delete_impl(MAX_PROCS as u8, 0);
        assert_eq!(r, E_INVAL);
    }
}
