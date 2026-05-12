// SPDX-License-Identifier: AGPL-3.0-only
//! Net driver build script — embeds `WARI_BUILD` so the kernel can
//! verify that the signed wasm it `include_bytes!`s came from the
//! same build cycle.
//!
//! Diagnosed May 2026: builds 107..114 silently used a stale wasm
//! artifact because `core::arch::asm!("fence ow,ow")` had been added
//! to driver code, which doesn't compile to wasm32. Cargo's last-
//! known-good cache let `cd kernel && cargo build` proceed with the
//! build-106 driver blob while the kernel banner read "build 114".
//! The kernel-side check in `kernel/build.rs` greps the embedded
//! `WARI-DRV-BUILD-TAG-N` string out of the signed envelope and
//! fails the build if N != current WARI_BUILD.

fn main() {
    // CRITICAL: same env-var dance as kernel/build.rs. Without this
    // line, bumping .build_number doesn't cause cargo to recompile
    // the driver, so the embedded build tag goes stale.
    println!("cargo:rerun-if-env-changed=WARI_BUILD");
    let build = std::env::var("WARI_BUILD").unwrap_or_else(|_| "0".into());
    // Re-export as a compile-time env so lib.rs can embed it.
    println!("cargo:rustc-env=WARI_BUILD={}", build);
}
