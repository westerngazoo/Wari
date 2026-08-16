// SPDX-License-Identifier: AGPL-3.0-only
//! Module registry — runtime staging and spawning of Tier-1 WASM.
//!
//! One concern: turn *bytes that arrived at runtime* into a running,
//! minimally-privileged Tier-1 tenant. This is the spine of the
//! Phase-2 cloud-OS story (`CLAUDE.md` roadmap `p2a`): until this
//! module, every instance Wari ever ran was `include_bytes!`-baked
//! into the kernel image at compile time.
//!
//! ## Lifecycle
//!
//! ```text
//! stage(bytes)  — copy a signed envelope into a free registry slot
//!      │           (bounded static store; no allocation)
//!      ▼
//! spawn(slot)   — verify the ed25519 envelope (INV-11/INV-13 gate),
//!      │           allocate a dynamic proc_id, install the MINIMAL
//!      │           Tier-1 cap set, register with the scheduler
//!      ▼
//! scheduler runs it exactly like a baked tenant (same pool, same
//! resumable-suspend machinery); exit reaps it normally
//! ```
//!
//! ## Authority story (read this before adding a cap)
//!
//! A dynamic tenant gets the baseline (stdout + exit) plus exactly the
//! optional authorities the **Supervisor grants** — `requested &
//! ceiling`, computed in [`spawn_with_grants`] against
//! `wari_cap::grants` (agents brick 4, the AI-layer design in
//! `docs/ai-os-assistant-design.md`). A module cannot obtain authority
//! the ceiling withholds, however it asks: attenuation is monotone
//! (INV-10 at the grant point). The self-test demonstrates all three
//! shapes: a plain tenant (no optional authority), a guard (requests
//! and receives `EVENTLOG`), and a greedy module (requests
//! `EVENTLOG + NET`, receives only `EVENTLOG` — `NET` is denied and a
//! `GrantAttenuated` audit record names the denial for the guard to
//! see). Same binary, different authority, decided by policy.
//!
//! ## What this slice is NOT
//!
//! No transport (upload arrives with `wari-http`), no reclamation of
//! registry slots after tenant exit, no per-tenant memory quotas, no
//! MCP `spawn` tool yet. Those are the next bricks; this one proves
//! verify→spawn→run→reap at runtime.

#![allow(dead_code)]

use core::ptr::{addr_of, addr_of_mut};

use crate::cap::grants::GrantSpec;
use crate::cap::{ModuleId, Tier};
use crate::error::KernelError;
use crate::runtime::sign;
use crate::{kprintln, sched};

/// Registry capacity. Four runtime modules is plenty for the demo
/// era; the constant is the single knob.
pub const DYN_SLOTS: usize = 4;

/// Per-slot byte budget for a staged envelope. The hello envelope is
/// ~3 KiB; the signed net driver is ~120 KiB. 256 KiB leaves headroom
/// without making `.bss` silly (total: 1 MiB, zeroed at boot).
pub const MAX_MODULE_BYTES: usize = 256 * 1024;

/// First proc_id the registry may hand out. 0–4 are boot-assigned
/// (`cap::boot`); everything from here to `MAX_PROCS` is dynamic.
pub const PROC_ID_DYN_BASE: u8 = 5;

/// Slot lifecycle. `Live` records where the verified payload sits
/// inside the staged envelope so `payload()` needs no re-verify.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotState {
    Free,
    /// Envelope staged, not yet verified. `len` = envelope bytes.
    Staged { len: usize },
    /// Verified and registered. Payload = `store[off .. off+len]`.
    Live { off: usize, len: usize, proc_id: u8 },
}

/// Backing store for staged envelopes. Static, bounded, zero-alloc —
/// uploads must never touch the bump arena (INV-12: it can't free, so
/// repeated uploads would leak it dry).
static mut STORE: [[u8; MAX_MODULE_BYTES]; DYN_SLOTS] = [[0; MAX_MODULE_BYTES]; DYN_SLOTS];

/// Slot states, parallel to `STORE`.
static mut SLOTS: [SlotState; DYN_SLOTS] = [SlotState::Free; DYN_SLOTS];

/// Stage a signed module envelope into a free slot.
///
/// # Contract
///
/// - Precondition: called outside interrupt context (boot or host-fn
///   context; R2 forbids neither here, but handlers must not call
///   this — it does bulk copies).
/// - Postcondition: on `Ok(slot)`, the bytes are copied into the
///   registry and the slot is `Staged`. Nothing is verified yet.
/// - Errors: `InvalidArgument` if `bytes` is empty or exceeds
///   [`MAX_MODULE_BYTES`]; `OutOfHandles` if no slot is free.
/// - Panics: never.
pub fn stage(bytes: &[u8]) -> Result<u8, KernelError> {
    if bytes.is_empty() || bytes.len() > MAX_MODULE_BYTES {
        return Err(KernelError::InvalidArgument);
    }
    // SAFETY: INV-1 (single-hart) + INV-8 (post-init singleton): the
    // registry statics are mutated only from boot/host-fn context,
    // never from an interrupt handler, and only one hart runs kernel
    // code.
    unsafe {
        let slots = &mut *addr_of_mut!(SLOTS);
        let store = &mut *addr_of_mut!(STORE);
        for (i, s) in slots.iter_mut().enumerate() {
            if *s == SlotState::Free {
                store[i][..bytes.len()].copy_from_slice(bytes);
                *s = SlotState::Staged { len: bytes.len() };
                crate::runtime::events::emit(
                    wari_events::EventKind::ModuleStaged,
                    i as u16,
                    bytes.len() as u32,
                );
                return Ok(i as u8);
            }
        }
    }
    Err(KernelError::OutOfHandles)
}

/// The Supervisor's grant ceiling for a runtime-loaded module: the
/// most optional authority any dynamic tenant may hold, whatever it
/// requests. Today that is observation of the audit stream (a module
/// may be a guard) and nothing else — outward authority like `NET`
/// requires an attestation the signing pipeline does not yet produce,
/// so the ceiling withholds it and a module that requests `NET` is
/// attenuated down. This constant *is* the policy; widening it is a
/// deliberate Supervisor decision, not an incidental edit.
const DYNAMIC_CEILING: GrantSpec = GrantSpec::EVENTLOG;

/// Verify a staged slot and bring it up as a plain tenant (no optional
/// authority — stdout + exit only). See [`spawn_with_grants`].
pub fn spawn(slot: u8) -> Result<u8, KernelError> {
    spawn_with_grants(slot, GrantSpec::EMPTY)
}

/// Verify a staged slot and bring it up requesting `requested`
/// authority. Thin wrapper preserved for call sites that name a class
/// of module rather than a bitset.
pub fn spawn_as(slot: u8, requested: GrantSpec) -> Result<u8, KernelError> {
    spawn_with_grants(slot, requested)
}

/// Verify a staged slot, attenuate its `requested` authority against
/// the Supervisor ceiling, and bring it up as a Tier-1 tenant holding
/// exactly the granted subset.
///
/// This is the Supervisor's grant point (agents brick 4). Two gates,
/// in order:
///   1. **Signature** (INV-11/INV-13): the ed25519 envelope is checked
///      before any byte is treated as a module. A bad signature emits
///      `SpawnRejected`, returns `BadWasm`, registers nothing.
///   2. **Attenuation** (INV-10 at the grant boundary): the module
///      receives `requested & DYNAMIC_CEILING` — never more than it
///      asked for, never more than policy allows. If the ceiling
///      withheld anything, a `GrantAttenuated` audit record names the
///      denied bits, so a guard sees a module reach for authority it
///      did not get.
///
/// # Contract
///
/// - Precondition: `slot` was returned by [`stage`] and not yet
///   spawned; scheduler and cap system are initialized.
/// - Postcondition: on `Ok(pid)`, the tenant is `Ready` holding
///   stdout + exit plus the *granted* (attenuated) optional
///   authorities, and runs on the next `sched::run` pass.
/// - Errors: `InvalidArgument` (bad slot / not staged), `BadWasm`
///   (signature), `OutOfHandles` (no free proc_id), plus anything
///   `sched::register_tenant` returns.
/// - Panics: never.
pub fn spawn_with_grants(slot: u8, requested: GrantSpec) -> Result<u8, KernelError> {
    let idx = slot as usize;
    if idx >= DYN_SLOTS {
        return Err(KernelError::InvalidArgument);
    }
    // SAFETY: INV-1 + INV-8, as in `stage`.
    let env_len = unsafe {
        match (*addr_of!(SLOTS))[idx] {
            SlotState::Staged { len } => len,
            _ => return Err(KernelError::InvalidArgument),
        }
    };

    // Verify the envelope in place. The payload slice borrows the
    // static store, so offsets derived from it remain valid for the
    // module's whole life — the store slot is never rewritten while
    // `Live` (state machine above; enforced by the `Staged` check).
    // SAFETY: INV-8 — shared read of a slot no writer touches while
    // Staged→Live transition runs (single-hart, INV-1).
    let (payload_off, payload_len) = {
        let envelope: &[u8] = unsafe { &(&*addr_of!(STORE))[idx][..env_len] };
        let payload = match sign::verify(envelope) {
            Ok(p) => p,
            Err(e) => {
                // The record every guard agent exists to see: bytes
                // arrived, claimed to be a module, and failed the
                // signature gate.
                crate::runtime::events::emit(wari_events::EventKind::SpawnRejected, slot as u16, 0);
                return Err(e);
            }
        };
        // Offset of the payload inside the envelope, computed from
        // the returned subslice — no duplicated envelope-layout math
        // (INV-24 spirit: one source of truth, sign.rs owns the
        // format).
        let off = payload.as_ptr() as usize - envelope.as_ptr() as usize;
        (off, payload.len())
    };

    let proc_id = alloc_proc_id()?;
    // Attenuate: the module gets requested ∩ ceiling — never more.
    let granted = requested.attenuate(DYNAMIC_CEILING);
    crate::cap::boot::install_tier1_grants(proc_id, granted)?;
    // Surface any withheld authority to the audit stream BEFORE the
    // SpawnVerified record, so a guard sees the denial attributed to
    // this proc_id in causal order. `b` carries the denied bits.
    let denied = requested.denied(granted);
    if !denied.is_empty() {
        crate::runtime::events::emit(
            wari_events::EventKind::GrantAttenuated,
            proc_id as u16,
            denied.bits(),
        );
    }
    sched::register_tenant(proc_id, Tier::One, ModuleId::Dynamic(slot))?;

    // SAFETY: INV-1 + INV-8.
    unsafe {
        (*addr_of_mut!(SLOTS))[idx] = SlotState::Live {
            off: payload_off,
            len: payload_len,
            proc_id,
        };
    }
    crate::runtime::events::emit(
        wari_events::EventKind::SpawnVerified,
        slot as u16,
        proc_id as u32,
    );
    Ok(proc_id)
}

/// The verified module bytes for a `Live` slot.
///
/// Called by the scheduler's `blob_for` when it instantiates
/// `ModuleId::Dynamic(slot)`. Returns the empty slice for any slot
/// that is not `Live` — same refuse-by-default shape as `blob_for`'s
/// unreachable arms (R5: no panics on the scheduler path).
pub fn payload(slot: u8) -> &'static [u8] {
    let idx = slot as usize;
    if idx >= DYN_SLOTS {
        return &[];
    }
    // SAFETY: INV-1 + INV-8; `Live` slots are never rewritten, so the
    // returned 'static borrow cannot be invalidated by a later stage()
    // (stage only writes `Free` slots).
    unsafe {
        match (*addr_of!(SLOTS))[idx] {
            SlotState::Live { off, len, .. } => &(&*addr_of!(STORE))[idx][off..off + len],
            _ => &[],
        }
    }
}

/// Lowest unused proc_id in the dynamic range.
fn alloc_proc_id() -> Result<u8, KernelError> {
    let table = sched::processes();
    for pid in (PROC_ID_DYN_BASE as usize)..table.len() {
        if table[pid].is_none() {
            return Ok(pid as u8);
        }
    }
    Err(KernelError::OutOfHandles)
}

/// Boot self-test: push the signed hello envelope through the full
/// runtime path — stage → verify → cap install → register — as if it
/// had been uploaded. The spawned tenant is the SAME binary the baked
/// tenants run, but holds only stdout+exit; its IPC and socket calls
/// are refused by the cap system and it exits cleanly. One boot line
/// proves the cloud-OS spine and the minimal-authority default at
/// once.
///
/// Removed when the `wari-http` upload path becomes the caller — the
/// test then moves to `tests/integration/`.
pub fn self_test_spawn() -> Result<u8, KernelError> {
    // Adversarial half first (tests/security discipline: a
    // trust-boundary feature ships WITH its attack, not before it).
    // Flip one bit in the payload region of a copy of the signed
    // envelope; spawn MUST refuse with BadWasm and register nothing.
    // The corrupted slot deliberately stays Staged — reclamation is
    // out of scope this slice — costing one of four demo slots.
    {
        let good = crate::runtime::hello_signed_blob::HELLO_SIGNED;
        let mut tampered = [0u8; MAX_MODULE_BYTES / 32];
        if good.len() <= tampered.len() {
            tampered[..good.len()].copy_from_slice(good);
            // Past the 96-byte header: corrupt module bytes, not the
            // signature fields, so this exercises the hash path.
            tampered[good.len() - 1] ^= 0x01;
            let t_slot = stage(&tampered[..good.len()])?;
            match spawn(t_slot) {
                Err(KernelError::BadWasm) => {
                    kprintln!("[modreg] tamper check: corrupted envelope rejected");
                }
                Ok(pid) => {
                    kprintln!("[modreg] SECURITY FAILURE: tampered module spawned as {}", pid);
                    return Err(KernelError::BadWasm);
                }
                Err(e) => {
                    kprintln!("[modreg] tamper check: unexpected error {:?}", e);
                    return Err(e);
                }
            }
        }
    }

    // Spawn the guard agent FIRST so it is already watching when the
    // tenant below is staged and spawned — its own spawn, and the
    // tenant's, appear in the stream it reads.
    let guard_slot = stage(crate::runtime::hello_signed_blob::GUARD_SIGNED)?;
    let guard_pid = spawn_as(guard_slot, GrantSpec::EVENTLOG)?;
    kprintln!("[modreg] guard agent spawned as pid {}", guard_pid);

    let slot = stage(crate::runtime::hello_signed_blob::HELLO_SIGNED)?;
    let pid = spawn(slot)?;
    kprintln!(
        "[modreg] staged slot {} ({} bytes) -> verified -> spawned pid {}",
        slot,
        crate::runtime::hello_signed_blob::HELLO_SIGNED.len(),
        pid,
    );

    // Attenuation demo (Supervisor grants): stage the same hello binary
    // but request MORE than the ceiling allows — EventLog AND Net. The
    // Supervisor grants EventLog (within the ceiling) and DENIES Net,
    // emitting a GrantAttenuated record the guard above prints. A module
    // cannot obtain authority policy withholds, however it asks.
    let greedy_slot = stage(crate::runtime::hello_signed_blob::HELLO_SIGNED)?;
    let requested = GrantSpec::EVENTLOG.with(GrantSpec::NET);
    let greedy_pid = spawn_with_grants(greedy_slot, requested)?;
    kprintln!(
        "[modreg] grant: pid {} requested EVENTLOG+NET, ceiling denied NET",
        greedy_pid,
    );
    Ok(pid)
}
