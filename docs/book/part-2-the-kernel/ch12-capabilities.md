---
sidebar_position: 12
sidebar_label: "Ch 12: Capabilities"
title: "Chapter 12 — Capabilities"
---

# Chapter 12 — Capabilities

Every host function in Chapter 11 began with the same line: a call to
`check_cap`. `fd_write` asked whether the caller held an
`Endpoint`/`WRITE` capability at slot 0; `mmio_write8` asked whether
the UART driver held an `Endpoint`/`READ` at slot 0. The runtime posed
the question and refused to answer it. This chapter is the answer.

The capability system is the kernel's permission layer. Every
privileged action in Wari — IPC, MMIO, socket creation, process exit —
is gated on the acting process holding the right capability with
sufficient rights. Capabilities are unforgeable: only the kernel
constructs them. They live in a per-process table. And they are
revocable transitively: revoke a parent, and every child dies with it.
This is the single most load-bearing subsystem in the kernel, and
Wari's whole sovereign-OS thesis — *auditable in under a week by a
small team* — rises or falls on it being right.

## The model, and where it comes from

Wari's capability system is seL4's, condensed. The design document is
explicit about the lineage and about the cost of choosing it
(`docs/cap-system-design.md` §2): the architect chose "seL4 puro" —
cap slots plus a derivation tree — knowing it costs 1,500–2,000 lines
of Rust instead of the 250–400 a simpler scheme would need, and knowing
it pushed the phase out by weeks. The justification is that Wari's
value proposition is auditability and formal-verification readiness,
and seL4 already paid the verification cost for this exact shape (Klein
et al., *seL4: Formal Verification of an OS Kernel*, SOSP 2009). By
aligning Wari's structures with seL4's, a future Phase-4 verification
effort can build on that work rather than start over.

What Wari inherits is the *concepts*: capabilities, CSpaces, kernel
objects, derivation, cascading revocation, badging. What it explicitly
does **not** inherit is the *implementations*
(`docs/cap-system-design.md` §2, "What we explicitly DO NOT inherit"):

| seL4 has | Wari Phase 1b uses | Why |
|---|---|---|
| Multi-level guarded CSpace (a CPtr is a path) | Single-level flat CSpace (a CPtr is a `u8` index) | 256 caps per process is plenty; Simplicity First |
| Mapping Database (a depth-ordered doubly-linked list) | Implicit tree via `parent: CapId` per cap | A recursive walk is O(n), and n ≤ 4096; cheap and obvious |
| Preemptable revoke | Atomic revoke, kernel runs to completion | Single-hart kernel; long revokes are tolerable |

These simplifications are documented so that a Phase-4 auditor asking
"why does this differ from seL4?" has a table to read. Genode's "every
kernel object is named by a capability" discipline and CHERI's
hardware-tagged capabilities are cited as secondary influences
(`docs/cap-system-design.md` §11); Wari's caps are software-enforced,
constructed by the kernel alone.

## What a capability is

A `Cap` is a 16-byte value. Its definition lives in the `wari-cap`
workspace crate — the pure-logic core was extracted there so it is
host-testable — at `wari-cap/src/types.rs:198`:

```rust
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap {
    pub badge: u32,          // Endpoint caller-id; 0 for other kinds
    pub parent: CapId,       // derivation parent; CapId::ROOT for kernel mints
    pub generation: u32,     // slot generation at mint time (anti-ABA)
    pub pool_index: u16,     // index into the per-kind object pool
    pub kind: ObjectKind,    // which kind of kernel object
    pub rights: u8,          // rights bitmap
}
```

The field order is chosen so `#[repr(C)]` packs to exactly 16 bytes
with no padding on RV64GC, and a test pins that size
(`types.rs:319`–`325`). The `#[repr(C)]` is not cosmetic: a stable
in-memory layout is what lets formal-verification tooling reason about
the representation, and what will let Phase 4 serialize caps for an
audit.

A capability names its object by `(kind, pool_index)`, never by a raw
pointer. `ObjectKind` (`types.rs:75`–`99`) is a `#[repr(u8)]` enum:

```
Empty = 0     Endpoint = 1     Notification = 2
Untyped = 3   Frame = 4        Net = 5          Socket = 6
```

`Empty` is the sentinel for an unused slot; the design doc sketched
four kinds for Phase 1b (Endpoint, Notification, Untyped, Frame) and
the network work added two more (Net, Socket) as `smoltcp` landed. The
kernel-object structs themselves live in `wari-cap/src/objects.rs` —
an `Endpoint` (`objects.rs:80`) carries bounded sender/receiver queues
and a refcount; a `Notification` (`objects.rs:118`) is a 32-bit signal
bitmap; `Net` (`objects.rs:250`) is a NIC handle; `Socket`
(`objects.rs:290`) wraps an opaque smoltcp handle. Each kind lives in a
fixed-capacity pool, and the pools are gathered into one `ObjectPools`
struct (`objects.rs:336`) with compile-time capacities
(`objects.rs:221`–`234`) — 64 endpoints, 64 notifications, 1024
frames, and so on. Fixed sizes, no dynamic growth: the resource
footprint is bounded by construction.

The rights bitmap is eight bits, of which Phase 1b uses four
(`types.rs:169`–`182`):

| Bit | Name | Meaning |
|---|---|---|
| 0 | `READ` | may read object state (recv on an Endpoint) |
| 1 | `WRITE` | may modify object state (send on an Endpoint) |
| 2 | `GRANT` | may pass this cap to another process via IPC |
| 3 | `GRANT_REPLY` | may pass via the reply path of synchronous IPC |

Bits 4–7 are reserved, and a mint that requests one is rejected. The
four names match seL4's deliberately, so the audit story aligns;
`GRANT_REPLY` is kept distinct from `GRANT` because seL4 proved that
conflating them opens a confused-deputy variant
(`docs/cap-system-design.md` §3.4).

`CapId` (`types.rs:118`) is how a cap points at its parent. It packs
`(generation << 16) | (proc_id << 8) | slot` into a `u32`
(`types.rs:132`–`141`), with `CapId::ROOT = u32::MAX`
(`types.rs:123`) marking an original kernel-issued cap that has no
parent. That 16-bit generation field is the anti-ABA mechanism, and we
will come back to it.

## Where capabilities live: the CSpace

Every process has exactly one CSpace, and a CSpace is a flat array of
slots (`wari-cap/src/cspace.rs:65`):

```rust
#[repr(C)]
pub struct CSpace {
    pub slots: [Cap; CSPACE_SLOTS],          // 256 slots × 16 B = 4 KiB
    pub generations: [u16; CSPACE_SLOTS],    // per-slot generation counters
}
```

`CSPACE_SLOTS = 256` (`cspace.rs:51`) is chosen so the slot array is
exactly one 4 KiB page. A process names its caps by slot index — a
`u8` the design calls a CPtr. There is no userspace pointer, no
userspace-visible address, no forgeable handle: a WASM module holds an
*index*, and the kernel resolves the index against its own memory.

`MAX_PROCS = 16` (`cspace.rs:58`) bounds how many CSpaces exist. The
whole array is a single static, `CSPACES: [CSpace; MAX_PROCS]`
(`kernel/src/cap/storage.rs:53`), initialized at compile time via a
`const fn` so boot has no per-static init step. It is `static mut`, and
the accessor `cspaces()` (`storage.rs:91`) hands out a `&'static mut`
under a SAFETY comment citing INV-1 (single-hart, so no aliasing
reader) and INV-8 (statically initialized, so always post-init). The
storage discipline is strict: take the reference, do the work in one
straight-line block, drop it — never hold two aliasing accessor results
at once (`storage.rs:16`–`30`).

The design explicitly flattened seL4's multi-level CSpace down to this
single level (`cspace.rs:26`–`34`): Phase-1b workloads are a
single-digit number of tenants each holding fewer than 32 caps, nowhere
near the scale that justifies a guarded multi-level structure. If a
workload ever needs more than 256 caps per process, the migration is
local to `cspace.rs` and does not touch the syscall ABI.

## `check_cap`: the trust-boundary gate

Every host function's permission check funnels through one function,
`check_cap` in `kernel/src/cap/syscall.rs:102`. It is short on purpose —
the design set a target of ≤ 30 lines for the lookup-and-rights path,
and this is it:

```rust
pub fn check_cap(proc_id: u8, slot: u8, expected_kind: ObjectKind,
                 required_rights: u8) -> Result<(), i32> {
    if (proc_id as usize) >= MAX_PROCS { return Err(E_PERM); }
    if (slot as usize) >= CSPACE_SLOTS { return Err(E_PERM); }
    let cs = cspaces();
    let cap = cs[proc_id as usize].slots[slot as usize];
    if cap.is_empty() { return Err(E_PERM); }
    if cap.kind != expected_kind { return Err(E_PERM); }
    if cap.rights & required_rights != required_rights { return Err(E_PERM); }
    Ok(())
}
```

Read it as five refusals and one success. The two bounds checks
(`syscall.rs:108`–`113`) are INV-18 — *every access bounds-checks
`proc_id < MAX_PROCS` and `slot < CSPACE_SLOTS` before indexing.* The
empty check, the kind check, and the rights check are the actual
gate: the cap must exist, be the kind the caller expects, and carry
*every* bit in `required_rights` (the `& == required` idiom means a
`READ` requirement is not satisfied by a cap that only has `WRITE`).

Two design decisions in this function matter for the security story.
First, every failure collapses to the same `E_PERM`
(`syscall.rs:90`–`94`): userspace cannot distinguish "I don't hold the
cap" from "I asked for a slot that doesn't exist." Both are caller
errors with the same remedy — don't do that — and leaking which one it
was would hand an attacker a probe. Second, and this is INV-15
(forgery prevention), `check_cap` only ever *reads* a cap; it never
constructs one. The kernel reads cap data exclusively from its own
static memory, indexed by arguments it has already bounds-checked. A
WASM module passing hostile bytes into a host function cannot smuggle a
synthetic cap through, because there is no code path that turns bytes
into a `Cap`.

## How the first caps come to exist: boot minting

`check_cap` reads caps, but something has to write them first. That
something is `init_root_caps` in `kernel/src/cap/boot.rs:140`, called
once from `kmain` after the trap vector is installed and before any
signed WASM module loads.

Root caps are special: they have no parent. The helper that installs
them (`install_root_cap`, `boot.rs:380`) sets `parent = CapId::ROOT`,
`generation = 0`, `badge = 0` — a cap at the top of a derivation tree,
issued by the kernel itself, answerable to no parent cap. These are the
only capabilities in the system not derived from another. Every other
cap traces, through its parent chain, back to one of these boot mints.
That is INV-11 in practice: every reachable capability originates in a
kernel-authorized root.

The proc-id assignments are fixed in Phase 1b (`boot.rs:64`–`78`):

| proc_id | role |
|---|---|
| 0 | reserved (kernel-self) |
| 1 | Tier-2 UART driver |
| 2 | Tier-1 hello (instance A) |
| 3 | Tier-1 hello (instance B) |
| 4 | Tier-2 net driver |

And the slot layout is the direct source of the `check_cap` calls we
saw in Chapter 11. `init_root_caps` allocates two kernel-resident
endpoints — a UART IPC endpoint and a kernel-exit endpoint
(`boot.rs:144`–`145`) — and then seeds each module's CSpace:

- The **UART driver** (proc_id 1) gets an `Endpoint`/`READ` cap at slot
  0 (`boot.rs:157`–`164`) — the receive side of the UART endpoint. That
  is exactly the cap `host_mmio_write8` checks at `host_fns.rs:203`.
- **Tier-1 hello** (proc_id 2, and its twin at 3) gets an
  `Endpoint`/`WRITE` cap at slot 0 (stdout) and an `Endpoint`/`WRITE`
  at slot 1 (exit) — `install_tier1_caps`, `boot.rs:322`–`348`. Those
  are the caps `host_fd_write` (`wasi.rs:405`) and `host_proc_exit`
  (`wasi.rs:501`) check.

The two hello instances are the same WASM blob loaded into two separate
CSpaces (`boot.rs:177`–`187`), which is the whole point: it proves
CSpace isolation between two instances of identical code. Instance A
revoking a cap cannot touch instance B's, because they index into
different rows of the `CSPACES` array. Phase 2+ replaces this
hardcoded pair with a real spawn API; for now the pair *is* the
isolation test.

## Derivation and monotonicity

A process that holds a cap can hand a *weaker* copy to someone else by
minting. The pure core of mint is `Cap::derive`
(`wari-cap/src/types.rs:265`), and it enforces two invariants at the
same place they can be seen:

```rust
// INV-10: rights must be a subset of parent's.
if requested_rights & !parent.rights != 0 {
    return Err(KernelError::PermissionDenied);
}
Ok(Cap {
    // ...
    pool_index: parent.pool_index, // INV-16
    kind: parent.kind,             // INV-16
    rights: requested_rights,
})
```

INV-10 (`types.rs:280`) is **capability monotonicity**: a child's
rights are always a subset of its parent's. `requested & !parent.rights
== 0` is the algebraic statement of "you cannot grant what you do not
hold." Rights cannot be amplified through a chain of mints; the kernel
never produces a child stronger than its parent. INV-16
(`types.rs:291`–`292`) is **derivation-chain integrity**: the child
inherits `kind` and `pool_index` from the parent verbatim, so a mint
can never retarget the underlying object. These two together are why
revocation is sound — a walk following `parent`-equality finds every
descendant, and no descendant can hide by claiming a different object
than its ancestor.

The syscall wrapper `cap_mint_impl` (`syscall.rs:219`) does the
bounds-checking and slot bookkeeping, then calls `Cap::derive` for the
actual rights logic. Because `derive` is a pure function with no
`unsafe` and no statics, it is exhaustively unit-tested in `wari-cap`
(`types.rs:378`–`463`) and is a Kani proof target — the design ships the
proofs *as* the specification (`docs/cap-system-design.md` §8.3).

## The generation counter, and an honest bug

The nastiest failure mode in a capability system is ABA: a slot holds
cap C, C is deleted, the slot is refilled with an unrelated cap D, and
a child that was derived from C now points at a slot occupied by D and
mistakenly claims descent. Wari defeats this with a per-slot generation
counter (INV-17).

Every CSpace slot carries a 16-bit generation in the `generations`
array. `bump_generation` (`cspace.rs:153`) increments it —
`saturating_add`, so it never wraps — every time the slot transitions
occupied → empty → occupied. `cap_delete_impl` bumps it on delete
(`syscall.rs:386`–`387`); the revocation cascade bumps it on every slot
it clears. A child cap records, in its `parent` CapId, the *generation*
of the parent slot at mint time. When a revocation walk considers
whether a cap is a descendant of the target, it compares the recorded
generation against the parent slot's current generation — and if they
differ, the cap is an orphan of a previous occupant, not a descendant
of the current one, and is left alone.

The cascade itself lives in `wari-cap/src/revoke.rs` (the kernel binds
it to the static storage through a thin shim,
`kernel/src/cap/revoke.rs`, that passes `cspaces()` and
`object_pools()` in). It is a two-phase algorithm: a discovery phase
(`revoke.rs:251`) that grows a bitmap of every slot transitively
descended from the target, and a clear phase (`revoke.rs:294`) that
empties each, bumps its generation, and decrements the underlying
object's refcount — freeing the object when the count hits zero
(`dec_refcount`, `revoke.rs:143`). The anti-ABA check is the guard at
`revoke.rs:280`:

```rust
if cs[p_proc as usize].generations[p_slot as usize] != p_gen {
    continue; // orphaned — not a descendant of the current occupant
}
```

This is also the place to be honest about a bug the pre-extraction
kernel shipped, because the code comments preserve it deliberately
(`revoke.rs:244`–`249`). `CSPACE_SLOTS` is 256, which is exactly the
`u8` value space. An earlier version iterated the slot loop as `0..
CSPACE_SLOTS as u8` — and `256 as u8` truncates to `0`, so the range
was empty and the cascade never cleared *anything*. Revoke was a silent
no-op. The fix iterates as `usize` and casts each element to `u8`
inside the loop, and a regression test (`cascade_reaches_the_last_slot`,
`revoke.rs:444`) pins the last slot specifically. Extracting the
cascade into a host-testable crate is what surfaced it: the whole B-3
extraction program exists so logic like this can be tested against
synthetic CSpaces on the host, off the target. The book states it
plainly because a security chapter that only describes the intended
behavior is not worth reading — the value is in what the tests caught.

## Why a Tier-1 tenant cannot forge or reach

Pull the threads together. A malicious Tier-1 module wants either to
forge a capability or to reach one belonging to another tenant. Neither
is possible, and each is blocked structurally rather than by a check
that could be forgotten:

- **It cannot construct a `Cap`.** WASM manipulates only slot indices.
  The `Cap` type's only constructors are `empty()` and `derive()`, and
  neither is reachable from WASM — a module passes integers to host
  functions, and the kernel reads caps from its own memory (INV-15).
  There is no `transmute` from tenant bytes to a cap anywhere in the
  path.
- **It cannot name another tenant's CSpace.** The `proc_id` a host
  function uses is not an argument the module supplies. It is baked into
  the host-function closure at registration time — `register_wasi_host_fns`
  captures `proc_id` by `move` (`wasi.rs:103`), and every closure passes
  *that* captured id to the cap layer. A tenant literally cannot express
  "operate on process 3's slot"; the only CSpace its calls can reach is
  its own. The Tier-2 UART driver goes further and hardcodes
  `PROC_ID_TIER2_UART` in every binding (`host_fns.rs:84`).
- **It cannot index out of bounds.** Every entry point bounds-checks
  `slot < CSPACE_SLOTS` before touching the array (INV-18), and an
  out-of-range slot returns `E_PERM` indistinguishably from a missing
  cap.
- **It cannot escalate rights.** Even where a tenant legitimately mints,
  monotonicity (INV-10) caps the child at the parent's rights.

The isolation is not "the kernel remembers to check." It is that the
question a tenant is even *able to ask* is scoped to its own CSpace by
the shape of the closures, and the answer is computed from kernel
memory the tenant cannot write.

> **Built-vs-planned: the Net cap and INV-19.** INV-19
> (`docs/invariants.md`) and the `Net` `ObjectKind` doc comment
> (`wari-cap/src/types.rs:90`–`93`) state that `Net` is "driver-only"
> and that a Tier-1 process cannot hold a `Net` cap. *The current code
> does not honor that.* `init_root_caps` installs a `Net`
> (`READ`+`WRITE`) cap into both Tier-1 hello CSpaces at `SLOT_NET`
> (`boot.rs:229`–`230`, via `install_tier1_net_cap` at `boot.rs:356`),
> so the tenant can call `net_socket_create`. This was added with the
> Phase-1c socket demo (PR Net-6b) and is a knowing simplification —
> every demo tenant gets the same Net cap rather than a
> manifest-declared one. It is called out here because the doc and the
> code disagree, and in this book the running code is the source of
> truth: as of this writing, Tier-1 hello *does* hold a Net cap.
> Reconciling INV-19's text with the Phase-1c grant is open work.

## Who runs next?

The capability system decides what a process *may* do. It says nothing
about *when* it does it. The two hello instances share a blob and a set
of caps and run one after the other — but something has to choose the
order, load each one, run its `_start`, reap it on exit, and move to
the next. Something has to turn a timer interrupt into a decision.

That something is the scheduler, and the process table it drives. It
is Chapter 13.
