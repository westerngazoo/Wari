// SPDX-License-Identifier: AGPL-3.0-only
//! Tier-0 WASM runtime — `wasmi` embedding for Phase 0.
//!
//! Phase 0b scope (this PR):
//!   - Pin `wasmi = "=0.32.0"` with `default-features = false`.
//!   - Hand-rolled bump allocator (`heap.rs`) backs `#[global_allocator]`
//!     so wasmi's internal `Vec`/`Box`/`String` can land somewhere.
//!   - Instantiate a minimal hand-encoded `.wasm` (`noop_blob.rs`) via
//!     `engine.rs` to prove the runtime links and validates cleanly.
//!
//! Out of scope: Tier-1 PID 1, host functions, scheduling, IPC. Those
//! land in PR 5+ per the approved Phase-0 plan.
//!
//! R2 note: the bump allocator is "heap" but is initialized in
//! `mem::kvm::init` and exercised here from `kmain` (boot context, not
//! trap/dispatch). No syscall path allocates. When wasmi internally
//! allocates during `Module::new` / `Linker::instantiate`, that work
//! happens once at boot before traps are taken.
//!
//! R5: every wasmi error folds into `KernelError::BadWasm`. No panics.

pub mod engine;
pub mod heap;
pub mod host_fns;
pub mod loader;
pub mod noop_blob;
pub mod sign;
pub mod uart_blob;

use crate::cap::ModuleId;
use crate::error::KernelError;

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

/// Boot the runtime: load the embedded signed Tier-2 UART driver.
///
/// Performs (in order): signature verification (INV-13), wasmi
/// `Module::new`, host-fn registration, instantiate, ensure-no-start.
/// On success, the loaded `Tier2Instance` is dropped at the end of
/// scope — Phase 0 has no proc table to keep it in. The driver's
/// internal state (linear memory, host imports binding) survives only
/// as long as the kernel needs to prove it loaded; PR 6 wires Tier-1
/// hello calls into it.
///
/// # Errors
///
/// `KernelError::BadWasm` for any verification, parse, link, or
/// instantiate failure (R5).
pub fn run_tier2_uart() -> Result<(), KernelError> {
    let _instance =
        loader::load_tier2(uart_blob::UART_DRIVER_SIGNED, ModuleId::Tier2Uart)?;
    Ok(())
}
