# Wari — Roadmap & Architecture (one-page brief)

> Share this with engineers, architects, and prospective contributors
> who need a quick read on what Wari is, why it exists, and where it
> is going. For depth, follow the cross-references at the bottom.

---

## What Wari is

A sovereign, WASM-native operating system for RISC-V. Built for
Latin American cloud infrastructure that wants to compete with
Cloudflare on edge density and with AWS/Azure on confidentiality —
without depending on x86, Nvidia, or any closed-firmware silicon.

**One sentence**: Wari is what Cloudflare Workers would look like if
it ran on open silicon, were built in Rust from boot zero, and could
host its own drivers as signed WASM modules instead of trusting
binary blobs.

---

## Why Wari exists

LATAM governments, banks, hospitals, and infrastructure operators
increasingly require:

- **Auditable code at every layer** — kernel, drivers, runtime, apps
- **Open hardware** — no black-box management engines
- **Local jurisdiction** — data and silicon physically + legally
  outside US/EU control
- **Defense in depth** — structural memory safety, not "trust us"
- **Cloudflare-class edge density** — thousands of tenants per
  machine, microsecond cold start

No existing OS hits all five. Linux misses on confidentiality + TCB
size. Cloudflare misses on hardware sovereignty. AWS misses on
auditability and silicon openness. Wari is the focused bet on hitting
all five at once.

---

## The architecture, in one diagram

```
┌─────────────────────────────────────────────────────────────┐
│  Tier 1 — Customer WASM  (U-mode, MMU + WASM sandbox)       │
│   • Customer apps as signed .wasm modules                   │
│   • Target: 10 000 – 50 000 instances per board             │
│   • Access only via WASI host functions                     │
│   • Cold start target: < 10 ms                              │
├─────────────────────────────────────────────────────────────┤
│  Tier 2 — System WASM  (S-mode, WASM-only sandbox)          │
│   • Drivers + system services, all signed .wasm             │
│   • Direct MMIO + IRQ access via static capabilities        │
│   • ~10–50 modules per board                                │
│   • Bytecode-verified before any execution                  │
├─────────────────────────────────────────────────────────────┤
│  Tier 0 — Native Kernel  (S-mode Rust, no_std)              │
│   • boot · trap · MMU · scheduler · wasmi runtime           │
│   • ~5–10 KLOC, formal-verification target                  │
│   • Only third-party code: wasmi (interpreter)              │
│   • No ELF loader. Ever. (Rule R7)                          │
└─────────────────────────────────────────────────────────────┘
```

**The architectural invariant**: all code that runs on Wari is
either native kernel Rust (Tier 0) or WASM (Tier 1 or Tier 2).
Privilege level is a per-module capability, not a language barrier.

**The security model**, three layers, all of which must hold:

1. **Structural** — WASM type system + validator. No module can
   construct pointers outside its linear memory.
2. **Hardware** — Sv39 MMU + (Phase 1) RISC-V PMP + (Phase 3) CoVE.
   Even if a structural escape happens, hardware contains it.
3. **Cryptographic** — (Phase 2) Zkn/Zks hardware crypto. Data at
   rest is AES-256-GCM, in-flight is BLAKE3-authenticated.

---

## Tech stack

| Layer | Choice | Why |
|---|---|---|
| ISA | RISC-V RV64GC | Open spec, no licensing, no proprietary blobs |
| Language | Rust stable, `no_std` | Memory safety + small TCB |
| WASM runtime | `wasmi` (no_std interpreter) | RISC-V ready today; JIT deferred |
| Drivers | WASM modules (Tier 2) | Auditable, sandboxed, signed |
| Boot chain | OpenSBI → U-Boot → Wari | Open, transparent |
| Target HW (Phase 0) | StarFive VisionFive 2 (JH7110) | Affordable, well-documented |
| Target HW (Phase 3) | Multi-board RISC-V + custom GAPU FPGA | Hardware sovereignty |
| License | AGPL-3.0-only | Auditable, hyperscaler-resistant |

---

## Phase roadmap

```
Phase 0  — Cloudflare-on-RISC-V demo
           Hello.wasm at boot. wasmi runtime. Tier-2 UART driver.
           Kernel < 5 KLOC, no scheduler, no IPC.
           Exit: signed .wasm prints to UART, halts, no kernel panic
           under adversarial inputs.

Phase 1  — Two-tier with capabilities
           Capability system (cap table, mint, grant, revoke).
           Tier-2 net driver (smoltcp-in-wasm). Module attestation.
           Multi-tenant scheduler + synchronous IPC.

Phase 2  — Sovereign AI + Docker ingress
           WASI-NN host functions. Tier-2 GPU driver over PCIe.
           Hardware crypto (Zkn/Zks). tools/oci2wasm — Docker→WASM
           compiler for Rust/Go/Python/Node workloads.

Phase 3  — Confidential compute + GAPU
           RISC-V CoVE integration (ciphertext RAM per tenant).
           GAPU FPGA Tier-2 driver. Per-module formal verification.
           Multi-board clustering. External security audit.

Phase 4  — Immutable kernel + custom silicon
           Functional-core / imperative-shell refactor of Tier 0.
           Kani proofs for capability + scheduler. wasmi correctness
           proof (academic collaboration). Hash-attested ROM kernel.
           Optional MMU-free SoC variant.
```

Phase 0 is in execution. Each phase has numbered, testable exit
criteria — see `CLAUDE.md` §Phase 0 Exit Criteria for the template.

---

## What we inherit, what we reject

Wari does not invent from scratch. Every architectural pattern is
either inherited (with credit) or deliberately rejected.

**Inherits from:**
- seL4 — capability system + synchronous IPC + formal verification
  ambition
- Fastly Compute@Edge — WASM as the process boundary
- Cloudflare Workers — shared-runtime density model
- Firecracker (AWS) + Hubris (Oxide) — narrow-purpose Rust kernel
  scope
- RedLeaf (UCI, SOSP '20) + Singularity (MSR) — language-enforced
  domain isolation
- AWS Nitro — HW/SW co-design (our analog: GAPU FPGA in Phase 3)

**Rejects:**
- V8 / JavaScript runtime — TCB too large, Google-controlled
- OCI / Docker compatibility as architecture — drag breaks the
  density and TCB stories. (Phase 2 handles Docker via host-side
  compilation to WASM, not retrofit.)
- Userspace syscall shims (gVisor) — unnecessary if Tier 0 is small
- Proprietary silicon isolation (Intel SGX lineage) — sovereignty
  requires open hardware

Full survey with citations: `docs/prior-art.md`.

---

## How we work

Wari is a high-discipline project. Every line of code lands via
pull request, each PR is reviewed locally + tested locally before
merge, and every non-obvious decision is documented in the PR body.

- **PR workflow**: branch per PR, mandatory PR-body template, the
  "Why/How depth rule" requires every non-obvious decision to
  answer four questions — what was picked, what was considered, why
  this won, what cost was accepted. Details: `docs/pr-workflow.md`.

- **Engineering principles**: Think Before Coding, Simplicity First,
  Surgical Changes, Goal-Driven Execution. Details:
  `docs/engineering-principles.md`.

- **Absolute rules**: every `unsafe` block has a SAFETY comment
  citing an INV-N invariant; no heap in interrupt context; MMIO
  through typed wrappers only; no panics in kernel syscall paths;
  no ELF anywhere in the customer ABI; reproducible builds. Details:
  `CLAUDE.md` §Absolute Rules.

- **Testing strategy**: four layers (unit, integration in QEMU,
  adversarial security, fuzz), with every trust-boundary-crossing
  feature blocked from merge until its adversarial test exists.
  Details: `docs/testing.md`.

- **Audit cadence**: every phase milestone produces a dated audit
  document in `docs/audits/`. External security review at Phase 3
  exit. Details: `docs/security-model.md`.

---

## Status today

Phase 0 in execution. Boot bringup PR (PR #1) open. Subsequent PRs
land memory primitives, MMU enable, wasmi embedding, Tier-2 UART
driver, Tier-1 hello module, and the Phase-0 audit gate.

Repo: `https://github.com/westerngazoo/Wari`. Private until Phase 0
demo lands.

---

## Where to go next

| If you want… | Read |
|---|---|
| Architectural depth | `docs/architecture.md` + the seven-chapter book in `docs/book/part-1-architecture/` |
| Why we picked these patterns | `docs/prior-art.md` |
| The unsafe-code audit framework | `docs/invariants.md` |
| The threat model | `docs/security-model.md` |
| How to contribute | `docs/pr-workflow.md` and `docs/engineering-principles.md` |
| The full project rules | `CLAUDE.md` (top-level) |
| The product / market thesis | this document, top section |
