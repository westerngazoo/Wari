// SPDX-License-Identifier: AGPL-3.0-only
//! Re-export of `wari-cap`'s kernel object kinds.
//!
//! The pure logic (and host tests) live in the `wari-cap` workspace
//! crate — B-3 slice 2 of the extraction program in
//! `docs/kernel-host-testing-design.md`. This kernel-side module
//! exists only so existing call sites using
//! `crate::cap::objects::{Endpoint, Notification, ObjectPools, …}`
//! keep compiling unchanged — the same shim pattern as
//! `mem/page_alloc.rs`.

#[allow(unused_imports)]
pub use wari_cap::objects::*;
