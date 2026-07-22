// SPDX-License-Identifier: AGPL-3.0-only
//! Wari — capability-system pure logic (host-testable core).
//!
//! Pure decision-and-data core of the capability subsystem, extracted
//! from `kernel/src/cap/` so it compiles and tests on the host — the
//! Option-B program of `docs/kernel-host-testing-design.md` (§4 lane
//! B-3), following the pattern `wari-mem` established: the kernel
//! keeps the imperative shell (static storage, boot wiring, syscall
//! glue) plus re-export shims, so kernel call sites are unchanged.
//!
//! Modules, in extraction order:
//!
//! - [`static_caps`] — the Phase-0 static capability table plus the
//!   `Tier` / `ModuleId` identity enums (seeded first because
//!   `wari-sched`'s `Process` carries `Tier` + `ModuleId`).
//! - [`types`] — the 16-byte runtime `Cap`, `CapId`, `ObjectKind`,
//!   the rights bitmap, and `Cap::derive` (INV-10 / INV-15 / INV-16
//!   enforcement lives here, next to the types).
//! - [`pool`] — `Pool<T, N>` slab and `BoundedQueue<T, N>` FIFO, the
//!   two allocation-free containers backing kernel objects.
//! - [`cspace`] — the per-process capability table (`CSpace`,
//!   `CSPACE_SLOTS`, `MAX_PROCS`); INV-17/INV-18 mechanics.
//! - [`objects`] — the kernel object kinds (`Endpoint`,
//!   `Notification`, `Frame`, `Untyped`, `Net`, `Socket`) and the
//!   `ObjectPools` aggregate.
//! - [`revoke`] — the revocation cascade and refcount primitives,
//!   parameterized over the CSpace array + pools so the walk is
//!   host-testable (the kernel binding passes its statics).
//! - `proofs` (`#[cfg(kani)]`) — the Kani harnesses for INV-10 /
//!   INV-15 / INV-16 / INV-17, now runnable against a plain lib
//!   crate instead of the kernel bin.
//!
//! Permanently kernel-side (the imperative shell): `cap/storage.rs`
//! (the statics + accessors), `cap/boot.rs`, `cap/reg.rs`,
//! `cap/ring_drain.rs`, and `cap/syscall.rs` glue.

#![cfg_attr(not(test), no_std)]

pub mod cspace;
pub mod objects;
pub mod pool;
#[cfg(kani)]
pub mod proofs;
pub mod revoke;
pub mod static_caps;
pub mod types;

pub use cspace::{CSpace, CSPACE_SLOTS, MAX_PROCS};
pub use objects::{
    Endpoint, Frame, Net, Notification, ObjectPools, Socket, TcbRef, Untyped,
    ENDPOINT_POOL_CAPACITY, FRAME_POOL_CAPACITY, NET_POOL_CAPACITY, NOTIFICATION_POOL_CAPACITY,
    SOCKET_POOL_CAPACITY, UNTYPED_POOL_CAPACITY,
};
pub use pool::{BoundedQueue, Pool};
pub use static_caps::{caps_for, Caps, ModuleId, Tier};
pub use types::{
    Cap, CapId, ObjectKind, CAP_RIGHTS_PHASE_1B_MASK, CAP_RIGHT_GRANT, CAP_RIGHT_GRANT_REPLY,
    CAP_RIGHT_READ, CAP_RIGHT_WRITE,
};
