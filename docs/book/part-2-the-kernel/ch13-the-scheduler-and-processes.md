---
sidebar_position: 13
sidebar_label: "Ch 13: The Scheduler & Processes"
title: "Chapter 13 — The Scheduler & Processes"
---

# Chapter 13 — The Scheduler & Processes

Every chapter so far could pretend the kernel does one thing at a time,
because it did. Boot ran to the banner. The MMU came up once. The wasmi
runtime loaded one Tier-1 module, ran its `_start`, and reaped it. That
was honest for Phase 0: the Phase-0/1a kernel ran exactly one Tier-2
driver and one Tier-1 app, both inline in `kmain` as a sequential
`run_tier2_uart()` then `run_tier1_hello()` chain
(`kernel/src/sched/mod.rs:5`). There was no scheduler because there was
nothing to schedule between.

This chapter is where "one thing at a time" stops being a simplification
and starts being a constraint we have to design against. Synchronous IPC
— the subject of Chapter 14 — needs a tenant to *stop in the middle of a
function call*, let another tenant run, and later pick up exactly where
it left off with a value it was waiting for. A run-to-completion loop
cannot express that. So before we can talk about messages, we have to
build the thing that can suspend and resume a WASM instance. That thing
is the scheduler, and the surprising part is how little of it is a
scheduler in the classic sense.

## From a chain of calls to a table of states

The first move is to stop thinking about *the currently-running module*
and start thinking about *a table of processes, each in a state*. The
process table is a plain static array, one slot per `proc_id`:

```rust
static mut PROCESSES: [Option<Process>; MAX_PROCS] = [const { None }; MAX_PROCS];
```

`kernel/src/sched/mod.rs:69`. `MAX_PROCS` is 16 today
(`wari-cap/src/cspace.rs:58`) — the same bound that sizes the capability
CSpaces, because slot `i` in this table is the process whose CSpace is
`cspaces()[i]`. One index, two meanings, deliberately kept in lockstep.
Reaching into that static is the kernel's oldest unsafe pattern, and it
carries the oldest justification: `processes()` returns `&'static mut`
under `// SAFETY: INV-1 + INV-8` (`kernel/src/sched/mod.rs:83`) —
single-hart, so no concurrent access; statically initialized, so the
reference is always to real state.

A `Process` is small on purpose. It is *not* the WASM instance. It is
the kernel-side handle that ties a `proc_id` to its tier, its module, and
its lifecycle state (`wari-sched/src/process.rs:104`). The heavy state —
the wasmi `Store` and `Instance` — lives elsewhere, and *where* it lives
is the whole story of this chapter. Hold that thought.

The states themselves are the vocabulary the rest of the kernel speaks
in (`wari-sched/src/process.rs:63`):

- `Free` — the slot is empty.
- `Ready` — loaded, waiting to be picked.
- `Running` — the current tenant on the hart.
- `Library` — loaded but never scheduled. This is how Tier-2 drivers
  live: the UART and net drivers are instantiated once at boot and then
  *called into* from host functions on a Tier-1 tenant's behalf. The
  scheduler does not pick a `Library` process to "run"
  (`kernel/src/sched/mod.rs:16`); it just needs a table entry so the
  cap layer has something to point at.
- `Blocked { reason, ep_idx }` — parked on a synchronous-IPC object.
  This is the variant that did not exist in Phase 0, and adding it is
  what turns the run loop into something that can host IPC.
- `Exited(i32)` — terminated cleanly with a code.
- `Faulted` — trapped, and being torn down.

Notice that `Blocked` is not a bare flag. It carries the *reason*
(`wari_ipc::BlockReason` — sender, receiver, caller, awaiting-reply) and
the `ep_idx`, the Endpoint pool index the process is queued on. That
pairing is an invariant, not a convenience: because a blocked process's
own state names the object it waits on, a future endpoint-revoke sweep
can walk the table, find every waiter on a dying endpoint, and wake it
with an error instead of leaking a permanently-stuck process
(`wari-sched/src/process.rs:83`, and `docs/ipc-design.md` §7). We are
writing down, in the type, the thing we will later need to prove.

## Policy is pure; mechanism is not

There is a discipline running through this subsystem that is worth
naming before we read the loop. The *decisions* the scheduler makes are
extracted into the `wari-sched` workspace crate, a `no_std` library with
no statics, no `unsafe`, and no process table — just functions over a
snapshot of states. The kernel keeps the imperative shell: the
`PROCESSES` static, the resumable-execution machinery, the run loop
itself (`wari-sched/src/lib.rs:9`). `kernel/src/sched/process.rs` is a
two-line re-export shim so call sites compile unchanged.

Two functions carry the policy. `pick_next_tenant` finds the lowest
`proc_id` in `Ready` (`wari-sched/src/policy.rs:37`) — that is the
entire scheduling algorithm, "run-to-completion in registration order,"
and it fits in an iterator chain. `count_blocked` counts `Blocked`
entries (`wari-sched/src/policy.rs:68`). Both take an iterator of states
and never touch memory, which is why both ship with exhaustive host unit
tests including the deliberately paranoid case where a `Ready` process
sits at index 256, outside the `u8` `proc_id` space, and must *not* be
truncated to 0 and picked (`wari-sched/src/policy.rs:116`). The kernel
wrappers (`kernel/src/sched/mod.rs:265` and `:304`) do nothing but
snapshot the table and defer. Policy is a thing we can test on a laptop;
mechanism is a thing that only exists on the hart. Keeping them apart is
how the interesting half stays provable.

## Why a tenant must suspend mid-execution

Here is the constraint that shapes everything. A Tier-1 tenant runs as a
wasmi interpreter driving its `_start` export. When that WASM code calls
an IPC host function — `ipc_recv`, say — and no peer is waiting, the
tenant has to block. But "block" inside a WASM interpreter is not a
context switch to a saved register frame. There *is* no saved register
frame; the tenant's entire execution state is the live wasmi call stack,
sitting in a Rust stack frame belonging to whoever called
`Func::call`. To let another tenant run, we have to unwind out of that
call *without destroying the call stack*, and later re-enter it with the
value the host function was supposed to return.

Until synchronous IPC, we never needed this. A Tier-1 instance lived in
`sched::run`'s stack frame for exactly one run-to-completion `_start`
call (`kernel/src/runtime/tier1_pool.rs:5`). Blocking breaks that model
in two places at once: the instance can no longer be scoped to a stack
frame, and the run loop can no longer assume a step ends in termination.

The instances therefore move into a resident **pool** — one slot per
`proc_id`, mirroring the process table and the CSpaces
(`kernel/src/runtime/tier1_pool.rs:97`). Each slot owns the `Store`, the
`Instance`, and — the crucial field — an `Option<ResumableInvocation>`:
the suspended `_start` call while the tenant is blocked, `None` while it
runs (`kernel/src/runtime/tier1_pool.rs:80`). It also holds a
`pending_resume: i32`, the value the blocked host function will "return"
when the tenant wakes.

The mechanism underneath is wasmi's own `Func::call_resumable`
(`kernel/src/runtime/tier1_pool.rs:184`), the engine-supported way to
unwind a host function out of WASM without losing the stack. This is not
a Wari invention; it is the same shape as a cooperative async runtime,
where an `await` point yields control back to an executor that resumes
the future later. `docs/ipc-design.md` §8 cites exactly this lineage —
"cooperative host-fn yielding … async runtimes / wasmi host-fn
re-entry." We are borrowing the coroutine, not building a thread.

## The IpcBlock yield protocol

So how does a host function, buried inside the interpreter, ask to be
suspended? It returns an error — but a very specific one. `IpcBlock` is
a zero-payload marker type that implements `wasmi::core::HostError`
(`kernel/src/runtime/tier1_pool.rs:69`). An IPC host function that
decides its caller must block does three things, in order, and then
yields:

1. It records the kernel-side truth: it queues the caller's `TcbRef` on
   the Endpoint and transitions the process to `Blocked` via
   `Process::block(...)` (`wari-sched/src/process.rs:184`). The *reason*
   for blocking lives in the process table, not in the marker — one
   source of truth (`kernel/src/runtime/tier1_pool.rs:63`).
2. It returns `Err(wasmi::Error::host(IpcBlock))`.
3. wasmi unwinds the interpreter back up to whoever called
   `call_resumable`, handing them a `ResumableInvocation` that captures
   the frozen call stack.

The catcher is `settle`, the shared tail of both `start` and `resume`
(`kernel/src/runtime/tier1_pool.rs:219`). When it sees
`ResumableCall::Resumable`, it checks whether the host error really is
`IpcBlock` (`:241`); if so, it parks the invocation in the slot and
returns `StepOutcome::Blocked` (`:244`). Any *other* host error is not a
sanctioned yield — the tenant is killed, fail-closed (`:251`). The
process was already moved to `Blocked` in step 1; `settle` only stores
the frozen stack.

Waking is the mirror image, run from the IPC rendezvous path (Chapter
14). The waker calls `set_resume_value(proc_id, rc)` to stash the
syscall's return code in the slot (`kernel/src/runtime/tier1_pool.rs:131`),
then `sched::wake` to flip the process `Blocked → Ready`
(`kernel/src/sched/mod.rs:285`). Later the scheduler picks the now-Ready
process and calls `resume`, which takes the parked invocation and feeds
the stashed value back as a single `Val::I32`
(`kernel/src/runtime/tier1_pool.rs:213`). Execution continues inside the
WASM as if the host function had simply returned that integer. There is
an arity contract holding this together — every blockable Wari host
function returns exactly one `i32`, so `resume` always feeds exactly one
`Val::I32` (`kernel/src/runtime/tier1_pool.rs:35`). A host function with
any other signature must never yield `IpcBlock`; if one did, the type
mismatch would surface as a wasmi error and fault the tenant rather than
corrupt anything.

## The run loop, read against that protocol

Now the loop reads cleanly (`kernel/src/sched/mod.rs:145`). Each
iteration:

1. `pick_next_tenant()` — lowest `Ready`, or `None`.
2. Mark it `Running` through a short borrow of the table, never held
   across another `processes()` call (`:182`). Every borrow
   `?`-propagates `NoSuchProcess` rather than `.unwrap()`-panicking,
   because R5 forbids panics in the scheduler path; `pick_next_tenant`
   only ever returns occupied slots, so the error arm is structurally
   unreachable, but making it *structural* rather than *implicit* is the
   point (`:170`).
3. Step the tenant. If the pool already holds a live instance for this
   `proc_id`, this is a resume; otherwise it is a first start
   (`:203`). A resume first flushes any delivered message into the
   tenant's linear memory (more on that timing below), then calls
   `resume`; a start calls `start` with the module's blob (`:209`).
4. Classify the `StepOutcome` (`:216`). `Exited(code)` and `Faulted`
   write the terminal state. `Blocked` is the interesting one: the
   scheduler *verifies* rather than performs. The yielding host function
   was supposed to have already moved the process to `Blocked`; the loop
   asserts `proc.is_blocked()` and, if a tenant yielded `IpcBlock`
   without actually blocking itself, treats that as a protocol violation
   and faults it (`:229`). The suspend decision belongs to the host
   function that has the context; the scheduler only sanity-checks the
   result.

There is no preemption here, no timer, no fuel. That is not an oversight
— it is the honest Phase-1b minimum. More sophisticated policies land
when there are workloads that need them (`kernel/src/sched/mod.rs:20`).
What we have built is not a time-sharing scheduler; it is a *rendezvous
driver* that happens to also pick the next runnable tenant.

## The bug the design caught: exits look like yields

The most instructive line in this subsystem is a comment explaining why
one `if` comes before another. It documents a real trap the resumable
model set, and stepping on it once was enough to write it down forever.

When wasmi's interpreter hits a host error while the WASM call stack is
non-empty, it does not propagate the error as an `Err` — it wraps it as
a *resumable yield*. That is the mechanism `IpcBlock` relies on. But
`proc_exit` also raises a host error: wasmi models a clean
`proc_exit(code)` as `Error::i32_exit`. On a non-root frame — and
`_start` calling `proc_exit` is exactly that — a clean exit therefore
arrives at `settle` wearing the same `ResumableCall::Resumable`
clothing as an IPC block (`kernel/src/runtime/tier1_pool.rs:229`).

If `settle` checked for `IpcBlock` first, every clean `proc_exit` would
fail that check, fall through to "unknown host-error yield," and the
kernel would *fault a tenant at the exact moment it exited successfully*.
The fix is ordering: classify exits **before** the `IpcBlock` check.

```rust
if let Some(code) = inv.host_error().i32_exit_status() {
    kprintln!("[t1:{}] exit({})", proc_id, code);
    release(proc_id);
    return StepOutcome::Exited(code);
}
if inv.host_error().downcast_ref::<IpcBlock>().is_some() {
    // ... park the invocation, StepOutcome::Blocked
}
```

`kernel/src/runtime/tier1_pool.rs:236`. The exit check wins because an
exit and a block are *both* resumable yields carrying host errors, and
only their payload distinguishes them. It is the kind of bug you only
find by running the code and watching a tenant "fault" on `exit(0)`; the
comment at `:229` exists so the next person does not have to rediscover
it. (The same `i32_exit_status()` classification appears again in the
plain-`Err` arm at `:263`, for the root-frame case where wasmi does
propagate the error directly — belt and suspenders across both shapes.)

## When everyone is blocked

A rendezvous-driven loop has a failure mode a run-to-completion loop does
not: deadlock. If `pick_next_tenant` returns `None` but the table still
holds `Blocked` processes, every one of them is waiting on a peer that
will never run again — a genuine IPC deadlock, or a peer that exited
without replying. The loop refuses to hang silently
(`kernel/src/sched/mod.rs:151`):

```rust
let blocked = count_blocked();
if blocked > 0 {
    kprintln!(
        "[sched] {} tenant(s) permanently blocked (no runnable peer) — abandoning",
        blocked
    );
}
return Ok(());
```

Phase 2's endpoint-revoke sweep will turn these into per-process errors —
walking each dying endpoint's waiters and waking them with a failure code,
the payoff of the `Blocked { ep_idx }` pairing we insisted on earlier.
Until then the loop reports the count and *returns*, so `kmain` reaches
its idle `wfi` loop and the Ctrl-R console stays alive, instead of the
whole kernel wedging on a lost message. Failing loud and recoverable beats
failing invisible.

## What the process table does not hold

We can now close the thread left dangling three sections up. The
`Process` struct carries two IPC-shaped fields we have not used yet:
`msg_regs`, the message registers a rendezvous transfer writes while the
process is blocked, and `msg_buf`, the linear-memory offset where a
delivered message should eventually land — sentinel `NO_MSG_BUF` when
there is nothing to flush (`wari-sched/src/process.rs:118`). The
scheduler touches `msg_buf` in exactly one place: immediately before a
resume, it calls `flush_msg_to_linmem`, copying the delivered `msg_regs`
into the tenant's linear memory (`kernel/src/sched/mod.rs:209`).

Why *there* and nowhere else? Because that is the one moment the kernel
may safely write a Tier-1 instance's `Store`: the instance is blocked,
its invocation parked in the pool, so no wasmi frame of that instance can
possibly be live to alias the memory
(`kernel/src/runtime/tier1_pool.rs:285`). Writing a peer's linear memory
at any other time would mean touching a `Store` that wasmi might hold. The
scheduler is not just picking who runs next — it is providing the single
un-aliased window in which cross-instance message delivery is sound.

That window, and the marshaling rules that live on either side of it, are
the subject of the next chapter.

## Closing hook

Chapter 14 — we have a scheduler that can suspend a tenant mid-call and
resume it with a value. Now we make two tenants actually *talk*: the
seL4-style synchronous rendezvous, the pure decision plane that decides
who blocks, and the marshaling discipline that lets instance A hand a
message to instance B without either one's `Store` ever aliasing the
other's. First cross-tenant IPC on Wari — `PING`, then `PONG`.
