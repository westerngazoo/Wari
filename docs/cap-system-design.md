# Wari — Capability System Design (Phase 1b PR 0)

> **Status**: Design draft v1. **No code yet.** This document is the
> contract that PR 1, 2, and 3 implement. It is intentionally
> exhaustive: read once, sign off, then we write Rust.
>
> **Authors**: Gustavo Delgadillo + Wari project. Best-engineering-first
> mandate (no timeline pressure).
>
> **Audience**: Wari maintainer (Gustavo), future external auditor,
> formal verification reviewer (Phase 4+ academic collaboration).

---

## 1 · Goals and non-goals

### Goals (what this design must achieve)

1. **Replace the static `caps_for(Tier, ModuleId) -> Caps` table with a
   per-process, runtime-mutable capability system** that supports the
   four core operations: mint, copy, grant (transfer via IPC), revoke.
2. **Preserve and strengthen Wari's auditability story**: the cap
   system must be implementable in <2,000 LOC of safe Rust (excluding
   tests and Kani harnesses), every `unsafe` block must cite an INV-N,
   and the lookup-and-rights-check path must be ≤ 30 LOC.
3. **Implement seL4-style cap derivation**: every non-root cap has a
   parent; revoking a cap revokes every descendant transitively.
4. **Provide formal verification readiness**: every state-modifying
   operation has an explicit precondition, postcondition, and invariant
   list. Kani harnesses ship in the same PR as each operation.
5. **Enable Phase 1b's downstream work**: synchronous IPC via
   Endpoint caps, Tier-1 ↔ Tier-2 cross-tier calls via signed Endpoint
   caps, multi-tenant Tier-1 isolation via per-process CSpaces.

### Non-goals (deferred to later phases)

| Item | Why deferred |
|---|---|
| Distributed caps across multiple boards | Phase 3 (clustering) |
| Persistent caps surviving reboot | Phase 4 (immutable kernel) |
| Confidential caps (CoVE-encrypted) | Phase 4 |
| Multi-level CSpaces (nested CNodes) | Phase 2+ if 256-slot flat CNode proves insufficient |
| Performance optimization beyond linear lookup | Phase 2+ (256 slots × 16 bytes fits one cache line traversal) |
| TCB caps (capability over a thread control block) | Phase 2 — Phase 1b uses a hardcoded scheduler |
| AsidPool caps | Phase 2 — Phase 1b has one VAS per process, no AsidPool |
| IRQ handler caps | Phase 1c — Phase 1b uses static IRQ → process mapping |
| `sys_untyped_retype` advanced patterns (sub-allocation) | Phase 2 — Phase 1b retypes in fixed sizes |

This list is the explicit **scope fence**. Anything not on the goals
list and not in the kernel-objects catalog (§4.3) is out of scope and
gets rejected at review with reference to this section.

---

## 2 · Background and motivation

### What Phase 0 / 1a left us

`kernel/src/cap/static_caps.rs` defines a hand-written const-fn lookup
table:

```rust
pub const fn caps_for(tier: Tier, module_id: ModuleId) -> Caps {
    match (tier, module_id) {
        (Tier::Two, ModuleId::Tier2Uart) => TIER2_UART_DRIVER_CAPS,
        (Tier::One, ModuleId::Tier1Hello) => TIER1_DEFAULT_CAPS,
        _ => Caps::empty(),
    }
}
```

`Caps` itself is a 3-bit struct (`stdout`, `mmio_uart`, `exit`). The
docstring on this file is explicit: *"Phase 1 replaces this with a
per-process capability table backed by seL4-style derivation rules."*
This document is that replacement.

### Why we need more

1. **Multi-tenant Tier-1 cannot work with static caps.** Every Tier-1
   process needs its own cap set, mintable at spawn, distinct from
   peers, revocable on exit. A compiled-in match arm per ModuleId
   does not scale past the Phase 0 demo.
2. **Synchronous IPC requires Endpoint caps.** seL4-style sync IPC
   passes through an Endpoint kernel object; only holders of caps to
   that Endpoint can call/reply on it. Without caps as runtime objects,
   we cannot implement IPC.
3. **Tier-1 → Tier-2 calls need signed cap delegation.** When a
   Tier-1 process wants to write to UART, it must hold a cap to the
   Tier-2 UART driver's Endpoint. That cap is granted at Tier-1 spawn
   from the kernel's root-CSpace, with rights determined by the
   Tier-1 module's signed manifest (Phase 1b ties to INV-11).
4. **Process termination needs cascading revocation.** When a Tier-1
   process exits, every cap it held that was minted from another
   process's caps must be revoked, transitively. Static caps have no
   notion of derivation chain; nothing to revoke.

### Why seL4-style (and not a simpler model)

The user explicitly chose Option A from the design discussion ("seL4
puro: cap slots + derivation tree"). The choice was made knowing the
LOC cost (1500-2000 vs 250-400 for a simpler scheme) and the timeline
implication (Phase 1b shifts from 8 to 11-12 weeks).

The principle behind the choice: **Wari's value proposition is
auditability and formal-verification readiness**, not feature parity
with Linux. A simpler cap scheme would ship sooner but would have to
be replaced when Phase 4's external audit asks "where is the
derivation tree". seL4 already paid the formal-verification cost for
this design (Klein et al., SOSP 2009; Sewell et al., POPL 2013). We
inherit that work conceptually and align our structures with theirs
so Phase 4's verification effort can build on theirs.

### What we explicitly DO NOT inherit from seL4

seL4 is a research kernel with 11 years of design refinement. We
borrow the *concepts* (caps, CSpace, derivation, revocation, kernel
objects); we do **not** borrow the *implementations* (guarded page
tables for CSpaces, the Mapping Database doubly-linked list,
preemptable revoke, untyped retype with arbitrary alignment).

| seL4 has | Wari Phase 1b uses | Why |
|---|---|---|
| Multi-level guarded CSpace (CPtr is a path) | Single-level flat CSpace (CPtr is u8 index) | 256 caps per process is plenty for Phase 1b; Simplicity First |
| MDB (Mapping Database) doubly-linked list, depth-ordered | Implicit DAG via `parent: Option<CapId>` per cap | Recursive walk is O(n) but n ≤ 256 × num_procs; cheap and obvious |
| Preemptable revoke (long revokes can be interrupted) | Atomic revoke (kernel runs to completion) | Phase 1b kernel is single-hart; long revokes are tolerable |
| Untyped retype with size class | `sys_untyped_retype` only retypes in fixed object sizes | One less degree of freedom; fewer bugs |

The simplifications above are documented; if Phase 4's auditor asks
about a difference from seL4, this table is the answer.

---

## 3 · Conceptual model

A reader who has never seen capability systems should be able to
build a correct mental model from this section alone.

### 3.1 Capability

A **capability** is a kernel-issued reference to a kernel object,
plus a set of rights describing what the holder may do with that
object. It is unforgeable: only the kernel can construct one, and
userspace (Tier-1 / Tier-2 WASM) can never produce a `Cap` value
that the kernel did not derive.

In Wari, a `Cap` is a 16-byte value stored in a CSpace slot. The
holder process refers to it by **slot index** (a `u8` we call `CPtr`).
There is no userspace pointer, no userspace-visible address, no
forgeable handle.

### 3.2 CSpace

A **CSpace** is a process's capability table. In Wari Phase 1b, every
process has exactly one CSpace, which is a flat array of 256 slots:

```
Process(pid=42).cspace = [Slot 0, Slot 1, ..., Slot 255]
                          \________________________/
                                  256 × 16 bytes
                                = 4 KB (one page)
```

A slot is either **empty** (`Cap::empty()`) or contains a cap. The
process refers to its caps by index 0..255. Index 0 is reserved for
the kernel's "self-cap" and is read-only from userspace.

CSpace memory is allocated from the page allocator at process spawn
and freed at process exit (after revocation cascade — see §3.6).

### 3.3 Kernel object

A **kernel object** is a piece of kernel-managed state that
capabilities can refer to. Phase 1b ships exactly four object types:

| Object | Purpose | Allocated from |
|---|---|---|
| `Endpoint` | Synchronous IPC rendezvous point | Untyped retype |
| `Notification` | Asynchronous binary signal (semaphore-like) | Untyped retype |
| `Frame` | A 4 KB page of physical memory, mappable into a VAS | Untyped retype |
| `Untyped` | A pool of typed-as-`Untyped` memory, retypable into the above | Page allocator at boot |

Each object lives in a per-type pool. Caps refer to objects by
`(kind, pool_index)`, never by raw pointer.

### 3.4 Rights

A capability carries an 8-bit **rights bitmap**. Phase 1b uses 4 bits;
4 are reserved for Phase 2+ extensions:

| Bit | Name | Meaning |
|---|---|---|
| 0 | `READ` | May read object state (recv on Endpoint, read Frame contents) |
| 1 | `WRITE` | May modify object state (send on Endpoint, write Frame contents) |
| 2 | `GRANT` | May pass this cap to other processes via IPC |
| 3 | `GRANT_REPLY` | May pass via reply path of a synchronous IPC |
| 4-7 | Reserved | (Phase 2+: badge mutability, IRQ ack, CoVE-confidential, etc.) |

The four-bit space is chosen to match seL4's terminology so the audit
story aligns. `GRANT_REPLY` is distinct from `GRANT` because seL4
proved that conflating them creates a confused-deputy variant where
a server inadvertently delegates more than it intended.

### 3.5 Derivation

When a process holds cap `C₁` and wants to delegate a weaker form to
another process (or store a weaker copy in another local slot), it
**mints** a new cap `C₂` from `C₁`. The kernel records `C₂.parent =
C₁`, and enforces:

```
C₂.rights ⊆ C₁.rights         (rights monotonicity, INV-15)
C₂.object = C₁.object         (mint cannot retarget)
```

`C₁` is the **parent**, `C₂` the **child**. The set of caps with a
given root form a derivation **tree** (one parent per child, but a
parent may have many children). Multiple roots exist (one per
original kernel-issued cap to a kernel object).

Wari represents derivation by storing `parent_id: Option<CapId>` in
each cap, where `CapId = (proc_id, slot_index, generation)`. The
generation field protects against ABA when a slot is freed and reused
(INV-17).

### 3.6 Revocation

**Revoking** cap `C` invalidates `C` and every descendant of `C` in
the derivation tree. After revoke:

- `C`'s slot becomes empty (`Cap::empty()`).
- For every cap `D` in the kernel where `D` is a transitive descendant
  of `C` (`D.parent`-chain includes `C`'s `CapId`), `D`'s slot also
  becomes empty.
- Kernel objects whose only remaining caps were just revoked have
  their refcount decremented; objects reaching refcount=0 are returned
  to their containing Untyped pool.

Phase 1b implements revoke as an **atomic depth-first traversal**: the
kernel iterates every CSpace's every slot once, checking parent-chain
membership. Worst case O(num_procs × CSPACE_SLOTS) = O(256 × 16) =
O(4096) for a 16-process system. Fast enough for Phase 1b.

Phase 2+ may upgrade to seL4's MDB doubly-linked list for O(num
descendants) revoke. Not now.

### 3.7 Badging

An Endpoint cap may carry a 32-bit **badge** set at mint time. When a
sender uses a badged Endpoint cap to call/send, the receiver sees the
badge in the message header. This lets a server distinguish callers
without requiring them to authenticate explicitly: the cap *is* the
authentication, the badge is the caller-id.

The badge is set on mint and immutable thereafter. A child cap
inherits its parent's badge unless re-badged at mint time (which is
allowed only if the parent has rights bit 4 — reserved Phase 2+).

Non-Endpoint caps ignore the badge field.

---

## 4 · Concrete data structures

This section is the contract for PR 1's Rust types. PR 1 implements
exactly these; deviations require a follow-up PR to this design doc.

### 4.1 `Cap` — 16-byte capability

```rust
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap {
    /// Kind of kernel object this cap refers to. `ObjectKind::Empty`
    /// indicates an unused slot.
    pub kind: ObjectKind,           // 1 byte

    /// Index into the per-kind pool. `u16::MAX` for Empty.
    pub pool_index: u16,            // 2 bytes

    /// Rights bitmap. 4 bits used in Phase 1b; remainder reserved.
    pub rights: u8,                 // 1 byte

    /// 32-bit badge for Endpoint caps. Zero for other kinds.
    pub badge: u32,                 // 4 bytes

    /// Parent cap reference. `CapId::ROOT` for original kernel mints.
    pub parent: CapId,              // 4 bytes (proc_id:8 | slot:8 | gen:16)

    /// Generation counter for THIS slot. Incremented every time the
    /// slot is re-occupied. Protects against ABA when a child still
    /// references a parent slot that has been freed and reused.
    pub generation: u32,            // 4 bytes
}
```

Total: 16 bytes. `#[repr(C)]` ensures the layout is stable for
formal-verification tooling and for serialization in Phase 4 audit.

```rust
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Empty       = 0,
    Endpoint    = 1,
    Notification = 2,
    Untyped     = 3,
    Frame       = 4,
    // Phase 2+: Tcb = 5, AsidPool = 6, IrqHandler = 7, ...
}
```

```rust
/// Globally-unique reference to a cap slot.
///
/// Encoding: bits 0..7 = slot, 8..15 = proc_id, 16..31 = generation.
/// `CapId::ROOT` (= u32::MAX) marks original kernel-issued caps with
/// no parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapId(pub u32);

impl CapId {
    pub const ROOT: CapId = CapId(u32::MAX);
    pub const fn new(proc_id: u8, slot: u8, generation: u16) -> Self { ... }
    pub const fn proc_id(&self) -> u8 { ... }
    pub const fn slot(&self) -> u8 { ... }
    pub const fn generation(&self) -> u16 { ... }
    pub const fn is_root(&self) -> bool { self.0 == u32::MAX }
}
```

### 4.2 `CSpace` — per-process cap table

```rust
pub const CSPACE_SLOTS: usize = 256;

#[repr(C)]
pub struct CSpace {
    pub slots: [Cap; CSPACE_SLOTS],          // 4 KB

    /// Per-slot generation counters, separate from the `Cap` field
    /// for fast comparison during revocation walks.
    pub generations: [u16; CSPACE_SLOTS],    // 512 bytes
}
```

Each `CSpace` is allocated in its own page (4 KB) at process spawn.
The `generations` array is a small extension on top — total CSpace
memory is 4.5 KB but rounds to 8 KB pages. (Phase 2+ optimization:
pack the generation into the slot's `Cap.generation` field and free
the separate array. Punt for now.)

### 4.3 Kernel object catalog

```rust
/// Synchronous IPC rendezvous point.
pub struct Endpoint {
    /// Queue of senders waiting to deliver a message.
    pub senders: BoundedQueue<TcbRef, 8>,
    /// Queue of receivers waiting for a message.
    pub receivers: BoundedQueue<TcbRef, 8>,
    /// Refcount of caps pointing here. When zero, returned to Untyped pool.
    pub refcount: u16,
}

/// Asynchronous binary signal.
pub struct Notification {
    /// Bitmap of pending signals (one bit per badge).
    pub signals: u32,
    /// Receiver waiting for a signal, or None.
    pub waiter: Option<TcbRef>,
    pub refcount: u16,
}

/// A 4 KB physical page, mappable into a VAS.
pub struct Frame {
    pub pa: PhysAddr,
    pub refcount: u16,
}

/// A pool of typed-as-Untyped memory.
pub struct Untyped {
    pub pa: PhysAddr,
    pub size_bits: u8,             // 12 = 4 KB, 13 = 8 KB, ...
    pub watermark: usize,          // Bytes already retyped from this pool.
    pub refcount: u16,
}
```

`TcbRef` is a forward-declared placeholder for Phase 1b's hardcoded
scheduler integration. Once Phase 2 lands real TCB caps, this becomes
a `Cap` of `ObjectKind::Tcb`. For Phase 1b, it's a `(proc_id: u8)`
that the scheduler resolves directly.

`BoundedQueue<T, N>` is a fixed-size queue (no allocation in IPC fast
path). 8-deep is enough for Phase 1b's small process counts; if it
overflows, the sender/receiver returns `KernelError::EndpointQueueFull`
and userspace retries.

### 4.4 Per-kind object pools

Each kernel object kind has a fixed-capacity pool:

```rust
pub const ENDPOINT_POOL_CAPACITY: usize = 64;
pub const NOTIFICATION_POOL_CAPACITY: usize = 64;
pub const FRAME_POOL_CAPACITY: usize = 1024;       // 4 MB worth
pub const UNTYPED_POOL_CAPACITY: usize = 16;

pub struct ObjectPools {
    pub endpoints: Pool<Endpoint, ENDPOINT_POOL_CAPACITY>,
    pub notifications: Pool<Notification, NOTIFICATION_POOL_CAPACITY>,
    pub frames: Pool<Frame, FRAME_POOL_CAPACITY>,
    pub untypeds: Pool<Untyped, UNTYPED_POOL_CAPACITY>,
}
```

`Pool<T, N>` is a fixed-size slab allocator with a free-list:

```rust
pub struct Pool<T, const N: usize> {
    pub slots: [Option<T>; N],
    pub free_head: Option<u16>,
    pub free_next: [u16; N],         // intrusive free-list
}
```

Pool sizes are deliberately small. They cap the system's resource
footprint and make every object fit in one cache-friendly array.
Phase 2+ may grow them or migrate to dynamic allocation; today they
are constants in `cap::config`.

### 4.5 Global cap state

The kernel's per-process cap state lives in:

```rust
pub static CSPACES: [CSpace; MAX_PROCS] = ...;        // one per proc
pub static OBJECT_POOLS: ObjectPools = ...;           // shared
```

`MAX_PROCS` is currently 16 in Phase 1b (Phase 2+ may grow). All cap
state is `static mut` on a single-hart kernel — INV-1 covers the lack
of synchronization.

---

## 5 · Syscall ABI

Phase 1b adds five capability-management syscalls plus two IPC
syscalls that consume cap arguments. All follow the existing
`abi-shared` convention (sysnum + 6 register args, return in `a0`).

### 5.1 `SYS_CAP_MINT` — derive a child cap

```
sysnum = 16
a0  = parent_slot:    u8           (CSpace slot of parent cap)
a1  = target_slot:    u8           (CSpace slot to receive new cap)
a2  = rights:         u8           (must satisfy child ⊆ parent.rights)
a3  = badge:          u32          (new badge; ignored for non-Endpoint)
a4  = (reserved)
a5  = (reserved)
ret = 0 on success, negative SyscallError otherwise
```

**Errors**:
- `E_PERM` if `parent_slot` is empty or rights monotonicity violated.
- `E_INVAL` if `target_slot` is occupied or out of range.
- `E_INVAL` if `parent` is `Untyped` and rights includes `WRITE` but
  not `READ` (Untyped requires both for retype).

**Postcondition**: `target_slot` contains a cap with the same kind
and pool_index as `parent_slot`'s cap, with the requested rights and
badge, with `parent = parent_slot's CapId`, generation incremented.

### 5.2 `SYS_CAP_COPY` — same-rights copy without derivation

```
sysnum = 17
a0  = src_slot:       u8
a1  = target_slot:    u8
a2  = (reserved)
a3  = (reserved)
ret = 0 on success, negative SyscallError otherwise
```

A copy is **not** a derivation: `target.parent = src.parent` (the
copy is a sibling, not a child). This is needed when a process wants
two slot-references to the same cap (e.g., to pass via IPC while
keeping its own).

**Errors**: same shape as MINT.

### 5.3 `SYS_CAP_REVOKE` — revoke + cascade

```
sysnum = 18
a0  = slot:           u8
a1  = (reserved)
ret = 0 on success, negative SyscallError otherwise
```

**Effect**: the cap in `slot` is removed. Every cap in the kernel
whose `parent`-chain includes the revoked cap's `CapId` is also
removed. Kernel objects whose refcount drops to zero are returned to
their pool.

**Errors**:
- `E_PERM` if the cap does not have `WRITE` rights (revoke is a
  privileged operation; not every holder may revoke).
- `E_INVAL` if `slot` is empty.

### 5.4 `SYS_CAP_DELETE` — delete one cap, no cascade

```
sysnum = 19
a0  = slot:           u8
ret = 0 on success, negative SyscallError otherwise
```

Removes the cap in `slot` without affecting descendants. Used during
process exit cleanup, where the kernel revokes from-roots and then
deletes leftover slot entries. Userspace also uses this to free its
own caps without cascading.

### 5.5 `SYS_CAP_LOOKUP` — read cap metadata

```
sysnum = 20
a0  = slot:           u8
a1  = out_buf_ptr:    u32          (Tier-1: WASM linear-mem offset)
ret = 0 on success, writes (kind, rights, badge) to out_buf
```

Returns metadata about a cap: kind, rights, badge. Does NOT return
`pool_index` or `parent` — those are kernel-internal. Userspace can
verify "do I have rights X on slot Y" but cannot inspect the
derivation chain or kernel object pointers.

### 5.6 IPC syscalls (preview — formal spec in Phase 1b PR 4)

```
SYS_ENDPOINT_SEND  (sysnum 21):   send msg + transfer caps
SYS_ENDPOINT_RECV  (sysnum 22):   recv msg + receive caps
```

These are out of scope for THIS design doc (cap system) but the cap
system must support them. Specifically:

- `SEND` accepts a list of up to 4 source slots whose caps are
  transferred (copy-with-derivation) into the receiver's CSpace.
- `RECV` declares a "receive window" of up to 4 empty target slots.
  If the sender transferred fewer caps than the window, the unused
  target slots remain empty.

Cap transfer in IPC is a `SYS_CAP_MINT` operation across processes,
with the kernel as the broker. Source process's `GRANT` right is
required.

### 5.7 ABI summary table

| Sysnum | Name | Args | Returns | INV-N's checked |
|---|---|---|---|---|
| 16 | `SYS_CAP_MINT` | (parent_slot, target_slot, rights, badge) | 0 / SyscallError | INV-15, INV-18 |
| 17 | `SYS_CAP_COPY` | (src_slot, target_slot) | 0 / SyscallError | INV-18 |
| 18 | `SYS_CAP_REVOKE` | (slot) | 0 / SyscallError | INV-16, INV-17 |
| 19 | `SYS_CAP_DELETE` | (slot) | 0 / SyscallError | INV-18 |
| 20 | `SYS_CAP_LOOKUP` | (slot, out_buf) | 0 / SyscallError | INV-18 |
| 21 | `SYS_ENDPOINT_SEND` | (slot, msg_ptr, msg_len, cap_slots) | 0 / SyscallError | INV-15, INV-19 |
| 22 | `SYS_ENDPOINT_RECV` | (slot, recv_window) | 0 / SyscallError | INV-18 |

---

## 6 · Invariants

The cap system introduces five new invariants and significantly
expands two reserved ones. Every `unsafe` block in PR 1/2/3 must cite
one or more of these.

### INV-10 · Capability Monotonicity *(expanded from Phase 1b reservation)*

> A child cap's rights are a subset of its parent's. For any cap `C`
> with `C.parent != ROOT`, let `P` be the cap referenced by
> `C.parent`. Then `C.rights & !P.rights == 0`.
>
> The kernel enforces this at every mint operation; no syscall path
> can produce a cap that violates the relation. Userspace, regardless
> of whether it is Tier-1 or Tier-2, cannot construct a `Cap` value
> directly — only by syscall request.

**Consequence**: rights cannot be silently amplified through a chain
of mints. The audit story for Phase 4: a static analysis of every
mint site verifies the `rights & !parent.rights == 0` check.

**When this breaks**: never legitimately. Any code path that produces
a cap without going through the mint check is a soundness bug.

### INV-11 · Tier-2 Grants Are Signed *(expanded from Phase 1b reservation)*

> A Tier-2 module's initial CSpace is populated only from the caps
> declared in its signed manifest (`runtime::sign::verify` already
> establishes this gate for the Tier-2 binary itself; Phase 1b
> extends it to the cap manifest).
>
> Tier-1 modules are similarly populated, but their cap manifest is
> compiled into the kernel at Phase 1b (one manifest per known
> ModuleId). Phase 2+ moves Tier-1 manifests to signed distribution.

**Consequence**: every cap reachable by Tier-2 traces back, via
parent-chain or IPC delegation, to a kernel-issued root cap that was
authorized by signature.

**When this breaks**: if a manifest signature check is bypassed.
Defended by INV-13 (existing) at the binary level.

### INV-15 · Capability Forgery Prevention *(new, Phase 1b)*

> No userspace code path produces a `Cap` value that the kernel did
> not construct. The `Cap` type is `pub` so consumers can read it,
> but only `kernel::cap` module functions construct one (every
> constructor is `pub(crate) fn` or `pub(in cap) fn`, never `pub fn`).

**Consequence**: a Tier-1 or Tier-2 WASM module passing untrusted
bytes to a syscall cannot smuggle a synthetic cap; the kernel only
ever reads cap data from its own static memory, indexed by syscall
arguments that are themselves bounds-checked.

**Enforcement**: by Rust's privacy rules + a Kani harness in PR 1
that shows no public function with a non-`Cap` return type
constructs one.

**When this breaks**: a `mem::transmute<[u8; 16], Cap>` slipping past
review. Caught by `unsafe` audit (every `transmute` requires INV-N
citation).

### INV-16 · Derivation Chain Integrity *(new, Phase 1b)*

> For every non-root cap `C`, the cap referenced by `C.parent`
> exists, has the same `kind` as `C`, has rights that are a superset
> of `C.rights`, and has the same `pool_index` as `C` (mint cannot
> retarget the underlying object).

**Consequence**: revocation is sound: a depth-first walk from `C`
following `parent`-equality finds every descendant; no descendant can
escape revocation by having a "broken" parent pointer to a non-
existent cap.

**Enforcement**: on every mint, the kernel constructs the parent
linkage atomically. On every revoke, the kernel atomically clears
the descendant slot. No userspace path mutates `parent`.

**When this breaks**: SMP, where a mint and a revoke could race. INV-1
covers Phase 1b; the Phase 2+ SMP migration revisits this invariant.

### INV-17 · Generation-Counter Anti-ABA *(new, Phase 1b)*

> When a CSpace slot is freed (by `delete` or by being a revocation
> target) and then refilled, its generation counter is incremented.
> A cap stored elsewhere referencing the old generation via `parent`
> will be considered orphaned and cleaned up at the next revoke walk.

**Consequence**: a child cap whose parent slot has been re-occupied
with an unrelated cap does not "inherit" from the new occupant.

**Enforcement**: the slot's generation counter (in
`CSpace.generations[i]`) is monotone increasing, only the kernel
writes it, and every parent-chain walk compares both `proc_id` and
`generation`.

**When this breaks**: generation counter overflow (16 bits = 65,536
re-occupations of the same slot before wraparound). Caught by
saturating arithmetic + a Kani harness asserting that wraparound
returns `E_INVAL` instead of looping.

### INV-18 · CSpace Slot Index Bounds *(new, Phase 1b)*

> Every syscall that takes a slot argument validates `slot <
> CSPACE_SLOTS` before dereferencing. No path indexes a CSpace with
> an unchecked u8.

**Consequence**: `CSpace.slots[i]` is always sound (`i < 256`).

**Enforcement**: the syscall trampoline (Phase 1b PR 1's `cap_lookup`
helper) does the bounds check once; downstream consumers receive
`Option<&Cap>` or `Result<&Cap>`.

**When this breaks**: `CSPACE_SLOTS` ever increases past 256. Then
the slot type widens past `u8`; ABI breaking change requiring a
versioned syscall set.

### INV-19 · Tier-Shape Compatibility *(new, Phase 1b)*

> A Tier-1 process cannot hold a cap to a kernel object kind that is
> Tier-2-only. Phase 1b has no Tier-2-only kinds yet (Endpoint,
> Notification, Untyped, Frame are all kind-agnostic from a cap
> perspective; Tier-2-ness applies to *Tier-2 module loading*, not
> to cap objects). The invariant is reserved against Phase 2+ when
> Tier-2-only kinds appear (e.g., `IrqHandler`, `MmioWindow`).

**Consequence**: a Tier-1 module cannot, today or in the future,
acquire a cap whose mint path is gated on "caller is Tier-2".

**Enforcement**: per-kind mint paths inspect the caller's tier (via
`Process::tier`) and refuse if the kind is incompatible. Phase 1b's
mint path is uniform across kinds; the check is a no-op today, and
shaped to grow.

**When this breaks**: an attacker minting a Tier-2-only kind from a
Tier-1 process. Caught structurally.

### Summary of all cap-system invariants

| INV | Name | Phase | Status after PR 3 |
|---|---|---|---|
| 10 | Capability Monotonicity | 1b | Active, enforced at mint |
| 11 | Tier-2 Grants Are Signed | 1b | Active, enforced at load |
| 15 | Capability Forgery Prevention | 1b | Active, enforced by Rust privacy + Kani |
| 16 | Derivation Chain Integrity | 1b | Active, enforced at mint + revoke |
| 17 | Generation-Counter Anti-ABA | 1b | Active, enforced at slot reuse |
| 18 | CSpace Slot Index Bounds | 1b | Active, enforced at every syscall |
| 19 | Tier-Shape Compatibility | 1b (reserved) | Reserved, no enforcement until Phase 2+ |

PR 1's commit message includes the INV-N expansions to
`docs/invariants.md` so the catalog stays in sync.

---

## 7 · Migration from static caps

The current static system (`caps_for(Tier, ModuleId) -> Caps`) does
not disappear in PR 1. It becomes the **source of the kernel's root
cap mints during boot**.

### 7.1 Boot-time root cap construction

At boot, the kernel:

1. Allocates the global `OBJECT_POOLS` (PR 2 work).
2. For each known module, reads its `caps_for` static struct.
3. Translates each boolean cap field into a kernel-issued root cap
   in the module's CSpace:

   | Static `Caps` field | Phase 1b kernel object | Mint operation |
   |---|---|---|
   | `stdout: true` | Endpoint cap to `KERNEL_STDOUT_EP` (a kernel-resident endpoint that drives `kprintln!`) | Root mint, rights = WRITE |
   | `mmio_uart: true` | Endpoint cap to the Tier-2 UART driver's IPC endpoint | Root mint, rights = WRITE+GRANT |
   | `exit: true` | Endpoint cap to `KERNEL_EXIT_EP` (a kernel-resident endpoint that triggers process exit) | Root mint, rights = WRITE |

4. Boots Tier-2 driver with its CSpace populated; it can then receive
   IPC from Tier-1.

The legacy `Caps` struct is preserved as the **specification of
which root caps a module gets**, but the runtime path goes through
the new system. `Caps` becomes a manifest type, not a runtime check.

### 7.2 Removal of `host_fns.rs` direct MMIO check

After PR 3 lands, `host_fns::host_mmio_write8` no longer reads
`host.caps.mmio_uart` (the Phase 0 boolean check). Instead:

1. Tier-1 calls `wari::write_uart` (a host fn that becomes an IPC
   send to the Tier-2 UART driver's Endpoint).
2. The IPC is gated on Tier-1 holding a cap to that Endpoint.
3. The Tier-2 driver, upon receiving the IPC, performs the MMIO
   write — gated only by `is_uart_mmio_addr` (INV-3 still applies)
   because Tier-2 holding the Endpoint cap is itself the
   authorization.

INV-13 (Tier-2 bytecode signed) and INV-3 (MMIO address narrowing)
both stay in force. INV-15-18 add the cap layer above them.

### 7.3 Static `caps_for` retirement timeline

| PR | Action |
|---|---|
| 1 | `caps_for` unchanged; new cap system lives alongside |
| 2 | Boot-time root cap construction reads `caps_for` and produces real caps |
| 3 | Tier-1 → Tier-2 calls go through IPC; old `caps.mmio_uart` checks deleted from `host_fns.rs` |
| Phase 1b PR 4 (out of scope here) | `Caps` struct retired entirely, replaced by signed manifest format |

---

## 8 · Test plan

Every PR ships its own test plan. The bar: a reviewer can run the
listed commands and know the moment the work is complete (Goal-Driven
Execution, principle 4).

### 8.1 Unit tests (host-side, `cargo test --workspace`)

Per type / per syscall. Specifically:

- `Cap::empty()` returns a slot-with-no-cap.
- `Cap::is_empty()` ↔ `Cap::kind == ObjectKind::Empty`.
- `CapId::new(p, s, g).proc_id() == p`, etc.
- `cap::lookup(cspace, slot)` returns `None` for empty slot, `Some(&cap)` for occupied.
- `cap::mint(parent, target_slot, rights)` rejects rights ⊄ parent.rights.
- `cap::mint` rejects target_slot already occupied.
- `cap::revoke` clears all descendants.
- `cap::revoke` decrements object refcount.
- `cap::delete` does NOT cascade.
- Generation counter increments on every slot reuse.

### 8.2 Adversarial tests (`tests/security/cap_*.rs`)

Per the testing.md "every trust-boundary-crossing feature has an
adversarial test" rule:

- `tier1_forge_cap.rs` — Tier-1 WASM tries to invoke a syscall with a
  slot index >256. Expect: `E_INVAL`, kernel survives.
- `tier1_amplify_rights.rs` — Tier-1 mints with rights superset of
  parent. Expect: `E_PERM`, parent unchanged.
- `tier1_orphan_parent.rs` — Tier-1 deletes a slot, then tries to
  use a previously-minted child. Expect: at next revoke walk, child
  is cleaned up (via generation mismatch).
- `tier1_revoke_others.rs` — Tier-1 calls `SYS_CAP_REVOKE` on a slot
  in another process's CSpace. Expect: `E_INVAL` (slot out of own
  CSpace; the syscall takes a u8 in own space, not a CapId).
- `tier1_revoke_no_write.rs` — Tier-1 holds a READ-only cap, calls
  REVOKE. Expect: `E_PERM`.
- `cap_double_revoke.rs` — process revokes, then revokes the same
  slot. Expect: second call is `E_INVAL`.
- `endpoint_send_no_grant.rs` — Tier-1 tries IPC send while passing a
  cap it lacks GRANT rights on. Expect: `E_PERM`, no cap transferred.
- `revocation_cascade_depth_64.rs` — build a derivation chain of
  depth 64, revoke root, verify all 64 are cleared.

### 8.3 Property tests (Kani harnesses, in same PR as code)

PR 1 ships harnesses asserting:

```rust
#[kani::proof]
fn mint_preserves_rights_monotonicity() {
    let parent: Cap = kani::any();
    let req_rights: u8 = kani::any();
    kani::assume(parent.kind != ObjectKind::Empty);
    kani::assume(req_rights & !parent.rights == 0);  // precondition

    let child = Cap::derive(parent, req_rights, 0);
    assert!(child.rights & !parent.rights == 0);     // INV-10
    assert!(child.kind == parent.kind);               // INV-16
    assert!(child.pool_index == parent.pool_index);   // INV-16
}

#[kani::proof]
fn slot_index_bounded() {
    let slot: u8 = kani::any();
    let result = cap_lookup(slot);
    assert!(slot as usize <= CSPACE_SLOTS);           // INV-18
}

#[kani::proof]
fn revoke_cascade_terminates() {
    // Construct a derivation chain of bounded length.
    let chain = build_chain(kani::any_with_bound::<u8>(64));
    revoke(chain.root());
    assert!(chain.all_descendants_empty());
}

#[kani::proof]
fn generation_counter_strict_monotonic() {
    let cspace: CSpace = kani::any();
    let slot: u8 = kani::any_with_bound(CSPACE_SLOTS as u8);
    let pre = cspace.generations[slot as usize];
    let _ = delete(slot);
    let _ = mint(parent_slot, slot, rights, badge);
    let post = cspace.generations[slot as usize];
    assert!(post > pre);                              // INV-17
}
```

PRs 2 and 3 add harnesses for their own surface (object pools,
revocation cascade, IPC cap transfer).

### 8.4 Integration tests (in QEMU)

After PR 3:

- Boot Wari, observe Tier-2 UART driver loaded via real cap mint
  (banner + UART output unchanged).
- Tier-1 hello calls `fd_write` → IPC → Tier-2 driver → MMIO →
  `Hello from Wari` on UART.
- Spawn two Tier-1 processes; verify each has its own CSpace; verify
  one revoking its UART cap does not affect the other.

---

## 9 · PR breakdown

Three implementation PRs, plus this design PR. Each is independently
testable, mergeable, and reviewable.

### PR 0 (this document)

**Files**: `docs/cap-system-design.md` only.

**Effort**: ~1 session of writing + 1-2 sessions of iteration with
Gustavo on review. No code.

**Exit gate**: Gustavo's sign-off ("designs are correct, build
this").

### PR 1 — Cap primitive + CSpace + lookup

**Scope**: types and pure functions, no syscalls yet.

**Files added/modified**:
- `kernel/src/cap/mod.rs` (extended; current `pub use static_caps::*`
  becomes `pub mod static_caps; pub use ...; pub mod cap_dynamic;`)
- `kernel/src/cap/cap.rs` (new): `Cap`, `CapId`, `ObjectKind` types
- `kernel/src/cap/cspace.rs` (new): `CSpace` type, `lookup`, `is_occupied`
- `kernel/src/cap/derive.rs` (new): pure-function `Cap::derive`
- `kernel/src/cap/tests/` (new): unit tests
- `kernel/src/cap/proofs.rs` (new): Kani harnesses
- `docs/invariants.md` (modified): INV-10/15/16/17/18 expanded/added

**Estimated LOC**:
- Code: ~400
- Tests: ~250
- Proofs: ~150
- Total: ~800

**Exit gate**:
- `cargo test --workspace` green (unit tests pass)
- `cargo kani --harness mint_preserves_rights_monotonicity` proves
- `cargo kani --harness slot_index_bounded` proves
- `cargo kani --harness generation_counter_strict_monotonic` proves
- INV catalog updated

### PR 2 — Kernel objects + boot-time root cap construction

**Scope**: object pools, kernel objects, boot integration replacing
`caps_for`-as-runtime-check with `caps_for`-as-manifest.

**Files added/modified**:
- `kernel/src/cap/objects/mod.rs` (new)
- `kernel/src/cap/objects/endpoint.rs`, `notification.rs`,
  `untyped.rs`, `frame.rs` (new)
- `kernel/src/cap/pool.rs` (new): `Pool<T, N>`
- `kernel/src/cap/boot.rs` (new): `init_root_caps`
- `kernel/src/main.rs` / `kernel/src/boot.rs` (modified): call
  `cap::boot::init_root_caps` in stage_runtime
- Tests + proofs added

**Estimated LOC**:
- Code: ~500
- Tests: ~200
- Proofs: ~100
- Total: ~800

**Exit gate**:
- All PR 1 gates still green
- New unit tests for object pools pass
- Kernel boots in QEMU, banner unchanged, Tier-2 UART driver loads,
  Tier-1 hello runs (cap-mediated this time)

### PR 3 — Mint / copy / revoke / delete syscalls + IPC cap transfer

**Scope**: the cap-management syscalls + endpoint send/recv that
consume caps.

**Files added/modified**:
- `kernel/src/cap/syscall.rs` (new)
- `kernel/src/cap/revoke.rs` (new): cascade walk
- `kernel/src/abi/cap.rs` (new): SYS_CAP_* constants in abi-shared
- `kernel/src/runtime/host_fns.rs` (modified): UART IPC replaces
  direct MMIO check
- `tests/security/cap_*.rs` (new): adversarial tests per §8.2
- Tests + proofs added

**Estimated LOC**:
- Code: ~600
- Tests: ~400 (adversarial-heavy)
- Proofs: ~150
- Total: ~1150

**Exit gate**:
- All PR 1+2 gates still green
- All adversarial tests pass (§8.2 list)
- `revoke_cascade_terminates` Kani proof passes
- Integration test (§8.4) passes: Tier-1 hello reaches UART via IPC

### Cumulative estimate

~2,750 LOC across 3 implementation PRs (vs. the rough 1,500-2,000
range from the option-A discussion — closer to the high end because
adversarial tests add ~25% on top of code).

This is **the largest single subsystem in Wari to date.** Phase 0's
runtime + loader was ~1,200 LOC; the cap system rivals the entire
Phase 0 scope. That is appropriate: capabilities are the single most
load-bearing piece of any sovereign-OS thesis, and Wari's value
proposition rises or falls on this design being right.

---

## 10 · Open questions

Items where the design is not yet final. Resolved in review with
Gustavo before PR 1 starts.

1. **`MAX_PROCS = 16` for Phase 1b?** Or 32, or 64? Each proc costs
   ~5 KB of static state (CSpace + bookkeeping). 16 × 5 = 80 KB — fits
   in our 4 MB heap easily. Suggest 16 unless there's a specific
   reason for more.

2. **Pool capacities — fixed vs configurable?** Currently constants
   (`ENDPOINT_POOL_CAPACITY = 64`). Phase 2+ may want compile-time
   config. Phase 1b: hardcode and document.

3. **`BoundedQueue` size 8 in Endpoints — empirically validated?** No
   actual workload exists yet. 8 is a guess. Monitor in Phase 1b
   testing; bump if observed overflow.

4. **CapId encoding**: 8+8+16 bits (proc:8, slot:8, gen:16) or
   10+8+14? With `MAX_PROCS=16`, 4 bits for proc is enough; the
   other 4 bits could go to generation. Suggest leaving 8+8+16
   because the symmetry helps debugging and 16 bits of generation is
   plenty. Decide at PR 1.

5. **Should `SYS_CAP_LOOKUP` include the cap's parent?** No
   (security: hides derivation chain from userspace). But debugging
   would benefit. Phase 2+ adds a debug-only `SYS_CAP_INSPECT_DEBUG`
   that's gated on a kernel-only debug cap.

6. **`SYS_CAP_REVOKE` requires `WRITE` on the cap — should it also
   require some "delegator" right?** seL4 has the concept of
   "mintable rights" being a separate right (so a holder can grant
   but not revoke). Phase 1b: simplify, WRITE = revoke. Phase 2+
   reconsiders if real workloads complain.

7. **What about caps to frames currently mapped into a VAS?**
   Revoking a frame cap should also unmap. PR 2's frame integration
   handles this — open question is whether unmap-on-revoke is
   atomic from userspace's view. Suggest: yes (kernel performs both
   under INV-1's single-hart assumption).

---

## 11 · Prior art consulted

This section is the **citations the auditor will check**. Every
non-obvious design decision should trace to one of these.

### Primary

- **Klein et al., "seL4: Formal verification of an OS kernel",
  SOSP 2009.** The capability system, the derivation tree concept,
  the proof obligations. Wari's INV-10/15/16 are direct
  generalizations of seL4's mint-rights theorem.
  https://dl.acm.org/doi/10.1145/1629575.1629596

- **Sewell et al., "Translation Validation for a Verified OS Kernel",
  POPL 2013.** How seL4's C-to-binary verification works. Relevant
  for Phase 4 when we attempt similar for Wari.

- **Elphinstone & Heiser, "From L3 to seL4 - What Have We Learnt in
  20 Years of L4 Microkernels?", SOSP 2013.** Design lineage of the
  capability model from L4's earlier endpoints.

### Secondary

- **Hubris (Oxide Computer Company)** — Rust embedded microkernel.
  Their task model uses static cap allocation and does not implement
  derivation. Wari's design is closer to seL4 than Hubris because
  Wari's tenancy model (multi-tenant Tier-1) requires runtime cap
  flexibility Hubris's single-task-per-purpose model does not.
  https://hubris.oxide.computer/

- **Genode** — capability-based OS framework. Their CSpace model is
  hierarchical; we adopt their "every kernel object is referenced by
  cap" discipline but flatten the CSpace shape.

- **CHERI architecture (UCAM)** — hardware-enforced capability bounds
  via tagged pointers. Wari's caps are software-enforced (kernel-only
  construction); CHERI's hardware enforcement is complementary and
  worth revisiting at Phase 4 (alongside CoVE).

### Internal

- `docs/prior-art.md` — Wari project's existing inheritance-and-
  rejection table. seL4 is already credited there for "capability
  system + synchronous IPC + formal verification ambition".
- `docs/invariants.md` — current INV catalog. INV-10/11 reservations
  came from this design's predecessor outline.
- `kernel/src/cap/static_caps.rs` — the Phase 0 implementation this
  design replaces.
- `docs/security-model.md` — threat model that this design must
  satisfy (Tier-1 untrusted, Tier-2 signed-but-isolated, Tier-0
  trusted).

---

## 12 · Decision log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-04-26 | Adopt seL4-style cap system (Option A from design discussion) | Best engineering first; preserves auditability and formal-verification readiness |
| 2026-04-26 | Flat single-level CSpace (256 slots, 1 page) | Simplicity First; multi-level only when needed |
| 2026-04-26 | Implicit DAG via `parent: Option<CapId>` instead of MDB | Recursive walk is O(n*m) but n*m ≤ 4096 in Phase 1b — fast enough |
| 2026-04-26 | Four kernel object kinds in Phase 1b (Endpoint, Notification, Untyped, Frame) | Minimum viable for IPC + memory mgmt; TCB/AsidPool/IRQ deferred |
| 2026-04-26 | 16-byte cap layout, `#[repr(C)]` | Stability for formal verification; cache-line friendly |
| 2026-04-26 | Generation counter for ABA protection (INV-17) | Standard technique; 16 bits = 65k reuses before wraparound, plenty for Phase 1b |
| 2026-04-26 | Kani harnesses ship in same PR as code, not deferred | Best engineering first; the proofs are the spec |
| 2026-04-26 | Three implementation PRs (cap+lookup / objects+boot / syscalls+IPC) | Each independently reviewable; cumulative ~2,750 LOC |
| TBD | `MAX_PROCS` final value | Pending §10 question 1 |
| TBD | Pool capacity tuning | Pending §10 question 2 + Phase 1b workload measurement |

---

## Appendix A · Glossary

| Term | Definition |
|------|------------|
| **Cap** / **Capability** | A 16-byte kernel-issued reference to a kernel object plus rights bitmap. Stored in a CSpace slot. |
| **CSpace** | A process's capability table. 256 slots × 16 bytes = 4 KB per process in Phase 1b. |
| **CPtr** | The slot index referencing a cap. In Phase 1b, a `u8` (0..255). |
| **Kernel object** | A piece of kernel-managed state that caps can refer to: Endpoint, Notification, Untyped, Frame. |
| **Mint** | Derive a child cap from a parent with possibly-reduced rights. |
| **Copy** | Same-rights, same-parent duplication of a cap into another slot. |
| **Grant** | Transfer a cap to another process via IPC. Requires `GRANT` right. |
| **Revoke** | Invalidate a cap and every descendant in the derivation tree. |
| **Delete** | Invalidate one cap without cascading. |
| **Derivation tree** | The DAG formed by `parent` pointers from minted caps to their parents. |
| **Badge** | 32-bit caller-id stored in an Endpoint cap, set at mint, immutable thereafter. |
| **Untyped** | A pool of memory typed as untyped, retypable into Endpoint/Notification/Frame. |
| **Generation counter** | Per-slot counter incremented on slot reuse. Prevents ABA attacks (INV-17). |
| **Refcount** | Per-kernel-object counter of caps pointing at it. Object freed when refcount=0. |

---

## Appendix B · Why this is the right shape (the elevator pitch)

A reviewer skimming this doc looking for the "why" should land here:

> **Wari's capability system is the kernel's permission layer.**
> Every privileged action in Wari — IPC, memory mapping, MMIO access,
> process spawn — is gated on the calling process holding the right
> capability with sufficient rights. Capabilities are unforgeable
> (only the kernel mints them), tracked in a per-process table, and
> revocable transitively (revoking a parent revokes every child).
>
> This is the seL4 model, condensed to the four object kinds Wari
> needs in Phase 1b, with simplifications (flat CSpace, atomic
> revoke) appropriate to Wari's single-hart kernel and 8-KLOC TCB
> target.
>
> The design's success criterion is: a Phase 4 external auditor
> reads this doc, reads the INV catalog, reads the Kani proofs, and
> can sign off on the cap-system soundness without reading every
> line of Rust. That is what "auditable in <1 week by a team of 3"
> means in practice.

---

*End of design draft v1. Review and sign-off needed before PR 1
starts.*
