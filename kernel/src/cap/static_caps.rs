// SPDX-License-Identifier: AGPL-3.0-only
//! Re-export of `wari-cap`'s Phase-0 static capability table.
//!
//! The pure logic (and host tests) live in the `wari-cap` workspace
//! crate — the first B-3 slice of the extraction program in
//! `docs/kernel-host-testing-design.md`. This kernel-side module
//! exists only so existing call sites using
//! `crate::cap::{Tier, ModuleId, Caps, caps_for, …}` keep compiling
//! unchanged — the same shim pattern as `mem/page_alloc.rs`.

#[allow(unused_imports)]
pub use wari_cap::static_caps::*;
