// SPDX-License-Identifier: AGPL-3.0-only
//! Capability primitive types — `Cap`, `CapId`, `ObjectKind`.
//!
//! This module defines the data shapes that the dynamic capability
//! system manipulates. It is **pure**: every function in this file is
//! `#[no_mangle]`-free, allocation-free, and side-effect-free. The
//! mint operation lives here as `Cap::derive` so the rights-
//! monotonicity check (INV-10) and kind/pool-preservation check
//! (INV-16) live next to the type definitions they enforce.
//!
//! Storage of caps in per-process CSpaces lives in `cspace.rs`.
//! Kernel object pools (`Endpoint`, `Notification`, `Untyped`,
//! `Frame`) and the boot-time root cap construction live in PR 2.
//! Mint/copy/revoke/delete/lookup syscalls land in PR 3.
//!
//! ## Why these types here, not in `static_caps.rs`
//!
//! The Phase-0 file `static_caps.rs` defines a *configuration* type
//! (`Caps`: a 3-bit struct over `stdout`/`mmio_uart`/`exit`) used to
//! seed the kernel's compile-time module manifest. The Phase-1b
//! `Cap` type defined here is the *runtime* capability — a 16-byte
//! reference to a kernel object plus a rights bitmap. The two types
//! coexist during PR 1 and PR 2; PR 3 introduces IPC, at which point
//! `static_caps::Caps` becomes input to the boot-time mint pass and
//! `Cap` becomes the runtime currency.
//!
//! ## Layout (`#[repr(C)]`, 16 bytes total)
//!
//! Fields are ordered for natural alignment (largest first), so the
//! struct is exactly 16 bytes with zero padding on RV64GC:
//!
//! ```text
//!   offset  size  field
//!   ──────  ────  ─────────────
//!   0       4     badge
//!   4       4     parent     (CapId, wraps u32)
//!   8       4     generation
//!   12      2     pool_index
//!   14      1     kind       (ObjectKind, repr(u8))
//!   15      1     rights
//! ```
//!
//! The 16-byte size is asserted by `proofs::cap_size_is_16`.
//!
//! ## Invariants enforced here
//!
//! - **INV-10** (Capability Monotonicity): `Cap::derive` rejects any
//!   request where `requested_rights & !parent.rights != 0`.
//! - **INV-15** (Forgery Prevention): the only public constructors
//!   are `empty()` (for unused slots) and `derive()` (which requires
//!   a parent and goes through the rights check). All-public-fields
//!   `Cap { ... }` literal construction is permitted in `cap::*`
//!   modules only (Rust privacy + an internal-use-only convention),
//!   never in user-reachable code paths.
//! - **INV-16** (Derivation Chain Integrity): `Cap::derive`
//!   preserves `kind` and `pool_index` from parent to child.

#![allow(dead_code)]
#![allow(clippy::doc_lazy_continuation)]

use crate::error::KernelError;

// ─────────────────────────────────────────────────────────────────
// ObjectKind
// ─────────────────────────────────────────────────────────────────

/// Kind of kernel object a capability refers to.
///
/// `Empty` is the sentinel for unused CSpace slots. Every other
/// variant identifies a kernel object kind whose pool lives in
/// `cap::objects` (PR 2).
///
/// `#[repr(u8)]` is required so a `Cap` packs into 16 bytes and so
/// the discriminants are stable for formal-verification tools that
/// reason about the in-memory representation.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    /// Slot is unoccupied. The other `Cap` fields carry no meaning
    /// for an `Empty` cap and are conventionally zeroed.
    Empty = 0,
    /// Synchronous IPC rendezvous point.
    Endpoint = 1,
    /// Asynchronous binary signal (semaphore-like).
    Notification = 2,
    /// A pool of typed-as-untyped memory, retypable into other
    /// kernel objects.
    Untyped = 3,
    /// A 4 KiB physical page, mappable into a virtual address space.
    Frame = 4,
    /// NIC handle. **Driver-only** in Phase 1b — INV-19 enforces
    /// that Tier-1 cannot mint or hold a Net cap. Added in PR Net-2
    /// per `docs/net-driver-design.md` §6.1.
    Net = 5,
    /// Per-tenant TCP/UDP socket. Minted from a `Net` cap by the
    /// Tier-2 net driver and granted to the calling Tier-1 tenant
    /// via cap-IPC. Added in PR Net-2.
    Socket = 6,
    // Reserved Phase 2+: Tcb = 7, AsidPool = 8, IrqHandler = 9, ...
}

// ─────────────────────────────────────────────────────────────────
// CapId
// ─────────────────────────────────────────────────────────────────

/// Globally-unique reference to a CSpace slot.
///
/// Encoding: `(generation: u16) << 16 | (proc_id: u8) << 8 | (slot: u8)`.
///
/// The 16-bit generation counter protects against ABA when a slot is
/// freed and re-occupied (INV-17): a child cap whose `parent` field
/// references generation `N` of slot `S` becomes orphaned when the
/// slot is reused at generation `N+1`. The kernel's revocation walk
/// detects this mismatch and clears the orphaned child.
///
/// `CapId::ROOT` (= `u32::MAX`) marks original kernel-issued caps
/// that have no parent (e.g., the caps minted at boot from the
/// static manifest).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapId(pub u32);

impl CapId {
    /// Sentinel for original kernel-issued caps with no parent.
    pub const ROOT: CapId = CapId(u32::MAX);

    /// Construct a `CapId` from its three components.
    ///
    /// # Contract
    /// `proc_id` must be `< MAX_PROCS` and `slot` must be `<
    /// CSPACE_SLOTS`; callers in `cap::*` enforce these bounds
    /// before calling. This constructor itself is total — invalid
    /// inputs simply produce a `CapId` that no real cap will match.
    pub const fn new(proc_id: u8, slot: u8, generation: u16) -> Self {
        let val: u32 = ((generation as u32) << 16) | ((proc_id as u32) << 8) | (slot as u32);
        // ROOT collides if every component is max. No real (proc,
        // slot, gen) hits this because gen is incremented per slot
        // reuse and saturates well below u16::MAX in practice — but
        // we keep the assert to surface the corner case if it ever
        // does.
        debug_assert!(val != u32::MAX, "CapId collides with ROOT sentinel");
        CapId(val)
    }

    /// Process id encoded in this CapId. Undefined for `ROOT`.
    pub const fn proc_id(self) -> u8 {
        ((self.0 >> 8) & 0xFF) as u8
    }

    /// Slot index encoded in this CapId. Undefined for `ROOT`.
    pub const fn slot(self) -> u8 {
        (self.0 & 0xFF) as u8
    }

    /// Generation counter encoded in this CapId. Undefined for `ROOT`.
    pub const fn generation(self) -> u16 {
        ((self.0 >> 16) & 0xFFFF) as u16
    }

    /// `true` if this CapId is the root sentinel.
    pub const fn is_root(self) -> bool {
        self.0 == u32::MAX
    }
}

// ─────────────────────────────────────────────────────────────────
// Rights bitmap
// ─────────────────────────────────────────────────────────────────

/// Bit 0 — may read object state (e.g., recv on Endpoint).
pub const CAP_RIGHT_READ: u8 = 1 << 0;
/// Bit 1 — may modify object state (e.g., send on Endpoint).
pub const CAP_RIGHT_WRITE: u8 = 1 << 1;
/// Bit 2 — may pass this cap to other processes via IPC.
pub const CAP_RIGHT_GRANT: u8 = 1 << 2;
/// Bit 3 — may pass via the reply path of a synchronous IPC.
pub const CAP_RIGHT_GRANT_REPLY: u8 = 1 << 3;

/// Mask of the rights bits Phase 1b uses. Bits 4–7 are reserved for
/// Phase 2+ extensions (badge mutability, IRQ ack, CoVE-confidential,
/// etc.). A request to `derive` with any reserved bit set is
/// rejected.
pub const CAP_RIGHTS_PHASE_1B_MASK: u8 =
    CAP_RIGHT_READ | CAP_RIGHT_WRITE | CAP_RIGHT_GRANT | CAP_RIGHT_GRANT_REPLY;

// ─────────────────────────────────────────────────────────────────
// Cap
// ─────────────────────────────────────────────────────────────────

/// A 16-byte runtime capability.
///
/// A `Cap` references a kernel object by `(kind, pool_index)`,
/// carries a rights bitmap describing what the holder may do with
/// it, and records its derivation parent in `parent` so revocation
/// can cascade.
///
/// Field order is chosen for natural alignment so `#[repr(C)]` packs
/// into exactly 16 bytes — see the module docstring for the
/// canonical layout table.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap {
    /// Endpoint badge (caller-id). Zero for non-Endpoint kinds.
    pub badge: u32,
    /// Parent cap reference. `CapId::ROOT` for original kernel
    /// mints (boot-time, from the static manifest).
    pub parent: CapId,
    /// Generation counter copied from the holder slot at mint time.
    /// Used to detect orphaned children when a parent slot has been
    /// freed and reused (INV-17).
    pub generation: u32,
    /// Index into the per-kind kernel object pool. `u16::MAX` for
    /// `Empty` caps (no pool reference).
    pub pool_index: u16,
    /// Kind of kernel object this cap references.
    pub kind: ObjectKind,
    /// Rights bitmap (low 4 bits used in Phase 1b).
    pub rights: u8,
}

impl Cap {
    /// All-zeroes / unoccupied cap. Used as the initial value for
    /// every CSpace slot.
    pub const fn empty() -> Self {
        Self {
            badge: 0,
            parent: CapId::ROOT, // empty caps have no parent
            generation: 0,
            pool_index: u16::MAX,
            kind: ObjectKind::Empty,
            rights: 0,
        }
    }

    /// `true` if this cap is unoccupied.
    pub const fn is_empty(&self) -> bool {
        matches!(self.kind, ObjectKind::Empty)
    }

    /// Derive a child cap from a parent.
    ///
    /// This is the **pure-function** core of the mint operation. The
    /// caller (the mint syscall handler in PR 3) supplies the parent
    /// cap, the parent's `CapId` (so the child's `parent` field can
    /// be set), and the requested rights and badge.
    ///
    /// # Invariants enforced
    ///
    /// - **INV-10** (Capability Monotonicity): the requested rights
    ///   must be a subset of the parent's rights. `requested_rights
    ///   & !parent.rights == 0`.
    /// - **INV-16** (Derivation Chain Integrity): the child inherits
    ///   `kind` and `pool_index` from the parent verbatim. The mint
    ///   never retargets the underlying kernel object.
    ///
    /// # Errors
    ///
    /// - `KernelError::InvalidArgument` if `parent` is `Empty`, or
    ///   if `requested_rights` sets any reserved (Phase 2+) bit.
    /// - `KernelError::PermissionDenied` if rights monotonicity is
    ///   violated (INV-10).
    ///
    /// The new child is returned with `generation = 0`; the calling
    /// mint operation overwrites this with the target slot's current
    /// generation just before placement (so children carry the slot
    /// generation they were minted into, supporting INV-17).
    pub fn derive(
        parent: &Cap,
        parent_id: CapId,
        requested_rights: u8,
        badge: u32,
    ) -> Result<Cap, KernelError> {
        // Cannot derive from an empty slot.
        if parent.is_empty() {
            return Err(KernelError::InvalidArgument);
        }
        // Reserved Phase 2+ bits cannot be set by Phase 1b mints.
        if requested_rights & !CAP_RIGHTS_PHASE_1B_MASK != 0 {
            return Err(KernelError::InvalidArgument);
        }
        // INV-10: rights must be a subset of parent's.
        if requested_rights & !parent.rights != 0 {
            return Err(KernelError::PermissionDenied);
        }
        Ok(Cap {
            badge: if matches!(parent.kind, ObjectKind::Endpoint) {
                badge
            } else {
                0
            },
            parent: parent_id,
            generation: 0, // overwritten by the mint syscall before placement
            pool_index: parent.pool_index, // INV-16
            kind: parent.kind, // INV-16
            rights: requested_rights,
        })
    }
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parent_with(kind: ObjectKind, rights: u8) -> Cap {
        Cap {
            badge: 0,
            parent: CapId::ROOT,
            generation: 1,
            pool_index: 7,
            kind,
            rights,
        }
    }

    // ---- ObjectKind / size sanity ----

    #[test]
    fn cap_is_16_bytes() {
        // INV: Cap must pack to 16 bytes for the layout the design
        // doc commits to. If this test fails, the field order
        // changed without updating the docstring.
        assert_eq!(core::mem::size_of::<Cap>(), 16);
    }

    #[test]
    fn capid_is_4_bytes() {
        assert_eq!(core::mem::size_of::<CapId>(), 4);
    }

    #[test]
    fn objectkind_is_1_byte() {
        assert_eq!(core::mem::size_of::<ObjectKind>(), 1);
    }

    // ---- empty / is_empty ----

    #[test]
    fn empty_cap_is_empty() {
        let c = Cap::empty();
        assert!(c.is_empty());
        assert_eq!(c.kind, ObjectKind::Empty);
        assert_eq!(c.rights, 0);
        assert_eq!(c.badge, 0);
        assert_eq!(c.pool_index, u16::MAX);
        assert!(c.parent.is_root());
    }

    #[test]
    fn non_empty_cap_is_not_empty() {
        let c = parent_with(ObjectKind::Endpoint, CAP_RIGHT_READ | CAP_RIGHT_WRITE);
        assert!(!c.is_empty());
    }

    // ---- CapId encoding ----

    #[test]
    fn capid_round_trips() {
        let id = CapId::new(7, 42, 1234);
        assert_eq!(id.proc_id(), 7);
        assert_eq!(id.slot(), 42);
        assert_eq!(id.generation(), 1234);
        assert!(!id.is_root());
    }

    #[test]
    fn capid_root_recognized() {
        assert!(CapId::ROOT.is_root());
    }

    #[test]
    fn capid_zero_is_not_root() {
        let id = CapId::new(0, 0, 0);
        assert!(!id.is_root());
    }

    // ---- derive: rights monotonicity (INV-10) ----

    #[test]
    fn derive_with_subset_rights_succeeds() {
        let parent = parent_with(
            ObjectKind::Endpoint,
            CAP_RIGHT_READ | CAP_RIGHT_WRITE | CAP_RIGHT_GRANT,
        );
        let parent_id = CapId::new(0, 1, 1);
        let child = Cap::derive(&parent, parent_id, CAP_RIGHT_READ | CAP_RIGHT_WRITE, 0).unwrap();
        assert_eq!(child.rights, CAP_RIGHT_READ | CAP_RIGHT_WRITE);
        assert_eq!(child.kind, ObjectKind::Endpoint);
        assert_eq!(child.pool_index, parent.pool_index);
        assert_eq!(child.parent, parent_id);
    }

    #[test]
    fn derive_with_equal_rights_succeeds() {
        let parent = parent_with(ObjectKind::Endpoint, CAP_RIGHT_READ | CAP_RIGHT_WRITE);
        let parent_id = CapId::new(0, 1, 1);
        let child = Cap::derive(&parent, parent_id, CAP_RIGHT_READ | CAP_RIGHT_WRITE, 0).unwrap();
        assert_eq!(child.rights, parent.rights);
    }

    #[test]
    fn derive_with_superset_rights_rejected() {
        // Parent has READ only; child requests READ+WRITE.
        let parent = parent_with(ObjectKind::Endpoint, CAP_RIGHT_READ);
        let parent_id = CapId::new(0, 1, 1);
        let result = Cap::derive(&parent, parent_id, CAP_RIGHT_READ | CAP_RIGHT_WRITE, 0);
        assert_eq!(result, Err(KernelError::PermissionDenied));
    }

    #[test]
    fn derive_with_disjoint_rights_rejected() {
        // Parent has READ; child requests WRITE.
        let parent = parent_with(ObjectKind::Endpoint, CAP_RIGHT_READ);
        let parent_id = CapId::new(0, 1, 1);
        let result = Cap::derive(&parent, parent_id, CAP_RIGHT_WRITE, 0);
        assert_eq!(result, Err(KernelError::PermissionDenied));
    }

    // ---- derive: kind and pool preservation (INV-16) ----

    #[test]
    fn derive_preserves_kind() {
        for kind in [
            ObjectKind::Endpoint,
            ObjectKind::Notification,
            ObjectKind::Untyped,
            ObjectKind::Frame,
        ] {
            let parent = parent_with(kind, CAP_RIGHT_READ);
            let parent_id = CapId::new(0, 1, 1);
            let child = Cap::derive(&parent, parent_id, CAP_RIGHT_READ, 0).unwrap();
            assert_eq!(child.kind, kind);
        }
    }

    #[test]
    fn derive_preserves_pool_index() {
        let mut parent = parent_with(ObjectKind::Endpoint, CAP_RIGHT_READ);
        parent.pool_index = 31337;
        let parent_id = CapId::new(0, 1, 1);
        let child = Cap::derive(&parent, parent_id, CAP_RIGHT_READ, 0).unwrap();
        assert_eq!(child.pool_index, 31337);
    }

    // ---- derive: empty parent and reserved bits (INV-15) ----

    #[test]
    fn derive_from_empty_parent_rejected() {
        let parent = Cap::empty();
        let parent_id = CapId::new(0, 1, 1);
        let result = Cap::derive(&parent, parent_id, 0, 0);
        assert_eq!(result, Err(KernelError::InvalidArgument));
    }

    #[test]
    fn derive_with_reserved_bits_rejected() {
        let parent = parent_with(ObjectKind::Endpoint, 0xFF); // all bits set
        let parent_id = CapId::new(0, 1, 1);
        // Bit 4 is reserved; cannot be requested in Phase 1b.
        let result = Cap::derive(&parent, parent_id, 1 << 4, 0);
        assert_eq!(result, Err(KernelError::InvalidArgument));
    }

    // ---- derive: badge handling ----

    #[test]
    fn derive_endpoint_carries_badge() {
        let parent = parent_with(ObjectKind::Endpoint, CAP_RIGHT_WRITE);
        let parent_id = CapId::new(0, 1, 1);
        let child = Cap::derive(&parent, parent_id, CAP_RIGHT_WRITE, 0xCAFEBABE).unwrap();
        assert_eq!(child.badge, 0xCAFEBABE);
    }

    #[test]
    fn derive_non_endpoint_drops_badge() {
        // Frame caps don't carry badges; the badge arg is silently zeroed.
        let parent = parent_with(ObjectKind::Frame, CAP_RIGHT_READ);
        let parent_id = CapId::new(0, 1, 1);
        let child = Cap::derive(&parent, parent_id, CAP_RIGHT_READ, 0xCAFEBABE).unwrap();
        assert_eq!(child.badge, 0);
    }
}
