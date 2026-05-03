// SPDX-License-Identifier: AGPL-3.0-only
//! Tier-1 + Tier-2 loader orchestration.
//!
//! ## Tier-2 pipeline (PR 5)
//!
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
//! ## Tier-1 pipeline (PR 6)
//!
//! Mirrors Tier-2 with two differences:
//!   - **No signature verification.** Phase-0 decision: Tier-1 modules
//!     are unsigned. Tier-1 signing arrives in Phase 1 with the
//!     manifest registry. The raw `.wasm` is taken directly from the
//!     embedded blob (`hello_blob::HELLO_WASM`).
//!   - **WASI host fns instead of `wari::mmio_write8`.** The Tier-1
//!     linker is parameterised by `Tier1HostState` (separate from
//!     `Tier2HostState`) so the Tier-1 cap shape is enforced at the
//!     type level — a Tier-1 instance cannot accidentally see Tier-2
//!     host imports.
//!
//! ## Why two HostState types (Why/How depth)
//!
//! Picked: `Tier1HostState` (in `runtime::wasi`) and `Tier2HostState`
//! (in `runtime::host_fns`) as two distinct structs.
//! Considered:
//!   - one shared `HostState { tier: Tier, caps: Caps, exit: Option<u32> }`
//!     → rejected: every host fn would runtime-discriminate the tier
//!     instead of relying on the type system; a Tier-2 host fn could
//!     compile against Tier-1's caps.
//!   - generic `HostState<C: CapShape>` → rejected: trait+generic
//!     overhead with one impl per tier ≡ debt per CLAUDE §Code Quality
//!     #4.
//! Why this won: each `Linker<T>` is parameterised by exactly the
//! state its host fns need. Cost accepted: small duplication of the
//! `caps: Caps` field; offset by clearer cap-tier separation.
//!
//! Coexists with `engine::instantiate_noop`. PR 4's noop path remains
//! for the fuzz target landing in PR 7 (`#[allow(dead_code)]` until then).

#![allow(dead_code)]
#![allow(clippy::doc_lazy_continuation)]

use wasmi::{Engine, Linker, Module, Store};

use crate::cap::{caps_for, Caps, ModuleId, Tier};
use crate::error::KernelError;
use crate::runtime::host_fns::{self, Tier2HostState};
use crate::runtime::manifest;
use crate::runtime::sign;
use crate::runtime::wasi::{self, Tier1HostState};

// Phase-1b PR Net-4a: a second Tier-2 loader path for the net
// driver. Mirrors `load_tier2` (signature verify → parse →
// instantiate) but uses `register_net_host_fns` so the driver gets
// `wari::net_mmio_*` plus the cap/notification surface with its own
// `proc_id` baked in. `load_tier2` (UART path) and `load_tier2_net`
// share signature verification (INV-13) and the wasmi pipeline; the
// only difference is which set of host fns the linker carries.

/// A live Tier-2 WASM instance plus the per-instance state the kernel
/// keeps next to it (capabilities, tier, owning store).
///
/// Phase-0 holds the instance for the lifetime of the boot — there is
/// no proc table yet. PR 6 routes `fd_write` calls into it via the
/// `tier2_uart` singleton; the long-term home is a per-process record
/// landing in Phase 1.
pub struct Tier2Instance {
    /// The wasmi instance handle.
    pub instance: wasmi::Instance,
    /// Per-instance store, owned by this struct so host fns retain
    /// access to `Tier2HostState` for the instance's lifetime.
    pub store: Store<Tier2HostState>,
    /// Capabilities granted at load time. Immutable post-load.
    pub caps: Caps,
    /// Tier tag — `Tier::Two` for everything this loader produces.
    pub tier: Tier,
}

/// A live Tier-1 WASM instance.
///
/// Differs from `Tier2Instance` only in the `Store`'s host-state shape
/// and tier tag. The kernel runs `_start` on this and observes the
/// resulting `wasmi::Error` for `i32_exit_status` — the WASI
/// `proc_exit` mechanism (see `runtime::wasi::host_proc_exit`).
pub struct Tier1Instance {
    /// The wasmi instance handle.
    pub instance: wasmi::Instance,
    /// Per-instance store; carries `Tier1HostState`.
    pub store: Store<Tier1HostState>,
    /// Capabilities granted at load time. Immutable post-load.
    pub caps: Caps,
    /// Tier tag — `Tier::One` for everything this loader produces.
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
    load_tier2_with_kind(
        envelope,
        module_id,
        wari_driver_iface::DriverKind::Uart,
    )
}

/// Internal helper — same as [`load_tier2`] but with the manifest
/// kind explicitly named, so the net loader can share the manifest-
/// verification path. Phase-2 PR DI-3.
fn load_tier2_with_kind(
    envelope: &[u8],
    module_id: ModuleId,
    expected_kind: wari_driver_iface::DriverKind,
) -> Result<Tier2Instance, KernelError> {
    // Step 1 — INV-13: verify signature before any parse runs.
    let wasm_bytes = sign::verify(envelope)?;

    // Step 2 (NEW, PR DI-3) — parse the embedded driver manifest
    // BEFORE wasmi sees the bytes. Catches wrong-kind binaries +
    // ABI-version mismatches at the cheapest possible stage.
    let view = manifest::parse_from_wasm(wasm_bytes)?;
    let kind = view.kind()?;
    if kind != expected_kind {
        return Err(KernelError::DriverWrongKind);
    }

    // Step 3 — wasmi parse + validate. The custom section is
    // ignored by wasmi; only the function/export/import sections
    // shape the instance.
    let engine = Engine::default();
    let module =
        Module::new(&engine, wasm_bytes).map_err(|_| KernelError::BadWasm)?;

    // Step 4 — assign caps and build the per-instance store.
    let caps = caps_for(Tier::Two, module_id);
    let mut store = Store::new(&engine, Tier2HostState { caps });
    let mut linker = <Linker<Tier2HostState>>::new(&engine);
    host_fns::register_host_fns(&mut linker)?;

    // Step 5 — instantiate.
    let instance = linker
        .instantiate(&mut store, &module)
        .and_then(|pre| pre.start(&mut store))
        .map_err(|_| KernelError::BadWasm)?;

    // Step 6 (NEW, PR DI-3) — verify every export the manifest
    // declares actually resolves with the declared signature.
    // Catches typos / signature drift the sign tool somehow let
    // through. See manifest::verify_exports for details.
    manifest::verify_exports(&view, &instance, &store)?;

    Ok(Tier2Instance {
        instance,
        store,
        caps,
        tier: Tier::Two,
    })
}

/// Load + verify + instantiate a Tier-2 net driver envelope.
///
/// Parallels `load_tier2`, but registers the net driver host fn
/// surface (`wari::net_mmio_*` + cap/notification surface) instead
/// of the UART surface. `proc_id` is baked into each cap closure
/// via `register_net_host_fns`; for Phase 1b this is always
/// `PROC_ID_TIER2_NET`.
///
/// # Contract
///
/// - Precondition: `runtime::heap` initialized.
/// - Precondition: single-hart boot (INV-1).
/// - On success returns a `Tier2Instance` with caps =
///   `caps_for(Tier::Two, module_id)`.
/// - On failure returns `KernelError::BadWasm`.
pub fn load_tier2_net(
    envelope: &[u8],
    module_id: ModuleId,
    proc_id: u8,
) -> Result<Tier2Instance, KernelError> {
    // Step 1 — INV-13: signature verification before parse.
    let wasm_bytes = sign::verify(envelope)?;

    // Step 2 (PR DI-3) — manifest gate. The net driver migrates
    // to the macro in PR DI-4; for binaries built before that
    // migration the parser returns Missing, and the loader treats
    // it as a soft warning in Phase 2 (the kernel still loads,
    // logs `[net] no driver manifest — phase-1 binary`, and the
    // operator sees the gap). After DI-4 lands, the net driver's
    // wasm carries a Net-kind manifest and this branch becomes a
    // hard gate identical to load_tier2's.
    match manifest::parse_from_wasm(wasm_bytes) {
        Ok(view) => {
            let kind = view.kind()?;
            if kind != wari_driver_iface::DriverKind::Net {
                return Err(KernelError::DriverWrongKind);
            }
            // Defer export verification to after instantiate.
            let _ = view;
        }
        Err(wari_driver_iface::DriverManifestError::Missing) => {
            crate::kprintln!(
                "[net] driver loaded without manifest (pre-DI-4 binary, soft-allow)"
            );
        }
        Err(e) => return Err(e.into()),
    }

    let engine = Engine::default();
    let module = Module::new(&engine, wasm_bytes).map_err(|_| KernelError::BadWasm)?;

    let caps = caps_for(Tier::Two, module_id);
    let mut store = Store::new(&engine, Tier2HostState { caps });
    let mut linker = <Linker<Tier2HostState>>::new(&engine);
    host_fns::register_net_host_fns(&mut linker, proc_id)?;

    let instance = linker
        .instantiate(&mut store, &module)
        .and_then(|pre| pre.start(&mut store))
        .map_err(|_| KernelError::BadWasm)?;

    // Re-parse and verify exports if the manifest was present
    // (the borrow above is dropped by now because `wasm_bytes`
    // was loaned to wasmi). Cheap — the parser is O(sections).
    if let Ok(view) = manifest::parse_from_wasm(wasm_bytes) {
        manifest::verify_exports(&view, &instance, &store)?;
    }

    Ok(Tier2Instance {
        instance,
        store,
        caps,
        tier: Tier::Two,
    })
}

/// Load + instantiate a Tier-1 unsigned `.wasm` blob.
///
/// Phase-0 Tier-1 is **unsigned** (decision Q4): hello world is a build
/// artefact compiled in the same workspace as the kernel, embedded via
/// `include_bytes!`. There is no third-party Tier-1 path in Phase 0,
/// so signature verification adds no security and a non-trivial cost.
/// Phase 1's manifest registry brings Tier-1 signing online.
///
/// # Contract
///
/// - Precondition: `runtime::heap` initialized; single-hart boot.
/// - Precondition: `tier2_uart::install` has run if the module imports
///   WASI `fd_write` (which routes through the Tier-2 driver).
/// - On success, returns a `Tier1Instance` whose `caps` reflects the
///   compiled-in policy for `(Tier::One, module_id)`. **Note**: this
///   loader does **not** run `_start` — `runtime::run_tier1_hello`
///   does that, so the caller can observe the exit-status `Error`.
/// - On failure, returns `KernelError::BadWasm`. R5: never panics.
///
/// # Why instantiate-without-start (Why/How depth)
///
/// Picked: `linker.instantiate(...)?.ensure_no_start(...)`. The Tier-1
/// hello uses `_start` as a regular WASI export, **not** a WASM start
/// function, so calling `instantiate_and_start` would not run `_start`
/// (and a future module that did declare a WASM start fn would run it
/// before the kernel could observe the trap, blocking the proc_exit
/// detection). Considered: `instantiate_and_start` (rejected for the
/// reason above). Why this won: gives the kernel a clean hand-off
/// point. Cost accepted: kernel must explicitly resolve `_start` and
/// call it (in `run_tier1_hello`).
pub fn load_tier1(
    wasm_bytes: &[u8],
    module_id: ModuleId,
    proc_id: u8,
) -> Result<Tier1Instance, KernelError> {
    // Step 1 — parse + validate.
    let engine = Engine::default();
    let module = Module::new(&engine, wasm_bytes).map_err(|_| KernelError::BadWasm)?;

    // Step 2 — assign caps and build the per-instance store.
    // The `caps` boolean struct is now legacy state on the runtime
    // path (PR 3b retired its host-fn use); kept on the store for
    // backward compat. The cap-mediated checks live in the cap
    // host fns and use `proc_id` to reach the right CSpace.
    let caps = caps_for(Tier::One, module_id);
    let mut store = Store::new(
        &engine,
        Tier1HostState {
            caps,
            exit_code: None,
        },
    );
    let mut linker = <Linker<Tier1HostState>>::new(&engine);
    wasi::register_wasi_host_fns(&mut linker, proc_id)?;

    // Step 3 — instantiate. wasmi 1.0's `instantiate_and_start` runs the
    // WASM `(start ...)` section if any. Tier-1 hello has no WASM start
    // (its `_start` is an exported WASI entry, not a WASM start), so
    // this is a no-op past validation. The kernel-side caller invokes
    // `_start` explicitly via `Instance::get_typed_func` so the
    // `i32_exit` Error from `proc_exit` propagates as a clean Result.
    let instance = linker
        .instantiate(&mut store, &module)
        .and_then(|pre| pre.start(&mut store))
        .map_err(|_| KernelError::BadWasm)?;

    Ok(Tier1Instance {
        instance,
        store,
        caps,
        tier: Tier::One,
    })
}
