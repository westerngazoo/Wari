// SPDX-License-Identifier: AGPL-3.0-only
//! Wasmi engine wiring for Phase-0b.
//!
//! Builds an `Engine` + `Store` + `Module` + `Linker`, instantiates
//! the noop module, and drops everything. Any wasmi error folds into
//! `KernelError::BadWasm` (R5: no panics in kernel paths).
//!
//! No host functions are registered yet — the noop blob exports
//! nothing and imports nothing. The host-fn surface lands with the
//! Tier-2 UART driver (PR 5).

use wasmi::{Engine, Linker, Module, Store};

use crate::error::KernelError;
use crate::runtime::noop_blob::NOOP_WASM;

/// Empty per-store host context — the noop module needs no state.
struct HostState;

/// Build an `Engine`, parse the noop module, instantiate it under a
/// fresh `Store`, and call its start function if any (the noop blob
/// has none — `ensure_no_start` succeeds trivially).
///
/// # Errors
///
/// Returns `KernelError::BadWasm` for any wasmi-side failure (parse,
/// validation, link, instantiate, start). The kernel does not need to
/// distinguish them at this layer; the parent gate inspects logs.
pub fn instantiate_noop() -> Result<(), KernelError> {
    let engine = Engine::default();

    let module = Module::new(&engine, NOOP_WASM).map_err(|_| KernelError::BadWasm)?;

    let mut store = Store::new(&engine, HostState);
    let linker = <Linker<HostState>>::new(&engine);

    // wasmi 1.0 API: `instantiate_and_start` does both the instantiate
    // step and the start-fn invocation. The noop blob has no start fn,
    // so this is structurally a no-op past validation. Any wasmi error
    // folds to `BadWasm` (R5: typed error, never panic).
    let _instance = linker
        .instantiate_and_start(&mut store, &module)
        .map_err(|_| KernelError::BadWasm)?;

    Ok(())
}
