<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Wari — AOT Compiler Build Plan

> **Status:** Build plan (Phase 2/3). Companion to
> [`wasm-jit-design.md`](wasm-jit-design.md) (the AOT-over-JIT decision)
> and [`cap-registered-fastpath-design.md`](cap-registered-fastpath-design.md).
> The **WNM artifact format + `load_plan`** already exist (`wari-wnm`);
> this doc maps the remaining work to actually *have* the compiler, in
> dependency order, with the measure-first gate and the long pole called
> out honestly.
>
> **The headline:** most of the cost is **not** the compiler. It is (a)
> proving we need it at all, and (b) the safety-certificate + on-device
> checker that lets us run native code *without trusting the compiler* —
> the piece that keeps AOT compatible with Wari's correctness/security/
> formal-verification thesis and its MMU-free endgame.

---

## 0 · The gate: measure first (do we even need it?)

AOT only pays for itself if WASM execution is a real bottleneck. Wari's
heavy Phase-2 compute (LLM inference) runs on GPU/GAPU via WASI-NN, so the
WASM core is orchestration, not the hot path. **Before building a
compiler, build the benchmark + differential-testing oracle** — it both
answers "is interpreter tuning (Option A) enough?" and is the harness
needed to validate AOT output later, so it is a no-regret first step.

This is **M0** below.

---

## 1 · The one decision that gates everything (Gustavo)

**Compiler backend:**

| Option | Pros | Cons |
|--------|------|------|
| **Cranelift, offline** (recommended) | Mature, has an RV64 target, deterministic; runs in the signing pipeline so its size/TCB cost never touches the device | Large host-side dep; must be driven as a library to emit a WNM |
| Bespoke `no_std` codegen | Smallest, fully ours, no external trust | A from-scratch WASM→RV64 compiler is its own multi-month project |
| `wasm2c` → C → `riscv64-gcc` | Extreme portability | Pulls a C toolchain into the pipeline; codegen quality varies |

Recommendation from `wasm-jit-design.md`: **Cranelift offline first**,
bespoke later only if size demands it. This decision sets the shape of M1.

---

## 2 · What already exists

- **WNM container format** — `wari-wnm`: header + section table
  (`Text`/`Relocs`/`SafetyCert`/`Wasm`), `validate_header`, duplicate
  rejection.
- **Loader front-half** — `wari_wnm::load_plan`: validates + resolves the
  section byte-ranges the loader needs.

So the *output contract* and the loader's *parsing* are done. That is the
easy ~10%.

---

## 3 · Components to build (dependency order)

```
 M0  benchmark + differential oracle ───────────────┐ (gate: do we need it?)
                                                     │
 D1  target ABI for compiled code  ◀── decision: backend, memory model
       │  (linmem addressing, host-call trampoline, trap/fuel mapping)
       ▼
 M1  compiler driver (tools/wari-aot): .wasm → native+relocs → WNM → sign
       │  (deterministic / reproducible — R8)
       ▼
 M2  safety certificate + on-device checker  ◀── THE LONG POLE
       │  (VeriWasm-style: prove native code stays in linear memory)
       ▼
 M3  kernel loader: verify sig+cert → map .text RX-only → relocs → enter
       ▼
 M4  end-to-end: AOT module runs on QEMU, differential-equal to wasmi
```

**D1 — target ABI for compiled code.** How linear memory is addressed
(base register + bounds, or guard pages), how compiled code calls the
kernel's host fns (the trampoline ABI), how traps / fuel exhaustion / OOB
map back to the kernel. This is the contract the compiler emits *to*; it
must be pinned before M1.

**M1 — compiler driver.** A host tool that drives the chosen backend:
`.wasm` → native `.text` + relocations → pack into a WNM (`wari-wnm`) →
sign with the existing envelope. Must be **deterministic** so the WNM is
bitwise-reproducible and attestable (R8).

**M2 — safety certificate + checker.** See §4 — the crux.

**M3 — kernel loader.** Consumes the WNM (`load_plan` done): verify
signature + safety cert, map `.text` **RX-only** (never W+X), apply
relocations into a per-instance arena, enter. Deferred today precisely
because it needs M2's cert format.

**M4 — end-to-end + reproducibility.** Run each compiled module under
QEMU and assert it is observably identical to the `wasmi` interpretation
(the M0 oracle as the reference), and confirm bitwise-reproducible output.

---

## 4 · The long pole: the safety certificate + checker (M2)

This is the piece that makes AOT *Wari's* AOT rather than "trust a big
compiler in your TCB."

- **What it is.** The compiler emits a certificate; the device runs a
  small checker proving the emitted native code provably stays inside its
  linear memory (bounds checks present, no arbitrary indirect branches
  outside an allowed set). The device trusts the **signature + the
  certificate**, not the compiler. A compiler bug cannot ship an escaping
  module that the checker accepts.
- **Why it is load-bearing.** Without it, AOT discards the
  correctness/security ordering and the formal-verification path. It is
  also what makes the **MMU-free endpoint** viable: when the MMU is not
  the backstop, the *verified output* is the isolation.
- **The coupled fork.** The memory-safety model splits into
  **guard-pages (lean on the MMU)** vs **explicit bounds-checks + verified
  output (MMU-free-compatible)**. Wari's endgame wants the latter — which
  is exactly why M2 is mandatory, not optional.
- **Honesty.** This is research-grade and the bulk of the effort —
  plausibly months and an external/academic collaboration. Prior art to
  lean on: **VeriWasm** (verify the compiled output's sandbox safety),
  **CompCert** / **Alive2** (the cost of verifying a compiler vs.
  validating a translation). Translation validation (prove each
  compilation refines the WASM semantics, offline) is the stronger
  optional layer on top.

---

## 5 · Milestones & rough effort

| Milestone | Deliverable | Effort | Verifiable offline? |
|-----------|-------------|--------|---------------------|
| **M0** | benchmark + differential oracle (wasmi reference) | days | yes (host + QEMU) |
| spike | Cranelift compiles one trivial module, runs under QEMU | ~weeks | yes (QEMU) |
| **M1** | `tools/wari-aot`: .wasm → WNM → sign, reproducible | weeks | yes (host) |
| **M2** | safety cert format + on-device checker | **months** | partly |
| **M3** | kernel loader (RX-only map, relocs, enter) | weeks | yes (QEMU) |
| **M4** | end-to-end differential-equal on QEMU | weeks | yes (QEMU) |

Sequencing: M0 first (it may say "don't build it"). M1 + the spike
de-risk the backend with real numbers. M2 is the long pole and should not
start before M0 justifies the investment and ideally before Tier-0 +
`wasmi` are closer to formally verified.

---

## 6 · Decisions needed from the architect

1. **Backend** (§1): Cranelift-offline / bespoke / wasm2c. Gates M1.
2. **Memory-safety model** (§4): guard-pages vs explicit-checks+cert.
   Gates D1 and M2; the MMU-free endgame argues for the latter.
3. **Safety-cert format/approach** (§4): adopt/adapt VeriWasm, or
   commission a proposal for approval. Gates M2 and the M3 loader.

Until #2/#3 are made, the M3 loader stays deferred (as it is today).

---

## 7 · Recommended first move

**Build M0** — the benchmark + differential-testing oracle — plus a
throwaway **Cranelift spike** that AOT-compiles one trivial module and
runs it under the existing QEMU harness. Together they (a) tell us if the
whole thing is worth it, and (b) de-risk the backend choice with real
numbers, *before* committing to the M2 cert/checker effort. Both are
local-QEMU-verifiable and need no hardware.

---

## 8 · Work that can proceed in parallel

To shorten wall-clock, these are independent of the M0→M4 critical path:

- **Decide the backend (§6.1)** — a quick call that unblocks M1.
- **Start the safety-cert track early (§6.3)** — it is the long pole.
  Reading VeriWasm + lining up an academic collaboration *now* is the
  single biggest schedule win, because M2 dominates the timeline.
- **Curate representative WASM workloads** for the M0 oracle — only the
  architect knows the real target shapes (the AI-assistant orchestration
  loop, sovereign-cloud apps). The oracle is only as honest as its
  inputs; gathering them is high-value parallel work.
- **Land the in-flight stack** (the cap-fastpath + WNM PRs) so this work
  builds on `main`, not a tower of stacked branches.
- **(Different track) the VF2 GMAC1 RGMII last-mile** at the board —
  unrelated to AOT, but advances the net milestone in parallel.

---

## 9 · Prior art

| Pattern | Source | Role |
|---------|--------|------|
| AOT Wasm→native, no runtime compiler | **Fastly Lucet** | the model (`wasm-jit-design.md`) |
| Verify compiled-output sandbox safety | **VeriWasm** | M2 cert/checker |
| Offline codegen backend (RV64) | **Cranelift / Wasmtime** | M1 backend candidate |
| Verified compilation / translation validation | **CompCert**, **Alive2** | bounds the M2 trust story |
| Wasm→C lowering | **wasm2c / wabt** | alternate M1 backend |
| Pure interpreter reference | **wasmi** | the M0 differential oracle |

---

## 10 · Decision log

- **D1 — Measure before compiling.** M0 (oracle) is the gate; AOT may be
  unnecessary if compute offloads to accelerators.
- **D2 — Cranelift offline first** (recommended), bespoke later if size
  demands. Pending architect confirmation.
- **D3 — The trust anchor is the on-device cert checker, not the
  compiler** (M2). This is the long pole and the reason AOT preserves
  Wari's thesis.
- **D4 — RX-only, never W+X.** The loader maps compiled `.text` RX-only;
  no runtime codegen, ever.
- **D5 — Parallelize the cert track + workload curation** (§8) since M2
  dominates the schedule.
