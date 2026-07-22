// SPDX-License-Identifier: AGPL-3.0-only
//! Capability revocation cascade.
//!
//! Revoking a cap invalidates that cap **and every descendant** of it
//! in the derivation tree. A descendant is any cap whose `parent`
//! chain (followed transitively) reaches the revoked cap.
//!
//! Extracted from `kernel/src/cap/revoke.rs` (B-3 slice 3,
//! `docs/kernel-host-testing-design.md`) and **parameterized**: every
//! function here takes the CSpace array and object pools as `&mut`
//! arguments instead of reaching for the kernel's statics. The kernel
//! binding (`kernel/src/cap/revoke.rs`) passes
//! `storage::{cspaces(), object_pools()}` and keeps the original
//! signatures, so syscall call sites are unchanged — and the cascade
//! is host-testable against synthetic CSpaces for the first time.
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
//! For every (proc, slot) in R: read `(kind, pool_index)`, clear the
//! cap, bump the slot generation, then decrement that object's
//! refcount — deallocating it when the count reaches zero.
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

#![allow(clippy::doc_lazy_continuation)]

use wari_error::KernelError;

use crate::cspace::{CSpace, CSPACE_SLOTS, MAX_PROCS};
use crate::objects::ObjectPools;
use crate::types::{Cap, ObjectKind};

// ─────────────────────────────────────────────────────────────────
// RevokeSet — fixed-size bitmap of (proc_id, slot) pairs
// ─────────────────────────────────────────────────────────────────

/// Number of `u64` words in the revoke bitmap.
///
/// `MAX_PROCS × CSPACE_SLOTS = 16 × 256 = 4 096` bits = 64 u64 words
/// = 512 bytes.
const REVOKE_BITS_WORDS: usize = (MAX_PROCS * CSPACE_SLOTS).div_ceil(64);

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
///   is empty or out of range.
/// - **Panics**: never.
pub fn revoke(
    cs: &mut [CSpace; MAX_PROCS],
    pools: &mut ObjectPools,
    proc_id: u8,
    slot: u8,
) -> Result<(), KernelError> {
    if (proc_id as usize) >= MAX_PROCS || (slot as usize) >= CSPACE_SLOTS {
        return Err(KernelError::InvalidArgument);
    }
    if cs[proc_id as usize].slots[slot as usize].is_empty() {
        return Err(KernelError::InvalidArgument);
    }

    let mut set = RevokeSet::new();
    set.add(proc_id, slot);
    discover(cs, &mut set);
    clear(cs, pools, &set);

    Ok(())
}

/// Decrement a kernel object's refcount, deallocating if zero. Used
/// by `cap_delete` (single-cap removal, no cascade) and by the
/// cascade-clear path in this module.
///
/// # Contract
/// - Out-of-range or unallocated `pool_index` is a no-op (the object
///   is already gone; double-decrement cannot underflow past the
///   `saturating_sub`).
/// - Panics: never.
pub fn dec_refcount(pools: &mut ObjectPools, kind: ObjectKind, pool_index: u16) {
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
///
/// # Contract
/// - Out-of-range or unallocated `pool_index` is a no-op.
/// - Saturates at `u16::MAX` (no wraparound).
/// - Panics: never.
pub fn inc_refcount(pools: &mut ObjectPools, kind: ObjectKind, pool_index: u16) {
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

// NOTE on loop bounds: proc/slot indices iterate as `usize` and are
// cast to `u8` per-element. `CSPACE_SLOTS` is 256 — exactly the u8
// value space — so `0..CSPACE_SLOTS as u8` would truncate 256 to 0
// and never iterate. That exact bug shipped in the pre-extraction
// kernel version of this file and made the whole cascade a no-op;
// the regression tests below pin the corrected behavior.

fn discover(cs: &[CSpace; MAX_PROCS], set: &mut RevokeSet) {
    let mut changed = true;
    while changed {
        changed = false;
        for proc_idx in 0..MAX_PROCS {
            for slot_idx in 0..CSPACE_SLOTS {
                let (p, s) = (proc_idx as u8, slot_idx as u8);
                if set.contains(p, s) {
                    continue;
                }
                let cap = &cs[proc_idx].slots[slot_idx];
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
                set.add(p, s);
                changed = true;
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// Phase B: clear
// ─────────────────────────────────────────────────────────────────

fn clear(cs: &mut [CSpace; MAX_PROCS], pools: &mut ObjectPools, set: &RevokeSet) {
    for (proc_idx, cspace) in cs.iter_mut().enumerate() {
        for slot_idx in 0..CSPACE_SLOTS {
            if !set.contains(proc_idx as u8, slot_idx as u8) {
                continue;
            }
            let slot = &mut cspace.slots[slot_idx];
            if slot.is_empty() {
                continue;
            }
            let (kind, pool_index) = (slot.kind, slot.pool_index);
            *slot = Cap::empty();
            let g = &mut cspace.generations[slot_idx];
            *g = g.saturating_add(1);
            dec_refcount(pools, kind, pool_index);
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::Endpoint;
    use crate::types::{CapId, CAP_RIGHT_GRANT, CAP_RIGHT_READ, CAP_RIGHT_WRITE};

    // ---- helpers ----

    fn fresh() -> ([CSpace; MAX_PROCS], ObjectPools) {
        ([const { CSpace::new() }; MAX_PROCS], ObjectPools::new())
    }

    /// Install a root (kernel-minted, parent = ROOT) Endpoint cap at
    /// `(proc_id, slot)` over a freshly-allocated endpoint. Returns
    /// the endpoint's pool index.
    fn install_root_ep(
        cs: &mut [CSpace; MAX_PROCS],
        pools: &mut ObjectPools,
        proc_id: u8,
        slot: u8,
    ) -> u16 {
        let ep_idx = pools.endpoints.alloc(Endpoint::new()).unwrap();
        inc_refcount(pools, ObjectKind::Endpoint, ep_idx);
        cs[proc_id as usize].slots[slot as usize] = Cap {
            badge: 0,
            parent: CapId::ROOT,
            generation: cs[proc_id as usize].generations[slot as usize] as u32,
            pool_index: ep_idx,
            kind: ObjectKind::Endpoint,
            rights: CAP_RIGHT_READ | CAP_RIGHT_WRITE | CAP_RIGHT_GRANT,
        };
        ep_idx
    }

    /// Derive a child of the cap at `parent` and install it at
    /// `child`, recording the parent slot's CURRENT generation (the
    /// well-formed, non-orphaned case).
    fn install_child(
        cs: &mut [CSpace; MAX_PROCS],
        pools: &mut ObjectPools,
        parent: (u8, u8),
        child: (u8, u8),
    ) {
        let (pp, ps) = parent;
        let p_gen = cs[pp as usize].generations[ps as usize];
        let parent_cap = cs[pp as usize].slots[ps as usize];
        let parent_id = CapId::new(pp, ps, p_gen);
        let child_cap = Cap::derive(&parent_cap, parent_id, CAP_RIGHT_READ, 0).unwrap();
        inc_refcount(pools, child_cap.kind, child_cap.pool_index);
        cs[child.0 as usize].slots[child.1 as usize] = child_cap;
    }

    // ---- RevokeSet (moved from the kernel; the starts-empty scan
    // now actually iterates all 4 096 bits — see the loop-bounds
    // NOTE above) ----

    #[test]
    fn revoke_set_starts_empty() {
        let s = RevokeSet::new();
        for p in 0..MAX_PROCS {
            for s_idx in 0..CSPACE_SLOTS {
                assert!(!s.contains(p as u8, s_idx as u8));
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

    // ---- the cascade itself (new — first host coverage ever) ----

    #[test]
    fn revoke_clears_target_and_frees_object() {
        let (mut cs, mut pools) = fresh();
        let ep_idx = install_root_ep(&mut cs, &mut pools, 0, 1);

        revoke(&mut cs, &mut pools, 0, 1).unwrap();

        assert!(cs[0].slots[1].is_empty());
        // Generation bumped so stale children are orphaned (INV-17).
        assert_eq!(cs[0].generations[1], 1);
        // Refcount hit zero → endpoint returned to the pool.
        assert!(!pools.endpoints.is_allocated(ep_idx));
    }

    #[test]
    fn revoke_cascades_to_descendants() {
        let (mut cs, mut pools) = fresh();
        let ep_idx = install_root_ep(&mut cs, &mut pools, 0, 1);
        install_child(&mut cs, &mut pools, (0, 1), (1, 3));
        install_child(&mut cs, &mut pools, (1, 3), (2, 5)); // grandchild
        assert_eq!(pools.endpoints.get(ep_idx).unwrap().refcount, 3);

        revoke(&mut cs, &mut pools, 0, 1).unwrap();

        assert!(cs[0].slots[1].is_empty());
        assert!(cs[1].slots[3].is_empty());
        assert!(cs[2].slots[5].is_empty());
        assert!(!pools.endpoints.is_allocated(ep_idx));
    }

    #[test]
    fn cascade_reaches_the_last_slot() {
        // Regression pin for the `256 as u8 == 0` loop-bounds bug the
        // pre-extraction kernel shipped: with an empty iteration
        // range, neither the target nor any descendant was ever
        // cleared. Slot 255 is the last slot the scan visits.
        let (mut cs, mut pools) = fresh();
        let ep_idx = install_root_ep(&mut cs, &mut pools, 0, 255);
        install_child(&mut cs, &mut pools, (0, 255), (15, 255));

        revoke(&mut cs, &mut pools, 0, 255).unwrap();

        assert!(cs[0].slots[255].is_empty());
        assert!(cs[15].slots[255].is_empty());
        assert!(!pools.endpoints.is_allocated(ep_idx));
    }

    #[test]
    fn sibling_cap_to_same_object_survives() {
        // A second ROOT cap to the same endpoint is not a descendant
        // of the revoked one — it must survive, and the object must
        // stay allocated with its refcount decremented by exactly the
        // caps that died.
        let (mut cs, mut pools) = fresh();
        let ep_idx = install_root_ep(&mut cs, &mut pools, 0, 1);
        // Second root cap in another proc, same endpoint.
        cs[1].slots[1] = Cap {
            badge: 0,
            parent: CapId::ROOT,
            generation: 0,
            pool_index: ep_idx,
            kind: ObjectKind::Endpoint,
            rights: CAP_RIGHT_READ,
        };
        inc_refcount(&mut pools, ObjectKind::Endpoint, ep_idx);

        revoke(&mut cs, &mut pools, 0, 1).unwrap();

        assert!(cs[0].slots[1].is_empty());
        assert!(!cs[1].slots[1].is_empty());
        assert!(pools.endpoints.is_allocated(ep_idx));
        assert_eq!(pools.endpoints.get(ep_idx).unwrap().refcount, 1);
    }

    #[test]
    fn orphaned_child_survives_cascade() {
        // INV-17 anti-ABA: a child whose recorded parent generation
        // no longer matches the parent slot's current generation is
        // orphaned — NOT a descendant of the current occupant — and
        // must not be swept.
        let (mut cs, mut pools) = fresh();
        let ep_idx = install_root_ep(&mut cs, &mut pools, 0, 1);
        install_child(&mut cs, &mut pools, (0, 1), (1, 3));
        // Simulate the parent slot having been freed and re-occupied
        // since the child was minted: bump the slot generation past
        // the one the child recorded.
        cs[0].generations[1] = cs[0].generations[1].saturating_add(1);

        revoke(&mut cs, &mut pools, 0, 1).unwrap();

        // Target cleared; orphan untouched; object survives because
        // the orphan still holds a reference.
        assert!(cs[0].slots[1].is_empty());
        assert!(!cs[1].slots[3].is_empty());
        assert!(pools.endpoints.is_allocated(ep_idx));
        assert_eq!(pools.endpoints.get(ep_idx).unwrap().refcount, 1);
    }

    #[test]
    fn revoke_empty_slot_errors() {
        let (mut cs, mut pools) = fresh();
        assert_eq!(
            revoke(&mut cs, &mut pools, 0, 1),
            Err(KernelError::InvalidArgument)
        );
    }

    #[test]
    fn revoke_out_of_range_proc_errors() {
        let (mut cs, mut pools) = fresh();
        assert_eq!(
            revoke(&mut cs, &mut pools, MAX_PROCS as u8, 0),
            Err(KernelError::InvalidArgument)
        );
    }

    // ---- refcount primitives ----

    #[test]
    fn dec_refcount_deallocates_at_zero() {
        let (_, mut pools) = fresh();
        let idx = pools.endpoints.alloc(Endpoint::new()).unwrap();
        inc_refcount(&mut pools, ObjectKind::Endpoint, idx);
        inc_refcount(&mut pools, ObjectKind::Endpoint, idx);
        dec_refcount(&mut pools, ObjectKind::Endpoint, idx);
        assert!(pools.endpoints.is_allocated(idx));
        dec_refcount(&mut pools, ObjectKind::Endpoint, idx);
        assert!(!pools.endpoints.is_allocated(idx));
    }

    #[test]
    fn refcount_ops_on_unallocated_index_are_noops() {
        let (_, mut pools) = fresh();
        // Neither call may panic or allocate anything.
        inc_refcount(&mut pools, ObjectKind::Endpoint, 7);
        dec_refcount(&mut pools, ObjectKind::Endpoint, 7);
        assert!(!pools.endpoints.is_allocated(7));
    }
}
