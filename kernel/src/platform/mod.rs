// SPDX-License-Identifier: AGPL-3.0-only
//! Platform abstraction — compile-time-selected per `qemu` / `vf2` feature.
//!
//! Phase-1a introduces this module to make platform constants
//! discoverable from one place. Pre-PR-8, MMIO constants were
//! hardcoded in `mmio/uart_ns16550.rs` (`UART_BASE = 0x1000_0000`),
//! `mem/kvm.rs` (`UART_MMIO_BASE = 0x1000_0000`), and
//! `validate.rs` (`UART_MMIO_BASE`). All three now defer to
//! `platform::UART_BASE` so the same kernel code compiles cleanly
//! for QEMU and VF2 targets.
//!
//! ## Why feature flags vs runtime detection
//!
//! Picked: compile-time `#[cfg(feature = ...)]` selection.
//! Considered: runtime detection via DTB parse — rejected as
//! Phase-2+ work that pulls in a DT parser the kernel doesn't need.
//! Considered: build-time DTB inspection — rejected as out-of-tree
//! configuration.
//! Why this won: goose-os used feature flags across ~100 builds without
//! incident; the same one-binary-per-platform discipline keeps the
//! release pipeline simple (Makefile already has `qemu` / `vf2` build
//! variants per goose-os pattern). Cost accepted: two binaries to
//! ship instead of one.

#[cfg(feature = "qemu")]
mod qemu_virt;
#[cfg(feature = "qemu")]
pub use qemu_virt::*;

#[cfg(feature = "vf2")]
mod vf2;
#[cfg(feature = "vf2")]
pub use vf2::*;

#[cfg(not(any(feature = "qemu", feature = "vf2")))]
compile_error!("Wari requires exactly one platform feature: `qemu` or `vf2`.");

#[cfg(all(feature = "qemu", feature = "vf2"))]
compile_error!(
    "Wari requires exactly one platform feature: `qemu` or `vf2` — not both."
);
