// SPDX-License-Identifier: AGPL-3.0-only
//! Embedded signed Tier-2 UART driver — platform-conditional.
//!
//! PR 9 split the single `uart.signed.wasm` into two per-platform
//! variants: `uart-qemu.signed.wasm` and `uart-vf2.signed.wasm`. The
//! kernel `include_bytes!`s the platform-matched blob at compile time
//! via the `qemu` / `vf2` cargo features.
//!
//! Build pipeline (per `Makefile::sign-uart-driver`):
//!
//! ```text
//! 1. cargo build --release --features qemu  → wari_driver_uart.wasm
//!    cp …                                    → build/drivers/uart-qemu.wasm
//! 2. cargo build --release --features vf2   → wari_driver_uart.wasm
//!    cp …                                    → build/drivers/uart-vf2.wasm
//! 3. sign-module qemu  → build/drivers/uart-qemu.signed.wasm
//! 4. sign-module vf2   → build/drivers/uart-vf2.signed.wasm
//! ```

#[cfg(feature = "qemu")]
pub static UART_DRIVER_SIGNED: &[u8] =
    include_bytes!("../../../build/drivers/uart-qemu.signed.wasm");

#[cfg(feature = "vf2")]
pub static UART_DRIVER_SIGNED: &[u8] =
    include_bytes!("../../../build/drivers/uart-vf2.signed.wasm");
