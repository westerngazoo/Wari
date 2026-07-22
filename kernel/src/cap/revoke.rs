// SPDX-License-Identifier: AGPL-3.0-only
//! Kernel binding of `wari-cap`'s revocation cascade to the static
//! storage.
//!
//! The cascade algorithm, the refcount primitives, and their host
//! tests live in `wari_cap::revoke` (B-3 slice 3 of
//! `docs/kernel-host-testing-design.md`), parameterized over the
//! CSpace array and object pools. The wrappers here pass the
//! `storage::{cspaces(), object_pools()}` statics and keep the
//! original signatures, so `cap/syscall.rs` call sites are
//! unchanged. Unlike the other `cap/*` shims this is not a pure
//! re-export: binding the statics IS the imperative-shell half of
//! the split.

use super::storage::{cspaces, object_pools};
use super::types::ObjectKind;
use crate::error::KernelError;

/// Revoke the cap at `(proc_id, slot)` and every descendant.
/// Contract: `wari_cap::revoke::revoke` (bounds-checked, cascades
/// per INV-17, frees zero-refcount objects; errors with
/// `InvalidArgument` on an empty or out-of-range target).
///
/// Holding the two accessor results simultaneously is sound: they
/// reference two *disjoint* statics — the storage discipline forbids
/// aliasing the SAME memory (see `storage.rs`), which this does not.
pub fn revoke(proc_id: u8, slot: u8) -> Result<(), KernelError> {
    wari_cap::revoke::revoke(cspaces(), object_pools(), proc_id, slot)
}

/// Decrement a kernel object's refcount, deallocating at zero.
/// Contract: `wari_cap::revoke::dec_refcount`.
pub fn dec_refcount(kind: ObjectKind, pool_index: u16) {
    wari_cap::revoke::dec_refcount(object_pools(), kind, pool_index);
}

/// Increment a kernel object's refcount (saturating).
/// Contract: `wari_cap::revoke::inc_refcount`.
pub fn inc_refcount(kind: ObjectKind, pool_index: u16) {
    wari_cap::revoke::inc_refcount(object_pools(), kind, pool_index);
}
