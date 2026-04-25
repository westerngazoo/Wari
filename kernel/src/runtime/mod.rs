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

/// Boot the runtime: load the embedded Tier-1 hello blob, run its
/// `_start` export, observe the `proc_exit` trap, and return.
///
/// # Contract
///
/// - Precondition: `run_tier2_uart` has succeeded (so `tier2_uart`'s
///   singleton is installed; otherwise `fd_write` would `EIO`).
/// - On a clean `proc_exit(code)`, returns `Ok(())` and prints
///   `[hello] exit(code)`.
/// - On any other wasmi error (parse, instantiate, runtime trap),
///   returns `KernelError::BadWasm`.
///
/// # Why catch `i32_exit_status` rather than treat it as an error
///
/// Picked: detect `wasmi::Error::i32_exit_status` and map to `Ok`.
/// Considered: treat any `Error` as a kernel-side `BadWasm` (rejected
/// — `proc_exit` is the *expected* termination path; conflating it
/// with "module failed to validate" loses signal). Why this won:
/// matches WASI semantics. Cost: one extra branch on the error path.
pub fn run_tier1_hello() -> Result<(), KernelError> {
    let tier1 =
        loader::load_tier1(hello_blob::HELLO_WASM, ModuleId::Tier1Hello)?;
    let loader::Tier1Instance { instance, mut store, .. } = tier1;

    // Resolve `_start` — the hello module exports it as a typed
    // `() -> ()` WASI entry. (It never *returns* — `proc_exit` traps —
    // but the WASM-level signature is `() -> ()`.)
    let start = instance
        .get_typed_func::<(), ()>(&store, "_start")
        .map_err(|_| KernelError::BadWasm)?;

    match start.call(&mut store, ()) {
        Ok(()) => {
            // Hello returned without calling proc_exit. This is a
            // protocol violation by the module (Phase 0 hello always
            // calls proc_exit) but not a kernel fault. Log and return
            // Ok — the kernel halts in `kmain`'s wfi loop.
            kprintln!("[hello] returned cleanly without proc_exit");
            Ok(())
        }
        Err(e) => {
            if let Some(code) = e.i32_exit_status() {
                kprintln!("[hello] exit({})", code);
                Ok(())
            } else {
                kprintln!("[hello] runtime trap: {:?}", e.kind());
                Err(KernelError::BadWasm)
            }
        }
    }
}
