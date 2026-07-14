// SPDX-License-Identifier: AGPL-3.0-only
//! Resident Tier-1 instance pool + resumable execution (Option B, brick 3a).
//!
//! Until this PR, a Tier-1 instance lived in `sched::run`'s stack
//! frame for exactly one run-to-completion `_start` call. Synchronous
//! IPC breaks that model: a tenant that blocks in `recv`/`call` must
//! be **suspended mid-execution** — its wasmi call stack kept alive —
//! while the scheduler runs someone else, and later resumed with the
//! syscall's return value. This module provides that mechanism:
//!
//! - a static **pool** of live instances (`Store` + `Instance` +
//!   suspended `ResumableInvocation`), one slot per `proc_id`;
//! - `start` / `resume` drivers built on wasmi's resumable calls
//!   (`Func::call_resumable`), the engine-supported way to unwind a
//!   host fn out of WASM without losing the call stack;
//! - the [`IpcBlock`] host-error marker an IPC host fn returns
//!   (`Err(wasmi::Error::host(IpcBlock))`) to request suspension.
//!
//! ## The yield protocol
//!
//! 1. A host fn decides the caller must block (no peer on the
//!    Endpoint). It records the kernel-side state (queue the TcbRef,
//!    `Process::block(...)`) and returns `Err(Error::host(IpcBlock))`.
//! 2. wasmi unwinds to `start`/`resume`, which sees
//!    `ResumableCall::Resumable`, checks the host error really is
//!    [`IpcBlock`] (any other host error faults the tenant), and
//!    parks the invocation in the slot. [`StepOutcome::Blocked`].
//! 3. Later, a peer's rendezvous wakes the process
//!    (`sched::wake`) and stores the syscall's result via
//!    [`set_resume_value`]. The scheduler picks the now-Ready proc
//!    and calls [`resume`], which feeds that value back as the host
//!    fn's return value. Execution continues inside WASM as if the
//!    host fn had returned it synchronously.
//!
//! **Arity contract:** every blockable Wari host fn returns exactly
//! one `i32` — `resume` always feeds `&[Val::I32(pending_resume)]`.
//! A host fn with any other signature must never return `IpcBlock`;
//! if one does, the type mismatch surfaces as a wasmi error and the
//! tenant is faulted (fail-closed, no UB).
//!
//! ## Memory honesty
//!
//! Instances are now resident across their whole lifetime instead of
//! scoped to one scheduler stack frame. Under the Phase-1 bump
//! allocator (`runtime::heap`, no dealloc) peak heap use is
//! **unchanged** — sequential instances were never reclaimed either.
//! `release` drops the `Store` anyway so a future real allocator
//! reclaims without this file changing.

#![allow(dead_code)]

use core::ptr::addr_of_mut;

use wasmi::{Instance, ResumableCall, ResumableInvocation, Store, Val};

use crate::cap::{ModuleId, MAX_PROCS};
use crate::error::KernelError;
use crate::kprintln;
use crate::runtime::loader;
use crate::runtime::wasi::Tier1HostState;

/// Host-error marker: "the calling tenant must block". Returned by
/// IPC host fns as `Err(wasmi::Error::host(IpcBlock))`; recognized
/// by [`start`]/[`resume`] via `Error::downcast_ref::<IpcBlock>()`.
/// Carries no payload — the *reason* lives in the process table
/// (`ProcessState::Blocked { reason, ep_idx }`), written by the
/// host fn before it yields. One source of truth.
#[derive(Debug)]
pub struct IpcBlock;

impl core::fmt::Display for IpcBlock {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("tenant blocked on IPC (suspend requested)")
    }
}

impl wasmi::core::HostError for IpcBlock {}

/// One live Tier-1 instance.
pub struct Tier1Slot {
    /// Per-instance store (host state, linear memory, fuel).
    pub store: Store<Tier1HostState>,
    /// The instantiated module.
    pub instance: Instance,
    /// Suspended `_start` invocation while the tenant is Blocked;
    /// `None` while it is Running (invocation temporarily taken) or
    /// before the first `start`.
    invocation: Option<ResumableInvocation>,
    /// The i32 the blocked host fn will "return" when resumed. Set
    /// by the waker (rendezvous path) via [`set_resume_value`]
    /// before `sched::wake` makes the proc Ready again.
    pending_resume: i32,
}

/// The pool: one slot per `proc_id`, mirroring `sched::PROCESSES`
/// and `cap::CSPACES` indexing.
static mut TIER1_POOL: [Option<Tier1Slot>; MAX_PROCS] = [const { None }; MAX_PROCS];

/// Mutable accessor for the pool.
///
/// # Safety contract
/// INV-1 (single-hart) + INV-8 (statically initialized, first use
/// after boot). Callers must not hold the returned reference across
/// another call — same discipline as `sched::processes()`.
fn pool() -> &'static mut [Option<Tier1Slot>; MAX_PROCS] {
    // SAFETY: INV-1 + INV-8 as documented above.
    unsafe { &mut *addr_of_mut!(TIER1_POOL) }
}

/// What one scheduling step of a tenant produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    /// `_start` finished: clean `proc_exit(code)` or plain return.
    Exited(i32),
    /// The tenant suspended on an [`IpcBlock`] yield; its invocation
    /// is parked in the pool. The host fn has already transitioned
    /// the process to `Blocked` and queued it on its Endpoint.
    Blocked,
    /// wasmi error, non-IpcBlock host error, or protocol violation.
    /// The slot has been released; the scheduler marks Faulted.
    Faulted,
}

/// `true` if `proc_id` has a live (started, unfinished) instance.
pub fn is_live(proc_id: u8) -> bool {
    (proc_id as usize) < MAX_PROCS && pool()[proc_id as usize].is_some()
}

/// Record the value the suspended host fn returns on resume.
/// Called by the rendezvous/waker path *before* `sched::wake`.
pub fn set_resume_value(proc_id: u8, rc: i32) -> Result<(), KernelError> {
    if (proc_id as usize) >= MAX_PROCS {
        return Err(KernelError::InvalidArgument);
    }
    let slot = pool()[proc_id as usize]
        .as_mut()
        .ok_or(KernelError::NoSuchProcess)?;
    slot.pending_resume = rc;
    Ok(())
}

/// Load, instantiate, and run `_start` resumably until it exits,
/// blocks, or faults.
///
/// # Contract
/// - Precondition: `proc_id < MAX_PROCS`, slot not already live,
///   CSpace populated (`cap::boot::init_root_caps`), heap up.
/// - Postcondition: on [`StepOutcome::Blocked`] the slot is live and
///   holds the parked invocation; on `Exited`/`Faulted` the slot is
///   released.
/// - Panics: never (R5).
pub fn start(proc_id: u8, wasm_bytes: &[u8], module_id: ModuleId) -> StepOutcome {
    if (proc_id as usize) >= MAX_PROCS || is_live(proc_id) {
        return StepOutcome::Faulted;
    }
    let tier1 = match loader::load_tier1(wasm_bytes, module_id, proc_id) {
        Ok(t) => t,
        Err(_) => return StepOutcome::Faulted,
    };
    let loader::Tier1Instance { instance, store, .. } = tier1;
    pool()[proc_id as usize] = Some(Tier1Slot {
        store,
        instance,
        invocation: None,
        pending_resume: 0,
    });

    // Borrow the slot back and kick off `_start` resumably.
    // (Two-step insert-then-borrow keeps the Store owned by the
    // static pool for the whole invocation lifetime.)
    let slot = match pool()[proc_id as usize].as_mut() {
        Some(s) => s,
        None => return StepOutcome::Faulted, // unreachable; fail closed
    };
    let start_fn = match slot.instance.get_func(&slot.store, "_start") {
        Some(f) => f,
        None => {
            release(proc_id);
            return StepOutcome::Faulted;
        }
    };
    let call = start_fn.call_resumable(&mut slot.store, &[], &mut []);
    settle(proc_id, call)
}

/// Resume a previously-blocked tenant, feeding `pending_resume` back
/// as the suspended host fn's return value.
///
/// # Contract
/// - Precondition: the slot is live and holds a parked invocation
///   (the process was Blocked and has been woken to Ready).
/// - Postcondition/panics: as [`start`].
pub fn resume(proc_id: u8) -> StepOutcome {
    if (proc_id as usize) >= MAX_PROCS {
        return StepOutcome::Faulted;
    }
    let slot = match pool()[proc_id as usize].as_mut() {
        Some(s) => s,
        None => return StepOutcome::Faulted,
    };
    let inv = match slot.invocation.take() {
        Some(i) => i,
        None => {
            // Resuming a never-blocked instance is a scheduler bug;
            // fail closed rather than re-running `_start`.
            release(proc_id);
            return StepOutcome::Faulted;
        }
    };
    let rc = slot.pending_resume;
    let call = inv.resume(&mut slot.store, &[Val::I32(rc)], &mut []);
    settle(proc_id, call)
}

/// Common tail for [`start`]/[`resume`]: classify the wasmi result,
/// park or release the slot, and translate to a [`StepOutcome`].
fn settle(
    proc_id: u8,
    call: Result<ResumableCall, wasmi::Error>,
) -> StepOutcome {
    match call {
        Ok(ResumableCall::Finished) => {
            // Returned without proc_exit — protocol violation but
            // not a kernel fault (same policy as run_tier1).
            kprintln!("[t1:{}] returned cleanly without proc_exit", proc_id);
            release(proc_id);
            StepOutcome::Exited(0)
        }
        Ok(ResumableCall::Resumable(inv)) => {
            // wasmi wraps ANY host-fn error as a resumable yield
            // when the WASM call stack is non-empty (executor/
            // instrs/call.rs, `ResumableHostError::new` on non-root
            // frames) — including `proc_exit`'s `Error::i32_exit`.
            // Classify exits BEFORE the IpcBlock check, or every
            // clean `proc_exit` reads as an unknown yield and the
            // tenant gets faulted at its own exit.
            if let Some(code) = inv.host_error().i32_exit_status() {
                kprintln!("[t1:{}] exit({})", proc_id, code);
                release(proc_id);
                return StepOutcome::Exited(code);
            }
            if inv.host_error().downcast_ref::<IpcBlock>().is_some() {
                // Legitimate IPC yield: park the invocation. The
                // host fn already blocked the process kernel-side.
                if let Some(slot) = pool()[proc_id as usize].as_mut() {
                    slot.invocation = Some(inv);
                    return StepOutcome::Blocked;
                }
                // Slot vanished mid-call: impossible under INV-1;
                // fail closed.
                StepOutcome::Faulted
            } else {
                // A host error we did not define: not a sanctioned
                // yield. Kill the tenant (fail closed).
                kprintln!(
                    "[t1:{}] unknown host-error yield — faulting tenant",
                    proc_id
                );
                release(proc_id);
                StepOutcome::Faulted
            }
        }
        Err(e) => {
            if let Some(code) = e.i32_exit_status() {
                kprintln!("[t1:{}] exit({})", proc_id, code);
                release(proc_id);
                StepOutcome::Exited(code)
            } else {
                kprintln!("[t1:{}] runtime trap: {:?}", proc_id, e.kind());
                release(proc_id);
                StepOutcome::Faulted
            }
        }
    }
}

/// Drop a tenant's slot (Store, Instance, any parked invocation).
/// Idempotent. Under the bump allocator this reclaims nothing yet,
/// but keeps the drop semantics right for a real allocator later.
pub fn release(proc_id: u8) {
    if (proc_id as usize) < MAX_PROCS {
        pool()[proc_id as usize] = None;
    }
}
<<<<<<< HEAD

/// Flush a woken process's delivered message (`Process::msg_regs`)
/// into its linear memory at the offset it recorded when blocking
/// (`Process::msg_buf`), then clear the record. Called by the
/// scheduler immediately before [`resume`] — the only moment the
/// kernel may safely touch this instance's `Store` (no wasmi frame
/// of this instance can be live: its invocation is parked here).
///
/// No-op if nothing was recorded. A failed write (offset out of
/// bounds — the tenant passed a bogus pointer when it blocked)
/// leaves the buffer unwritten; the resume value already carries
/// the syscall's rc, and a tenant that lied about its own buffer
/// only corrupts its own view (fails closed at the trust boundary).
pub fn flush_msg_to_linmem(proc_id: u8) {
    use crate::sched::process::NO_MSG_BUF;
    let (regs, ptr) = {
        let table = crate::sched::processes();
        match table[proc_id as usize].as_mut() {
            Some(p) if p.msg_buf != NO_MSG_BUF => {
                let out = (p.msg_regs, p.msg_buf);
                p.msg_buf = NO_MSG_BUF;
                out
            }
            _ => return,
        }
    };
    let slot = match pool()[proc_id as usize].as_mut() {
        Some(s) => s,
        None => return,
    };
    let memory = match slot
        .instance
        .get_export(&slot.store, "memory")
        .and_then(|e| e.into_memory())
    {
        Some(m) => m,
        None => return,
    };
    let bytes = crate::ipc::encode_msg(&regs);
    let _ = memory.write(&mut slot.store, ptr as usize, &bytes);
}
=======
>>>>>>> origin/main
