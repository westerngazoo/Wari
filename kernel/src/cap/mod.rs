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

// Phase-1b dynamic capability primitive. Pure types and pure
// functions only — no syscall wiring, no boot-time integration.
// The mint/copy/revoke/delete/lookup syscalls land in PR 3; the
// kernel-object pools and boot-time root mint land in PR 2.
//
// See `docs/cap-system-design.md` for the architectural contract
// these modules implement.
pub mod cspace;
pub mod types;

#[cfg(kani)]
pub mod proofs;

pub use static_caps::{caps_for, Caps, ModuleId, Tier};
// `TIER1_DEFAULT_CAPS` / `TIER2_UART_DRIVER_CAPS` are referenced only
// from `static_caps::caps_for`; kept module-private until a future
// caller needs them.

// Phase-1b dynamic re-exports. These are the userspace-facing names
// for the new cap layer; downstream PRs (2, 3) build on top.
pub use cspace::{CSpace, CSPACE_SLOTS, MAX_PROCS};
pub use types::{
    Cap, CapId, ObjectKind, CAP_RIGHTS_PHASE_1B_MASK, CAP_RIGHT_GRANT,
    CAP_RIGHT_GRANT_REPLY, CAP_RIGHT_READ, CAP_RIGHT_WRITE,
};
