// SPDX-License-Identifier: AGPL-3.0-only
//! Capability module — the gate between WASM modules and privileged
//! kernel facilities (MMIO, exit, stdout, …).
//!
//! ## Phase 0 vs Phase 1
//!
//! Phase 0 ships a **static** capability table: there is one compiled-in
//! `Caps` struct per known `ModuleId`, and the loader picks one based on
//! `Tier` + `ModuleId`. There is no mint, no grant, no revoke. Phase 0
//! recognizes exactly two modules: the Tier-2 UART driver (PR 5) and
//! the Tier-1 hello app (PR 6).
//!
//! Phase 1 replaces this with a per-process capability table backed by
//! seL4-style derivation rules; INV-10 (capability monotonicity) and the
//! signed-manifest form of INV-11 land at that point. This file is
//! therefore deliberately small — it covers only what Phase 0 needs and
//! retires when the dynamic system arrives.
//!
//! ## Why static dispatch (Why/How depth)
//!
//! Picked: a `const fn caps_for(Tier, ModuleId) -> Caps` table.
//! Considered:
//!   - dynamic registry indexed by hash → rejected as Phase-1 work that
//!     would expand R8's audit surface for no Phase-0 benefit;
//!   - per-module config files in `drivers/uart/` → rejected as
//!     out-of-tree configuration that breaks "ABI in one place".
//! Why this won: Phase 0 has two modules, both compiled in. A const
//! lookup is the smallest sound primitive that gates `wari_mmio_write8`.
//! Cost accepted: Phase 1 will rewrite this module wholesale.

#![allow(dead_code)]
#![allow(unused_imports)] // PR 1 re-exports types consumed by PR 2/3 only
#![allow(clippy::doc_lazy_continuation)]

// Phase-0 static capability table. Kept until PR 2 retires it in
// favour of boot-time root-cap construction over the new dynamic
// types (`types::Cap` + `cspace::CSpace`). Both subsystems coexist
// during PR 1: nothing in this module currently calls into `types`
// or `cspace`, so the runtime path is unchanged.
pub mod static_caps;

// Phase-1b dynamic capability primitive. PR 1 landed the pure types
// (Cap, CapId, ObjectKind) and per-process CSpace storage; PR 2
// (this set of additions) lands the kernel-object kinds, the global
// object pools, the static storage backing CSpaces and pools, and
// the boot-time root-cap construction. PR 3 ships the
// mint/copy/revoke/delete/lookup syscalls and IPC cap transfer.
//
// See `docs/cap-system-design.md` for the architectural contract
// these modules implement.
pub mod boot;
pub mod cspace;
pub mod objects;
pub mod pool;
pub mod revoke;
pub mod storage;
pub mod syscall;
pub mod types;

#[cfg(kani)]
pub mod proofs;

pub use static_caps::{caps_for, Caps, ModuleId, Tier};
// `TIER1_DEFAULT_CAPS` / `TIER2_UART_DRIVER_CAPS` are referenced only
// from `static_caps::caps_for`; kept module-private until a future
// caller needs them.

// Phase-1b dynamic re-exports. These are the userspace-facing names
// for the new cap layer; downstream PRs build on top.
pub use boot::{
    PROC_ID_RESERVED, PROC_ID_TIER1_HELLO, PROC_ID_TIER1_HELLO_B,
    PROC_ID_TIER2_UART,
};
pub use cspace::{CSpace, CSPACE_SLOTS, MAX_PROCS};
pub use objects::{
    Endpoint, Frame, Notification, ObjectPools, TcbRef, Untyped,
    ENDPOINT_POOL_CAPACITY, FRAME_POOL_CAPACITY, NOTIFICATION_POOL_CAPACITY,
    UNTYPED_POOL_CAPACITY,
};
pub use pool::{BoundedQueue, Pool};
pub use storage::{cspaces, object_pools};
pub use syscall::{
    cap_copy_impl, cap_delete_impl, cap_lookup_impl, cap_mint_impl,
    cap_revoke_impl, check_cap, notification_ack_impl, notification_wait_impl,
    E_AGAIN, E_INVAL, E_NOMEM, E_PERM,
};
pub use types::{
    Cap, CapId, ObjectKind, CAP_RIGHTS_PHASE_1B_MASK, CAP_RIGHT_GRANT,
    CAP_RIGHT_GRANT_REPLY, CAP_RIGHT_READ, CAP_RIGHT_WRITE,
};
