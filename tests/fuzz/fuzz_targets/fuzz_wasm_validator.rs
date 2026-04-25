// SPDX-License-Identifier: AGPL-3.0-only
//! Fuzz `wasmi::Module::new` against arbitrary byte streams.
//!
//! ## Property
//!
//! Every input either parses successfully or returns a typed
//! `wasmi::Error`. **Zero panics.** A panic indicates a wasmi-side bug
//! that breaches Wari's Layer-1 (structural) defense — see
//! `docs/security-model.md` §"Three layers, three sandboxes". Layer 2
//! (Sv39 MMU) would still contain a single tenant's escape, but a
//! validator-side panic in the kernel address space is a kernel bug
//! we must catch at fuzz time, not in production.
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
