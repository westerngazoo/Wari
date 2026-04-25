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
#![allow(clippy::doc_lazy_continuation)]

pub mod static_caps;

pub use static_caps::{caps_for, Caps, ModuleId, Tier};
// `TIER1_DEFAULT_CAPS` / `TIER2_UART_DRIVER_CAPS` are referenced only
// from `static_caps::caps_for`; kept module-private until a future
// caller needs them.
