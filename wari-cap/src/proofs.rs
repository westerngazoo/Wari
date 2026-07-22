// SPDX-License-Identifier: AGPL-3.0-only
//! Kani harnesses for the cap-primitive layer.
//!
//! This module is **only compiled under `#[cfg(kani)]`** so a stock
//! `cargo build` / `cargo check` does not need the Kani toolchain
//! installed. To run the proofs:
//!
//! ```bash
//! cargo install --locked kani-verifier
//! cargo kani setup
//! cargo kani --harness <harness_name>
//! ```
//!
//! ## What's proved here (PR 1 scope)
//!
//! Each `#[kani::proof]` below corresponds to one numbered claim in
//! `docs/cap-system-design.md` §8.3 and to an `INV-N` in
//! `docs/invariants.md`. The harnesses use `kani::any()` to range
//! over all possible inputs (modulo declared assumptions) and assert
//! the postcondition holds.
//!
//! Harnesses outside PR 1's scope (revocation cascade termination;
//! generation counter monotonicity across mint+delete state machines)
//! land in PR 2 and PR 3 as the operations they verify land.
//!
//! ## Why the proofs ship in the same PR as the code
//!
//! The Wari thesis is auditability + formal-verification readiness.
//! Deferring proofs to a hypothetical Phase-4 verification track
//! turns "verified" into a marketing word. Shipping the harnesses
//! now means the spec is the proof and any change to the underlying
//! function that breaks the proof is caught at the next CI run.

#![cfg(kani)]
#![allow(dead_code)]

use crate::types::{Cap, CapId, ObjectKind, CAP_RIGHTS_PHASE_1B_MASK};

// ─────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────

/// Build an arbitrary non-empty parent cap that satisfies Phase-1b
/// shape constraints (kind in the four valid variants, rights inside
/// the Phase-1b mask). Used by harnesses that need a "valid parent".
fn arbitrary_non_empty_parent() -> Cap {
    let kind_disc: u8 = kani::any();
    kani::assume(kind_disc >= 1 && kind_disc <= 4);
    // SAFETY-equivalent (Kani-verified): kind_disc ∈ {1,2,3,4} matches
    // the Endpoint/Notification/Untyped/Frame discriminants of
    // ObjectKind, so the cast is in-range.
    let kind = match kind_disc {
        1 => ObjectKind::Endpoint,
        2 => ObjectKind::Notification,
        3 => ObjectKind::Untyped,
        _ => ObjectKind::Frame,
    };
    let rights: u8 = kani::any();
    kani::assume(rights & !CAP_RIGHTS_PHASE_1B_MASK == 0);
    let pool_index: u16 = kani::any();
    kani::assume(pool_index != u16::MAX);

    Cap {
        badge: kani::any(),
        parent: CapId::ROOT,
        generation: kani::any(),
        pool_index,
        kind,
        rights,
    }
}

// ─────────────────────────────────────────────────────────────────
// INV-10: Capability Monotonicity
// ─────────────────────────────────────────────────────────────────

/// **INV-10**: For any successful derivation, the child's rights are
/// a subset of the parent's. Equivalently, `child.rights & !parent.
/// rights == 0`.
///
/// This is the load-bearing soundness property of the cap system.
/// If this proof ever fails, a Tier-1 process could mint a child cap
/// with rights its parent did not hold — a privilege-escalation
/// soundness bug.
#[kani::proof]
fn derive_preserves_rights_monotonicity() {
    let parent = arbitrary_non_empty_parent();
    let parent_id = CapId::new(kani::any(), kani::any(), kani::any());
    let requested_rights: u8 = kani::any();
    let badge: u32 = kani::any();

    if let Ok(child) = Cap::derive(&parent, parent_id, requested_rights, badge) {
        // Postcondition: rights are a subset of the parent's.
        assert!(child.rights & !parent.rights == 0);
        // Stronger postcondition: rights equal exactly the request
        // (the function does not silently widen).
        assert!(child.rights == requested_rights);
    }
}

/// **INV-10 negative case**: any request with a bit set that the
/// parent does not hold MUST fail. We assume `requested_rights`
/// strictly exceeds parent rights and assert `derive` returns Err.
#[kani::proof]
fn derive_rejects_rights_amplification() {
    let parent = arbitrary_non_empty_parent();
    let parent_id = CapId::new(kani::any(), kani::any(), kani::any());
    let requested_rights: u8 = kani::any();
    kani::assume(requested_rights & !CAP_RIGHTS_PHASE_1B_MASK == 0);
    // The interesting case: child requests at least one bit the
    // parent does not have.
    kani::assume(requested_rights & !parent.rights != 0);

    let result = Cap::derive(&parent, parent_id, requested_rights, kani::any());
    assert!(result.is_err());
}

// ─────────────────────────────────────────────────────────────────
// INV-15: Forgery Prevention (reserved-bit rejection)
// ─────────────────────────────────────────────────────────────────

/// **INV-15**: a request with any reserved (Phase 2+) bit set MUST
/// be rejected. This prevents userspace from probing for or
/// constructing caps with rights the kernel does not yet model.
#[kani::proof]
fn derive_rejects_reserved_rights_bits() {
    let parent = arbitrary_non_empty_parent();
    let parent_id = CapId::new(kani::any(), kani::any(), kani::any());
    let requested_rights: u8 = kani::any();
    // The interesting case: at least one reserved bit (4..=7) is set.
    kani::assume(requested_rights & !CAP_RIGHTS_PHASE_1B_MASK != 0);

    let result = Cap::derive(&parent, parent_id, requested_rights, kani::any());
    assert!(result.is_err());
}

// ─────────────────────────────────────────────────────────────────
// INV-16: Derivation Chain Integrity (kind + pool preservation)
// ─────────────────────────────────────────────────────────────────

/// **INV-16**: a successfully derived child has the same `kind`
/// and `pool_index` as its parent. The mint operation never
/// retargets the underlying kernel object.
#[kani::proof]
fn derive_preserves_kind_and_pool_index() {
    let parent = arbitrary_non_empty_parent();
    let parent_id = CapId::new(kani::any(), kani::any(), kani::any());
    let requested_rights: u8 = kani::any();

    if let Ok(child) = Cap::derive(&parent, parent_id, requested_rights, kani::any()) {
        assert!(child.kind as u8 == parent.kind as u8);
        assert!(child.pool_index == parent.pool_index);
    }
}

/// **INV-16 ancillary**: the child's `parent` field is the
/// `parent_id` argument supplied by the caller. (The mint syscall
/// in PR 3 is responsible for computing parent_id correctly; this
/// harness verifies the pure-function contract.)
#[kani::proof]
fn derive_records_parent_id() {
    let parent = arbitrary_non_empty_parent();
    let parent_id = CapId::new(kani::any(), kani::any(), kani::any());
    let requested_rights: u8 = kani::any();

    if let Ok(child) = Cap::derive(&parent, parent_id, requested_rights, kani::any()) {
        assert!(child.parent == parent_id);
    }
}

// ─────────────────────────────────────────────────────────────────
// Empty parent rejection
// ─────────────────────────────────────────────────────────────────

/// Deriving from an empty cap MUST fail. There is no kernel object
/// behind an empty slot to take a capability over.
#[kani::proof]
fn derive_rejects_empty_parent() {
    let parent = Cap::empty();
    let parent_id = CapId::new(kani::any(), kani::any(), kani::any());
    let result = Cap::derive(&parent, parent_id, kani::any(), kani::any());
    assert!(result.is_err());
}

// ─────────────────────────────────────────────────────────────────
// Layout sanity (compile-time-equivalent at proof time)
// ─────────────────────────────────────────────────────────────────

/// The `Cap` struct must be exactly 16 bytes for the layout
/// committed in the design doc. A change in field order or type
/// that breaks this is caught here before any code reaches main.
#[kani::proof]
fn cap_size_is_16_bytes() {
    assert!(core::mem::size_of::<Cap>() == 16);
}

/// `CapId` must be exactly 4 bytes (it wraps a `u32`).
#[kani::proof]
fn capid_size_is_4_bytes() {
    assert!(core::mem::size_of::<CapId>() == 4);
}

// ─────────────────────────────────────────────────────────────────
// CapId encoding round-trip
// ─────────────────────────────────────────────────────────────────

/// `CapId::new(p, s, g).proc_id() == p` etc., for all in-range
/// inputs. The kernel relies on these accessors to recover (proc,
/// slot, generation) from a stored CapId during the revocation
/// walk; if encoding/decoding desynchronizes, parent-chain matches
/// silently fail.
#[kani::proof]
fn capid_round_trips() {
    let proc_id: u8 = kani::any();
    let slot: u8 = kani::any();
    let gen: u16 = kani::any();
    // Avoid the ROOT collision corner case (handled by debug_assert
    // at construction time).
    kani::assume(!(proc_id == u8::MAX && slot == u8::MAX && gen == u16::MAX));

    let id = CapId::new(proc_id, slot, gen);
    assert!(id.proc_id() == proc_id);
    assert!(id.slot() == slot);
    assert!(id.generation() == gen);
    assert!(!id.is_root());
}
