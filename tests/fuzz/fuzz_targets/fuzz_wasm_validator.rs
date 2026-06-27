// SPDX-License-Identifier: AGPL-3.0-only
//! Fuzz `wasmi::Module::new` against arbitrary byte streams.
//!
//! ## Property
//!
//! Every input either parses successfully or returns a typed
//! `wasmi::Error`. **Zero panics.** A panic in the validator path is
//! a `wasmi`-host-side soundness bug. Per `docs/security-model.md`,
//! such a panic is NOT contained by Layer 2 (the MMU): wasmi
//! executes in the kernel address space, so a host-Rust panic in
//! the validator IS a kernel bug. We catch it at fuzz time so it
//! does not become a Tier-1 → Tier-0 escape primitive in production.
//!
//! This harness MUST exercise the exact `wasmi` version the kernel
//! embeds (see `tests/fuzz/Cargo.toml` — the `wasmi` pin is required
//! to track `/kernel/Cargo.toml`). Drift here produces false
//! assurance and was the May-2026 finding that this fuzz-version-sync
//! PR closed.
//!
//! ## Run
//!
//! ```bash
//! cargo fuzz run fuzz_wasm_validator -- -max_total_time=3600
//! ```
//!
//! Phase-0 gate per `docs/testing.md`: 1 h clean.

#![no_main]

use libfuzzer_sys::fuzz_target;
use wasmi::{Engine, Module};

fuzz_target!(|data: &[u8]| {
    let engine = Engine::default();
    // The property is "no panic". libFuzzer detects panics and aborts
    // the worker; a typed Err result is the success path.
    let _ = Module::new(&engine, data);
});
