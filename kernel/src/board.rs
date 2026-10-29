// SPDX-License-Identifier: AGPL-3.0-only
//! The one place platform constants are selected (audit blocker B3).
//!
//! `BOARD` is the single `cfg` ladder for per-SoC constants: every
//! site that used to carry its own `#[cfg(feature)]` pair (UART base +
//! stride, PLIC base + hart context, boot hart, DRAM origin, timebase)
//! now reads `board::BOARD.field`. Adding a third board is one arm
//! here plus `wari_validate::R2S` — not an edit across ~15 sites.
//!
//! The descriptor and its per-platform values (with host tests pinning
//! each to this kernel's values) live in `wari_validate::board`. This
//! shim only binds the compile-time platform choice, mirroring
//! `kernel/src/validate.rs`'s NIC-window selection.
use wari_validate::board::BoardDescriptor;

/// The active board. Selected by exactly one platform feature; the
/// `#[cfg]` guards in `main.rs` (B1/B5) guarantee exactly one is set.
#[cfg(feature = "qemu")]
pub const BOARD: BoardDescriptor = wari_validate::board::QEMU;
/// See [`BOARD`].
#[cfg(feature = "vf2")]
pub const BOARD: BoardDescriptor = wari_validate::board::VF2;
/// See [`BOARD`]. Orange Pi R2S first-light (roadmap p3b) — partial /
/// provisional descriptor; the DT-blocked fields are sentinels the
/// kernel never reaches before its first-light halt. See
/// `docs/r2s-bringup.md`.
#[cfg(feature = "r2s")]
pub const BOARD: BoardDescriptor = wari_validate::board::R2S;
