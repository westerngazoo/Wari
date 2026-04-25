// SPDX-License-Identifier: AGPL-3.0-only
//! Tier-2 signed-loader orchestration.
//!
//! Pipeline:
//!   1. `sign::verify(envelope)` — INV-13. Returns the raw `.wasm`
//!      slice on success.
//!   2. `wasmi::Module::new(&engine, wasm_bytes)` — parse + validate.
//!   3. `Linker::new` + `host_fns::register_host_fns(&mut linker)` —
//!      register `wari::mmio_write8`.
//!   4. `linker.instantiate(&mut store, &module)?.start(&mut store)?`
//!      — wasmi 1.0 API: instantiate then run start fn (if any).
//!   5. Build a `Tier2Instance` carrying the instance, its store, the
//!      `Caps` chosen for `(Tier::Two, module_id)`, and the tier tag.
//!
//! The `Store` carries `HostState { caps }` so that host fns
//! (`host_fns.rs`) can check the caller's capability set on every call.
//!
//! Coexists with `engine::instantiate_noop`. PR 4's noop path remains
//! for the fuzz target landing in PR 7 (`#[allow(dead_code)]` until then).

#![allow(dead_code)]

use wasmi::{Engine, Linker, Module, Store};

use crate::cap::{caps_for, Caps, ModuleId, Tier};
use crate::error::KernelError;
use crate::runtime::host_fns::{self, HostState};
use crate::runtime::sign;

/// A live Tier-2 WASM instance plus the per-instance state the kernel
/// keeps next to it (capabilities, tier, owning store).
///
/// Phase-0 holds the instance for the lifetime of the boot — there is
/// no proc table yet. PR 6 / Phase 1 will move ownership into a process
/// record.
pub struct Tier2Instance {
    /// The wasmi instance handle.
    pub instance: wasmi::Instance,
    /// Per-instance store, owned by this struct so host fns retain
    /// access to `HostState` for the instance's lifetime.
    pub store: Store<HostState>,
    /// Capabilities granted at load time. Immutable post-load.
    pub caps: Caps,
    /// Tier tag — `Tier::Two` for everything this loader produces.
    pub tier: Tier,
}

/// Load + verify + instantiate a Tier-2 signed envelope.
///
/// # Contract
///
/// - Precondition: `runtime::heap` is initialized (wasmi will allocate
///   during `Module::new` and `instantiate`).
/// - Precondition: single-hart boot context (INV-1).
/// - On success, returns a `Tier2Instance` whose `caps` reflects the
///   compiled-in policy for `(Tier::Two, module_id)`.
/// - On failure, returns `KernelError::BadWasm` for any verification,
///   parse, link, instantiate, or start error. R5: never panics.
///
/// # Invariants
///
/// INV-13 — signature verification is the **first** step. wasmi never
/// sees the bytes until `sign::verify` returns `Ok`.
pub fn load_tier2(
    envelope: &[u8],
    module_id: ModuleId,
) -> Result<Tier2Instance, KernelError> {
    // Step 1 — INV-13: verify before parse.
    let wasm_bytes = sign::verify(envelope)?;

    // Step 2 — parse + validate.
    let engine = Engine::default();
    let module = Module::new(&engine, wasm_bytes).map_err(|_| KernelError::BadWasm)?;

    // Step 3 — assign caps and build the per-instance store.
    let caps = caps_for(Tier::Two, module_id);
    let mut store = Store::new(&engine, HostState { caps });
    let mut linker = <Linker<HostState>>::new(&engine);
    host_fns::register_host_fns(&mut linker)?;

    // Step 4 — instantiate. Wasmi 1.0's `instantiate_and_start` runs the
    // start function if one exists; the UART driver has none, so this is
    // structurally a validate + link step. If a future Tier-2 module
    // ships with a start fn it runs here under wasmi's default budget.
    let instance = linker
        .instantiate_and_start(&mut store, &module)
        .map_err(|_| KernelError::BadWasm)?;

    Ok(Tier2Instance {
        instance,
        store,
        caps,
        tier: Tier::Two,
    })
}
