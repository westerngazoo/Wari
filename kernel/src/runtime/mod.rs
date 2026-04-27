// SPDX-License-Identifier: AGPL-3.0-only
//! Tier-0 WASM runtime — `wasmi` embedding for Phase 0.
//!
//! Phase 0 scope (cumulative through PR 6):
//!   - Pin `wasmi = "=1.0.9"` with `default-features = false` (PR 4).
//!   - Hand-rolled bump allocator (`heap.rs`) backs `#[global_allocator]`
//!     so wasmi's internal `Vec`/`Box`/`String` can land somewhere.
//!   - Tier-2 signed-loader pipeline + UART driver (PR 5).
//!   - Tier-2 UART driver instance held as a boot-initialized
//!     singleton (`tier2_uart`) reachable from Tier-1 host fns (PR 6).
//!   - Tier-1 unsigned-loader + WASI `fd_write` + `proc_exit` (PR 6).
//!
//! Out of scope: scheduling, IPC, multiple Tier-1 instances. Those land
//! with the capability system in Phase 1.
//!
//! R2 note: the bump allocator is "heap" but is initialized in
//! `mem::kvm::init` and exercised here from `kmain` (boot context, not
//! trap/dispatch). No syscall path allocates. When wasmi internally
//! allocates during `Module::new` / `Linker::instantiate`, that work
//! happens once at boot before traps are taken.
//!
//! R5: every wasmi error folds into `KernelError::BadWasm` (or
//! `DriverError` for cross-tier marshaling). No panics.

pub mod engine;
pub mod heap;
pub mod hello_blob;
pub mod host_fns;
pub mod loader;
pub mod net_blob;
pub mod noop_blob;
pub mod sign;
pub mod tier2_uart;
pub mod uart_blob;
pub mod wasi;

use crate::cap::ModuleId;
use crate::error::KernelError;
use crate::kprintln;

/// Boot the runtime: instantiate the noop module and prove the engine
/// links. Returns `Ok(())` on success; on any wasmi error returns
/// `KernelError::BadWasm` (R5: no panics).
///
/// Kept for the PR-7 fuzz target — the live boot path uses
/// `run_tier2_uart` instead.
///
/// # Preconditions
/// - `heap::init` has been called (the global allocator is live).
/// - Single-hart boot context (INV-1).
///
/// # Postconditions
/// - On `Ok`, a wasmi `Engine` + `Module` + `Instance` for the noop
///   blob were constructed and dropped. The arena retains whatever
///   wasmi internally allocated (Phase 0 is arena-per-boot).
#[allow(dead_code)]
pub fn run_noop() -> Result<(), KernelError> {
    engine::instantiate_noop()
}

/// Boot the runtime: load the embedded signed Tier-2 UART driver, then
/// install it as the boot-initialized singleton (`tier2_uart`).
///
/// Performs (in order): signature verification (INV-13), wasmi
/// `Module::new`, host-fn registration, instantiate. On success,
/// resolves the driver's `write` typed export and installs the handle
/// for Tier-1 host fns to reach.
///
/// # Errors
///
/// `KernelError::BadWasm` for any verification, parse, link, or
/// instantiate failure (R5). `KernelError::DriverError` if the driver
/// is missing the expected `write(buf_ptr, len) -> i32` export.
/// Boot the Tier-2 net driver: load the embedded signed blob and
/// run its `_start`. Phase-1b PR Net-4a's driver `_start` is a
/// stub (returns immediately); the instance is dropped after this
/// call, but the driver's `Net` cap (installed by
/// `cap::boot::init_root_caps`) persists in
/// `cspaces[PROC_ID_TIER2_NET]` for use by future PRs (Net-4b will
/// install the instance as a singleton and bring up the NIC).
///
/// # Errors
///
/// `KernelError::BadWasm` for any verification, parse, link, or
/// instantiate failure (R5).
pub fn run_tier2_net() -> Result<(), KernelError> {
    use crate::cap::{object_pools, PROC_ID_TIER2_NET};
    let _net = loader::load_tier2_net(
        net_blob::NET_DRIVER_SIGNED,
        ModuleId::Tier2Net,
        PROC_ID_TIER2_NET,
    )?;
    // PR Net-4b: the driver's `_start` ran the VirtIO init sequence
    // and (on success) called `wari::nic_set_mac`, which set
    // `Net.initialized = true` and stored the MAC. Read both back
    // and emit a diagnostic boot line. A zeroed MAC + initialized=false
    // means VirtIO init failed silently; the boot still proceeds (no
    // panic — the system is usable without networking).
    //
    // Phase-1b net pool index is 0 — `init_root_caps` allocates
    // exactly one Net pool entry. PR Net-4c will install a
    // `tier2_net::Tier2NetHandle` singleton so future Tier-1
    // socket calls can re-enter the driver via host fns.
    let pools = object_pools();
    if let Some(net) = pools.nets.get(0) {
        if net.initialized {
            let m = net.mac;
            kprintln!(
                "[net] virtio-net up, mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                m[0], m[1], m[2], m[3], m[4], m[5]
            );
        } else {
            kprintln!("[net] virtio init failed (mac zeroed) — net unavailable");
        }
    } else {
        kprintln!("[net] no net pool entry — init_root_caps failure?");
    }
    Ok(())
}

pub fn run_tier2_uart() -> Result<(), KernelError> {
    let tier2 =
        loader::load_tier2(uart_blob::UART_DRIVER_SIGNED, ModuleId::Tier2Uart)?;

    // Decompose and resolve the typed `write` export. The
    // `get_typed_func` immutable borrow of `store` is released at the
    // end of the let-statement, freeing `store` to move into the
    // handle below.
    let loader::Tier2Instance { instance, store, .. } = tier2;
    let write_fn = instance
        .get_typed_func::<(u32, u32), i32>(&store, "write")
        .map_err(|_| KernelError::DriverError)?;

    let handle = tier2_uart::Tier2UartHandle {
        instance,
        store,
        write_fn,
    };

    // SAFETY: INV-1 (single-hart) + INV-8 (boot-time post-init) +
    // INV-14 (one-time install). `kmain` orders this call before any
    // Tier-1 host fn dispatch.
    unsafe { tier2_uart::install(handle) };
    Ok(())
}

/// Run a Tier-1 instance to completion (clean `proc_exit` or trap).
///
/// Phase 1b's scheduler calls this once per registered Tier-1
/// tenant. Each call:
///
/// 1. Loads the supplied WASM blob with the supplied `proc_id`
///    baked into the host-fn closures (see
///    `wasi::register_wasi_host_fns`), so `cap_*` and the
///    cap-mediated WASI checks reach the right CSpace.
/// 2. Resolves `_start` and calls it.
/// 3. Returns `Ok(code)` on a clean `proc_exit(code)`, or `Err(...)`
///    on any other wasmi error.
///
/// # Contract
///
/// - Precondition: `tier2_uart` is installed (Tier-1 `fd_write`
///   marshals through it).
/// - Precondition: `cap::boot::init_root_caps` has populated the
///   `proc_id` CSpace with the caps the instance needs (UART send,
///   exit). Without those caps the instance hits `WASI_EPERM` and
///   exits with a -1 trap.
/// - On clean exit returns `Ok(code)` and prints
///   `[t1:proc_id] exit(code)` for boot-trace observability.
/// - On other wasmi error returns `Err(KernelError::BadWasm)`.
pub fn run_tier1(
    proc_id: u8,
    wasm_bytes: &[u8],
    module_id: ModuleId,
) -> Result<i32, KernelError> {
    let tier1 = loader::load_tier1(wasm_bytes, module_id, proc_id)?;
    let loader::Tier1Instance { instance, mut store, .. } = tier1;

    // Resolve `_start` — the Phase-1b Tier-1 modules export it as a
    // typed `() -> ()` WASI entry. (It never *returns* —
    // `proc_exit` traps — but the WASM-level signature is `() -> ()`.)
    let start = instance
        .get_typed_func::<(), ()>(&store, "_start")
        .map_err(|_| KernelError::BadWasm)?;

    match start.call(&mut store, ()) {
        Ok(()) => {
            // Returned without calling proc_exit. Phase-1b modules
            // are expected to call proc_exit; this is a protocol
            // violation but not a kernel fault. Treat as exit(0).
            kprintln!("[t1:{}] returned cleanly without proc_exit", proc_id);
            Ok(0)
        }
        Err(e) => {
            if let Some(code) = e.i32_exit_status() {
                kprintln!("[t1:{}] exit({})", proc_id, code);
                Ok(code)
            } else {
                kprintln!("[t1:{}] runtime trap: {:?}", proc_id, e.kind());
                Err(KernelError::BadWasm)
            }
        }
    }
}

/// Phase-0/1a back-compat wrapper: run the embedded hello blob with
/// the default `PROC_ID_TIER1_HELLO`. Retained so PRs that have not
/// yet migrated to the scheduler keep working.
///
/// New callers should go through `sched::run` instead.
#[allow(dead_code)]
pub fn run_tier1_hello() -> Result<(), KernelError> {
    let proc_id = crate::cap::PROC_ID_TIER1_HELLO;
    run_tier1(proc_id, hello_blob::HELLO_WASM, ModuleId::Tier1Hello).map(|_| ())
}
