// SPDX-License-Identifier: AGPL-3.0-only
//! Static storage for the cap subsystem — per-process CSpaces and
//! the global `ObjectPools`.
//!
//! Two `static mut`s live here:
//!
//! - `CSPACES`: `[CSpace; MAX_PROCS]`, one CSpace per process. Each
//!   CSpace is ~4.5 KiB; total ≈ 72 KiB for `MAX_PROCS = 16`.
//! - `OBJECT_POOLS`: a single `ObjectPools` containing the four
//!   per-kind pools.
//!
//! Both are initialized at compile time via `const fn` constructors,
//! so boot has no per-static `init()` step — the data is already in
//! place when `kmain` starts.
//!
//! ## Access discipline (INV-1, INV-8)
//!
//! Both `static mut`s are accessed only through accessor functions
//! that return `&'static mut`. Callers must:
//!
//! 1. Not hold one accessor's reference while calling another that
//!    aliases the same memory. (E.g., calling `cspaces()` twice and
//!    holding both results simultaneously is unsound.) The Phase-1b
//!    rule of thumb: take the reference, do the work, drop it.
//! 2. Run on a single hart (INV-1) — the accessors do no
//!    synchronization. Phase 2+ SMP migration replaces these with
//!    per-hart slabs or proper locks.
//!
//! Both invariants are documented at the SAFETY comment of each
//! accessor.

#![allow(dead_code)]
#![allow(clippy::doc_lazy_continuation)]

use core::ptr::addr_of_mut;

use super::cspace::{CSpace, MAX_PROCS};
use super::objects::ObjectPools;
use super::reg::RegTable;

// ─────────────────────────────────────────────────────────────────
// Statics
// ─────────────────────────────────────────────────────────────────

/// Per-process CSpaces. Indexed by process id (0..MAX_PROCS).
///
/// The inline-const `[const { CSpace::new() }; MAX_PROCS]` syntax
/// produces an array of fresh CSpaces with no Copy requirement on
/// `CSpace`. Each CSpace is ~4.5 KiB; the array is therefore ~72
/// KiB of `.bss` (zero-initialized at boot — `boot.S` clears `.bss`
/// before calling `kmain`).
static mut CSPACES: [CSpace; MAX_PROCS] = [const { CSpace::new() }; MAX_PROCS];

/// Global per-kind object pools, shared across all processes. Each
/// pool is a fixed-size slab; capacities live in `objects::*_POOL_CAPACITY`
/// constants.
static mut OBJECT_POOLS: ObjectPools = ObjectPools::new();

/// Per-process registered-capability fast-path tables (PR cap-fastpath-1).
/// Indexed by process id like `CSPACES`. Each `RegTable` is small
/// (`REG_SLOTS` × a few bytes); the array is zero/`Empty`-initialized at
/// boot via `const fn`. See `docs/cap-registered-fastpath-design.md`.
static mut REG_TABLES: [RegTable; MAX_PROCS] = [const { RegTable::new() }; MAX_PROCS];

// ─────────────────────────────────────────────────────────────────
// Accessors
// ─────────────────────────────────────────────────────────────────

/// Return a mutable reference to the per-process CSpaces array.
///
/// # Safety contract
///
/// - **INV-1 (single-hart kernel)**: only one hart executes kernel
///   code, so the returned `&mut` does not alias with any concurrent
///   reader.
/// - **INV-8 (post-init access)**: `CSPACES` is statically
///   initialized via `const fn`; there is no `init()` step that must
///   precede first access.
///
/// Callers must not hold the returned reference across another call
/// to this function — Rust's aliasing rules forbid two simultaneous
/// `&mut` to the same memory even if both are dropped before the
/// next yield. The Phase-1b convention: take the reference, do the
/// work in one straight-line block, return.
pub fn cspaces() -> &'static mut [CSpace; MAX_PROCS] {
    // SAFETY: INV-1 + INV-8 — single-hart, statically initialized.
    unsafe { &mut *addr_of_mut!(CSPACES) }
}

/// Return a mutable reference to the global object pools.
///
/// # Safety contract
///
/// Same as `cspaces()`: single-hart (INV-1), statically initialized
/// (INV-8), no concurrent aliasing requirement on the caller.
pub fn object_pools() -> &'static mut ObjectPools {
    // SAFETY: INV-1 + INV-8 — single-hart, statically initialized.
    unsafe { &mut *addr_of_mut!(OBJECT_POOLS) }
}

/// Return a mutable reference to the per-process registered-capability
/// tables.
///
/// # Safety contract
///
/// Same as `cspaces()`: single-hart (INV-1), statically initialized via
/// `const fn` (INV-8), no concurrent aliasing requirement. Callers must
/// not hold the returned reference across another call to this function.
pub fn reg_tables() -> &'static mut [RegTable; MAX_PROCS] {
    // SAFETY: INV-1 + INV-8 — single-hart, statically initialized.
    unsafe { &mut *addr_of_mut!(REG_TABLES) }
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

// Note: host-side tests would require lifting cap/* into a separate
// crate (the kernel binary cannot be built for host). The boot-time
// integration is verified by the QEMU smoke test after PR 2 lands
// (kernel boots, banner unchanged, Tier-2 driver loads, Tier-1 hello
// runs — same observable behaviour as PR 1, just with init_root_caps
// having populated the pools/CSpaces invisibly).
