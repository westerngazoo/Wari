// SPDX-License-Identifier: AGPL-3.0-only
//! Re-export of `wari-mem`'s page allocator for kernel consumers.
//!
//! The pure logic (and host tests) live in the `wari-mem` workspace
//! crate; this kernel-side module exists only so existing call sites
//! using `crate::mem::page_alloc::*` keep compiling unchanged.
//!
//! Phase 0a PR 3 will add the `kvm.rs` consumer that drives `install()`
//! from linker symbols at boot. Until then the re-export has no in-tree
//! caller — `unused_imports` is silenced here, not workspace-wide.

#[allow(unused_imports)]
pub use wari_mem::page_alloc::*;
