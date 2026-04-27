// SPDX-License-Identifier: AGPL-3.0-only
//! Embedded signed Tier-2 net driver — platform-conditional.
//!
//! Mirrors the Tier-2 UART driver pattern (`uart_blob.rs`). Two
//! per-platform variants — `net-qemu.signed.wasm` (VirtIO-net) and
//! `net-vf2.signed.wasm` (JH7110 GMAC, Phase 1c stub for now). The
//! kernel `include_bytes!`s the platform-matched blob at compile
//! time via the `qemu` / `vf2` cargo features.
//!
//! Build pipeline (per `Makefile::sign-net-driver`):
//!
//! ```text
//! 1. cargo build --release --features qemu  -> wari_driver_net.wasm
//!    cp ...                                  -> build/drivers/net-qemu.wasm
//! 2. cargo build --release --features vf2   -> wari_driver_net.wasm
//!    cp ...                                  -> build/drivers/net-vf2.wasm
//! 3. sign-module qemu  -> build/drivers/net-qemu.signed.wasm
//! 4. sign-module vf2   -> build/drivers/net-vf2.signed.wasm
//! ```
//!
//! The signed blobs are gitignored (`/build/*` rule); the developer
//! or CI must run `make sign-net-driver` before `cargo build` of the
//! kernel will succeed (the `include_bytes!` paths below resolve at
//! kernel-compile time).

#[cfg(feature = "qemu")]
pub static NET_DRIVER_SIGNED: &[u8] =
    include_bytes!("../../../build/drivers/net-qemu.signed.wasm");

#[cfg(feature = "vf2")]
pub static NET_DRIVER_SIGNED: &[u8] =
    include_bytes!("../../../build/drivers/net-vf2.signed.wasm");
