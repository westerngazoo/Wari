// SPDX-License-Identifier: AGPL-3.0-only
//! Re-export of `wari-mem`'s Sv39 page-table primitives for kernel consumers.
//!
//! The pure logic (and host tests) live in the `wari-mem` workspace
//! crate; this kernel-side module exists only so existing call sites
//! using `crate::mem::page_table::*` keep compiling unchanged.
//!
//! Phase 0a PR 3 (`kvm.rs`) will be the first in-tree consumer; until
//! then the re-export has no caller and `unused_imports` is silenced
//! here, not workspace-wide.

#[allow(unused_imports)]
pub use wari_mem::page_table::*;
