// SPDX-License-Identifier: AGPL-3.0-only
//! Wari — capability-system pure logic (host-testable core).
//!
//! Pure decision-and-data core of the capability subsystem, extracted
//! from `kernel/src/cap/` so it compiles and tests on the host — the
//! Option-B program of `docs/kernel-host-testing-design.md` (§4 lane
//! B-3), following the pattern `wari-mem` established: the kernel
//! keeps the imperative shell (static storage, syscall glue) plus
//! re-export shims, so kernel call sites are unchanged.
//!
//! This first slice carries only [`static_caps`] — the Phase-0 static
//! capability table plus the `Tier` / `ModuleId` identity enums. It
//! is seeded ahead of the RFC's §9 order because `wari-sched`'s
//! `Process` carries `Tier` + `ModuleId` fields, and duplicating
//! identity enums across crates (or making `Process` generic over
//! them) was rejected — one source of truth. The dynamic cap modules
//! (`types`, `pool`, `cspace`, `objects`, `revoke`, the Kani
//! harnesses) migrate here in the follow-up B-3 slices.

#![cfg_attr(not(test), no_std)]

pub mod static_caps;

pub use static_caps::{caps_for, Caps, ModuleId, Tier};
