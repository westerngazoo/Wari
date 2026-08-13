# Wari — A Sovereign OS

> **Volume 2 of The Goose Factor.** Volume 1 (goose-os) told the
> "learning to build an OS" story. Volume 2 tells the "building a
> production sovereign cloud OS" story — the architecture, the kernel
> from reset to runtime, silicon bring-up, and how to write the drivers
> and apps that run on it.

This is both a **narrative** (why every decision was made) and a
**developer manual** (how to build, sign, flash, and extend Wari). Read
Part 1 top-to-bottom before reading code; Parts 2–4 are the working
reference; Parts 5+ are the road ahead.

Each chapter lands as a **draft** the architect approves before it is
final — the same discipline as every PR.

---

## The spine

### Part 1 — Architecture & Philosophy *(the derivation — written before code)*
| Ch | Title | Purpose |
|----|-------|---------|
| 1 | Why Wari | The sovereign-cloud thesis, the LATAM context, the name |
| 2 | The Shoulders We Stand On | Prior-art survey — what we inherit, what we reject |
| 3 | The WASM-Only Bet | Why no ELF; Cloudflare/Fastly/Firecracker comparison |
| 4 | Two Tiers | The Tier 0/1/2 model; why drivers run in ring 0 |
| 5 | Inheritance from Goose | Cherry-pick audit — what survives, what is rewritten |
| 6 | The Invariants | INV-N as first-class documentation |
| 7 | The Immutable Endpoint | Phase 4: MMU-free custom SoC + hash-attested ROM kernel |

### Part 2 — The Kernel, From Reset *(how it actually works — init → runtime)*
| Ch | Title | Purpose |
|----|-------|---------|
| 8 | Boot & Init | `boot.S` → `kmain`, the staged boot sequence, the handoff to S-mode |
| 9 | Memory & the Sv39 MMU | Page allocator, the page-table walker, `kvm` map, the identity+kernel window |
| 10 | Traps & the PLIC | The trap vector, dispatch table, external-interrupt routing |
| 11 | The wasmi Runtime & WASI | Embedding the interpreter, the host-fn surface, `fd_write`/`proc_exit`, linear memory |
| 12 | Capabilities | The seL4-derived cap system — CSpace, objects, rights, the trust boundary |
| 13 | The Scheduler & Processes | Process states, the resumable Tier-1 pool, run-to-block-to-resume |
| 14 | Synchronous IPC | Endpoints, the rendezvous state machine, the Option-B blocking model |

### Part 3 — Silicon Bring-up *(the VisionFive 2)*
| Ch | Title | Purpose |
|----|-------|---------|
| 15 | Cross-Compiling for the VF2 | The RV64GC target, the two-address-space problem, the linker script |
| 16 | Per-Platform Drivers | One blob or two; the cfg blob switch; MMIO stride |
| 17 | Hello from Silicon | The first Tier-1 WASM on real hardware |

### Part 4 — Writing Drivers *(the developer manual: write, sign, create)*
| Ch | Title | Purpose |
|----|-------|---------|
| 18 | Anatomy of a Tier-2 Driver | The trait + macro, the `cfg`/features platform system, MMIO/MDIO via host fns |
| 19 | The Manifest & Signing | The driver contract, the bidirectional sign check, the trust chain |
| 20 | Build, Embed, Flash | The pipeline, the stale-driver guard, the release/pointer flow |
| 21 | War Story: The Net Driver | GMAC bring-up, the RGMII delay hunt, reading the diagnostic trace |

### Part 5 — The AOT Engine *(off-device compilation — in progress)*
| Ch | Title | Purpose |
|----|-------|---------|
| 22 | Interpreter, JIT, or AOT | The execution-strategy fork and why AOT-not-JIT |
| 23 | The Safety Certificate | Running native code without trusting the compiler (VeriWasm-derived) |

### Part 6 — The Road Ahead
| Ch | Title | Purpose |
|----|-------|---------|
| 24 | Confidential Compute | RISC-V CoVE, per-tenant ciphertext RAM |
| 25 | The GAPU | The FPGA coprocessor thesis |
| 26 | Kernel in ROM | Formal verification + the frozen-image endpoint |

---

## Reading paths

- **New contributor:** Part 1 → Part 2 → Part 4. Then read code.
- **Driver author:** Part 4 (with Part 2 Ch 11 for the host-fn surface).
- **Auditor:** Part 1 Ch 6 (invariants) → Part 2 Ch 12 (capabilities) →
  `docs/security-model.md`.
- **The curious:** Part 1 Ch 1 and Ch 7 — the thesis and the endgame.

Chapters cite the source-of-truth docs (`docs/*.md`) and the code
(`file:line`) they narrate; the docs are the spec, the book is the
explanation, the code is the truth. Where they disagree, the code wins
and the chapter is stale — fix it.
