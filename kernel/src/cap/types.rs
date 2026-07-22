// SPDX-License-Identifier: AGPL-3.0-only
//! Re-export of `wari-cap`'s capability primitive types.
//!
//! The pure logic (and host tests) live in the `wari-cap` workspace
//! crate — B-3 slice 1 of the extraction program in
//! `docs/kernel-host-testing-design.md`. This kernel-side module
//! exists only so existing call sites using
//! `crate::cap::types::{Cap, CapId, ObjectKind, CAP_RIGHT_*, …}`
//! keep compiling unchanged — the same shim pattern as
//! `mem/page_alloc.rs`.

#[allow(unused_imports)]
pub use wari_cap::types::*;
