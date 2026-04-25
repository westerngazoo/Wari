// SPDX-License-Identifier: AGPL-3.0-only
//! Embedded signed Tier-2 UART driver blob.
//!
//! The bytes are produced by the parent build pipeline:
//!
//! ```text
//! 1. cargo build --release  →  drivers/uart/target/.../wari_driver_uart.wasm
//! 2. cp …                    →  build/drivers/uart.wasm
//! 3. sign-module             →  build/drivers/uart.signed.wasm
//! ```
//!
//! `make build-uart-driver` performs steps 1–2; the parent agent runs
//! step 3 once the dev keypair has been generated. The kernel
//! `include_bytes!`s the result here so the entire Tier-2 trust base is
//! in the kernel image (R8: reproducible build, no filesystem).

/// Signed envelope: `pubkey (32) || signature (64) || wasm_bytes (..)`.
pub static UART_DRIVER_SIGNED: &[u8] =
    include_bytes!("../../../build/drivers/uart.signed.wasm");
