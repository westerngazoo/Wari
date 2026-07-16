// SPDX-License-Identifier: AGPL-3.0-only
//! Re-export of `wari-error`'s `KernelError` — the single error
//! taxonomy for the kernel (CLAUDE R5).
//!
//! The enum (and its `From<DriverManifestError>` / `into_syscall`
//! conversions) lives in the `wari-error` workspace crate so the
//! pure crates extracted from the kernel can return the same
//! taxonomy — see `docs/kernel-host-testing-design.md`. This
//! kernel-side module exists only so existing call sites using
//! `crate::error::KernelError` keep compiling unchanged — the same
//! shim pattern as `mem/page_alloc.rs`.

#[allow(unused_imports)]
pub use wari_error::*;
