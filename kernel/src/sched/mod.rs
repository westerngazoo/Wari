// SPDX-License-Identifier: AGPL-3.0-only
//! Phase-1b scheduler — multi-instance process orchestration.
//!
//! The Phase-0/1a kernel ran exactly one Tier-2 driver and one
//! Tier-1 app, both inline in `kmain` as a sequential
//! `run_tier2_uart()` then `run_tier1_hello()` chain. Phase 1b's
//! scheduler abstracts that pattern into a real subsystem and adds
//! the first piece of multi-tenancy: **two Tier-1 instances**
//! running sequentially with **isolated CSpaces**.
//!
//! ## What's actually scheduled
//!
//! - **Tier-2 drivers** are loaded as "libraries" — they expose
//!   exports (e.g., the UART driver's `write`) called from host fns
//!   on Tier-1 tenants' behalf. The scheduler does not pick a
//!   Tier-2 process to "run"; it loads them once during boot and
//!   marks them `Library`.
//! - **Tier-1 tenants** are scheduled. Phase 1b's policy is **run
//!   to completion in registration order** — no preemption, no
//!   blocking, no fuel timer. Each Tier-1 process runs from start
//!   to `proc_exit` (or trap), then the scheduler advances. This
//!   is honest minimum viable: more sophisticated policies land
//!   when there are workloads that need them.
//!
//! ## Cap isolation between Tier-1 instances
//!
//! Each Tier-1 process gets its own:
//!   - `proc_id` (index into `CSPACES`, distinct slots in the
//!     global cap table)
//!   - WASM `Instance` + `Store` (separate linear memory, separate
//!     register state)
//!   - WASI host-fn closures with the proc-specific `proc_id` baked
//!     in (`register_wasi_host_fns(linker, proc_id)` from `wasi.rs`)
//!
//! At runtime each instance's `host_fd_write` consults its own
//! CSpace via `cap::check_cap(proc_id, slot, …)`. The cap layer
//! cannot leak across instances even though they share the same
//! WASM blob.
//!
//! ## Phase-2+ extensions documented at the call site
//!
//! - Multi-instance runqueue with priority / fuel-based preemption
//! - IPC `endpoint_send` / `endpoint_recv` blocking — `Process.state`
//!   gains a `Blocked` variant that pairs with the `Endpoint`
//!   queues from `cap::objects`
//! - Process exit cleanup that walks the exiting process's CSpace
//!   and calls `cap::syscall::cap_revoke` on each held cap

#![allow(dead_code)]
#![allow(clippy::doc_lazy_continuation)]

pub mod process;

pub use process::{Process, ProcessState};

use core::ptr::addr_of_mut;

use crate::cap::{ModuleId, Tier, MAX_PROCS};
use crate::error::KernelError;
use crate::kprintln;
use crate::runtime;

// ─────────────────────────────────────────────────────────────────
// Static process table
// ─────────────────────────────────────────────────────────────────

/// Phase-1b process table — one slot per `proc_id`. The slot at
/// index `i` corresponds to the CSpace at `cspaces()[i]`.
static mut PROCESSES: [Option<Process>; MAX_PROCS] = [const { None }; MAX_PROCS];

/// Mutable accessor for the process table.
///
/// # Safety contract
///
/// - **INV-1** (single-hart kernel): no concurrent access on a
///   single hart.
/// - **INV-8** (post-init access): `PROCESSES` is statically
///   initialized via `const { None }`; first access is during the
///   scheduler's first `register_*` call.
///
/// Callers must not hold the returned reference across another call
/// to this function.
pub fn processes() -> &'static mut [Option<Process>; MAX_PROCS] {
    // SAFETY: INV-1 + INV-8 — single-hart, statically initialized.
    unsafe { &mut *addr_of_mut!(PROCESSES) }
}

// ─────────────────────────────────────────────────────────────────
// Registration
// ─────────────────────────────────────────────────────────────────

/// Register a Tier-2 driver as a "library" process — loaded but not
/// scheduled to run. The driver's `Instance` + `Store` lives in
/// `runtime::tier2_uart` (singleton, INV-14) per Phase-1b's single-
/// driver constraint.
///
/// # Errors
///
/// `KernelError::InvalidArgument` if `proc_id >= MAX_PROCS` or the
/// slot is already occupied.
pub fn register_library(
    proc_id: u8,
    tier: Tier,
    module_id: ModuleId,
) -> Result<(), KernelError> {
    if (proc_id as usize) >= MAX_PROCS {
        return Err(KernelError::InvalidArgument);
    }
    let table = processes();
    if table[proc_id as usize].is_some() {
        return Err(KernelError::InvalidArgument);
    }
    table[proc_id as usize] = Some(Process::new_library(proc_id, tier, module_id));
    Ok(())
}

/// Register a Tier-1 tenant in the `Ready` state. The actual WASM
/// load + execution happens later in `run_tier1`.
pub fn register_tenant(
    proc_id: u8,
    tier: Tier,
    module_id: ModuleId,
) -> Result<(), KernelError> {
    if (proc_id as usize) >= MAX_PROCS {
        return Err(KernelError::InvalidArgument);
    }
    let table = processes();
    if table[proc_id as usize].is_some() {
        return Err(KernelError::InvalidArgument);
    }
    table[proc_id as usize] = Some(Process::new(proc_id, tier, module_id));
    Ok(())
}

// ─────────────────────────────────────────────────────────────────
// Scheduler loop
// ─────────────────────────────────────────────────────────────────

/// Run all `Ready` Tier-1 tenants in `proc_id` order until each
/// has terminated (`Exited` or `Faulted`).
///
/// Phase-1b semantics:
///   - For each `Ready` Tier-1 process in ascending proc_id order:
///       1. Mark it `Running`.
///       2. Call `runtime::run_tier1(proc_id, blob, module_id)`,
///          which loads the embedded blob with that proc_id and
///          runs `_start` to completion.
///       3. On clean exit, mark `Exited(code)`. On wasmi error,
///          mark `Faulted` and continue (other tenants are
///          unaffected — INV-isolation).
///   - Returns when no more `Ready` tenants exist. The caller
///     (`kmain`) typically halts in a wfi loop afterward.
pub fn run() -> Result<(), KernelError> {
    loop {
        // Pick the next runnable tenant, in ascending proc_id order.
        let next_id = pick_next_tenant();
        let proc_id = match next_id {
            Some(id) => id,
            None => return Ok(()), // no tenants left to run
        };

        // Mark Running, run, mark terminated. Three short borrows
        // of `processes()` so we never alias.
        //
        // Each borrow `?`-propagates `KernelError::NoSuchProcess`
        // rather than `.unwrap()`-panicking. `pick_next_tenant`
        // only returns IDs of `Some(Process)` entries, so the
        // `None` arm should be unreachable; using `?` makes R5
        // (no panics in syscall paths) structural rather than
        // implicit. If INV-1 ever breaks or `pick_next_tenant`
        // ever has a bug, the scheduler returns an error and
        // `kmain` halts cleanly instead of panicking.
        {
            let table = processes();
            let proc = table[proc_id as usize]
                .as_mut()
                .ok_or(KernelError::NoSuchProcess)?;
            proc.state = ProcessState::Running;
        }

        kprintln!(
            "[sched] starting Tier-1 instance proc_id={}", proc_id
        );

        // Resolve the module_id from the registered process and
        // dispatch to runtime::run_tier1. Phase 1b only supports
        // ModuleId::Tier1Hello, but the dispatch is shaped to grow.
        let module_id = {
            let table = processes();
            table[proc_id as usize]
                .as_ref()
                .ok_or(KernelError::NoSuchProcess)?
                .module_id
        };
        let blob = blob_for(module_id);
        let result = runtime::run_tier1(proc_id, blob, module_id);

        let final_state = match result {
            Ok(code) => {
                kprintln!(
                    "[sched] Tier-1 instance proc_id={} exited (code={})",
                    proc_id, code
                );
                ProcessState::Exited(code)
            }
            Err(e) => {
                kprintln!(
                    "[sched] Tier-1 instance proc_id={} faulted: {:?}",
                    proc_id, e
                );
                ProcessState::Faulted
            }
        };
        {
            let table = processes();
            let proc = table[proc_id as usize]
                .as_mut()
                .ok_or(KernelError::NoSuchProcess)?;
            proc.state = final_state;
        }
    }
}

/// Wake the process at `proc_id` out of `Blocked` into `Ready`.
///
/// The rendezvous path calls this when a peer's send/recv/reply
/// completes a blocked process's wait; the Phase-2 endpoint-revoke
/// sweep calls it for every waiter queued on a dying endpoint (the
/// no-permanent-block invariant, docs/ipc-design.md §7).
///
/// # Errors
/// - `KernelError::InvalidArgument` — `proc_id` out of range.
/// - `KernelError::NoSuchProcess` — empty slot, or the process is
///   not Blocked ("not in the expected state" per the taxonomy).
///   Waking a Running/Exited process is a caller bug the kernel
///   refuses rather than absorbs; the *idempotent* variant for
///   revoke sweeps is `Process::wake`'s bool return, reached via
///   `processes()` directly.
pub fn wake(proc_id: u8) -> Result<(), KernelError> {
    if (proc_id as usize) >= MAX_PROCS {
        return Err(KernelError::InvalidArgument);
    }
    let table = processes();
    let proc = table[proc_id as usize]
        .as_mut()
        .ok_or(KernelError::NoSuchProcess)?;
    if proc.wake() {
        Ok(())
    } else {
        Err(KernelError::NoSuchProcess)
    }
}

/// Find the lowest `proc_id` whose state is `Ready`. Returns `None`
/// if no `Ready` tenant exists.
fn pick_next_tenant() -> Option<u8> {
    let table = processes();
    for i in 0..MAX_PROCS {
        if let Some(p) = &table[i] {
            if matches!(p.state, ProcessState::Ready) {
                return Some(i as u8);
            }
        }
    }
    None
}

/// Resolve a `ModuleId` to the embedded WASM blob bytes.
///
/// Phase 1b has exactly one Tier-1 module variant in the kernel
/// image: the hello blob. Phase 2+ multi-tenant brings a real
/// module registry indexed by signed manifest hash.
fn blob_for(module_id: ModuleId) -> &'static [u8] {
    match module_id {
        ModuleId::Tier1Hello => runtime::hello_blob::HELLO_WASM,
        // Tier2 drivers are libraries, not run via run_tier1; the
        // match arms should never fire from the scheduler. Returning
        // an empty slice is the safest fallback for unreachable
        // cases (R5: no panics).
        ModuleId::Tier2Uart | ModuleId::Tier2Net => &[],
    }
}
