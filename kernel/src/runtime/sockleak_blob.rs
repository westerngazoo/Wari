// SPDX-License-Identifier: AGPL-3.0-only
//! Embedded raw Tier-1 socket-leak probe (`apps/sockleak`).
//!
//! Baked, not runtime-loaded: the probe needs a Net cap, and the
//! Supervisor ceiling withholds Net from every runtime-loaded module
//! (`runtime::modreg`). Like the hello blob it rides the kernel image
//! unsigned and inherits the image's provenance.
//!
//! Built and staged by `scripts/build.sh` step 2 (`--programs`).

/// Raw `.wasm` bytes of the socket-leak probe. No envelope.
pub static SOCKLEAK_WASM: &[u8] = include_bytes!("../../../build/apps/sockleak.wasm");
