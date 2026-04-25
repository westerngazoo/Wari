// SPDX-License-Identifier: AGPL-3.0-only
//! Hand-encoded minimal WASM module for Phase-0b engine smoke.
//!
//! ## Why hand-encoded
//!
//! The execution agent that wrote PR 4 has no shell access, so it
//! cannot run `wat2wasm` to produce a binary from `.wat` source.
//! Hand-encoding 8 bytes is auditable on inspection; it avoids
//! shipping a `.wasm` blob whose provenance isn't visible in source.
//!
//! When PR 5 (Tier-2 UART driver) introduces a Cargo `apps/`-style
//! WASM build, that machinery becomes the standard path and this
//! module retires.
//!
//! ## What's encoded
//!
//! The minimum module the WebAssembly binary format permits is the
//! 8-byte preamble: magic + version. No type, function, or export
//! sections. wasmi 0.32 must accept this as a valid (empty) module.
//! If validation requires non-zero sections, the engine layer surfaces
//! a `KernelError::BadWasm` and the parent gate flags the assumption.
//!
//! Spec reference: WebAssembly Core Spec 1.0, §5.5.1 "Module" —
//! `magic` + `version` are mandatory; every section is optional and
//! `version` is little-endian `0x00000001`.

/// Minimal valid WASM module — magic + version, no sections.
///
/// Bytes:
/// - `0x00 0x61 0x73 0x6D` — magic (`\0asm`)
/// - `0x01 0x00 0x00 0x00` — version 1 (little-endian u32)
pub static NOOP_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6D, // magic: \0asm
    0x01, 0x00, 0x00, 0x00, // version: 1
];
