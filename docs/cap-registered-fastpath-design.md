<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Wari — Registered Capabilities + Submission Ring (Cap-System Fast Path)

> **Status:** Design proposal (Phase 2+). Extends
> [`cap-system-design.md`](cap-system-design.md) with the "fastest-safe"
> execution mechanism from
> [`ai-os-assistant-design.md`](ai-os-assistant-design.md) §4. Per the
> Co-Architect Protocol the design is **Gustavo's call**; this is the
> concrete-data-structures proposal that turns §4.2–4.4 into kernel code.
>
> **One sentence:** prove a capability **once** at registration and cache
> the resolved object behind a small index, so the hot path is an O(1)
> bounds + generation check instead of a full CSpace walk — amortizing the
> capability check without ever skipping it, with revocation made atomic
> for free by the existing generation counter (INV-17).

---

## 1 · Goals and non-goals

### Goals
- Give trusted Tier-2 modules (the AI assistant, drivers) a syscall path
  whose hot-path cost is ~an array index, not a capability resolution.
- Batch many operations per kernel entry (one trap / N ops).
- Add **zero** new ways to forge or escalate authority; reuse the
  generation-counter / rights / forgery machinery already specified.
- Keep every executed operation capability-checked.

### Non-goals
- A new isolation tier (this rides Tier 2; see ai-os-assistant §2).
- Raw/unchecked syscalls (the "rung 0" line not to cross).
- SMP concurrency on the ring (INV-1 single-hart holds for now; §9 notes
  the Phase-2 SMP revisit).
- The AOT compiler (separate, [`wasm-jit-design.md`](wasm-jit-design.md)).

---

## 2 · Why the current path is slow

Today a syscall that uses a capability does, every call:
1. bounds-check the CSpace slot index (INV-18),
2. read the `Cap`, check `kind`, check `rights`,
3. resolve `pool_index` → the kernel object,
4. (for derived caps) validate the chain / generation (INV-16/17),
5. execute.

Steps 1–4 repeat on **every** call even when the same socket/frame is
used a million times. For a module issuing a high rate of syscalls (the
assistant's control loop), that revalidation dominates. The fix is to do
1–4 **once** and cache the result behind a cheap handle — without losing
the guarantees 1–4 provide.

---

## 3 · The registered-resource table

A new per-process structure, sibling to `CSpace`. Small and fixed so the
hot-path lookup is bounded and branch-predictable.

```rust
/// Per-process registered-resource table. 64 slots keeps it to one
/// cache-friendly page-fraction and bounds the hot-path index range.
pub const REG_SLOTS: usize = 64;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RegEntry {
    /// The capability proven at registration time. `CapId::ROOT`-style
    /// sentinel / `ObjectKind::Empty` marks a free slot.
    pub cap_id: CapId,        // 4 bytes — names the originating cap slot
    pub kind: ObjectKind,     // 1 byte  — cached for the op-permitted check
    pub rights: u8,           // 1 byte  — cached rights bitmap
    /// Generation of the originating cap slot AT REGISTRATION. The
    /// hot path compares this against the slot's *current* generation;
    /// any revoke/delete/reuse bumps the slot generation (INV-17) and
    /// this entry auto-invalidates. This is the whole revocation story.
    pub reg_generation: u16,  // 2 bytes
    /// Resolved per-kind pool index, cached so the hot path skips the
    /// Cap→object resolution (step 3 above).
    pub pool_index: u16,      // 2 bytes
    // 2 bytes pad → 12 bytes/entry; 64 × 12 = 768 B/table.
}

#[repr(C)]
pub struct RegTable {
    pub slots: [RegEntry; REG_SLOTS],
}
```

A **registered handle** (`reg_idx: u32`) is just the index into
`slots`. It is **not** a capability and confers no authority on its own —
it only *names* a cap the kernel already proved. An out-of-range, empty,
or generation-stale `reg_idx` resolves to nothing and is rejected.

---

## 4 · Registration / unregistration

Two new syscalls, in the cap-system ABI family (alongside `SYS_CAP_MINT`
etc. in [`cap-system-design.md`](cap-system-design.md) §5).

### `SYS_CAP_REGISTER(cspace_slot) -> reg_idx | -errno`
1. Resolve `cspace_slot` through the **full** path (§2 steps 1–4): bounds
   (INV-18), kind, rights, pool resolve, chain/generation (INV-16/17).
2. Allocate a free `RegEntry`; cache `{cap_id, kind, rights,
   reg_generation = current slot generation, pool_index}`.
3. Return `reg_idx`. Fails closed: bad slot / revoked cap / table full →
   negative errno, no entry created.

This is where the cost lives — paid **once** per resource, not per use.

### `SYS_CAP_UNREGISTER(reg_idx) -> 0 | -errno`
Mark the entry `Empty`. Idempotent on already-empty. Does **not** affect
the underlying cap (that's `SYS_CAP_DELETE`/`REVOKE`); it only drops the
fast handle.

> **Registration grants no new authority.** It is a *cache* of an
> existing cap's resolution. You can only register a cap you already hold
> in your CSpace with sufficient rights.

---

## 5 · The submission / completion ring

A shared-memory pair of queues living in the **module's own linear
memory** (so a corrupt ring harms only its owner), registered once via
`SYS_RING_SETUP`.

### 5.1 Layout
```rust
/// Submission queue entry. 32 bytes, 8-byte aligned.
#[repr(C)]
pub struct Sqe {
    pub op: u32,         // operation code (NET_SEND, FRAME_READ, …)
    pub reg_idx: u32,    // registered-resource handle (§3)
    pub flags: u32,
    pub _pad: u32,
    pub user_data: u64,  // opaque, echoed in the Cqe for correlation
    pub arg0: u64,       // op-specific (e.g. linmem offset of a buffer)
    pub arg1: u64,       // op-specific (e.g. length)
}

/// Completion queue entry. 16 bytes.
#[repr(C)]
pub struct Cqe {
    pub user_data: u64,  // copied from the matching Sqe
    pub result: i64,     // >= 0 success / negative errno
}
```
Plus a small control header (SQ head/tail, CQ head/tail) — single-hart,
so plain indices, no atomics (INV-1).

### `SYS_RING_SETUP(sq_ptr, cq_ptr, entries) -> 0 | -errno`
Validate `sq_ptr`/`cq_ptr`/sizes lie wholly inside the caller's linear
memory; record the (kernel-side) ring descriptor. One-time.

### `SYS_RING_SUBMIT(n) -> processed | -errno`
Drain up to `n` SQEs. For **each** entry the kernel:
1. **Copies** the `Sqe` out of linear memory into a kernel-local struct
   — *read once* (no TOCTOU; the module can't mutate it mid-validation).
2. Bounds-checks `reg_idx < REG_SLOTS`, entry not `Empty`.
3. **Generation check:** `RegEntry.reg_generation == current generation
   of cap_id's slot`. Mismatch ⇒ the cap was revoked/reused ⇒ reject.
4. **Op-permitted check:** `op` is legal for `RegEntry.kind` and the
   cached `rights` allow it.
5. Resolve the object via cached `pool_index`; for any `arg` that is a
   linear-memory pointer/length, **bounds-check against the submitting
   module's memory at execution time**.
6. Execute; write a `Cqe` (`user_data`, `result`).

One trap processes the whole batch. The per-entry check is steps 2–4 —
all O(1) array reads + comparisons, AOT-inlinable. The expensive
resolution (§2 steps 1–4) was already amortized into registration.

### 5.2 Why this is safe
- **No TOCTOU:** SQEs are copied before use (step 1); pointer args are
  bounds-checked at execution (step 5).
- **No forgery:** `reg_idx` names a pre-proven cap; a forged index hits an
  empty/stale slot (INV-15 lineage).
- **Revocation is atomic + free:** any `SYS_CAP_REVOKE`/`DELETE` bumps the
  slot generation (INV-17); the very next ring use of any handle to that
  cap fails step 3. No registration-sweep needed.
- **Blast radius = self:** the ring is in the module's own sandbox.

---

## 6 · The seL4-style synchronous fastpath

For a single latency-critical call (not throughput), extend the existing
Endpoint IPC with a register-only fastpath: sender + receiver both ready,
message fits in registers, no copy/alloc — the seL4 fastpath pattern. The
capability check stays in the hot path but is the hand-tuned variant.
This is an optimization of the *existing* `SYS_CALL`/Endpoint mechanism
(cap-system-design §5.6), not a new object. Spec'd in detail when the IPC
fastpath PR lands; noted here so the three rungs (registered ring,
fastpath, per-call trampoline) live in one place.

---

## 7 · Proposed invariants

Numbering to be confirmed against [`invariants.md`](invariants.md) at
land time (INV-23 is the last used in-tree; these take the next free
numbers). Each builds on existing cap invariants rather than inventing
new trust.

- **INV-α · Registered-Handle Soundness.** A `reg_idx` authorizes an
  operation iff: `reg_idx < REG_SLOTS` (cf. INV-18) ∧ slot is live ∧
  `reg_generation == current cap-slot generation` (INV-17) ∧ `op`
  permitted by cached `kind`+`rights`. Otherwise the operation is
  rejected. The index alone is never authority (cf. INV-15 forgery).
- **INV-β · Ring Entry Copy-Before-Use.** Every `Sqe` is copied to kernel
  memory before validation/execution; every linear-memory pointer arg is
  bounds-checked against the submitting module's memory at execution.
  (Closes TOCTOU.)
- **INV-γ · Revocation Invalidates Registrations.** `REVOKE`/`DELETE`
  bumping a slot's generation (INV-17) invalidates *all* registered
  handles to that cap on next use, with no separate sweep. Registration
  never outlives the authority it caches.

---

## 8 · Adversarial test plan (mandatory, per CLAUDE testing rules)

`tests/security/` additions — each must fail safely, no kernel panic:

- `reg_handle_forgery.rs` — submit SQEs with `reg_idx` out of range /
  pointing at an `Empty` slot ⇒ rejected.
- `reg_handle_stale.rs` — register a cap, `REVOKE` it, then submit via the
  stale handle ⇒ generation mismatch ⇒ rejected (INV-γ).
- `ring_toctou.rs` — mutate an SQE's args after submit but before drain ⇒
  kernel acted on the copied snapshot, never the mutated value (INV-β).
- `ring_oob_buffer.rs` — SQE whose `arg` buffer ptr/len exceeds the
  module's linear memory ⇒ rejected at execution.
- `reg_rights_escalation.rs` — register a read-only cap, submit a write op
  ⇒ op-permitted check denies (INV-α).
- `ring_overrun.rs` — SQ head/tail manipulated to over-read ⇒ bounded by
  the kernel's own copy of the ring descriptor, not the module's claims.

Unit (host-testable, pure logic): the `RegEntry` validation predicate
(bounds ∧ live ∧ generation ∧ op-permitted) extracted into
`validate.rs`-style pure functions with the full truth table.

---

## 9 · Open questions (for Gustavo)

1. **`REG_SLOTS = 64`** per process — right size? (Drivers need few; the
   assistant maybe more. Could be per-tier.)
2. **Ring sizing / backpressure** — fixed entries; what does the module
   do on a full CQ? (Lean: `SYS_RING_SUBMIT` returns short count; module
   drains CQ and retries.)
3. **SMP (Phase 2+)** — the ring control indices become atomics and the
   RegTable needs per-hart or locked access; INV-1 currently saves us.
4. **Does the ring drain on an explicit `SYS_RING_SUBMIT` only, or also
   opportunistically** from the kernel idle loop on a Notification? (Lean:
   explicit submit for determinism; Notification as a later latency
   optimization.)
5. **Generation width** — slot generation is `u16`; a pathological
   register/revoke loop could wrap it. Acceptable? (seL4 uses larger;
   note as a hardening item.)

---

## 10 · Prior art

| Pattern | Source | Relevance |
|---------|--------|-----------|
| Registered files/buffers; SQ/CQ rings | **io_uring** (Linux) | §3 + §5 recast through capabilities |
| Capability + generation/badge, mint/derive | **seL4** | The validation + the §6 fastpath; INV-17 generation is the revocation mechanism |
| Validate-once handle caching | OS handle tables generally | The registered table is a *capability-checked* handle table |
| Object-capability least authority | Miller, *Robust Composition* | A registered handle is an attenuated, cached reference, not new authority |

---

## 11 · Decision log

- **D1 — Cache resolution, not authority.** Registration stores a proven
  cap's resolution behind an index; the index is never authority.
- **D2 — Revocation rides INV-17.** The generation check makes
  revoke/delete invalidate all handles atomically with zero bookkeeping.
- **D3 — Copy SQEs before use; bounds-check pointer args at execution.**
  Closes TOCTOU without copying entire buffers.
- **D4 — Ring lives in the module's own linear memory.** Corruption is
  self-harm; the kernel trusts only its own copies.
- **D5 — Reuse the cap ABI family + invariant lineage** (INV-15/17/18);
  add INV-α/β/γ rather than a parallel trust model.
- **D6 — Three rungs, one home:** registered ring (throughput), seL4
  fastpath (latency), per-call trampoline (fallback). Rung 0 (raw native)
  remains the line not to cross.

---

## Appendix · Glossary
- **Registered handle (`reg_idx`)** — a small per-process index naming a
  capability the kernel proved at registration; not a capability.
- **RegEntry / RegTable** — the per-process cache of resolved caps.
- **SQE / CQE** — submission / completion queue entries.
- **Generation check** — comparing a cached slot generation against the
  live one to detect revocation/reuse (INV-17).
- **TOCTOU** — time-of-check-to-time-of-use; defeated by copy-before-use.
