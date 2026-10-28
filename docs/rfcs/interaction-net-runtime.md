# RFC — An Interaction-Net Runtime as a Tier-1 Workload

> **Status**: draft 2026-10-28, awaiting architect approval. Nothing here
> is scheduled: it does not change the v2.0 strategic order (cloud-OS
> core → AI layer → multikernel → GAPU) and adds nothing to Tier 0.
>
> **Scope class**: design note + one measurement spike. No kernel
> change, no new host function, no new syscall.
>
> **Origin**: a feasibility study of GC-free memory management for Wari
> (Perceus, linear/quantitative types, regions, interaction nets, an
> FPGA reduction unit, analog Geometry of Interaction). This RFC keeps
> the parts that fit Wari's architecture and records why the rest does
> not.

---

## 1. The question, in one sentence

Can functional workloads run on Wari without a tracing GC, and can a
live functional computation move between isolated instances, without
the kernel learning anything about lambda calculus?

Yes for the first, in user space, today. A conditional yes for the
second: it needs a bulk-transfer path between instances that does not
exist yet (§4.3), and it is a copy, not a zero-copy move.

## 2. What survives the review

| Vector from the study | Verdict | Where it lands |
|---|---|---|
| **V1** Perceus / FBIP (Koka → C → wasm32) | Viable in user space | §5: a spike that measures it |
| **V2** Quantitative types / regions (Idris 2, MLKit) | Viable in user space; **the kernel cannot rely on it** | §6 non-goal |
| **V3** Interaction nets on WASM linear memory | Viable as a **Tier-1 library**, not as a syscall | §4: the design |
| **V4** Reduction unit on an FPGA | Feasible research; competes for the only FPGA slot | §6: input to the GAPU review |
| **V5** Analog / mixed-signal GoI | Not an OS concern | §6 non-goal |

The principle that decides every row is the one already adopted for AI:
**the kernel knows nothing about the workload's computational model.**
A reducer is a workload.

## 3. Corrections to the source study

Recorded so they are not re-derived later.

1. **"Zero-copy migration of sub-graphs between WASM instances" is not
   possible under Wari's isolation.** Linear memories are disjoint by
   construction (`𝓜ᵢ ∩ 𝓜ⱼ = ∅`, enforced by the validator *and* the MMU).
   Moving bytes between them is a copy. The study's own stack diagram
   says `memcpy`.
2. **Strong confluence does not make a sub-graph relocatable.** Confluence
   is about reduction order. Relocating by `addr + Δ` is sound only for a
   **closed** sub-graph (no wires to cells outside it), or one whose free
   wires are explicitly listed and re-linked on the receiving side.
3. **A received graph is adversarial input.** It crosses a tenant
   boundary, so forged addresses, asymmetric links and oversized graphs
   must be rejected before the first reduction (§4.2). The study had no
   validation step.
4. **The latency figures are native x86 figures.** "~1–5 ns per node"
   is roughly 1.5–7 cycles on a ~1.5 GHz in-order U74, before the
   interpreter. Tier-1 code runs under `wasmi` today (AOT is in
   progress), so any number has to be measured on the VF2 (§5.2).
5. **The "one-cycle" FPGA annihilation leaves out the memory.** The
   reference RTL takes both nodes' auxiliary ports as inputs that are
   already fetched (a BRAM read is 1–2 cycles). Re-linking writes up to
   four cells, and freeing two addresses per cycle needs two write ports
   on the free-list. Commutation (γ⋈δ) needs several writes. One redex
   per cycle is plausible for throughput with a pipeline, not as latency.
6. **The port encoding is inconsistent.** It uses 64-bit ports with the
   tag at bit 60 in one place and 32-bit ports (3+1+28) in another. §4.1
   picks one.

## 4. Proposed design: V3 as a Tier-1 library

### 4.1 Shape

- A pure `no_std` crate (working name `inet`) compiled **into** the
  Tier-1 module that wants it. Host-testable, no `unsafe`, no allocation
  after setup. This follows the pure-before-impure rule.
- The node pool is a fixed array in the module's own linear memory:
  `Cell = [Port; 2]` plus a `Port` for the principal, with
  `Port = u32 { tag: 3, slot: 2, index: 27 }`. That is 32-bit ports
  addressing up to 2²⁷ cells (1.5 GiB at 12 bytes per cell), well beyond
  any per-instance memory budget. A redex queue and a free-list stack sit
  beside it. Combinators: γ, δ, ε (symmetric interaction combinators,
  see §7).
- The reducer is a loop over the redex queue. Allocation and freeing are
  pushes and pops on the free-list, so memory is reclaimed at the
  annihilation step with no GC. This is the property the study was after,
  and it needs nothing from the kernel.

### 4.2 Receiving a graph: the trust boundary

A graph arriving from another instance is a byte buffer, decoded and
checked by a **pure, total, O(n) validator** before any cell is
reduced:

| Check | Rejects |
|---|---|
| `n ≤ budget` (from the receiver's own config) | Memory-exhaustion bombs |
| Every tag ∈ {VAR, γ, δ, ε, FREE} | Forged node kinds |
| Every `index < n` | Out-of-range pointers |
| Link symmetry: if *a.p* → *b.q* then *b.q* → *a.p* | Half-wired cells, aliasing |
| Free wires appear only in the declared interface list | Pointers into the sender's memory |

The validator is the module's own defense. Wasm already prevents an
escape *out* of the receiver's linear memory. What the validator adds is
that the receiver's reducer never acts on a malformed graph. It ships
with a host unit test per row, an adversarial test per row, and a
`cargo-fuzz` target (decoder + validator) on the same schedule as the
existing parser fuzzing.

### 4.3 Moving a graph between instances: what is missing

Today's IPC carries a badge plus a few words in registers, which is
enough to name a graph but not to carry one. `docs/ipc-design.md` sends
larger payloads through "a shared linmem region referenced by a
registered handle", but the registered-capability ring that exists
today is **module ↔ kernel, inside the module's own linear memory**. No
capability-gated buffer exists between two instances yet.

So cross-instance graph transfer depends on a general bulk-transfer
mechanism. It must:

- be capability-gated and size-bounded by the receiver,
- copy (the kernel copies between linear memories, or a granted shared
  page is mapped into both, with the MMU caveats that implies),
- not allocate in dispatch context (R2),
- know nothing about graphs. Graphs are just one client of it.

That mechanism belongs to the Phase-2 module lifecycle and IPC work, on
its own merits. This RFC does **not** propose it. It records that V3's
migration story waits on it.

## 5. Spike: Koka on Wari (V1)

### 5.1 Question

What does it take to run a Koka program as a Tier-1 module? Koka emits
C with a small runtime. Perceus gives garbage-free reference counting
with in-place reuse, which is a credible "no GC" story for ordinary
functional user code.

### 5.2 What to measure

1. **Toolchain**: does Koka's C output plus its runtime compile to
   `wasm32-unknown-unknown` (or wasm32-wasi via clang) without threads,
   with only Wari's WASI subset (`fd_write`, `proc_exit`,
   `clock_time_get`, `args_*`, `environ_*`, `random_get`)? List every
   missing import.
2. **Size**: the signed module size for a hello-world and a small
   benchmark (e.g. a red-black tree insert loop).
3. **Cost on silicon**: wall time on the VF2 under `wasmi`, against the
   same algorithm in Rust compiled to wasm32. That gives one honest
   ratio instead of borrowed nanosecond figures.

### 5.3 Exit

A short results note in `docs/research/`: works / works with listed
shims / does not work. No code lands in the kernel either way. A missing
WASI call becomes a separate WASI-surface proposal, judged on its own
merits.

## 6. Non-goals

- **Kernel-level linearity from source types (V2).** Quantitative types
  and session types are erased when compiled to wasm. Wari's guarantees
  come from the wasm validator, the MMU and capabilities, never from the
  customer's source language. Where the kernel needs a use-once
  guarantee it already has the runtime form: one-shot reply
  capabilities, as in seL4. Customers are free to use Idris 2 or region
  inference inside their modules.
- **A reduction syscall** (`sys_*_migrate` or similar). This is rejected
  by §2's principle, by R2, and by §4.3.
- **An FPGA reduction unit (V4) on the current roadmap.** The roadmap's
  only FPGA slot is the GAPU (Phase 5, deferred by decision). A reduction
  unit is recorded as an **alternative workload for that slot**, to be
  weighed in the GAPU review with §3.5's corrections. The descriptor-ring
  and DMA-authority answers the GAPU review already presumes would serve
  either one.
- **Analog Geometry of Interaction (V5).** `(I − σU)⁻¹` exists only when
  the execution series converges, which fails for non-normalizing terms.
  Noise and parasitics bound depth to a few nodes. It is device-physics
  research, not OS design.
- **Lean 4 as a Tier-1 language.** Its runtime (GMP, threads) is far
  heavier than Koka's for the same question.

## 7. Prior art

| Idea | Source |
|---|---|
| Interaction nets | Y. Lafont, *Interaction Nets*, POPL 1990 |
| Interaction combinators (γ, δ, ε) | Y. Lafont, *Interaction Combinators*, Information and Computation 137(1), 1997 |
| Symmetric combinators (both annihilations straight) | D. Mazza, *A denotational semantics for the symmetric interaction combinators*, MSCS 17(3), 2007 |
| λ-calculus on interaction nets | J. Lamping, *An algorithm for optimal lambda calculus reduction*, POPL 1990; I. Mackie, *Efficient λ-evaluation with interaction nets*, RTA 2004 |
| Parallel IC runtime in practice | V. Taelin et al., HVM / HVM2 (Higher Order Company, 2022–) |
| Garbage-free RC with reuse | A. Reinking, N. Xie, L. de Moura, D. Leijen, *Perceus*, PLDI 2021 |
| Fully in-place functional programming | A. Lorenzen, D. Leijen, W. Swierstra, *FP²*, ICFP 2023 |
| Region-based memory management | M. Tofte, J.-P. Talpin, Information and Computation 132(2), 1997 |
| Quantitative type theory | E. Brady, *Idris 2: Quantitative Type Theory in Practice*, ECOOP 2021 |
| Geometry of Interaction | J.-Y. Girard, *Geometry of Interaction I*, 1989 |
| GoI compiled to hardware (the V4/V5 precedent) | D. R. Ghica, *Geometry of Synthesis*, POPL 2007 |
| Use-once reply capabilities | seL4 (Heiser et al.) |

## 8. Decisions requested

1. **Accept §2's verdicts**: V3 as a Tier-1 library and V1 as a spike,
   with V2, V4 and V5 as recorded non-goals or GAPU-review input.
2. **Approve the §5 spike** as unscheduled backlog that does not block
   anything on the critical path.
3. **Note §4.3** as a requirement for the Phase-2 IPC / lifecycle work:
   a capability-gated, receiver-bounded bulk-transfer path between
   instances, needed for reasons beyond this RFC.
