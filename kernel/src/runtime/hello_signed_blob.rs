// SPDX-License-Identifier: AGPL-3.0-only
//! Signed hello envelope — test payload for the module registry.
//!
//! Same `apps/hello` binary as `hello_blob::HELLO_WASM`, but wrapped
//! in the ed25519 envelope `scripts/sign-module` produces (the format
//! `runtime::sign::verify` checks — the gate every *runtime-arriving*
//! module must pass, per INV-11/INV-13). Embedded solely so
//! `modreg::self_test_spawn` can exercise the full dynamic path
//! without a network transport; retires with that self-test once the
//! `wari-http` upload path lands.
//!
//! Built and signed by `scripts/build.sh` step 2 (which stages
//! `build/apps/hello.signed.wasm`); a bare `cd kernel && cargo build`
//! after touching `apps/hello` embeds a stale blob — same rule as
//! every other embedded artifact, see CLAUDE.md "Build pipeline".

/// The signed envelope: `pubkey(32) ‖ signature(64) ‖ wasm payload`.
pub static HELLO_SIGNED: &[u8] = include_bytes!("../../../build/apps/hello.signed.wasm");
