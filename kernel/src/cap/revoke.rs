// SPDX-License-Identifier: AGPL-3.0-only
//! Capability revocation cascade.
//!
//! Revoking a cap invalidates that cap **and every descendant** of it
//! in the derivation tree. A descendant is any cap whose `parent`
//! chain (followed transitively) reaches the revoked cap.
//!
//! ## Algorithm
//!
//! **Phase A — discovery (no mutation)**:
//!
//! 1. Initialize the revoke set R = {(target_proc, target_slot)}.
//! 2. Repeat until no changes:
//!    For every (proc, slot) in every CSpace, if cap is not empty
//!    and `cap.parent` is non-ROOT and `(cap.parent.proc, .slot)` is
//!    in R and `cap.parent.generation` matches the parent slot's
//!    current generation (else cap is orphaned, INV-17 anti-ABA),
//!    add (proc, slot) to R.
//!
//! **Phase B — clear (mutation)**:
//!
//! For every (proc, slot) in R, alternating short borrows:
//!   1. Take a `cspaces()` ref, read `(kind, pool_index)`, clear the
//!      cap, bump generation, drop the ref.
//!   2. Take an `object_pools()` ref, decrement that object's
//!      refcount; if zero, deallocate; drop the ref.
//!
//! The alternation avoids holding both `cspaces` and `object_pools`
//! references simultaneously and keeps stack usage constant (no big
//! snapshot array).
//!
//! ## Complexity
//!
//! Discovery: worst case `MAX_PROCS × CSPACE_SLOTS = 4 096` passes,
//! each scanning the same 4 096 caps → ~16 M cap inspections.
//! Microseconds on a modern RV64 core; revoke is not a hot path.
//!
//! ## Atomicity
//!
//! Phase 1b runs revoke to completion under INV-1 (single-hart
//! kernel, interrupts masked during S-mode trap service). No other
//! code path observes a partial revoke.

#![allow(dead_code)]
#![allow(clippy::doc_lazy_continuation)]

use crate::error::KernelError;

use super::cspace::{CSPACE_SLOTS, MAX_PROCS};
use super::storage::{cspaces, object_pools};
use super::types::{Cap, ObjectKind};

// ─────────────────────────────────────────────────────────────────
// RevokeSet — fixed-size bitmap of (proc_id, slot) pairs
// ─────────────────────────────────────────────────────────────────

/// Number of `u64` words in the revoke bitmap.
///
/// `MAX_PROCS × CSPACE_SLOTS = 16 × 256 = 4 096` bits = 64 u64 words
/// = 512 bytes.
const REVOKE_BITS_WORDS: usize = (MAX_PROCS * CSPACE_SLOTS + 63) / 64;

/// Stack-allocated bitmap tracking which CSpace slots are part of
/// the current revoke cascade.
struct RevokeSet {
    bits: [u64; REVOKE_BITS_WORDS],
}

impl RevokeSet {
    const fn new() -> Self {
        Self {
            bits: [0; REVOKE_BITS_WORDS],
        }
    }

    /// Bit index for `(proc_id, slot)`.
    const fn idx(proc_id: u8, slot: u8) -> usize {
        proc_id as usize * CSPACE_SLOTS + slot as usize
    }

    fn add(&mut self, proc_id: u8, slot: u8) {
        let i = Self::idx(proc_id, slot);
        self.bits[i / 64] |= 1u64 << (i % 64);
    }

    fn contains(&self, proc_id: u8, slot: u8) -> bool {
        let i = Self::idx(proc_id, slot);
        (self.bits[i / 64] >> (i % 64)) & 1 == 1
    }
}

// ─────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────

/// Revoke the cap at `(proc_id, slot)` and every descendant.
///
/// # Contract
///
/// - **Precondition**: callers bounds-check `proc_id < MAX_PROCS`
///   and `slot < CSPACE_SLOTS` (INV-18). This function repeats the
///   check defensively.
/// - **Postcondition on success**:
///   - The cap at `(proc_id, slot)` and every descendant are empty.
///   - Every cleared slot's generation counter has been bumped.
///   - Every kernel object whose refcount dropped to zero has been
///     returned to its pool.
/// - **Errors**: `KernelError::InvalidArgument` if the initial slot
///   is empty.
pub fn revoke(proc_id: u8, slot: u8) -> Result<(), KernelError> {
    if (proc_id as usize) >= MAX_PROCS || (slot as usize) >= CSPACE_SLOTS {
        return Err(KernelError::InvalidArgument);
    }
    {
        let cs = cspaces();
        if cs[proc_id as usize].slots[slot as usize].is_empty() {
            return Err(KernelError::InvalidArgument);
        }
    }

    let mut set = RevokeSet::new();
    set.add(proc_id, slot);
    discover(&mut set);
    clear(&set);

    Ok(())
}

/// Decrement a kernel object's refcount, deallocating if zero. Used
/// by `cap_delete` (single-cap removal, no cascade) and by the
/// cascade-clear path in this module.
pub fn dec_refcount(kind: ObjectKind, pool_index: u16) {
    let pools = object_pools();
    match kind {
        ObjectKind::Empty => {}
        ObjectKind::Endpoint => {
            if let Some(obj) = pools.endpoints.get_mut(pool_index) {
                obj.refcount = obj.refcount.saturating_sub(1);
                if obj.refcount == 0 {
                    let _ = pools.endpoints.dealloc(pool_index);
                }
            }
        }
        ObjectKind::Notification => {
            if let Some(obj) = pools.notifications.get_mut(pool_index) {
                obj.refcount = obj.refcount.saturating_sub(1);
                if obj.refcount == 0 {
                    let _ = pools.notifications.dealloc(pool_index);
                }
            }
        }
        ObjectKind::Untyped => {
            if let Some(obj) = pools.untypeds.get_mut(pool_index) {
                obj.refcount = obj.refcount.saturating_sub(1);
                if obj.refcount == 0 {
                    let _ = pools.untypeds.dealloc(pool_index);
                }
            }
        }
        ObjectKind::Frame => {
            if let Some(obj) = pools.frames.get_mut(pool_index) {
                obj.refcount = obj.refcount.saturating_sub(1);
                if obj.refcount == 0 {
                    let _ = pools.frames.dealloc(pool_index);
                }
            }
        }
        ObjectKind::Net => {
            if let Some(obj) = pools.nets.get_mut(pool_index) {
                obj.refcount = obj.refcount.saturating_sub(1);
                if obj.refcount == 0 {
                    let _ = pools.nets.dealloc(pool_index);
                }
            }
        }
        ObjectKind::Socket => {
            if let Some(obj) = pools.sockets.get_mut(pool_index) {
                obj.refcount = obj.refcount.saturating_sub(1);
                if obj.refcount == 0 {
                    let _ = pools.sockets.dealloc(pool_index);
                }
            }
        }
    }
}

/// Increment a kernel object's refcount. Used by mint / copy when a
/// new cap is installed pointing at an existing object.
pub fn inc_refcount(kind: ObjectKind, pool_index: u16) {
    let pools = object_pools();
    match kind {
        ObjectKind::Empty => {}
        ObjectKind::Endpoint => {
            if let Some(obj) = pools.endpoints.get_mut(pool_index) {
                obj.refcount = obj.refcount.saturating_add(1);
            }
        }
        ObjectKind::Notification => {
            if let Some(obj) = pools.notifications.get_mut(pool_index) {
                obj.refcount = obj.refcount.saturating_add(1);
            }
        }
        ObjectKind::Untyped => {
            if let Some(obj) = pools.untypeds.get_mut(pool_index) {
                obj.refcount = obj.refcount.saturating_add(1);
            }
        }
        ObjectKind::Frame => {
            if let Some(obj) = pools.frames.get_mut(pool_index) {
                obj.refcount = obj.refcount.saturating_add(1);
            }
        }
        ObjectKind::Net => {
            if let Some(obj) = pools.nets.get_mut(pool_index) {
                obj.refcount = obj.refcount.saturating_add(1);
            }
        }
        ObjectKind::Socket => {
            if let Some(obj) = pools.sockets.get_mut(pool_index) {
                obj.refcount = obj.refcount.saturating_add(1);
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// Phase A: discovery
// ─────────────────────────────────────────────────────────────────

fn discover(set: &mut RevokeSet) {
    let cs = cspaces();
    let mut changed = true;
    while changed {
        changed = false;
        for proc_idx in 0..MAX_PROCS as u8 {
            for slot_idx in 0..CSPACE_SLOTS as u8 {
                if set.contains(proc_idx, slot_idx) {
                    continue;
                }
                let cap = &cs[proc_idx as usize].slots[slot_idx as usize];
                if cap.is_empty() {
                    continue;
                }
                if cap.parent.is_root() {
                    continue;
                }
                let p_proc = cap.parent.proc_id();
                let p_slot = cap.parent.slot();
                let p_gen = cap.parent.generation();
                if !set.contains(p_proc, p_slot) {
                    continue;
                }
                // INV-17 anti-ABA: the parent slot's current
                // generation must match the generation recorded in
                // this cap's parent reference. Otherwise the cap is
                // orphaned and is NOT a descendant of the current
                // target; it will be cleaned up on its own future
                // revoke walk.
                if cs[p_proc as usize].generations[p_slot as usize] != p_gen {
                    continue;
                }
                set.add(proc_idx, slot_idx);
                changed = true;
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// Phase B: clear
// ─────────────────────────────────────────────────────────────────

fn clear(set: &RevokeSet) {
    for proc_idx in 0..MAX_PROCS as u8 {
        for slot_idx in 0..CSPACE_SLOTS as u8 {
            if !set.contains(proc_idx, slot_idx) {
                continue;
            }
            // Borrow scope 1: read cap kind+pool_index, clear cap,
            // bump generation. Drop borrow before touching pools.
            let (kind, pool_index) = {
                let cs = cspaces();
                let slot = &mut cs[proc_idx as usize].slots[slot_idx as usize];
                if slot.is_empty() {
                    continue;
                }
                let info = (slot.kind, slot.pool_index);
                *slot = Cap::empty();
                let g = &mut cs[proc_idx as usize].generations[slot_idx as usize];
                *g = g.saturating_add(1);
                info
            };
            // Borrow scope 2: decrement object refcount, possibly free.
            dec_refcount(kind, pool_index);
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // The discover/clear functions touch global static state
    // (cspaces, object_pools), which is awkward to set up in unit
    // tests. We instead test the RevokeSet bitmap directly here;
    // integration of the cascade walk against real CSpaces is
    // covered by the QEMU smoke test after PR 3a lands and by the
    // adversarial tests in `tests/security/`.

    #[test]
    fn revoke_set_starts_empty() {
        let s = RevokeSet::new();
        for p in 0..MAX_PROCS as u8 {
            for s_idx in 0..CSPACE_SLOTS as u8 {
                assert!(!s.contains(p, s_idx));
            }
        }
    }

    #[test]
    fn revoke_set_add_and_contains() {
        let mut s = RevokeSet::new();
        s.add(2, 5);
        assert!(s.contains(2, 5));
        assert!(!s.contains(2, 6));
        assert!(!s.contains(3, 5));
    }

    #[test]
    fn revoke_set_handles_extremes() {
        let mut s = RevokeSet::new();
        s.add(0, 0);
        s.add((MAX_PROCS - 1) as u8, (CSPACE_SLOTS - 1) as u8);
        assert!(s.contains(0, 0));
        assert!(s.contains((MAX_PROCS - 1) as u8, (CSPACE_SLOTS - 1) as u8));
    }

    #[test]
    fn revoke_set_bit_isolation() {
        let mut s = RevokeSet::new();
        s.add(7, 100);
        // Adjacent bits in same word stay clear.
        assert!(!s.contains(7, 99));
        assert!(!s.contains(7, 101));
        // Different proc, same slot stays clear.
        assert!(!s.contains(8, 100));
    }
}
