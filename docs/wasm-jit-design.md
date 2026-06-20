<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Wari — WASM Execution Strategy: Interpreter → AOT → JIT

> **Status:** Design exploration (Phase 2+ horizon). **Not a commitment.**
> This document lays out the execution-tier design space, confronts the
> tension between a runtime JIT and Wari's core thesis, and recommends a
> direction. Per the Co-Architect Protocol, the architectural choice
> (interpreter-only / AOT / runtime JIT / tiered) is **Gustavo's to make**;
> this doc is the options-with-trade-offs proposal that decision rests on.

> **TL;DR recommendation:** ship an **ahead-of-time (AOT) compiler in the
> offline signing pipeline**, not a runtime JIT. Compile WASM→RISC-V at
> sign/attest time, ship a signed native artifact, and have the runtime
> only *validate + map RX-only*. This buys ~optimizing-JIT steady-state
> speed and ~zero cold-start while keeping the compiler **out of the
> device TCB**, preserving W^X, attestation, reproducible builds (R8), and
> the Phase-4 immutable-kernel / formal-verification endgame. A true
> runtime JIT is deferred and may never be needed.

Companion to [`architecture.md`](architecture.md),
[`security-model.md`](security-model.md),
[`prior-art.md`](prior-art.md), and the roadmap in
[`CLAUDE.md`](../CLAUDE.md) (which currently reads: *"Runtime: wasmi
(no_std pure interpreter). JIT deferred to Phase 2+."*).

---

## 1 · Goals and non-goals

### Goals
- Define what "make WASM fast" means for Wari **without** sacrificing the
  ordering *correctness > security > size > convenience*.
- Enumerate the real execution-tier options and their trade-offs.
- Pick the option that survives contact with the Phase-4 endgame
  (verified Tier-0 + wasmi, hash-attested ROM `.text`, MMU-free silicon).
- Specify the recommended path concretely enough to estimate cost.

### Non-goals
- A line-by-line compiler-backend design. (Follow-up, once the tier is
  chosen.)
- Beating V8/Wasmtime on raw throughput. Wari optimizes density +
  correctness, not benchmark wins. Per prior-art, **V8 is explicitly
  rejected**.
- WASI-NN / accelerator offload. Heavy math (LLM inference) goes to
  GPU/GAPU via host functions; that path is orthogonal to how the *WASM
  glue* executes and is specced separately.

---

## 2 · Why ask the question now

`wasmi` (pure `no_std` interpreter) is correct, tiny, and host-testable —
the right Phase-0/1 choice. Its cost is steady-state speed: a switch-
threaded interpreter runs roughly **5–30× slower than native** on
compute-bound WASM (tighter for memory-bound or host-call-bound code).

Two Phase-2 pressures raise the question:

1. **Sovereign AI.** The headline workload is LLM inference. But the
   *math* belongs on the GPU/GAPU via WASI-NN host fns — the WASM itself
   is orchestration/glue. So inference does **not**, by itself, demand a
   fast WASM core. This weakens the usual "we need a JIT for AI" argument.
2. **Density + cold-start.** The Cloudflare-Workers-style thesis
   (10 000–50 000 Tier-1 instances/board, cold start < 10 ms) is the
   real driver. Here the interesting tension is **compile cost vs
   steady-state speed** — and it's exactly where a *runtime* JIT is worst
   (compile latency on the cold path) and where **AOT is best** (zero
   device-side compile, fast steady state).

So the honest framing is not "interpreter vs JIT" but **"where does
compilation happen — on the device at runtime, or offline in the
pipeline?"**

---

## 3 · The core tension: runtime JIT vs Wari's thesis

A runtime JIT generates executable code on the device. That collides with
four Wari commitments:

| Commitment | How a runtime JIT violates it |
|------------|-------------------------------|
| **Security (TCB minimalism)** | The compiler backend (10s of KLOC) joins the device TCB. A codegen bug = sandbox escape or kernel-ASID code injection. Today the TCB is ~5–10 KLOC Tier-0 + wasmi. |
| **W^X / no RWX** | Runtime codegen needs writable-then-executable pages. Even with strict W^X (write, flip to RX, never both), the window and the mechanism are new attack surface; PMP/MMU policy gets more complex. |
| **Size** | A compiler backend dwarfs the interpreter. Cranelift alone is larger than all of Tier-0. |
| **Phase-4 endgame** | Hash-attested **ROM `.text`** and the MMU-free custom-silicon option assume code is fixed and attestable. Runtime-generated code is neither. Formally verifying a runtime compiler is CompCert-scale and then some (Wasm→RISC-V, not C→asm). |

Conversely, **moving compilation offline** (AOT) dissolves all four:
the compiler runs in the build/signing host (not the device TCB), output
is fixed + signable + attestable + reproducible, the device only ever
*maps* RX pages, and you can verify the *loader + output checker* instead
of the compiler.

This is the spine of the recommendation. A runtime JIT trades away
exactly the properties Wari is built to protect, for a speed win that the
density/cold-start model gets more cheaply from AOT.

---

## 4 · The execution-tier design space

Five options, ordered by device-side complexity.

### Option A — Optimized interpreter (stay wasmi, tune it)
Superinstructions, threaded dispatch, register-based IR. Stays pure
`no_std`, host-testable, formally tractable.
- **Speed:** ~3–10× native (a good 2–3× over naive wasmi).
- **Cold start:** ~zero (validate + go).
- **TCB/size:** unchanged. **Verification:** unchanged path.
- **Verdict:** the safe floor. Possibly *sufficient* if compute moves to
  accelerators. Pursue regardless as the fallback tier.

### Option B — AOT compile in the signing pipeline ★ recommended
Compile WASM→RISC-V **offline**, at module sign/attest time. Ship a
signed native artifact + its WASM (for re-validation). Device validates +
maps RX-only; **never compiles**.
- **Speed:** ~optimizing-JIT steady state (1.2–2× native).
- **Cold start:** ~zero device-side compile (load + verify + map).
- **TCB:** compiler **out** of device TCB. Device gains a loader + an
  output safety-checker (VeriWasm-style), both far smaller + verifiable.
- **Size:** device stays small; the compiler lives in the toolchain.
- **Reproducibility (R8):** deterministic codegen → bitwise-reproducible,
  attestable artifacts.
- **Verdict:** best fit for Wari's values + endgame. Prior art: Fastly
  **Lucet** + **VeriWasm**.

### Option C — Baseline single-pass runtime JIT
Per-module fast codegen on first run (Wasmtime **Winch** / Firefox
baseline style).
- **Speed:** ~2–5× native. **Cold start:** compile latency on cold path
  (bad for the density model).
- **TCB/size/W^X/verification:** all the §3 costs. **Verdict:** deferred.

### Option D — Tiered (interpreter + hot-path JIT)
Interpret first, JIT hot functions. Most complex; two execution engines
to verify + keep in sync. **Verdict:** deferred; only if A+B prove
insufficient and a runtime tier is unavoidable.

### Option E — Verified compilation / translation validation
Not a separate runtime tier — a *technique* that makes B (or C) sound:
prove each compilation's output refines the WASM semantics + preserves
sandbox isolation, per-compile, offline. **Verdict:** the Phase-4
companion to B. Prior art: VeriWasm, Alive2, Cranelift's verifier work.

### Trade-off summary

| | A: interp+ | **B: AOT** | C: baseline JIT | D: tiered |
|--|--|--|--|--|
| Steady-state speed | ~3–10× | **1.2–2×** | 2–5× | 1.2–2× |
| Cold start | ~0 | **~0** | compile latency | mixed |
| Compiler in device TCB | no | **no** | yes | yes |
| W^X simple | yes | **yes** | no | no |
| Device size impact | none | **small** | large | large |
| Reproducible/attestable code | n/a | **yes** | no | no |
| Verification path | clean | **clean (verify loader+checker)** | hard | hardest |
| Fits ROM/MMU-free endgame | yes | **yes** | no | no |

---

## 5 · Recommended architecture (Option B, AOT)

### 5.1 Pipeline (offline, in the signing host)
```
customer.wasm
  → wasm-validate (structural isolation proof, as today)
  → AOT compile to RISC-V (Cranelift offline, or a Wari codegen)
  → emit Wari native module (WNM): .text + relocations + metadata
  → VeriWasm-style safety check: output never escapes linear memory
  → sign + attest (same envelope as Tier-2 drivers, docs/driver-interface)
  → ship customer.wnm (+ original .wasm retained for re-validation)
```
Compilation, the safety check, and signing all happen **off-device**. The
device never sees the compiler.

### 5.2 Device-side loader (the only new runtime TCB)
1. Verify signature + attestation (existing Tier-2 machinery).
2. Re-validate the embedded `.wasm` (structural isolation — layer 1 still
   holds independent of the compiler).
3. Check the WNM safety certificate (layer: compiled code provably
   stays in its linear memory — this is what lets us trust native code
   without trusting the compiler).
4. Map `.text` **RX-only**, linear memory **RW-no-X**. Never RWX, never a
   W→X flip at runtime.
5. Apply relocations into a fixed per-instance arena; enter.

### 5.3 Isolation (all three layers still hold)
- **Structural:** WASM validator proves no out-of-bounds pointer
  generation — *before* compilation, on the source.
- **Compiled-output:** the VeriWasm-style checker proves the *native*
  code preserves that property (bounds checks present, no arbitrary
  branches). This replaces "trust the compiler" with "check the output."
- **Hardware:** Sv39 MMU + PMP (Phase 1) confine the instance regardless.
  In the Phase-4 MMU-free variant, layers 1+2 become primary — which is
  *only viable* because the output is verified, not interpreted-or-trusted.

### 5.4 What stays interpreted
`wasmi` remains the fallback + reference engine: unattested modules, dev
mode, and the correctness oracle for differential testing against AOT
output. Option A (interpreter tuning) proceeds in parallel as the floor.

---

## 6 · Security model deltas

- **W^X invariant (new INV):** no page is ever simultaneously writable
  and executable; AOT `.text` is mapped RX at load and never written.
- **Compiler trust:** the device trusts the *signature* + the *output
  safety certificate*, not the compiler. A compromised offline compiler
  still cannot ship escaping code that passes the on-device checker — the
  checker is the security boundary, and it is small + verifiable.
- **Spectre / speculation:** AOT must emit speculation-hardened bounds
  checks (e.g. SLH-style, or mask-based) — a known, addressable cost.
  Document as a codegen requirement; it is *easier* to enforce + audit in
  a fixed offline compiler than in a runtime JIT.
- **Reproducible builds (R8):** AOT codegen must be deterministic so the
  WNM is bitwise-reproducible and the attestation is meaningful.
- **TCB accounting:** new device code = loader + WNM safety-checker +
  relocator. Target: small enough to sit beside wasmi in the Phase-4
  verification scope. The compiler is explicitly **not** counted.

---

## 7 · Performance expectations (rough, to be measured)

| Path | Steady-state | Cold start | Notes |
|------|--------------|-----------|-------|
| wasmi today | 5–30× native | ~0 | baseline |
| Option A | 3–10× native | ~0 | interpreter tuning |
| **Option B (AOT)** | **1.2–2× native** | **~0 (no device compile)** | load+verify+map only |
| Option C (baseline JIT) | 2–5× native | compile latency | bad for density |

For the 10k–50k-instance density target, **B's "no device-side compile"**
is the decisive property: cold start stays in the load+map budget, and
steady-state is near-native — without paying per-instance compile cost or
per-instance compiler memory.

---

## 8 · Phasing

1. **Now/Phase-2:** Option A (interpreter tuning) as the no-risk floor.
   Stand up differential-testing harness (AOT-vs-interpreter oracle) early.
2. **Phase-2/3:** Option B prototype — offline AOT (lean on Cranelift
   offline first; a bespoke `no_std` codegen is a later size optimization),
   WNM format, device loader, RX-only mapping. Gate behind attestation.
3. **Phase-3/4:** Option E — VeriWasm-style output checker + translation
   validation; fold the loader+checker into the formal-verification scope.
   This is what makes AOT'd code admissible in the MMU-free endpoint.
4. **Deferred / likely-rejected:** Options C/D (runtime JIT) — revisit
   only if a measured workload needs runtime specialization that AOT
   cannot provide, *and* only after Tier-0+wasmi verification lands.

---

## 9 · Open questions (for Gustavo)

1. **Is AOT the agreed direction**, with runtime JIT deferred? (The whole
   doc hinges on this.)
2. **Reuse Cranelift (offline) or build a `no_std` Wari codegen?** Reuse
   is faster to ship; bespoke is smaller + fully ours (prior-art posture).
   Recommendation: Cranelift offline first, bespoke later if size demands.
3. **WNM artifact format** — extend the Tier-2 signed-module envelope, or
   a new container? (Lean: extend it; one signing/attestation path.)
4. **Do Tier-1 customer modules get AOT, or only Tier-2 system modules
   first?** (Lean: Tier-2 first — fewer, already signed/attested — then
   Tier-1 once the checker is trusted.)
5. **Acceptable Spectre-hardening cost** in codegen, and the threat model
   for cross-tenant speculation on shared cores.

---

## 10 · Prior art

| Pattern | Source | Relevance |
|---------|--------|-----------|
| **AOT Wasm→native, no runtime compiler** | **Fastly Lucet** (2019) | Direct precedent for Option B; Wari already cites Fastly. |
| **Verify compiled-output sandbox safety (not the compiler)** | **VeriWasm** (Fastly/UCSD) | Makes Option B sound without trusting codegen → Option E. |
| Optimizing JIT/AOT backend | **Cranelift / Wasmtime** | Candidate offline backend; deterministic codegen. |
| Baseline single-pass JIT | **Winch** (Wasmtime), Firefox baseline | Option C reference, if ever. |
| Interpreter + AOT + JIT modes in one runtime | **WAMR** | Shows the tier menu; we deliberately pick fewer tiers. |
| Wasm→C AOT | **wasm2c / wabt** | Extreme-portability AOT; cite as alternative lowering. |
| Verified compilation | **CompCert**, **Alive2** | Bounds the cost of "verify the compiler" → why we prefer translation validation. |
| Pure interpreter, small TCB | **wasmi** (current) | The floor + reference oracle. |
| **Rejected: runtime megamorphic JIT** | **V8** | Already rejected in prior-art.md; this doc explains why for codegen specifically. |

---

## 11 · Decision log

- **D1 — Frame as "where does compilation happen," not "interpreter vs
  JIT."** The device/offline axis is what actually trades against Wari's
  values.
- **D2 — Recommend AOT (offline) over runtime JIT.** Keeps compiler out
  of TCB, preserves W^X/ROM/attestation/reproducibility, fits Phase-4.
- **D3 — Security boundary is the on-device *output checker*, not the
  compiler.** Lets us use a big offline compiler (Cranelift) without
  trusting it.
- **D4 — Keep wasmi as fallback + differential-testing oracle.** Never
  removed; it's the correctness reference.
- **D5 — Pursue interpreter tuning (Option A) in parallel** as the
  zero-risk floor; it may suffice once compute offloads to accelerators.
- **D6 — Runtime JIT (C/D) deferred, possibly permanently.** Reopen only
  on measured need + after Tier-0 verification.

---

## Appendix · Glossary
- **AOT** — ahead-of-time: compile before execution, here *offline* in the
  signing pipeline.
- **JIT** — just-in-time: compile *on the device at runtime*.
- **WNM** — Wari Native Module: signed artifact = native `.text` +
  relocations + safety certificate + embedded `.wasm`.
- **W^X** — write-xor-execute: no page both writable and executable.
- **Translation validation** — prove a *specific* compilation output
  refines the source semantics, instead of proving the compiler correct.
- **TCB** — trusted computing base: code whose bugs can break isolation.
