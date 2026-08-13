<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Wari — AOT Target ABI (RFC)

> **Status:** RFC for architect approval — roadmap task **G5**, build-plan
> step **D1**. Companion to [`aot-build-plan.md`](aot-build-plan.md) (the
> dependency-ordered plan; D1 is the contract this pins),
> [`aot-parallel-roadmap.md`](aot-parallel-roadmap.md) (§4 G5 spec, decision
> gates DG-1/DG-2/DG-3) and
> [`aot-safety-cert-design.md`](aot-safety-cert-design.md) (the M2 checker
> that must be able to *see* every property this ABI promises).
>
> **This document exists to let Gustavo answer DG-2** (memory-safety model:
> guard pages vs. explicit bounds checks). §2 is written so that decision is
> answerable from this file alone.
>
> **Nothing here is measured yet.** The M0 gate (G1 bench + G4 spike) has
> not run. Every cost claim below is a *direction*, never a magnitude; where
> a decision genuinely depends on a number we do not have, §8 says so
> explicitly rather than inventing one.

---

## 0 · Scope

This RFC pins the contract that AOT-compiled native RV64 code is emitted
*against*. It is the shared contract between two consumers:

- **`tools/wari-aot`** (G6/M1, the AOT lane) — emits code that obeys it;
- **the kernel loader** (M3, the *kernel* lane, explicitly out of scope for
  the AOT track) — maps, relocates and enters code that obeys it.

Four sections, each with options, trade-offs and a boxed recommendation:

| § | Question | Feeds |
|---|----------|-------|
| 2 | Linear-memory addressing | **DG-2**, G7b |
| 3 | Host-call trampoline | G6, M3 |
| 4 | Trap and fuel mapping | G2 oracle, G6, M3 |
| 5 | Relocation model | `wari-wnm` `Relocs`, M3 |

**Out of scope:** the compiler backend choice (DG-1), the safety-cert wire
format (DG-3 / G7a), the scheduler's treatment of AOT instances, and any
`kernel/src/**` change. This RFC *states requirements on* the kernel loader;
it does not design it.

---

## 1 · Ground truth (facts, not proposals)

The ABI has to interoperate with what is actually in the tree today. These
were read out of the source, not assumed:

1. **`wasmi = "=0.32.3"`, `default-features = false`** (`kernel/Cargo.toml`).
   That is the reference semantics the G2 oracle enforces.
2. **Fuel metering is not enabled today.** There is no `Config::consume_fuel`
   anywhere in `kernel/src/`; `sched/mod.rs` documents "no fuel timer" as
   present-tense scope. So "identical to wasmi" currently means *identical to
   fuel-off wasmi* — see §4.5, this needs an explicit decision.
3. **Nothing runs in U-mode.** There is no `sret`-to-user path in the kernel;
   Tier-1 and Tier-2 are both wasmi-interpreted *inside* S-mode. The MMU is
   today a kernel-integrity mechanism, not a live Tier-1 boundary — the
   interpreter is the only enforcement actually in the loop. AOT is what makes
   the privilege-placement question real for the first time (§3.1).
4. **Host fns are wasmi closures.** `runtime/wasi.rs` and
   `runtime/host_fns.rs` bind `move |caller: Caller<'_, T>, a: u32, …| -> i32`
   with `proc_id` captured per instance. The bodies almost all delegate to
   plain `*_impl(proc_id, args…)` functions in `cap::`; only the ones that
   touch tenant memory take the `Caller` (`cap_lookup_impl`,
   `ring_submit_impl`, `ipc_*_impl`, `nic_attach_queue_impl`,
   `lin_mem_base_impl`).
5. **Memory-touching host fns resolve linear memory by string lookup on every
   call** — `caller.get_export("memory")`, twice per `fd_write` counting
   `write_nwritten`. This is the cost the AOT ABI deletes for free (§3.5).
6. **A batching path already exists.** `wari::ring_setup(sq, cq, entries)` /
   `wari::ring_submit(n)` (`kernel/src/cap/ring_drain.rs`) drains up to
   `MAX_RING_ENTRIES = 1024` SQEs from linear memory per crossing. Whatever
   the per-call cost turns out to be, the amortisation mechanism is already
   built and the AOT ABI should reuse it rather than invent one.
7. **The WNM container is fixed** (`wari-wnm`): `Text` / `Relocs` /
   `SafetyCert` / `Wasm`, 12-byte header, 12-byte section entries, `Relocs`
   optional, the other three required. Extensions are append-only.
8. **D4 — RX-only, never W+X, no runtime codegen ever**
   (`aot-build-plan.md` §10). This is load-bearing for §5.
9. **R8 — bitwise-reproducible output.** Same input + same tool version →
   identical WNM bytes.

### 1.1 Terminology used below

| Term | Meaning |
|------|---------|
| **`.text`** | the compiled native RV64 code, one image per *module*, mapped RX-only, shared by every instance of that module |
| **arena** | the per-*instance* data region the loader allocates: instance context + function table + machine stack |
| **WCTX** | the reserved register holding the arena's instance-context pointer (Wasmtime's `vmctx` analogue) |
| **linear memory** | the wasm module's own memory, a separate contiguous region, the only region compiled code may store into freely |

---

## 2 · Linear-memory addressing — **the DG-2 decision**

### 2.1 What must hold

Every load/store the compiled code performs must be provably inside this
instance's linear memory, and an out-of-bounds access must **trap with
wasmi's exact semantics** (§4). "Provably" is not rhetorical: the M2
certificate checker has to be able to establish it from the emitted
instruction stream (`aot-safety-cert-design.md` §1.1, §6.1).

### 2.2 Option 2A — Guard pages (virtual-address reservation)

Reserve a virtual region far larger than any legal wasm address, map the
live memory at its base and leave the rest unmapped. A wasm `i32` index can
then address at most 4 GiB − 1 from the base, so if the reservation plus
guard covers that span, **no bounds check is emitted at all** — the MMU
faults on escape and the fault handler converts the fault into a wasm trap.

Prior art: **Lucet** and **Wasmtime** both do this on 64-bit hosts (a 4 GiB
addressable reservation plus a guard region whose size has varied across
releases). It is the standard high-performance shape.

**Cost on Wari specifically:**

- **It requires an MMU. Permanently.** Phase 4's MMU-free SoC variant
  (`CLAUDE.md` §"Long-term endpoint", `docs/book/.../ch07`) has no paging
  hardware — a guard-page module is simply not runnable there. Choosing 2A
  means either abandoning the MMU-free endgame or writing the compiler
  backend, the cert design and the trap path a second time for it.
- **Sv39 has 512 GiB of VA, total.** At a multi-GiB reservation per
  instance, a single address space holds on the order of tens of instances.
  The density target is 10 000 – 50 000 Tier-1 instances per board. Guard
  pages and that target are not compatible in one address space; making them
  per-address-space adds a root page table plus a `satp` switch per instance,
  and whether the JH7110's U74 cores implement any ASID bits at all is
  **unverified** (§8) — if `ASIDLEN = 0`, every switch is a full TLB flush.
- **Trap reconstruction becomes a Tier-0 problem.** The fault handler must
  map a faulting PC back to "which wasm access was this, and therefore which
  `TrapCode`", and must distinguish a tenant OOB from a genuine kernel bug.
  That is a new, subtle, security-relevant path in the kernel — and the G2
  oracle will hold it to exact wasmi trap parity.
- **The cert cannot check it.** The accepted cert design's core property is
  literally "every memory access is preceded by a bounds check or uses a
  masked index provably within `[0, linmem_len)`" (§6.1). With guard pages
  there is nothing to check; the certificate degrades to "trust the MMU",
  which is exactly the property Phase 4 removes.

### 2.3 Option 2B — Reserved base register + explicit bounds check

Keep the linear-memory base and byte-length reachable from WCTX (cached in
registers where the allocator can), and emit an explicit check before each
access. Shape, for `i32.load offset=K` with the index in `a0`:

```asm
    ; conceptual shape — the backend picks the real sequence
    slli  t0, a0, 32
    srli  t0, t0, 32          ; zero-extend the wasm i32 index to u64
    addi  t0, t0, K           ; + static offset  (index<2^32, K<2^32 ⇒ no u64 wrap)
    addi  t1, t0, 4           ; end of the access
    bltu  s9, t1, .Ltrap_oob  ; s9 = mem_len_bytes; len < end ⇒ OOB
    add   t0, s10, t0         ; s10 = mem_base
    lw    a1, 0(t0)
```

Prior art: **wasm2c** (wabt) emits explicit range checks in portable C as its
default; **Wasmtime** falls back to this shape for "dynamic" memories and on
32-bit hosts. It is a production-proven shape, not a compromise.

**Properties:**

- **No MMU required.** Identical code runs on the Sv39 boards today and on
  the Phase-4 MMU-free SoC. One backend, one cert design, one trap path.
- **`memory.grow` is a field update**, not a VA remap: bump `mem_len` in the
  context, and if the allocation moved, `mem_base` with it. Linear memory can
  be a plain contiguous allocation, no per-instance page table.
- **It is what the cert checks.** Each access carries its own local, checkable
  evidence. This is the property VeriWasm establishes on Lucet output and the
  one `wari-cert` (G7b) is specified against.
- **It costs instructions.** A compare + a predictable, almost-never-taken
  branch per access that the optimiser cannot hoist. Cranelift's redundant
  bounds-check elimination removes many in loops; how many survive, and what
  they cost on an in-order dual-issue U74, is **unmeasured** (§8).

### 2.4 Option 2C — Power-of-two masking (NaCl-style SFI)

Force linear memory to a power-of-two size and replace the branch with
`and t0, t0, s9` (mask = size−1). One instruction, no branch, trivially
checkable. Prior art: **Wahbe et al. 1993** (the original SFI paper) and
**Native Client**.

**Rejected on semantics, not cost.** WebAssembly requires an out-of-bounds
access to *trap*; masking makes it silently wrap to a legal address. That is
still safe — it cannot escape — but it is observably different from wasmi,
so the G2 oracle reports DIVERGED, correctly. Masking is only admissible as
an optimisation *in addition to* a trapping check, which defeats the point.
Worth recording because it will be re-proposed: the answer is "the oracle
forbids it."

### 2.5 Where base and length live

Sub-decision, independent of the above:

| | Reserved registers (`mem_base`, `mem_len` pinned) | Context fields (loaded from WCTX) |
|---|---|---|
| Cost per access | none extra | up to two loads, usually hoisted out of loops by the backend |
| Backend support | needs ≥3 reservable registers | needs exactly **one** (WCTX) |
| After `memory.grow` / any trampoline call | must be re-materialised | re-loaded naturally |

Cranelift exposes a *single* pinned register (`enable_pinned_reg`); whether
its riscv64 backend honours more is **unverified and is a G4 deliverable**
(§8). The ABI therefore mandates only WCTX and treats base/length caching as
a backend optimisation.

Proposed register assignment (all in the **callee-saved `s` bank** on
purpose — see §3.4):

| Reg | Name | Role | Mandatory |
|-----|------|------|-----------|
| `x27` / `s11` | **WCTX** | pointer to this instance's context in the arena | **yes** |
| `x26` / `s10` | `WMBASE` | linear-memory base | if reservable |
| `x25` / `s9` | `WMLEN` | linear-memory length, bytes | if reservable |

> **RECOMMENDATION — A1 · Explicit bounds checks. No guard pages. Ever.**
>
> Adopt **Option 2B**: a reserved WCTX register, linear-memory base and
> length reachable from it, and an explicit compare-and-branch-to-trap
> before every access that the compiler cannot prove in range.
>
> The deciding argument is not performance, it is that **guard pages
> foreclose Phase 4.** Wari's stated endpoint is a ROM-attested Tier-0 on
> silicon with no paging hardware, where *the verified output is the
> isolation*. A guard-page module has no isolation property of its own; it
> borrows one from the MMU. Every hour spent on a guard-page backend is an
> hour spent on code that must be thrown away at the MMU-free line, plus a
> second cert design, plus a second trap path.
>
> The secondary argument is that guard pages and the 10 000–50 000
> instances/board density target do not fit in Sv39's 512 GiB of VA at any
> plausible reservation size.
>
> The cost is real and currently unquantified: a compare + branch per
> non-hoisted access. **If M0 shows that cost is intolerable, the correct
> response is to fall back to interpreter tuning (build-plan Option A) and
> not build AOT at all — not to reach for guard pages.** The build plan
> already frames "don't build it" as a success outcome.
>
> This is the answer to **DG-2**: *explicit bounds checks + certificate.*

---

## 3 · Host-call trampoline

### 3.1 The fork underneath the question: where does compiled code run?

Today Tier-1 is *data interpreted by an S-mode interpreter*. AOT turns it
into real native code, which must execute at some privilege level, and that
choice sets the trampoline's cost by two orders of magnitude:

| | **U-mode** | **S-mode + SFI cert** |
|---|---|---|
| Host call is | `ecall` → full trap-frame save/restore (INV-2 path) | a plain call |
| Isolation from | hardware (page tables) **and** the cert | the cert alone |
| Per-instance page table | required | not required |
| Matches `CLAUDE.md`'s "Tier 1 — U-mode, double-sandboxed" | yes | **no — needs architect sign-off** |
| Survives to Phase 4 (no MMU, so no privilege separation for tenants) | no | yes |
| Precedent inside Wari | — | Tier-2 drivers already run with no MMU barrier between them (`docs/prior-art.md`, Cloudflare Workers entry) |

Under S-mode execution the certificate must forbid strictly more than the
cert design currently lists. Its §1.3 says "no raw `ecall`"; S-mode
execution additionally requires **no CSR access (Zicsr), no `sret`/`mret`/
`wfi`, no `sfence.vma`/`hfence`, and no unrecognised encoding** — otherwise
tenant code could rewrite `satp` and the whole argument collapses. That
extension belongs in G7a; it is flagged here because it is an ABI-level
consequence, not a checker detail.

### 3.2 Option 3A — Indirect call through a context slot (recommended shape)

Every host call compiles to a load from a statically-known offset in the
instance context, then a `jalr`:

```asm
    ; a0 = WCTX (explicit first argument), a1.. = wasm arguments
    mv    a0, s11
    ld    t1, (IMPORT_VEC_OFF + 8*i)(s11)   ; i is a compile-time constant
    jalr  ra, 0(t1)
    ; a0 = result, a1 = trap code (0 = no trap)  — see §3.6
```

The critical property: **the compiled code does not know, and does not care,
what is on the other side of that slot.** The loader fills it. If the
architect picks S-mode, the slot holds the kernel's `extern "C"` shim
address directly. If U-mode, it holds the address of a tiny kernel-provided
stub, mapped U-executable, that performs the `ecall`. Same `.text`, same
certificate, same reproducible bytes — the privilege decision becomes a
*loader* decision, re-decidable per phase, and it does **not** block G6.

It also keeps the cert rule "no `ecall` appears in tenant `.text`" true in
*both* placements, because the `ecall`, when there is one, lives in
kernel-owned text.

Cert-friendliness: the import-vector offset is an immediate in the `ld`
encoding, so the checker reads the target slot straight out of the
instruction and can prove it lies inside the sanctioned import vector,
without dataflow analysis.

Prior art: Wasmtime/Lucet call imported functions through function pointers
in the `vmctx`; **Isolation Without Taxation** (Kolosick et al., POPL 2022 —
already cited by the cert design §10) is the cost model for making the
transition itself near-free once the privilege boundary is out of the way.

### 3.3 Option 3B — Direct `ecall` from tenant `.text`

Compiled code issues `ecall` with a host-fn number in a register, landing in
the existing `trap.rs` dispatch.

- Reuses the kernel path that already exists; hardware-enforced; the
  smallest conceptual change.
- Pays the full trap-frame save/restore on every host call, including for
  `cap_lookup` and the IPC fns that Tier-1 workloads call in a tight loop —
  the `hostcall.wat` fixture (G3) exists precisely because that shape is the
  target workload (AI-assistant orchestration).
- Bakes U-mode into `.text`, so the same artifact cannot run on the Phase-4
  part, and the cert must now *allow* `ecall`, weakening its strongest and
  simplest rule.

### 3.4 Register convention

| Register(s) | Role at a host call | Who preserves |
|---|---|---|
| `a0` | **WCTX** (explicit first argument, so the kernel side is a plain `extern "C" fn(ctx: *mut InstanceCtx, …)` — no hand-written assembly on the S-mode path) | caller-saved: compiled code re-materialises from `s11` |
| `a1`–`a7` | wasm arguments, psABI order. Max arity in the current surface is 5 (`nic_attach_queue`), so 7 slots is ample | caller-saved |
| `a0` (ret) | result value, zero/sign-extended per psABI | — |
| `a1` (ret) | **trap code**, 0 = no trap (§3.6) | — |
| `t0`–`t6`, `a*`, `ft*`/`fa*` | assumed clobbered | caller-saved (psABI) |
| `s11` (WCTX), `s10`, `s9` | reserved; must survive the call | **automatically** — they are callee-saved in the RV64 psABI, so rustc-compiled kernel code preserves them with no special handling |
| `sp` | the instance stack (§3.7) | callee-saved (psABI) |

Putting the reserved registers in the `s` bank is the whole trick: the
kernel-side shim needs *no* save/restore prologue for them, because the
standard psABI already obliges every compiled function — Rust or Cranelift —
to preserve them.

**Requirement this places on the kernel lane (M3):** the memory-touching
`*_impl` functions currently take `&mut Caller<'_, T>` purely to reach
linear memory. To serve both engines they need a small linear-memory
abstraction (a `&mut dyn` or a concrete `LinMem { base, len }`) so one impl
body backs both the wasmi binding and the AOT shim. The `proc_id` that is
today captured in each closure becomes a context field. This is a kernel-lane
change; it is stated here because the ABI depends on it.

### 3.5 How this compares with wasmi's boundary — honestly

No numbers exist yet; G1 produces them. What can be said from reading the
code, without measuring:

- wasmi's crossing is *interpreter* work — dispatch, `Caller` construction,
  argument marshalling through the value stack, closure invocation. The AOT
  crossing on the S-mode fill is a load and a `jalr` into a normal function.
  The direction is not in doubt; the magnitude is entirely unknown.
- One specific, nameable win: every memory-touching host fn today performs
  `caller.get_export("memory")` — a *string-keyed export lookup* — on each
  call (`wasi.rs:419`, and again at `:467`). Under this ABI the base and
  length are context fields. That lookup disappears.
- On the U-mode fill, the crossing is a hardware trap, which is very likely
  *more* expensive than wasmi's interpreted crossing. If the architect picks
  U-mode, the ring (`ring_setup`/`ring_submit`) stops being an optimisation
  and becomes the primary host-call path for anything hot.

### 3.6 Returning a trap from a host call

`proc_exit` must not return to compiled code; `fd_write` on a revoked cap
must be able to surface an error without one. Two shapes:

- **3B-i — trap code in `a1`.** The shim returns normally; compiled code
  branches to the trap thunk when `a1 != 0`. No unwinding machinery in
  Tier 0, no `setjmp` analogue, R5-friendly, and the branch is one predicted
  instruction.
- **3B-ii — the shim unwinds.** The kernel restores a saved entry context and
  never returns to the tenant. Fewer instructions in the hot path, but puts
  a context-restore path into Tier 0 and makes the cert reason about a
  non-local exit.

Recommend **3B-i**, with one honest caveat: the check is not self-enforcing.
A compiler that dropped the `a1` test would produce a module that ignores its
own fuel-exhaustion signal. That is a **liveness** bug contained to the
tenant's own time slice, not an isolation bug, and it is exactly the class
the SFI cert explicitly does not catch (cert design §14). If cheap, G7b
should add a local pattern check that every trampoline call site is followed
by the `a1` test — it is a purely syntactic, single-pass check.

### 3.7 Stack

Compiled code must not run on the kernel stack: a runaway wasm call chain
would then smash kernel memory. The loader gives each instance a stack
region inside its arena and the trampoline switches `sp` on entry. Every
compiled function prologue compares the prospective `sp` against
`WCTX.stack_limit` and branches to the trap thunk on underflow — Wasmtime's
shape (a stack limit in the runtime-limits struct, checked in prologues),
and it doubles as the cert's §1.4 stack-confinement evidence. The cert wire
format already carries `stack_frame_size` per function
(`aot-safety-cert-design.md` §13), so the two designs already line up.

> **RECOMMENDATION — A2 · One indirect call through a context slot; privilege placement deferred to the loader.**
>
> Adopt **Option 3A** as the *only* way out of compiled code: `ld` from a
> compile-time-constant offset in the import vector, then `jalr`, with
> `a0 = WCTX` and wasm arguments in `a1…`. Reserved registers live in the
> callee-saved `s` bank so the kernel side is ordinary `extern "C"` Rust
> with no assembly. Traps come back in `a1` (§3.6). `sp` is a per-instance
> stack with a prologue limit check (§3.7).
>
> **Default fill: the same-privilege (S-mode) shim**, because it is the only
> shape that survives to the MMU-free endpoint, and because Wari already
> accepts structural isolation as the primary line for Tier-2. **Keep the
> U-mode `ecall`-stub fill as a load-time-selectable alternative** for Tier-1
> while the MMU exists — it costs one extra indirect jump versus a direct
> call and buys a hardware backstop during Phase 2/3.
>
> **This half needs explicit architect sign-off**, because running tenant
> code in S-mode departs from `CLAUDE.md`'s "Tier 1 — U-mode,
> double-sandboxed". The indirection is designed so the decision does *not*
> block G6: the compiler emits identical bytes either way.
>
> Whatever is chosen, host-call-dense workloads route through the existing
> `ring_setup`/`ring_submit` batching path rather than a new mechanism.

---

## 4 · Trap and fuel mapping

### 4.1 The requirement

The G2 oracle compares (exit value, ordered host-call sequence, hash of
linear memory at exit) against wasmi. A trap is an observable event. **Any
divergence in *which* trap fires, or *when*, is a bug in the compiler, not a
tolerable difference.** This section is therefore a specification of
obligations on `wari-aot`, not a menu.

### 4.2 Mechanism: how a trap reaches the kernel

- **Option 4A — trap thunk call.** Every trap site is a branch to a
  per-module thunk that loads `WCTX.trap_entry` and calls it with the
  `TrapCode` in `a1` and the faulting wasm-function index in `a2`. Uniform,
  works identically in S- and U-mode, needs no fault decoding, and gives the
  checker one syntactic pattern to recognise. Costs a few bytes per site
  (shared thunk keeps it small).
- **Option 4B — hardware fault.** Let the access fault / execute `ebreak` and
  reconstruct the trap in the kernel's fault handler from the faulting PC.
  Requires a PC→trap-code side table in the WNM, a fault handler that can
  tell tenant faults from kernel bugs, and — for OOB specifically — the
  guard pages §2 rejects.

Recommend **4A**. It is the only option that is privilege-placement-agnostic
and MMU-free-compatible, and it is the only one where the certificate can see
the trap edge at all.

### 4.3 The mapping table

The compiler owes an explicit check wherever RV64 semantics differ from
wasm's. The dangerous cases are the ones where RV64 *silently succeeds*:

| wasm condition | wasmi behaviour | RV64 native behaviour | AOT obligation |
|---|---|---|---|
| load/store OOB | `MemoryOutOfBounds` trap | address wraps into whatever is mapped | explicit bounds check (§2), branch to thunk |
| `unreachable` | `UnreachableCodeReached` trap | — | unconditional branch to thunk |
| `i32.div_s` / `i64.div_s` by 0 | `IntegerDivisionByZero` trap | `div` returns −1, **no fault** | explicit zero test |
| `i32.rem_s` by 0 | `IntegerDivisionByZero` trap | `rem` returns the dividend, **no fault** | explicit zero test |
| `INT_MIN / -1` | `IntegerOverflow` trap | `div` returns `INT_MIN`, **no fault** | explicit operand test |
| `INT_MIN % -1` | result `0`, no trap | `rem` returns `0` | nothing — RV64 already matches |
| `i32.trunc_f32_s` of NaN / out of range | `BadConversionToInteger` trap | `fcvt.w.s` **saturates** | explicit NaN + range test (the saturating `trunc_sat` opcodes must *not* get the test) |
| `call_indirect`, null element | `IndirectCallToNull` trap | — | explicit null test on the table entry |
| `call_indirect`, wrong type | `BadSignature` trap | — | explicit type-id compare (§5.5) |
| table index OOB | `TableOutOfBounds` trap | — | explicit bounds check against `table_len` |
| call-depth / stack exhaustion | `StackOverflow` trap | silent stack growth | prologue `stack_limit` check (§3.7) |
| fuel exhausted | `OutOfFuel` trap | — | §4.5 |
| misaligned access | permitted, no trap | **implementation-defined on the U74** | §8 — unverified |

**Rule for G6:** this table is *illustrative*, not authoritative. The
compiler must generate its trap mapping **mechanically from the pinned
wasmi's own `TrapCode` enum**, and a test must fail if a variant exists in
wasmi that the mapping does not handle. Hand-transcribing an enum into a
markdown table is how divergence gets shipped.

### 4.4 Trap codes on the wire

The `TrapCode` passed to the thunk is a stable `u32` defined once in a pure,
host-testable crate shared by `wari-aot` and the loader (proposed:
`wari-aot-abi`, alongside the context layout and `RelocKind` of §5). It maps
1:1 onto wasmi's variants and onto the kernel's existing
`KernelError`/`i32_exit` surfacing so that an AOT trap and a wasmi trap
produce the same kernel-visible outcome and the same UART line.

### 4.5 Fuel — the honest part

Fuel is the hardest parity problem in this document, and it has a
precondition the architect must settle first: **the kernel does not enable
fuel metering today** (§1.2). "Identical to wasmi" is ambiguous until the
reference configuration is named.

Three options for the metering itself:

- **4F-A — bit-exact parity.** Mirror wasmi's block partitioning and its
  per-instruction cost table, decrementing the same amount at the same
  boundaries and trapping `OutOfFuel` at the same wasm instruction. The G2
  oracle then compares fuel traps like any other event, `fuel_bomb.wat`
  included. Costs: couples `wari-aot` to wasmi internals (whether the cost
  table is public API in 0.32.3 is **unverified**, §8); a wasmi upgrade can
  silently change the mapping. Mitigation: vendor the constants and let the
  oracle itself be the equality test — a wasmi bump that changes costs fails
  CI loudly, which is the correct behaviour.
  Upside beyond parity: fuel becomes an engine-independent resource unit, so
  scheduling and (eventually) tenant accounting mean the same thing whether a
  module is interpreted or compiled. For a sovereign-cloud billing story that
  matters more than the implementation cost.
- **4F-B — bounded divergence.** AOT charges a coarser cost (e.g. per-block
  instruction count) and the contract is only "terminates within N fuel of
  wasmi's trap point"; the oracle compares traces modulo the fuel event.
  Cheaper, but it blunts the differential oracle exactly at the fixture built
  to test it, and it makes fuel non-portable between the two engines.
- **4F-C — no fuel; rely on timer preemption.** Rejected: observably
  divergent, and neither mechanism exists in the kernel today — there is no
  Tier-1 preemption timer either (`sched/mod.rs`).

Where the counter lives: if only WCTX is reservable (§2.5), fuel is a context
field and the decrement is a store into the arena. That collides with the
rule in §5.2 that compiled code never writes the arena, so the certificate
must whitelist **exactly one** store offset — `WCTX + FUEL_OFF` — and reject
every other arena store. If a second register is reservable, fuel lives in it
and the arena stays strictly read-only to compiled code, which is the
cleaner property. Under the U-mode fill this also means the fuel word must
sit on its own page (U-RW) with the rest of the context U-RO.

> **RECOMMENDATION — A3 · Trap via thunk; generate the mapping from wasmi; fuel bit-exact.**
>
> Traps use **Option 4A** (branch to a thunk that calls `WCTX.trap_entry`
> with a stable `TrapCode`), never hardware faults — the only shape that is
> privilege-agnostic, MMU-free-compatible and visible to the certificate.
>
> The trap mapping is **generated from the pinned wasmi's `TrapCode` enum**
> with a test that fails on any unhandled variant. Div/rem-by-zero,
> `INT_MIN/−1`, and float→int conversion get explicit checks because RV64
> silently succeeds where wasm must trap; these three are where a
> "reasonable" backend will diverge if nobody writes it down.
>
> Fuel: **4F-A, bit-exact**, with the fuel word cert-whitelisted as the
> single legal arena store if only one register is reservable.
>
> **Architect decision needed first:** is the reference semantics fuel-on or
> fuel-off wasmi? Today's kernel is fuel-off. If AOT ships fuel metering,
> the interpreter must enable it too or the two tiers price the same workload
> differently.

---

## 5 · Relocation model

### 5.1 The constraint that decides this section

The `SafetyCert` binds **`text_hash`** — the checker verifies the hash
matches the mapped `.text` (`aot-safety-cert-design.md` §5, §6.5). If the
loader patched `.text` at load time, the mapped bytes would no longer match
the hash the compiler signed, and the certificate's binding to *this* code
would be void. Combined with D4 (RX-only, never W+X):

**`.text` carries zero relocations. It is hash-identical from the signing
pipeline to the instruction fetch, mapped RX once and never written.**

Everything genuinely per-instance is reached through WCTX. This also means
`.text` is **shared read-only across every instance of a module** — with a
10 000-instance density target, that is a first-order memory win over wasmi's
per-instance interpreter structures, and it comes free from the same
constraint.

Consequence for `wari-aot` (a G6 acceptance criterion): the compiler must
resolve every intra-`.text` reference offline — it knows the final layout —
and **assert the residual backend relocation set is empty**. Anything that
cannot be resolved offline must be expressed as a WCTX-slot indirection, not
a text patch.

### 5.2 The arena

```text
per-instance arena (loader-allocated, zeroed)
 ┌──────────────────────────────┐
 │ InstanceCtx  (fixed header)  │  read-only to compiled code¹
 ├──────────────────────────────┤
 │ import vector  (n × u64)     │  read-only to compiled code
 ├──────────────────────────────┤
 │ function table (m × u64)     │  read-only to compiled code²
 ├──────────────────────────────┤
 │ machine stack                │  read-write (sp, §3.7)
 └──────────────────────────────┘
separate region: linear memory     read-write (§2)
```

¹ except the fuel word, if fuel is a context field (§4.5).
² `table.set` and `memory.grow` mutate arena state **through the
trampoline**, never by direct store — a runtime call can validate the index;
a raw store cannot.

That gives the certificate a very sharp rule: *every store in compiled
`.text` is either a bounds-checked store into linear memory, a stack store
within the proven frame, or the single whitelisted fuel word.* Nothing else.

### 5.3 Option 5A — ELF-style symbolic relocations

A symbol table, a string table, `R_RISCV_*` relocation types patched into
instruction fields.

**Rejected.** It puts a string-table parser and an instruction-field encoder
in the kernel TCB, both operating on attacker-influenced input, for zero
benefit — there is exactly one module and one linker step, done offline.
It also patches `.text`, which §5.1 forbids.

### 5.4 Option 5B — Closed kind enum, fixed-size entries, arena-only targets

Relocations are 12-byte fixed records — the same width as a WNM section
entry, deliberately — that write one 8-byte word each into the arena. No
symbols, no strings, no instruction encoding, no `.text` writes.

**`Relocs` section layout** (entirely inside the existing section; **no WNM
format change, no `WNM_ABI_VERSION` bump**):

```text
 0                     RelocHeader (16 bytes)
   [0..4)   magic       = "WRL\0"
   [4..6)   version     : u16 LE  = 1
   [6..8)   reserved    (must be 0)
   [8..12)  arena_len   : u32 LE   (bytes the loader allocates and zeroes)
   [12..16) entry_count : u32 LE
 16                    entry_count × RelocEntry (12 bytes)
   [0]      kind        : u8   (RelocKind)
   [1..4)   reserved    (must be 0)
   [4..8)   arena_off   : u32 LE  (destination; 8-byte aligned)
   [8..12)  operand     : u32 LE  (kind-specific)
```

Validation the loader performs, in one pass, allocation-free (R2), returning
`Result` never panicking (R5):

1. `section_len == 16 + 12 * entry_count`, magic and version match, reserved
   bytes are zero;
2. `arena_len` is within the configured per-instance cap;
3. every `arena_off` is 8-byte aligned and `arena_off + 8 <= arena_len`;
4. `arena_off` is **strictly increasing** across entries — this makes overlap
   detection free, makes the encoding canonical, and is what R8 needs (there
   is exactly one legal byte sequence for a given relocation set);
5. `kind` is known and `operand` is in range for that kind.

`RelocKind`:

| # | Kind | `operand` | Loader writes |
|---|------|-----------|---------------|
| 1 | `HostFn` | host-fn id from the frozen ABI import table | address of the shim for that host fn (S-mode fill) or of its `ecall` stub (U-mode fill) — §3.2 |
| 2 | `TrapThunk` | 0 | address of the kernel trap entry |
| 3 | `TextAddr` | byte offset into `.text` | `text_base + operand` (function-table entries) |
| 4 | `ArenaAddr` | byte offset into the arena | `arena_base + operand` (intra-arena pointers, e.g. ctx → table) |

`mem_base`, `mem_len`, `stack_limit`, `proc_id` and `fuel` need no relocation
kinds: they live at fixed offsets in the **ABI-versioned `InstanceCtx`
header**, declared once as a `#[repr(C)]` struct in the shared pure crate and
written by the loader from its own state. A fixed header is worth more than
the flexibility of describing it in relocations: it is a Rust struct the
kernel can construct with contracts (R4) and that Kani can eventually reason
about, instead of a byte-poking loop.

An `abi_version` word in that header, checked by the loader against its own
constant, is how a stale artifact gets refused rather than misinterpreted.

### 5.5 Option 5C — Zero relocations

The loader derives everything from the embedded `Wasm` section: the import
list from its import section, the function table from its element segments.

Genuinely tempting — it is the smallest *format*. **Rejected on TCB size:**
the loader would have to re-implement a wasm module decoder in the kernel to
recover ordering, which is far more code than a bounded loop over 12-byte
records. (The embedded `.wasm` is still used to initialise linear memory
from data segments — that path exists regardless, and is also the audit and
fallback path the WNM keeps it for.)

### 5.6 Indirect calls

`call_indirect` needs a bounds check, a type check and a target address. To
keep raw code addresses out of tenant-adjacent data, the recommended shape
puts the **function descriptors — `(type_id, text_offset)` pairs, both
compile-time constants — inside `.text`** (read-only, hash-covered), and the
per-instance table holds `u32` **indices** into that array:

1. bounds-check the table index against `WCTX.table_len` → `TableOutOfBounds`
2. load the descriptor index; null → `IndirectCallToNull`
3. load `(type_id, text_off)` PC-relatively from the descriptor array
4. compare `type_id` → `BadSignature`
5. target = `text_base + text_off`; `jalr`

This is more work per indirect call than storing raw addresses in the arena,
and the alternative should be measured before it is dismissed. But it gives
the certificate exactly the artifact its §1.2 wants — *a verified,
immutable set of legal indirect targets, enumerable from `.text` itself* —
and it means a corrupted arena word can at worst select the wrong legal
function, never an arbitrary address.

> **RECOMMENDATION — A4 · `.text` is relocation-free; relocations initialise the arena only.**
>
> Adopt **Option 5B**: a 16-byte `Relocs` header plus strictly-ascending
> 12-byte fixed entries with a four-value closed `RelocKind`, all destinations
> 8-byte-aligned words inside the per-instance arena. **No relocation ever
> targets `.text`** — that is what keeps `text_hash` meaningful, keeps
> W^X trivially true, and lets one RX mapping of `.text` be shared by every
> instance of a module.
>
> The fixed part of the instance context is an **ABI-versioned `#[repr(C)]`
> struct in a shared pure crate**, not a reloc-described blob. Relocations
> cover only what the loader cannot know on its own: the import vector, the
> trap entry, intra-arena pointers, and function-table targets.
>
> This fits the existing `wari-wnm` container with **no format change and no
> `WNM_ABI_VERSION` bump** — the layout lives entirely inside the already
> allocated, already optional `Relocs` section.
>
> G6 acceptance criterion falling out of this: `wari-aot` must assert its
> backend's residual relocation set against `.text` is **empty**.

---

## 6 · Determinism (R8)

The ABI is only attestable if the same source produces the same bytes:

- relocation entries are strictly ascending by `arena_off` — one legal
  encoding per relocation set, no sort-stability question;
- no timestamps, no host paths, no hash-map iteration order anywhere in the
  WNM;
- the backend version is pinned in `Cargo.lock` (R8) and recorded in the
  `SafetyCert` alongside the verifier id the cert design §5 already carries;
- the `abi_version` from §5.4 is recorded too, so an artifact compiled
  against a superseded ABI is *refused*, not misread;
- G6's double-compile `sha256sum` check is the enforcement, and G8 re-runs it
  per fixture.

Backend register allocation must be deterministic for this to hold. That is
believed true of Cranelift for a fixed version and flag set; **G4 verifies it
rather than assuming it** (§8).

---

## 7 · What the certificate must be able to see

Cross-check against `aot-safety-cert-design.md` §1 and §6 — every property
the checker needs must be a *syntactic* consequence of this ABI, not
something requiring a fixpoint:

| Cert property | What this ABI gives the checker |
|---|---|
| §1.1 memory isolation | every access preceded by a compare-and-branch against a value derived from `WCTX.mem_len` (§2.3) |
| §1.2 control-flow safety | indirect targets come from an immutable in-`.text` descriptor array; direct calls are PC-relative to offsets inside `.text` (§5.6) |
| §1.3 bounded host transitions | the *only* exit is `ld` from a constant import-vector offset + `jalr`; the offset is an immediate in the encoding, readable without dataflow (§3.2). Plus, for S-mode execution, a forbidden-instruction list: `ecall`, CSR ops, `sret`/`mret`/`wfi`, `sfence.vma` (§3.1) |
| §1.4 stack confinement | prologue check against `WCTX.stack_limit`; static per-function frame size, which the cert format already records (§3.7) |
| §6.5 cert binds this text | guaranteed by §5.1 — `.text` is never patched, so `text_hash` covers the executed bytes exactly |

The one property this ABI *adds* to the checker's obligations is the
whitelisted fuel store (§4.5). That is a single constant offset, checkable in
the same pass.

---

## 8 · What is not known yet (do not guess these)

The M0 gate exists to answer these. Nothing in this RFC depends on a number
that has been invented:

1. **Bounds-check overhead on the U74** — an in-order, dual-issue core with
   a modest predictor. Unmeasured. G1 + G4 produce it. This is the only real
   argument *for* guard pages, and it is currently unquantified in both
   directions.
2. **wasmi host-call cost vs. trampoline cost.** Direction is clear (§3.5),
   magnitude is not. G1 with the `hostcall.wat` fixture.
3. **How many reservable registers the Cranelift riscv64 backend offers.**
   `enable_pinned_reg` provides one. Whether base/length can also be pinned
   is a G4 deliverable; the ABI is written to work with one.
4. **Whether wasmi 0.32.3's fuel cost table is reachable as public API.**
   Determines whether 4F-A is a dependency or a vendored copy guarded by an
   oracle test.
5. **JH7110 U74 ASID support.** If `ASIDLEN = 0`, the U-mode fill costs a
   full TLB flush per instance switch, which would decide §3.1 on its own.
   Readable from the SoC/core documentation — cheap to close, not yet closed.
6. **U74 misaligned load/store behaviour** — hardware, M-mode-emulated via
   OpenSBI, or faulting. wasm permits misaligned accesses, so this sets
   whether the compiler may emit plain loads or must synthesise byte
   sequences for possibly-misaligned accesses. Affects both correctness
   parity and cost.
7. **`.text`-to-`.wasm` size ratio** for the corpus. AOT trades RAM/flash for
   speed; at 10 000–50 000 instances the shared-`.text` win (§5.1) matters,
   but the per-module expansion factor is unmeasured.
8. **Cranelift output determinism** across runs on a fixed version — believed
   true, verified by G4/G6's double-compile check, not assumed.

---

## 9 · Prior art

| Pattern | Source | Used for |
|---|---|---|
| AOT wasm→native, no runtime codegen | **Fastly Lucet** | the overall model (`aot-build-plan.md`) |
| Guard-page / VA-reservation memory model | **Lucet**, **Wasmtime** | §2.2 — presented, and rejected for Wari's endgame |
| Explicit bounds checks in portable output | **wasm2c** (wabt); Wasmtime's dynamic-memory path | §2.3 — evidence the checked shape is production-viable |
| Mask-based SFI | **Wahbe et al. 1993**; **Native Client** | §2.4 — rejected on wasm trap semantics |
| Load-time validation of native binaries | **Native Client** validator | the ancestor of the §7 cert obligations |
| `vmctx`-mediated imports, stack limit in prologue | **Wasmtime** | §3.2, §3.7 |
| Near-zero-cost wasm↔native transitions | **Isolation Without Taxation** (Kolosick et al., POPL 2022) | §3.2 cost model |
| Post-compilation SFI verification | **VeriWasm** (Johnson et al., NDSS 2021) | §7 — what the ABI must expose to the checker |
| Reference semantics | **wasmi 0.32.3** | §4 — the oracle's ground truth |

---

## 10 · Decisions needed from the architect

1. **DG-2 — memory-safety model.** §2. Recommendation A1: explicit bounds
   checks, no guard pages, on the grounds that guard pages foreclose Phase 4
   and do not fit Sv39 at the density target. **This is the decision this
   document was written to enable.**
2. **Privilege placement for compiled Tier-1 code.** §3.1. Recommendation A2
   defaults to same-privilege + certificate with a U-mode fill retained as a
   selectable alternative — but S-mode tenant code departs from `CLAUDE.md`'s
   stated two-tier model and needs explicit sign-off. The ABI is built so
   this does *not* block G6.
3. **Fuel reference configuration.** §4.5. The kernel is fuel-off today. If
   AOT meters fuel, the interpreter must too, or the two tiers price the same
   workload differently.
4. **New shared crate.** §4.4/§5.4 propose `wari-aot-abi` — pure, `no_std`,
   zero-dep — holding `TrapCode`, `RelocKind`, the `InstanceCtx` layout and
   `abi_version`, shared by `wari-aot` and the M3 loader. It is a new crate in
   the AOT lane's allowed set, but it is *consumed* by the kernel lane, so the
   boundary is worth confirming.
5. **Cert-design extension.** §3.1: under same-privilege execution the
   forbidden-instruction list must extend past `ecall` to all CSR and
   privileged instructions. That belongs in G7a; flagging it so it is not
   discovered late.

---

## 11 · Decision log

- **A1 — Explicit bounds checks, never guard pages.** The memory model is a
  reserved WCTX register plus a compare-and-branch per unproven access.
  Decided against guard pages because they require an MMU permanently,
  foreclosing the Phase-4 MMU-free endpoint, and because multi-GiB per-instance
  VA reservations do not fit Sv39 at a 10 000–50 000-instance density target.
  If the measured cost is intolerable, the fallback is interpreter tuning, not
  guard pages. *(Answers DG-2.)*
- **A2 — One host-call shape: indirect through a context slot.** `ld` from a
  constant import-vector offset then `jalr`, `a0 = WCTX`, reserved registers
  in the callee-saved `s` bank so the kernel side is plain `extern "C"` Rust.
  Privilege placement becomes a loader-fill decision, not a codegen decision,
  and does not block G6.
- **A3 — Traps go through a thunk, and the mapping is generated from wasmi.**
  No hardware-fault trap reconstruction. Div/rem-by-zero, `INT_MIN/−1` and
  float→int conversion get explicit checks because RV64 silently succeeds
  where wasm must trap. Fuel is bit-exact against the pinned wasmi, with the
  fuel word as the single cert-whitelisted arena store.
- **A4 — `.text` is relocation-free.** Relocations initialise the per-instance
  arena only, as strictly-ascending 12-byte fixed records under a 16-byte
  header, entirely inside the existing `Relocs` section (no WNM format change).
  This is what keeps `text_hash` binding, W^X trivially true, and one RX
  `.text` mapping shareable across all instances of a module.
- **A5 — The ABI is versioned and refused on mismatch.** `abi_version` lives
  in the `InstanceCtx` header and in the `SafetyCert`; a stale artifact is
  rejected, never reinterpreted.
- **A6 — No number in this document is measured.** Every cost claim is a
  direction. §8 enumerates what M0 must close, including two hardware
  questions (U74 ASID bits, U74 misaligned-access behaviour) that are cheap to
  answer from documentation and are not yet answered.
