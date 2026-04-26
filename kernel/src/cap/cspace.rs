// SPDX-License-Identifier: AGPL-3.0-only
//! Per-process capability space (CSpace) — the slot array a process
//! holds its capabilities in.
//!
//! In Phase 1b a CSpace is a flat array of 256 slots, one page in
//! size. Each slot is a 16-byte `Cap`. Slot indices are `u8` (0..256
//! exclusive); userspace addresses caps by slot index (`CPtr`).
//!
//! ## Layout (`#[repr(C)]`)
//!
//! ```text
//!   offset  size       field
//!   ──────  ─────────  ─────────────
//!   0       4096       slots: [Cap; 256]
//!   4096    512        generations: [u16; 256]
//! ```
//!
//! Total: 4 608 bytes. Allocated in a single 8 KiB page from the
//! kernel page allocator at process spawn (PR 2 work). The
//! generations array is a small extension on top of the slot array;
//! Phase 2+ may pack it into the `Cap.generation` field and free
//! the separate array, but for now the explicit storage makes the
//! revocation walk's slot-vs-cap-generation comparison obvious.
//!
//! ## Why a flat single-level CSpace
//!
//! seL4 uses a multi-level guarded CSpace so a process can hold
//! tens of thousands of caps with logarithmic lookup. Wari's
//! Phase-1b workloads (a single-digit number of Tier-1 instances,
//! each holding fewer than 32 caps in practice) do not approach
//! that scale, and a flat 256-slot CSpace fits in one page with
//! constant-time indexing. When and if a Wari workload grows past
//! 256 caps per process, the migration to multi-level is local to
//! this module — no syscall ABI change.
//!
//! ## Invariants enforced here
//!
//! - **INV-18** (CSpace Slot Index Bounds): the `lookup` and
//!   `lookup_mut` accessors are the only paths into the slot array
//!   from outside this module. Both check `slot < CSPACE_SLOTS`
//!   before indexing.

#![allow(dead_code)]
#![allow(clippy::doc_lazy_continuation)]

use crate::error::KernelError;

use super::types::{Cap, CapId, ObjectKind};

/// Number of slots in a CSpace. Chosen so a CSpace's slot array
/// fits in exactly one 4 KiB page (256 × 16 = 4 096 bytes).
pub const CSPACE_SLOTS: usize = 256;

/// Maximum number of processes Phase 1b supports. Each process
/// owns one CSpace. This bound shows up in `CapId`'s 8-bit proc-id
/// field (which can express 256 procs) — Phase 1b tightens to 16
/// for memory-budget reasons; future expansion is a constant change
/// here, not an ABI change.
pub const MAX_PROCS: usize = 16;

/// Per-process capability table.
///
/// Constructed at process spawn (PR 2), torn down at process exit
/// after revocation cascade (PR 3).
#[repr(C)]
pub struct CSpace {
    /// The capability slots. Index by `u8` (slot < CSPACE_SLOTS).
    pub slots: [Cap; CSPACE_SLOTS],
    /// Per-slot generation counters. Bumped every time a slot
    /// transitions from occupied → empty → occupied (INV-17).
    pub generations: [u16; CSPACE_SLOTS],
}

impl CSpace {
    /// Construct a fresh CSpace with every slot empty and every
    /// generation counter at zero.
    ///
    /// `const fn` so a CSpace can appear in a `static` initializer
    /// without runtime work — useful for the global `[CSpace;
    /// MAX_PROCS]` array PR 2 introduces.
    pub const fn new() -> Self {
        Self {
            slots: [Cap::empty(); CSPACE_SLOTS],
            generations: [0; CSPACE_SLOTS],
        }
    }

    /// Look up a slot by index. Returns `None` if `slot` is out of
    /// bounds; returns `Some(&Cap)` otherwise (the cap may itself
    /// be `empty()`).
    ///
    /// # Why this returns `Option<&Cap>` and not `Result<&Cap>`
    ///
    /// Out-of-bounds is a programming error in the kernel, not a
    /// runtime failure mode the kernel propagates. Syscall
    /// trampolines map `None` to `KernelError::InvalidArgument`
    /// before returning to userspace; internal kernel callers use
    /// `expect("slot validated")` because they have already
    /// bounds-checked.
    pub fn lookup(&self, slot: u8) -> Option<&Cap> {
        let s = slot as usize;
        if s < CSPACE_SLOTS {
            Some(&self.slots[s])
        } else {
            // Unreachable because slot is u8 and CSPACE_SLOTS = 256.
            // Kept as a hard structural barrier so a future widening
            // of `slot` past u8 surfaces here rather than producing
            // an out-of-bounds index.
            None
        }
    }

    /// Mutable variant of `lookup`. Used by mint / copy / revoke /
    /// delete (PR 3) to install or clear caps.
    pub fn lookup_mut(&mut self, slot: u8) -> Option<&mut Cap> {
        let s = slot as usize;
        if s < CSPACE_SLOTS {
            Some(&mut self.slots[s])
        } else {
            None
        }
    }

    /// `true` if the slot at `slot` exists and holds a non-empty cap.
    /// `false` if the slot is empty or out of bounds.
    pub fn is_occupied(&self, slot: u8) -> bool {
        match self.lookup(slot) {
            Some(c) => !c.is_empty(),
            None => false,
        }
    }

    /// Read the generation counter for a slot. Returns 0 for
    /// out-of-bounds (no real slot has a generation, so the value is
    /// safe nonsense).
    pub fn generation(&self, slot: u8) -> u16 {
        let s = slot as usize;
        if s < CSPACE_SLOTS {
            self.generations[s]
        } else {
            0
        }
    }

    /// Bump the generation counter for a slot. Called by the mint /
    /// delete / revoke paths (PR 3) every time a slot transitions
    /// from occupied → empty → occupied.
    ///
    /// Saturates at `u16::MAX`. If a single slot has been re-occupied
    /// 65 535 times in one boot, callers that depend on generation
    /// freshness must treat the saturated value as "may be aliased"
    /// and refuse the operation. Phase 1b's mint path returns
    /// `KernelError::OutOfHandles` when this happens.
    pub fn bump_generation(&mut self, slot: u8) -> u16 {
        let s = slot as usize;
        if s < CSPACE_SLOTS {
            self.generations[s] = self.generations[s].saturating_add(1);
            self.generations[s]
        } else {
            0
        }
    }

    /// Find the first empty slot, returning its index.
    ///
    /// Used by mint / copy operations that let the kernel choose the
    /// target slot (rather than userspace specifying one). Linear
    /// scan; fine for 256 slots.
    pub fn find_empty(&self) -> Result<u8, KernelError> {
        for (i, slot) in self.slots.iter().enumerate() {
            if slot.is_empty() {
                return Ok(i as u8);
            }
        }
        Err(KernelError::OutOfHandles)
    }
}

impl Default for CSpace {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::types::{CAP_RIGHT_READ, CAP_RIGHT_WRITE};
    use super::*;

    fn occupied_cap(kind: ObjectKind) -> Cap {
        Cap {
            badge: 0,
            parent: CapId::ROOT,
            generation: 0,
            pool_index: 5,
            kind,
            rights: CAP_RIGHT_READ | CAP_RIGHT_WRITE,
        }
    }

    // ---- size + layout ----

    #[test]
    fn cspace_slots_pack_to_one_page() {
        // 256 slots × 16 bytes = 4096 = exactly one 4 KiB page.
        assert_eq!(core::mem::size_of::<[Cap; CSPACE_SLOTS]>(), 4096);
    }

    #[test]
    fn cspace_total_size_fits_two_pages() {
        // 4096 (slots) + 512 (generations) = 4608 bytes. Allocated
        // from an 8 KiB page in PR 2.
        assert!(core::mem::size_of::<CSpace>() <= 8192);
    }

    // ---- new / default ----

    #[test]
    fn fresh_cspace_is_all_empty() {
        let cs = CSpace::new();
        for i in 0..CSPACE_SLOTS {
            assert!(cs.slots[i].is_empty());
            assert_eq!(cs.generations[i], 0);
        }
    }

    #[test]
    fn default_equals_new() {
        let cs = CSpace::default();
        for slot in &cs.slots {
            assert!(slot.is_empty());
        }
    }

    // ---- lookup / lookup_mut ----

    #[test]
    fn lookup_empty_slot_returns_empty_cap() {
        let cs = CSpace::new();
        assert!(cs.lookup(0).unwrap().is_empty());
        assert!(cs.lookup(255).unwrap().is_empty());
    }

    #[test]
    fn lookup_occupied_slot_returns_cap() {
        let mut cs = CSpace::new();
        let c = occupied_cap(ObjectKind::Endpoint);
        cs.slots[42] = c;
        let looked_up = cs.lookup(42).unwrap();
        assert_eq!(looked_up.kind, ObjectKind::Endpoint);
        assert_eq!(looked_up.pool_index, 5);
    }

    #[test]
    fn lookup_mut_allows_writing() {
        let mut cs = CSpace::new();
        *cs.lookup_mut(7).unwrap() = occupied_cap(ObjectKind::Frame);
        assert_eq!(cs.lookup(7).unwrap().kind, ObjectKind::Frame);
    }

    // ---- is_occupied ----

    #[test]
    fn is_occupied_empty_slot_is_false() {
        let cs = CSpace::new();
        assert!(!cs.is_occupied(0));
        assert!(!cs.is_occupied(255));
    }

    #[test]
    fn is_occupied_filled_slot_is_true() {
        let mut cs = CSpace::new();
        cs.slots[12] = occupied_cap(ObjectKind::Endpoint);
        assert!(cs.is_occupied(12));
    }

    // ---- generation ----

    #[test]
    fn fresh_generation_is_zero() {
        let cs = CSpace::new();
        for s in 0..=255u8 {
            assert_eq!(cs.generation(s), 0);
        }
    }

    #[test]
    fn bump_generation_increments() {
        let mut cs = CSpace::new();
        assert_eq!(cs.bump_generation(5), 1);
        assert_eq!(cs.bump_generation(5), 2);
        assert_eq!(cs.bump_generation(5), 3);
        assert_eq!(cs.generation(5), 3);
    }

    #[test]
    fn bump_generation_isolates_slots() {
        let mut cs = CSpace::new();
        cs.bump_generation(1);
        cs.bump_generation(1);
        cs.bump_generation(2);
        assert_eq!(cs.generation(1), 2);
        assert_eq!(cs.generation(2), 1);
        assert_eq!(cs.generation(3), 0);
    }

    #[test]
    fn bump_generation_saturates_at_u16_max() {
        let mut cs = CSpace::new();
        cs.generations[0] = u16::MAX;
        // saturating_add prevents wraparound; downstream mint
        // operations check for the saturated value and refuse.
        assert_eq!(cs.bump_generation(0), u16::MAX);
        assert_eq!(cs.generation(0), u16::MAX);
    }

    // ---- find_empty ----

    #[test]
    fn find_empty_returns_first_empty_slot() {
        let mut cs = CSpace::new();
        cs.slots[0] = occupied_cap(ObjectKind::Endpoint);
        cs.slots[1] = occupied_cap(ObjectKind::Frame);
        // slot 2 is still empty
        assert_eq!(cs.find_empty().unwrap(), 2);
    }

    #[test]
    fn find_empty_in_full_cspace_errors() {
        let mut cs = CSpace::new();
        for i in 0..CSPACE_SLOTS {
            cs.slots[i] = occupied_cap(ObjectKind::Endpoint);
        }
        assert_eq!(cs.find_empty(), Err(KernelError::OutOfHandles));
    }

    #[test]
    fn find_empty_in_fresh_cspace_returns_zero() {
        let cs = CSpace::new();
        assert_eq!(cs.find_empty().unwrap(), 0);
    }
}
