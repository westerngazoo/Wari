<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Wari — Synchronous IPC + seL4 Fastpath (Lane B / B2)

> **Status:** Design proposal — **needs an architect decision before any
> code.** B2 (the IPC fastpath) is blocked on the base synchronous IPC not
> existing, and building it turns the run-to-completion scheduler into a
> context-switching one. That is a foundational fork; this doc lays it out
> for approval. Companion to
> [`cap-system-design.md`](cap-system-design.md) and
> [`ai-os-assistant-design.md`](ai-os-assistant-design.md) (the Planner →
> Executor channel *is* an Endpoint).

---

## 1 · Where we are (why B2 is blocked)

The `Endpoint` kernel object exists — two `BoundedQueue<TcbRef, 8>`
(senders/receivers) — but **nothing operates on it**:

- **No rendezvous** — there is no `send`/`recv`/`call`/`reply`.
- **No `Blocked` state** — `ProcessState` is `{Free, Ready, Running}`; the
  scheduler runs each module **to completion**, one after another.
- **No TCB context** — `TcbRef` is a `u8` placeholder; there are no saved
  registers to carry a message *through*.

A seL4 **fastpath** is an optimized shortcut over a *slow-path
rendezvous*. That slow path — plus blocking and a register-passing TCB —
must exist first. So B2 = (base IPC) + (the fastpath on top).

---

## 2 · The fork the architect must bless

**Blocking IPC requires a context-switching scheduler.** Today Wari runs
Tier-1/Tier-2 instances sequentially to completion (the wasmi host-fn
model). Synchronous `call`/`recv` means a running instance **blocks** and
the kernel switches to another — a real scheduler with saved contexts and
a `Blocked` state.

Two ways to introduce it:

| Option | What it means | Trade-off |
|--------|---------------|-----------|
| **A. wasmi-native blocking** | `recv`/`call` host fns that don't return until a peer rendezvouses; the kernel drives the wasmi call stacks cooperatively | Smaller step, stays inside the current host-fn model, but "blocking" is really cooperative yielding between wasmi instances |
| **B. Full preemptive TCB scheduler** | Real saved register contexts, `Blocked` state, timer preemption, context switch | The seL4-faithful model; bigger; the Phase-2 "real scheduler" the roadmap implies |

**Recommendation:** **A first** (cooperative, wasmi-native) — it delivers
functional synchronous IPC for the Planner→Executor channel with the
smallest change and no new `unsafe` context-switch code, and it's enough
to *have* a slow path to fastpath. **B** is the Phase-2/3 upgrade when
preemption + density demand it. The fastpath (§5) is designed to apply to
either.

---

## 3 · Message model

seL4-style **short messages in registers**, no copy on the fast path:

- A message is a small fixed set of machine words (a `badge` + up to N
  data words — say 4). Carried in the TCB's saved argument registers
  (or, under Option A, in a per-instance `MsgRegs` struct the host fn
  reads/writes).
- Larger payloads go through a shared linmem region referenced by a
  registered handle (the cap fast-path ring, B1) — IPC stays small+fast.

This keeps the fastpath a register transfer, never a buffer copy.

---

## 4 · Slow path (rendezvous state machine)

Pure-ish state machine over the `Endpoint` (host-testable core, matching
CLAUDE's "ipc rendezvous is mostly pure"):

- `send(ep)`: if a receiver is queued → **rendezvous**: transfer the
  message, mark the receiver Ready, return. Else enqueue self as sender,
  set `Blocked(SendWait, ep)`, yield.
- `recv(ep)`: if a sender is queued → rendezvous: transfer, mark sender
  Ready (or await reply for `call`), return the message. Else enqueue
  self as receiver, `Blocked(RecvWait, ep)`, yield.
- `call(ep)` = send + block for a reply (a one-shot reply capability minted
  to the receiver). `reply()` transfers back and readies the caller.

New `ProcessState::Blocked(BlockReason)` where `BlockReason ∈ {SendWait,
RecvWait, ReplyWait}` pairs with the `Endpoint`/reply object.

---

## 5 · The fastpath (B2 proper)

When `call`/`send` finds its peer **already blocked-waiting on the same
Endpoint**, skip the queues and the scheduler's general path:

1. Verify the fastpath conditions (peer blocked on this ep, message fits
   in registers, no queued higher-priority work) — a handful of checks.
2. Transfer the message registers **directly** peer-to-peer.
3. Switch to the peer without a full reschedule.

seL4's result: hundreds of cycles for a round-trip `call`/`reply`. Under
Option A this is "transfer `MsgRegs`, resume the peer's wasmi call"; under
Option B it's the classic register-context switch. The slow path (§4) is
the fallback for every condition the fastpath doesn't meet.

---

## 6 · ABI + integration

- Host fns (WASM path, per R7): `wari_ipc::{send, recv, call, reply}` over
  an Endpoint cap slot — cap-checked like every other op. Sysnums
  `SYS_SEND=2 / RECEIVE=3 / CALL=4 / REPLY=5` already reserved in
  `wari-abi`.
- The **Planner→Executor channel** (ai-os-assistant §4) is exactly a
  `call` on an Endpoint the Supervisor minted — so functional IPC
  unblocks the agentic layer's request path, and the fastpath makes each
  planner action low-latency.
- Batched actions still go through the cap-fastpath ring (B1); IPC is for
  the single synchronous request/reply.

---

## 7 · Invariants (to draft when it lands)

- Rendezvous transfers a message iff exactly one sender and one receiver
  meet on an Endpoint (no double-delivery, no lost message).
- `Blocked` is always paired with the object it waits on; revoking that
  object readies the waiter with an error (no permanent block).
- Fastpath is behaviourally identical to the slow path — it only skips
  work, never changes the result (the seL4 discipline).

---

## 8 · Prior art

| Pattern | Source |
|---------|--------|
| Synchronous IPC + register-message fastpath | **seL4** (the canonical design + the cycle counts) |
| Endpoints, badges, reply caps | seL4 / L4 family |
| Cooperative host-fn yielding (Option A) | async runtimes / wasmi host-fn re-entry |

---

## 9 · Decisions needed (before code)

1. **Option A (cooperative, recommended) vs B (preemptive TCB)** for
   introducing blocking. Gates the whole implementation.
2. **Message register count** (badge + N words; N=4?).
3. **Build order:** slow path first (functional IPC for Planner→Executor),
   fastpath second — agreed? The fastpath alone has nothing to optimize.

Until #1 is decided, B2 stays parked — implementing a fastpath now, or
unilaterally introducing a context-switching scheduler, would both be
wrong.
