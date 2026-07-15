// SPDX-License-Identifier: AGPL-3.0-only
//! Re-export of `wari-validate`'s pure argument validators, plus the
//! platform selection of the NIC MMIO window table.
//!
//! The pure logic (and host tests, covering BOTH platforms' window
//! tables) lives in the `wari-validate` workspace crate — lane B-2 of
//! the extraction program in `docs/kernel-host-testing-design.md`.
//! This kernel-side module keeps exactly two things:
//!
//! 1. the re-export shim so call sites using `crate::validate::*`
//!    keep compiling unchanged (the `mem/page_alloc.rs` pattern), and
//! 2. the `#[cfg(feature)]` choice of which platform's window table
//!    is live — platform features belong to the kernel, so the pure
//!    crate holds windows for both platforms as data and the
//!    selection happens here.

#![allow(dead_code)]

#[allow(unused_imports)]
pub use wari_validate::*;

/// NIC MMIO window table for the active platform — see
/// `wari_validate::windows` for the tables themselves (and the
/// per-window rationale comments).
#[cfg(feature = "qemu")]
pub const NET_MMIO_WINDOWS: &[MmioWindow] = wari_validate::windows::qemu::NET_WINDOWS;

/// NIC MMIO window table for the active platform — see
/// `wari_validate::windows` for the tables themselves (and the
/// per-window rationale comments).
#[cfg(feature = "vf2")]
pub const NET_MMIO_WINDOWS: &[MmioWindow] = wari_validate::windows::vf2::NET_WINDOWS;

/// Is `addr` inside the NIC register window set for the active
/// platform?
///
/// Sister to `is_uart_mmio_addr`. Phase 1b grants the `Net` cap
/// exclusively to the Tier-2 net driver; this validator narrows
/// INV-3 (MMIO address validity) and INV-20 (NIC MMIO Window
/// Validity) to the exact register set the driver is licensed to
/// touch. The window data and the predicate are host-tested in
/// `wari-validate`; this wrapper only binds the platform choice.
#[inline]
pub const fn is_net_mmio_addr(addr: usize) -> bool {
    wari_validate::addr_in_windows(addr, NET_MMIO_WINDOWS)
}
