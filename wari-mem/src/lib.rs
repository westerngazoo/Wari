// SPDX-License-Identifier: AGPL-3.0-only
//! Wari memory subsystem — pure logic.
//!
//! This crate hosts the host-testable pure-logic core of Wari's memory
//! subsystem: the bitmap page allocator and the Sv39 page-table data
//! structures plus walker. Both modules are free of `unsafe`, MMIO, and
//! linker-symbol references; integration glue (linker symbols, MMU
//! enable, MMIO maps) lives in `kernel/src/mem/kvm.rs` and is layered
//! on top of these primitives.
//!
//! Cherry-picked from `goose-os/kernel/src/{page_alloc,page_table}.rs`
//! at goose-os rev `69d9908b6956315684c567fb95cec542062a61a5` under the
//! "only copy what makes sense" discipline (Wari §Code Quality).
//!
//! The kernel re-exports both modules via `kernel/src/mem/page_alloc.rs`
//! and `kernel/src/mem/page_table.rs` so kernel call sites are unchanged.

#![cfg_attr(not(test), no_std)]
// `cfg(kani)` gates formal-verification harnesses run by the Kani model
// checker. Kani sets the cfg externally; rustc itself doesn't know about
// it and would emit `unexpected_cfgs`. The harnesses are inert unless
// Kani is the driver.
#![allow(unexpected_cfgs)]

pub mod page_alloc;
pub mod page_table;
