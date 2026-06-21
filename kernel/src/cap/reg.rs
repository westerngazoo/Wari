// SPDX-License-Identifier: AGPL-3.0-only
//! Registered-capability fast-path table — per-process cache of caps
//! proven once at registration.
//!
//! See `docs/cap-registered-fastpath-design.md`. A module registers a
//! capability once (`cap_register`); the kernel does the full cap
//! resolution then and caches the result behind a small integer handle
//! (`reg_idx`) in this table. Hot-path operations (the submission ring,
//! PR-2) reference the handle, so the per-op check collapses to an O(1)
//! bounds + generation comparison instead of a CSpace walk — amortizing
//! the capability check without skipping it.
//!
//! This file is **pure data + table mechanics**. The soundness predicate
//! lives in `wari_abi::reg` (host-testable); the static storage and its
//! `&'static mut` accessor live in `cap::storage` (the `unsafe` glue).
//! Nothing here is `unsafe`.

use wari_abi::reg::REG_SLOTS;

use super::types::ObjectKind;

/// `REG_SLOTS` as a `usize`, for array sizing. Single source of truth is
/// `wari_abi::reg::REG_SLOTS` so the kernel and the pure validator agree
/// on the handle range.
pub const REG_SLOTS_USIZE: usize = REG_SLOTS as usize;

/// One registered-resource slot: the resolution of a capability proven
/// at registration. `kind == ObjectKind::Empty` marks a free slot.
///
/// `reg_generation` is the originating CSpace slot's generation **at
/// registration**; the hot path (PR-2) compares it against the slot's
/// current generation, so any revoke/delete/reuse (which bumps the
/// generation, INV-17) invalidates this entry automatically.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RegEntry {
    /// Cached kind of the registered object; `Empty` ⇒ free slot.
    pub kind: ObjectKind,
    /// Cached rights bitmap from the originating cap.
    pub rights: u8,
    /// Originating CSpace slot (so the hot path can re-read its live
    /// generation for the anti-revoke check).
    pub cspace_slot: u8,
    /// CSpace slot generation captured at registration (INV-17 anchor).
    pub reg_generation: u16,
    /// Cached per-kind object pool index, so the hot path skips
    /// Cap→object resolution.
    pub pool_index: u16,
}

impl RegEntry {
    /// A free entry.
    pub const fn empty() -> Self {
        RegEntry {
            kind: ObjectKind::Empty,
            rights: 0,
            cspace_slot: 0,
            reg_generation: 0,
            pool_index: 0,
        }
    }

    /// Is this slot free?
    #[inline]
    pub const fn is_empty(&self) -> bool {
        matches!(self.kind, ObjectKind::Empty)
    }
}

/// Per-process registered-resource table. `REG_SLOTS` entries; one table
/// per process (parallel to `CSpace`). Small + fixed so the hot-path
/// index lookup is bounded.
#[repr(C)]
pub struct RegTable {
    /// The registered-resource slots, indexed by `reg_idx`.
    pub slots: [RegEntry; REG_SLOTS_USIZE],
}

impl RegTable {
    /// A fresh, all-empty table. `const` so the per-process array can be
    /// statically initialized in `cap::storage`.
    pub const fn new() -> Self {
        RegTable {
            slots: [RegEntry::empty(); REG_SLOTS_USIZE],
        }
    }

    /// Index of the first free slot, or `None` if the table is full.
    pub fn find_empty(&self) -> Option<usize> {
        self.slots.iter().position(RegEntry::is_empty)
    }
}
