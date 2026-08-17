// SPDX-License-Identifier: AGPL-3.0-only
//! The kernel audit stream — static ring + emit sites.
//!
//! One concern: hold the [`wari_events`] ring in kernel memory and
//! give kernel code one call, [`emit`], to append to it. The *shape*
//! of records and all ring arithmetic live in the pure `wari-events`
//! crate (host-tested; guard agents decode with the same crate).
//! This file is only the impure glue: two statics and their access
//! discipline.
//!
//! ## Who reads it
//!
//! Today: [`boot_summary`], printed once when the scheduler drains
//! (so QEMU integration greps can assert the stream exists and
//! counts what it should). Next brick: a cap-gated host fn hands
//! records to guard agents — resident security daemons parked on
//! this stream. The ring is the guards' sensory organ; it exists
//! before they do so they are born with something to watch.
//!
//! ## Emit-context rule (read before adding an emit site)
//!
//! `emit` is callable from boot, host-fn, and scheduler context —
//! NOT from the trap handler. The INV-1 amendment's handler rules
//! apply: handlers touch nothing another kernel path mutates, and
//! this ring is mutated by all of the above. When an interrupt-
//! context event source appears (brick: deferred signal delivery),
//! it records into the handler-owned pending word and the drain
//! point emits on its behalf.

#![allow(dead_code)]

use core::ptr::{addr_of, addr_of_mut};

use wari_events::{Event, EventKind, RingState, RING_CAPACITY};

use crate::kprintln;

/// The record array. 2 KiB of `.bss`; slots are claimed strictly by
/// `STATE` so a torn record is impossible under INV-1.
static mut RING: [Event; RING_CAPACITY] = [Event {
    seq: 0,
    kind: 0,
    a: 0,
    b: 0,
}; RING_CAPACITY];

/// Writer state (the pure crate's `RingState`).
static mut STATE: RingState = RingState { next_seq: 0 };

/// Maximum processes that can block on the audit stream at once.
///
/// Sized to `MAX_PROCS`: at most one waiter per process, so the set
/// can hold every process at once and [`register_waiter`] can never
/// return full. That is what makes the "full → degrade to polling"
/// fallback in `event_read` structurally unreachable rather than a
/// latent busy-spin — with no fuel or preemption configured
/// (`Engine::default`), a spinning fallback would wedge the hart, so
/// the set must be large enough that it is never hit. Cost: 16 bytes
/// of `.bss`. (Adversarial review, wf_3bbbda42: the earlier size-4
/// set made the fallback reachable and its "never wedged" comment a
/// lie; this closes it.)
const MAX_RING_WAITERS: usize = crate::cap::MAX_PROCS;

/// Proc-ids currently blocked in `event_read` with the stream caught
/// up. Registered by [`register_waiter`] when a guard finds
/// [`ReadResult::UpToDate`] and yields; drained by [`wake_ring_waiters`]
/// on the next [`emit`]. `None` = free slot.
static mut RING_WAITERS: [Option<u8>; MAX_RING_WAITERS] = [None; MAX_RING_WAITERS];

/// Register `proc_id` as blocked on the audit stream.
///
/// Idempotent: a proc-id already present is not duplicated (a guard
/// cannot be blocked twice, but re-registering after a spurious wake
/// must not overflow the set). Returns `false` if the set is full —
/// the caller then must NOT block (it would sleep unwakeably), and
/// `event_read` falls back to reporting up-to-date instead.
///
/// # Safety context
///
/// INV-1 (single hart) + the emit-context rule: registration runs
/// only from a Tier-1 host-fn call (`event_read`), never from an
/// interrupt handler, so the static is never concurrently mutated.
pub fn register_waiter(proc_id: u8) -> bool {
    // SAFETY: INV-1 + INV-8 — single-hart, non-interrupt mutation of a
    // boot-valid static.
    unsafe {
        let w = &mut *addr_of_mut!(RING_WAITERS);
        if w.iter().any(|s| *s == Some(proc_id)) {
            return true;
        }
        for slot in w.iter_mut() {
            if slot.is_none() {
                *slot = Some(proc_id);
                return true;
            }
        }
    }
    false
}

/// Wake every guard blocked on the stream: hand each `event_read`'s
/// suspended call the resume value 1 (`UpToDate`) and mark it Ready,
/// so it re-runs `event_read`, now finds the freshly-emitted record,
/// and drains forward until caught up again. Called at the tail of
/// [`emit`].
///
/// The resume value is deliberately `UpToDate` rather than the record
/// itself: the record lives in the ring, not in the waker's reach
/// (the waker has no handle on the guard's linear memory). The guard's
/// own next `event_read` call reads and writes it — the same
/// "wake, then re-read" shape the IPC reply path uses.
///
/// # INVARIANT — caller must not hold `&mut TIER1_POOL` or `&mut
/// PROCESSES`
///
/// This reaches into **both** `tier1_pool` (`set_resume_value`) and the
/// scheduler process table (`sched::wake`). Every caller of [`emit`]
/// today satisfies this: `sched::run`'s exit/fault arms emit *between*
/// closed borrow scopes, and `modreg::spawn` emits only during boot
/// (before `sched::run`). Single-hart makes those the only contexts.
///
/// The trap this guards against: a host fn that emits an event while a
/// `&mut TIER1_POOL` is held live up-stack — which is exactly the state
/// inside `tier1_pool::{start,resume}` across `call_resumable`. The
/// named future callers that would do this are the **MCP `spawn` tool**
/// and the **`wari-http` upload path** (a tenant asking the kernel to
/// stage/spawn a module, emitting `ModuleStaged`/`SpawnVerified` from
/// inside its own resumable frame). The re-borrow of `TIER1_POOL` there
/// is undefined behavior. **Before either brick lands, wakes must be
/// deferred to the scheduler drain point** (the pattern this module's
/// header already prescribes for interrupt sources) so `emit` again
/// only touches the ring. Recorded by adversarial review wf_3bbbda42.
fn wake_ring_waiters() {
    // SAFETY: INV-1 + INV-8, as above.
    let waiters = unsafe {
        let w = &mut *addr_of_mut!(RING_WAITERS);
        core::mem::replace(w, [None; MAX_RING_WAITERS])
    };
    for pid in waiters.into_iter().flatten() {
        // Order matters: the resume value must be in place before the
        // scheduler is allowed to pick the now-Ready process.
        let _ = crate::runtime::tier1_pool::set_resume_value(pid, 1);
        let _ = crate::sched::wake(pid);
    }
}

/// Append one record to the audit stream.
///
/// # Contract
///
/// - Precondition: called from boot / host-fn / scheduler context,
///   never from the trap handler (module docs).
/// - Postcondition: the record is in the ring with a fresh monotonic
///   sequence number; the oldest record may have been overwritten
///   (lossy-oldest by design — see `wari-events` for why an audit
///   ring must never refuse NEW events).
/// - Panics: never.
pub fn emit(kind: EventKind, a: u16, b: u32) {
    // SAFETY: INV-1 (single hart) + the emit-context rule above make
    // this the only mutator running; INV-8 — statics are
    // zero-initialized and valid from boot.
    unsafe {
        let (slot, seq) = (*addr_of_mut!(STATE)).claim();
        (*addr_of_mut!(RING))[slot] = Event {
            seq,
            kind: kind as u16,
            a,
            b,
        };
    }
    // A record now exists past every blocked guard's cursor; wake them
    // so they drain it. No-op (one branch-predictable scan) when no
    // guard is blocked. MUST run in emit-context (scheduler/boot), not
    // inside a tenant's own resumable frame — the same precondition the
    // emit-context rule already imposes.
    wake_ring_waiters();
}

/// Total events ever recorded (== the writer's next sequence).
pub fn recorded() -> u64 {
    // SAFETY: INV-1 read of a kernel-owned static.
    unsafe { (*addr_of!(STATE)).next_seq }
}

/// Status returned to a reader (guard agent) alongside an optional
/// record. Mirrors `wari_events::ReadPlan` but flattened for the
/// host-fn ABI: the WASM side gets a small integer and a 16-byte
/// buffer, not a Rust enum.
pub enum ReadResult {
    /// No new record; nothing written. WASM status 1.
    UpToDate,
    /// A record is available; caller advances to `event.seq + 1`.
    /// WASM status 0.
    Record(Event),
    /// The reader lagged; `event` is the OLDEST still-available
    /// record. The WASM side computes the gap as
    /// `event.seq - its_cursor`. WASM status 2.
    Lagged(Event),
}

/// Read the record a guard agent's `cursor` points at.
///
/// Pure with respect to the ring — reads only, never advances the
/// writer. The cap gate lives at the host-fn call site
/// (`ObjectKind::EventLog` + READ); this function assumes authority
/// was already checked.
///
/// # Contract
/// - `cursor` is the next sequence the reader wants.
/// - Returns [`ReadResult::UpToDate`] if the reader is caught up (or
///   ahead — a corrupt cursor is not trusted, per `wari_events`).
/// - Panics: never.
pub fn read_at(cursor: u64) -> ReadResult {
    // SAFETY: INV-1 read of kernel-owned statics; no concurrent
    // writer under single-hart + the emit-context rule.
    let (next_seq, ring) = unsafe { ((*addr_of!(STATE)).next_seq, &*addr_of!(RING)) };
    match wari_events::read_plan(cursor, next_seq) {
        wari_events::ReadPlan::UpToDate => ReadResult::UpToDate,
        wari_events::ReadPlan::Read { slot, .. } => ReadResult::Record(ring[slot]),
        wari_events::ReadPlan::Lagged { resume_seq, .. } => {
            // resume_seq is the oldest surviving record's sequence.
            let slot = (resume_seq as usize) & (RING_CAPACITY - 1);
            ReadResult::Lagged(ring[slot])
        }
    }
}

/// One-line boot-trace summary plus the tail of the stream, printed
/// when the scheduler drains. This is the integration test's
/// observation point until the guard-agent read path lands, and it
/// stays useful after — an operator on the serial console sees the
/// audit tail without any tooling.
pub fn boot_summary() {
    let total = recorded();
    kprintln!("[events] {} recorded, showing last {}:", total, total.min(8));
    let mut cursor = total.saturating_sub(8);
    // SAFETY: INV-1 read; the writer is not running concurrently.
    let (state_seq, ring) = unsafe { ((*addr_of!(STATE)).next_seq, &*addr_of!(RING)) };
    while let wari_events::ReadPlan::Read { slot, seq } = wari_events::read_plan(cursor, state_seq)
    {
        let e = ring[slot];
        let kind_str = match EventKind::from_raw(e.kind) {
            Some(EventKind::ModuleStaged) => "module-staged",
            Some(EventKind::SpawnVerified) => "spawn-verified",
            Some(EventKind::SpawnRejected) => "SPAWN-REJECTED",
            Some(EventKind::TenantExited) => "tenant-exited",
            Some(EventKind::TenantFaulted) => "TENANT-FAULTED",
            Some(EventKind::GrantAttenuated) => "GRANT-ATTENUATED",
            Some(EventKind::DaemonRestarted) => "daemon-restarted",
            Some(EventKind::DaemonGaveUp) => "DAEMON-GAVE-UP",
            None => "unknown-kind",
        };
        kprintln!("[events]   #{} {} a={} b={}", seq, kind_str, e.a, e.b);
        cursor = seq + 1;
    }
}
