---
sidebar_position: 14
sidebar_label: "Ch 14: Synchronous IPC"
title: "Chapter 14 — Synchronous IPC"
---

# Chapter 14 — Synchronous IPC

Chapter 13 built a scheduler that can freeze a Tier-1 tenant in the
middle of a host call and thaw it later with a return value. That was the
hard mechanical prerequisite. This chapter spends it: two tenants, one
Endpoint, and a message that crosses between them while neither one's
linear memory is ever touched by code that could corrupt it. By the end,
instance A will say `PING` and instance B will answer `PONG` — the first
cross-tenant synchronous IPC on Wari, running on the demo you can boot
today.

The design is deliberately, almost pedantically, seL4-shaped. seL4 is the
canonical synchronous-IPC kernel — endpoints, badges, reply capabilities,
and a register-message fastpath that turns a `call`/`reply` round-trip
into a few hundred cycles (`docs/ipc-design.md` §8). We inherit the model
and its vocabulary. We do *not*, yet, inherit the fastpath; more on that
honesty at the end.

## The fork we had to bless first

Synchronous IPC was blocked on a decision, not on code. `docs/ipc-design.md`
§2 lays out the fork: blocking IPC requires a context-switching scheduler,
and there are two ways to get one. **Option A** is wasmi-native
cooperative blocking — `recv`/`call` host functions that do not return
until a peer rendezvouses, driven by the interpreter's own re-entry.
**Option B** is a full preemptive TCB scheduler with saved register
contexts and timer preemption — the seL4-faithful endpoint.

Option B is the chosen destination, and the code says so in its file
headers. But what ships *today* is the base layer B is built on, and it
gets its blocking from Option A's cooperative mechanism: the resumable
wasmi invocations of Chapter 13, no saved GPR file, no timer. That is not
a contradiction — it is the build order. `MsgRegs` is described as "the
first slice of the full TCB register context"
(`wari-sched/src/process.rs:36`); the complete register save/restore area
and the preemption that needs it arrive as later bricks. We are building
the slow path first, because a fastpath with nothing to be fast *over*
optimizes nothing.

## Three planes

The implementation splits along a line the rest of the kernel will
recognize: pure decisions on one side, imperative mechanism on the other.
IPC takes it further and splits into three planes
(`kernel/src/ipc.rs:4`):

- **Decision plane** — `wari_ipc::resolve(op, peer_waiting)`. A pure,
  `const fn` state machine: given an operation and whether a compatible
  peer is currently waiting, it decides whether the caller rendezvouses
  now or enqueues and blocks. No allocation, no `unsafe`, no process
  table (`wari-ipc/src/lib.rs:107`).
- **Data plane** — `transfer_msg(sender, receiver)`. A pure copy of one
  `MsgRegs` into another, nothing more (`wari-sched/src/process.rs:214`).
- **Mechanism** — `kernel/src/ipc.rs`. The impure glue those two planes
  drive: Endpoint cap checks, the sender/receiver queues, the `Blocked`
  transitions, linear-memory marshaling, and the `IpcBlock` yield that
  suspends the caller.

Purity here is not aesthetic. The decision plane is the part most likely
to have a subtle bug — the promotion rules, the "a `call` always ends up
awaiting a reply" corner — and pulling it into a `no_std` crate with no
kernel dependencies means it can be exhaustively unit-tested on a laptop
and, later, handed to Kani as a proof target
(`wari-ipc/src/lib.rs:9`). The same is true of `transfer_msg`: a pure
function over two structs, host-testable and provable alongside the
decision it serves. What is left in `ipc.rs` is only the part that
*cannot* be pure — the part that reads a WASM instance's memory and
flips scheduler state — and it is small precisely because the thinking
was extracted out of it.

## The decision plane, in full

`resolve` is short enough to hold in your head, which is the point
(`wari-ipc/src/lib.rs:107`). There are four operations
(`wari-ipc/src/lib.rs:68`) and three outcomes
(`wari-ipc/src/lib.rs:48`):

- `Send`: peer waiting → `Rendezvous`, caller `Continue`; else `Enqueue`
  as `SendWait`.
- `Recv`: peer waiting → `Rendezvous`, caller `Continue`; else `Enqueue`
  as `RecvWait`.
- `Call`: peer waiting → `Rendezvous`, but caller `Block(ReplyWait)` —
  it delivered, now it waits for the answer; else `Enqueue` as
  `CallWait`.
- `Reply`: caller waiting → `Rendezvous`, `Continue`; else `Invalid` —
  a reply with nobody to reply to fails closed.

The asymmetry that makes `call` interesting is that it is the only
operation whose caller blocks *even on a successful rendezvous*
(`wari-ipc/src/lib.rs:132`). `send` and `recv` and `reply` that find a
peer are done and keep running. `call` that finds a receiver has only
finished the *first half* of a request/reply and must suspend for the
second. That single fact — encoded as `CallerNext::Block(ReplyWait)` —
is what a whole test asserts on its own
(`wari-ipc/src/lib.rs:210`). The state machine ships with a test per
operation, plus a `const_evaluable` test proving it folds at compile time.

The reason lives in the process table, never in the marker. `BlockReason`
is defined once in `wari-ipc` (`wari-ipc/src/lib.rs:20`) and re-exported
into the scheduler's `Process` type (`wari-sched/src/process.rs:26`), so
the scheduler literally cannot drift from the IPC state machine — they
share the enum. That is the "one source of truth" rule made structural.

## The message model

A Wari message is small by design: a `badge` plus four data words
(`wari-sched/src/process.rs:42`). `MSG_WORDS` is 4
(`wari-sched/src/process.rs:52`); the badge identifies the sender's
capability to the receiver, seL4-style. On the wire, in a tenant's linear
memory, that is a 40-byte little-endian buffer — `badge u64 | words
[u64; 4]` — the constant `IPC_MSG_BYTES = 40`
(`abi-shared/src/lib.rs:251`), which `ipc.rs` pins its wire size to
(`kernel/src/ipc.rs:55`). Larger payloads are explicitly *not* IPC's
job: they go through the shared-memory cap-ring, and IPC itself stays
register-sized (`wari-sched/src/process.rs:36`). Keeping the message the
size of a handful of registers is what makes the seL4 fastpath possible
later — a register transfer never becomes a buffer copy.

Encoding and decoding the 40 bytes is mechanical little-endian work
(`decode_msg` at `kernel/src/ipc.rs:58`, `encode_msg` at `:82`). The
interesting question is never the bytes — it is *whose* linear memory
gets read or written, and when.

## The marshaling rule that keeps two Stores un-aliased

This is the heart of the chapter, and it follows directly from Chapter
13's closing observation. Each Tier-1 instance owns its own wasmi
`Store`. The kernel must never write instance B's linear memory from
inside a host function running on instance A's behalf, because that would
mean aliasing B's `Store` while wasmi may hold it. So the rule
(`kernel/src/ipc.rs:16`):

- A **running** caller marshals through *its own* memory. When a tenant
  is the one executing the host call, its `Caller` handle is the safe
  path: `read_msg` pulls the outbound message from its own linear memory
  (`kernel/src/ipc.rs:108`), and `write_msg` writes an inbound message
  back into its own (`kernel/src/ipc.rs:121`).
- A **blocked** peer receives kernel-side only. The rendezvous copies
  the message into the peer's `Process::msg_regs` — plain kernel memory,
  no `Store` touched — and records nothing else. The scheduler flushes
  `msg_regs` into the peer's linear memory just before resuming it
  (`kernel/src/runtime/tier1_pool.rs:297`), in the one window where the
  peer is blocked and no wasmi frame of it can be live.

Put the two halves together and the delivery of a message from A to B is
split across time on purpose. The sender copies *out of its own memory*
while it runs. The bytes sit in kernel-owned `MsgRegs` while the receiver
is parked. The receiver copies *into its own memory* — either directly,
if it is the running side of the rendezvous, or via the scheduler's flush,
if it was the blocked side. At no instant does one instance's host call
reach into another instance's `Store`. The un-aliasing is not enforced by
a lock; it is enforced by *when* each copy is allowed to happen.

The wake path bundles the kernel-side half into one helper
(`deliver_and_wake`, `kernel/src/ipc.rs:152`): `transfer_msg` into the
peer's registers, `set_resume_value` to stash the syscall's return code,
then `sched::wake` to make it `Ready`. Order matters — the message and
the resume value are both in place before the process becomes eligible to
run.

## send, recv, call, reply

The four host functions are thin wrappers over the mechanism, dispatched
from the WASI linker at `kernel/src/runtime/wasi.rs:311`–`:347`
(`proc_self`, the demo's role-splitter, at `:357`). Each begins by
resolving the Endpoint capability at the caller's slot, and the required
rights follow the send/recv asymmetry: you need `WRITE` to *deliver into*
an endpoint, `READ` to *take from* it (`kernel/src/ipc.rs:98`, with
`send`/`call` demanding `CAP_RIGHT_WRITE` at `:214` and `recv` demanding
`CAP_RIGHT_READ` at `:278`).

`send` and `call` share a body — `do_send_like`
(`kernel/src/ipc.rs:207`) — because they differ only in the `Op` they
pass to `resolve` and in what the caller does afterward, which `resolve`
already encodes. The flow: resolve the endpoint, read the outbound
message from the caller's own memory, stash it kernel-side in
`Process::msg_regs` *first* so the authoritative copy exists whether we
rendezvous or enqueue (`:224`), then pop a waiting receiver and ask
`resolve` what to do. On `Rendezvous`, deliver to the receiver and either
return `0` (a `send`, `CallerNext::Continue`) or block for the reply (a
`call`, `CallerNext::Block`, via the `IpcBlock` yield at `:250`). On
`Enqueue`, push onto the sender queue and block (`:254`). A full queue
returns `E_AGAIN` — the caller may retry — rather than blocking on a
queue it could not join.

`recv` is the receiving mirror (`kernel/src/ipc.rs:272`). It pops a
waiting sender; on rendezvous it takes the sender's kernel-side message
and writes it into *its own* linear memory — the running-caller path,
its own `Caller` the safe route (`:296`). Then comes the promotion that
makes `call` work:

```rust
Some(ProcessState::Blocked { reason: BlockReason::CallWait, ep_idx: e }) => {
    // promote the caller: it has been received, now it awaits our reply
    p.block(BlockReason::ReplyWait, e);
}
_ => deliver_and_wake(tx, None, 0),
```

`kernel/src/ipc.rs:312`. A sender that arrived via plain `send` is simply
woken — its work is done. A sender that arrived via `call` is instead
*promoted* from `CallWait` to `ReplyWait` (`:318`): it stays blocked, but
now it is waiting for a reply rather than for a receiver. This is the
kernel half of `resolve`'s promise that a `call` always ends up awaiting
a reply. The `Blocked → Blocked` rewrite is exactly the transition
`Process::block` is documented to permit
(`wari-sched/src/process.rs:184`).

`reply` closes the loop (`kernel/src/ipc.rs:345`). A caller in
`ReplyWait` is *not* on the endpoint's queue — seL4 models this with a
one-shot reply capability. Phase-2's minimal stand-in scans the process
table for the lowest-pid process in `Blocked { ReplyWait }` on this
endpoint (`reply_waiter_on`, `kernel/src/ipc.rs:163`). That is honestly
labeled: it is correct for the Phase-2 workloads, which have one caller
per endpoint at a time, and the reply-cap object replaces the scan when
multi-caller endpoints arrive (`kernel/src/ipc.rs:30`). When a waiter is
found, `reply` reads its message from the replier's own memory and
`deliver_and_wake`s the caller with it. No waiter means `Invalid` from
`resolve`, which the mechanism turns into `E_INVAL`.

## PING, then PONG

The demo is real code, and it is worth tracing end to end because it
exercises every plane. `kmain` registers two Tier-1 instances of the same
`hello` module — instance A as `proc_id` 2, instance B as `proc_id` 3
(`kernel/src/main.rs:182` and `:195`; `PROC_ID_TIER1_HELLO_B = 3` at
`kernel/src/cap/boot.rs:72`). Boot gives *both* a READ+WRITE capability
to one shared Endpoint at slot 3 — `SLOT_IPC`
(`kernel/src/cap/boot.rs:96`, minted into both CSpaces at `:244`). Inside
`_start`, the two instances split roles by their own `proc_self()`
(`apps/hello/src/lib.rs:137`): instance A puts `PING` in word 0 and calls
`ipc_call`; instance B calls `ipc_recv`, prints what arrived, and replies
`PONG`.

Read against the scheduler's registration-order loop, the interleaving is
fully determined:

1. **A runs first** (lowest `Ready`). It prints its banner, then
   `ipc_call(SLOT_IPC, "PING")`. No receiver is waiting yet, so `resolve`
   returns `Enqueue { CallWait }`. A is pushed on the sender queue,
   transitions to `Blocked { CallWait }`, and yields `IpcBlock`.
   `settle` parks its invocation; the run loop verifies it is blocked and
   moves on.
2. **B runs next.** It prints its banner, then `ipc_recv(SLOT_IPC)`. It
   pops A off the sender queue — `resolve(Recv, true)` → `Rendezvous`. B
   takes A's kernel-side `PING` and writes it into B's *own* linear
   memory (B is running; its `Caller` is safe). A is in `CallWait`, so B
   promotes it to `ReplyWait` rather than waking it. B prints
   `got=PING -> replying PONG`, then `ipc_reply(SLOT_IPC, "PONG")`:
   `reply_waiter_on` finds A in `ReplyWait`, and `deliver_and_wake`
   copies `PONG` into A's `msg_regs`, sets A's resume value to `0`, and
   readies A. B's `reply` returns `Continue`, so B keeps running to the
   end of `_start` and exits.
3. **A resumes.** It is `Ready` again with a live pool slot. The
   scheduler flushes A's `msg_regs` — now holding `PONG` — into A's
   linear memory at the buffer offset A recorded when it blocked
   (`flush_msg_to_linmem`), then resumes the parked invocation feeding
   back `0`. Inside the WASM, `ipc_call` returns `0` as if it had
   returned synchronously; A reads the same buffer it sent, now
   overwritten with the reply (seL4 MR in/out), and prints
   `ipc: reply=PONG`. A finishes `_start` and exits.
4. The loop finds no more `Ready` tenants, no `Blocked` ones either, and
   returns to the idle loop.

`PING` out of A's memory, into the kernel, into B's memory; `PONG` back
the same way. Two `Store`s, one Endpoint, and not a single moment where a
host call reached across the instance boundary.

## What is real, and what is planned

Honesty, in the house style. The **decision and data planes are
host-tested** — `wari_ipc::resolve` and `transfer_msg` each ship
exhaustive unit tests (`wari-ipc/src/lib.rs:155`,
`wari-sched/src/process.rs:222`). The **end-to-end rendezvous is
exercised by the on-target `hello` demo above**, not yet by a dedicated
QEMU integration test in `tests/integration/`; adding one belongs with
the same follow-up that generalizes the demo past a hardcoded two-tenant
role split.

The larger unbuilt pieces are named in the design and named here. The
**seL4 fastpath** — the register-to-register shortcut that skips the
queues and the general scheduler path when a `call` finds its peer
already blocked on the same endpoint — is `docs/ipc-design.md` §5, and it
is future. So is the **preemptive TCB scheduler** proper: the saved GPR
context that `MsgRegs` is the first slice of, and the timer preemption
that needs it. And so is the **endpoint-revoke sweep** that will convert
the deadlock report from Chapter 13 into per-process wakeups with an
error code — the payoff still owed on the `Blocked { ep_idx }` pairing.
What exists today is the slow path, correct and cooperative, with the
fast path designed to drop in on top without changing a single observed
result. That is the seL4 discipline: the fastpath only skips work, it
never changes the answer.

## Closing hook

Part 3 — everything so far has run inside QEMU's generous fiction. Two
tenants exchanging `PING`/`PONG` is a satisfying thing to watch scroll
past a virtual UART. It is a different thing to watch it scroll past a
real one, on a RISC-V board you can hold, whose SoC does not forgive the
shortcuts an emulator does. Next, we take the kernel to silicon.
