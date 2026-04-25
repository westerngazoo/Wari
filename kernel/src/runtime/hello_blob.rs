// SPDX-License-Identifier: AGPL-3.0-only
//! Embedded raw Tier-1 hello WASM blob.
//!
//! Phase 0 Tier-1 is unsigned (decision Q4): hello is built in the same
//! workspace and its provenance is the kernel's own toolchain, so the
//! signature step that gates Tier-2 (INV-13) does not apply here. The
//! kernel `include_bytes!`s the raw `.wasm` directly.
//!
//! ## Build path
//!
//! ```text
//! 1. cd apps/hello && cargo build --release
//!    → target/wasm32-unknown-unknown/release/wari_hello.wasm
//! 2. cp …                    → build/apps/hello.wasm
//! ```
//!
//! `make build-hello` performs both steps; `make build` depends on it
//! so the blob is in place before the kernel link picks up
//! `include_bytes!`.

/// Raw `.wasm` bytes for the Tier-1 hello module. No envelope; no
/// signature.
pub static HELLO_WASM: &[u8] = include_bytes!("../../../build/apps/hello.wasm");
