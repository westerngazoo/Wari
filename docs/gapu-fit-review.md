# GAPU v2.0 — Architecture Fit Review

> **Status**: accepted 2026-08-15. Decisions recorded in §6 are the
> architect's; the analysis is the co-architect review of
> [`gapu-architecture-v2.md`](gapu-architecture-v2.md) against Wari as
> actually built (through build 162 / the Phase-1c close-out).
>
> The strategic order this review produced: **(1) WASM cloud OS core →
> (2) AI/agent capability layer → (3) multikernel → (4) GAPU.**

---

## 1. Verdict

The GAPU vision fits the OS we have — unusually well, and measurably
better than it would have fit a month ago, because the Phase-1c
close-out built primitives the vision depends on (interrupt delivery,
resumable suspend, the DMA-correctness audit). The document contains
one claim we have disproven on silicon, one hardware assumption that
needs a datasheet check, and three architectural gaps that are real
work. None of the gaps invalidates the direction.

## 2. The datapath math, checked

Verified rather than assumed:

| Claim | Check |
|---|---|
| Cl(1,3) = 16 blades; dense product = 256 partial products | 2⁴ = 16; 16×16 = 256 ✓ |
| Sum tree depth 4 | log₂ 16 = 4 ✓ |
| Cl(1,7): bivectors 28, even subalgebra 128 | C(8,2) = 28; 2⁸/2 = 128 ✓ |
| p = 2³¹−1 Mersenne, shift-add reduction | ✓ (M31) |
| ~4 DSP48E2 per 31×31 product × 256 ≈ 1024 of 1248 DSPs | arithmetically right; tight — grade sparsity is mandatory for Cl(1,7), as the doc says |
| >300 MHz fully combinational | optimistic; the doc's own "(o pipeline mínimo)" is the right escape hatch |

**Convergent prior art for the F_p bet** (per the prior-art
discipline, this is labeled **our bet**, not established practice):
M31 is the field the Circle-STARK community standardized on
(Plonky3/stwo) for the identical shift-add reason; NTT accelerator
datapaths; RNS arithmetic; BAM (binary angular measurement) for
τ-phase encoding. Risks to carry openly: silent wraparound on
overflow, no in-field ordering/norms, division as multiplicative
inverse. Mitigation: fixed-point scaling discipline with range
analysis — which is Kani-checkable, plugging into the Phase-4 formal
story. Possible dual-use: the same field primitives serve the
Phase-2/3 crypto and attestation work.

## 3. Corrections to the document

### 3.1 JH7110 DMA coherence — disproven for the GMAC, unverified for PCIe

§3.1 states the JH7110's DMA bus is not coherent with L2. For the
GMAC this is **wrong, with silicon evidence**: the packet loss that
presented exactly like a coherence problem was a missing `volatile`
(see `STATE-OF-PLAY.md`, build 162), and the driver runs at 0% loss
with zero cache maintenance in the entire codebase. The non-coherent
SoC was the JH7100; the JH7110 attaches its peripherals through the
coherent port. The **PCIe master's** coherence is genuinely unknown —
it is a one-experiment question at GAPU bring-up, not a design input
to assume in either direction.

Design consequence regardless of the answer: cache maintenance is a
**per-platform policy** behind hooks that are no-ops on coherent
paths (the R2S/Ky-X1 port needs the same seam). If flushes are ever
needed on the JH7110, the mechanism is the SiFive CCache `flush64`
MMIO register (the U74 predates Zicbom) — S-mode accessible, needs a
kernel MMIO window and an INV.

### 3.2 Zero-copy works today by accident, not by design

wasmi backs linear memory with a `Vec<u8>`: requested alignment **1**,
and the base pointer **moves on `memory.grow`**. DMA addresses are
physical only because the kernel identity-maps (VA==PA). These are
audit findings P2/P3/P4. The fix is Hito 2, and it is smaller than
the doc fears: GAPU-attached instances get `maximum == initial`
memory (grow can never reallocate) allocated from a boot-reserved,
pinned, physically contiguous, cache-line-aligned arena. One PR.

### 3.3 KV260 physical interconnect — verify before Hito 3 assumes PCIe

The K26 SOM has PCIe Gen3 x4 on its transceivers; whether the
KV260 *vision carrier* exposes them usably needs the carrier
datasheet. If not, the first integration hop is Ethernet — where Wari
now has a proven, debugged path. Open item, deferred with the GAPU.

## 4. Where Wari already serves the vision

| Doc requirement | Exists today |
|---|---|
| Orchestrator of async task descriptors | seL4-style IPC + caps + `wari-policy` mediation, proven cross-tenant on silicon; the Planner→Executor brick is the same shape as CPU→GAPU dispatch |
| Completion-interrupt semantics | Interrupt delivery works (build 155+): PLIC → dispatch → Notification; Ctrl-R proven on the VF2 |
| Don't block the CPU during offload | Option-B resumable suspend (`call_resumable`) already carries IPC; extending it to `notification_wait` yields Hito 4's async completion |
| No floating point | Kernel is already no-fp `no_std`; F_p ops are pure i32/i64 Wasm; the AOT track compiles them to native RISC-V integer ops |
| Derived, validated descriptors | INV-24 (wire formats derived, never transcribed) is the exact discipline GAPU task descriptors need |
| §3.1's physical constraints | Already enumerated as audit findings P1–P4 before the doc existed |

## 5. Gaps (real work the vision needs)

1. **Notification wait + re-entrancy-safe dispatch.** Notifications
   are poll-only, and the IRQ→Notification path calls
   `object_pools()` from trap context (the INV-1 amendment's known
   exposure). Prerequisite for any completion interrupt. Shared with
   the AI/MCP layer — nothing GAPU-specific about it.
2. **DMA authority (the S6 decision).** The GAPU feeds Tier-1
   customer buffers to a DMA master with no IOMMU. Tier 0 must own
   translation, pinning, and validation of every descriptor —
   kernel-validated descriptor rings — or "double-sandboxed" is false
   for every DMA-visible path.
3. **The pinned contiguous allocator** (Hito 2, design in §3.2).
4. **Multikernel** — evaluated in `adr/ADR-001-multikernel-smp.md`:
   adopted as the SMP direction, implementation deferred.

## 6. Decisions (architect, 2026-08-15)

| # | Decision |
|---|---|
| D1 | Strategic order: **cloud-OS core → AI/agent layer → multikernel → GAPU (deferred to the end)** |
| D2 | "Wari" is the canonical spelling; the doc's "Wary" was drift |
| D3 | AI capability delivery: evaluation delegated to the co-architect; outcome = **user-space architecture with MCP as the trust-boundary protocol** (Executor as a signed Tier-2 MCP server; Planner pluggable — external model first, on-device later; Supervisor mints per-session attenuated caps). Principle: *the kernel knows nothing about AI; AI is a workload.* |
| D4 | Multikernel: adopted as direction now (ADR-001), implemented after the AI layer |
| D5 | S6 / DMA authority and the physical interconnect: deferred with the GAPU, but the descriptor-ring answer is presumed by this review |

## 7. Sequencing

| Order | Work | Serves |
|---|---|---|
| 1 | This document + roadmap reorder | record |
| 2 | ADR-001 multikernel | locks the SMP direction cheaply |
| 3 | **Dynamic module loading** | the cloud-OS spine; also the MCP `spawn` tool |
| 4 | `wari-http`, `wari-mcp` pure crates | MCP transport; real HTTP for the cloud story |
| 5 | Executor-as-MCP-server (Tier-2) + Supervisor session caps | the AI milestone |
| 6 | Notification wait queues | MCP async completions now; GAPU later |

GAPU items Hito 2/3 share prerequisites with rows 3–6, so deferring
the GAPU wastes nothing: the road passes through the same ground.
