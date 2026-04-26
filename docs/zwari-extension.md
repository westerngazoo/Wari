# Zwari — RISC-V Custom Extension for WASM Acceleration

> **Status:** Design draft v0. No silicon yet. Software emulation
> path defined. FPGA prototype: Phase 3 milestone.
>
> **Audience:** technical readers (kernel, hardware, academic
> reviewers). For non-technical positioning see
> [`docs/pitch/jaime-v0.md`](pitch/jaime-v0.md) slide 8.
>
> **Authors:** Gustavo Delgadillo + Wari project. Open RFC.

---

## 1 · Why this exists

Wari is a WASM-native operating system for RISC-V. The Wari thesis
deliberately rejects JIT compilation as the path to native-comparable
performance, because adding a production JIT (Cranelift, LLVM-based)
to the kernel TCB:

- explodes the kernel from ~8 KLOC to ~250 KLOC (Cranelift alone),
  killing the "auditable in <1 week" property that is Wari's core
  market differentiator;
- requires runtime code generation, which forces W^X violations and
  introduces JIT-spray / JIT-ROP attack vectors that have produced
  hundreds of CVEs in browser engines over the past decade;
- defeats formal verification: a verified kernel + unverified JIT is
  an unverified system.

But the performance gap remains real. A pure interpreter
(wasmi 1.0.9, current) is 10-50× slower than native code on CPU-bound
WASM workloads. Even with an optimized interpreter (Wasm3, Phase 2
candidate, ~3-10× slowdown), there is significant headroom that
sovereign-cloud customers will eventually demand.

**Zwari** closes that gap **in hardware** — without runtime code
generation, without W^X violations, without growing the kernel TCB.

Zwari is a small RISC-V custom extension (the RISC-V ISA explicitly
reserves opcode space for vendor extensions) that adds instructions
specialized to the hot paths of a WASM interpreter. The interpreter
calls these instructions; if the silicon implements them, they run
at hardware speed; if not, a software fallback executes the same
semantics. **Compatibility is free, performance is opt-in.**

---

## 2 · Design principles

| # | Principle | Why |
|---|-----------|-----|
| Z1 | Each Zwari instruction has a software fallback with bit-identical semantics | Forward and backward compat across silicon generations |
| Z2 | The extension stays small (target: ≤16 instructions in v1) | Hardware verification is tractable; FPGA fits on cheap parts |
| Z3 | No runtime code generation. Ever | Preserves the no-JIT thesis; auditable RTL replaces unauditable runtime codegen |
| Z4 | Operates on plain RISC-V GPRs and memory; no hidden state | Trap-and-save behaves like ordinary RISC-V; OS scheduling unaffected |
| Z5 | RTL must be open-source under permissive license | Sovereignty pillar: silicon you can audit |
| Z6 | Encoding stays inside RISC-V `custom-0` / `custom-1` opcode space | No conflict with future ratified extensions |

These principles eliminate most of the JIT attack surface by
construction. There is no writable+executable memory region. There
is no compiler running at runtime. The hardware does one thing:
execute predefined instructions faster than a software loop can
emulate them.

---

## 3 · Proposed instruction set (v1 draft)

This is a **first draft** intended for community feedback, not a
ratified spec. Encoding details (immediates, register fields) are
omitted; this is the semantic surface.

### 3.1 · Dispatch acceleration

```
zwari.dispatch  rd, rs1
```

Read one byte at `[rs1]` (the next WASM opcode), increment `rs1`,
store the address of the corresponding handler in `rd`. Subsumes
the bytecode fetch + dispatch-table lookup that dominates
interpreter inner loops (~30% of CPU time in profiled wasmi).

**Hardware win**: 1 cycle vs ~5-8 cycles in software (load + mask +
shift + indexed load + jump prep).

### 3.2 · Memory bounds check

```
zwari.bounds  rd, rs1, rs2
```

Test whether `[rs1, rs1 + rs2)` lies within the current WASM linear
memory. Sets `rd` to 0 on success, 1 on out-of-bounds. The current
linear-memory base + length live in two new CSRs (`zwari.lmbase`,
`zwari.lmlen`) writable only by Tier-0.

**Hardware win**: 1 cycle vs 4-6 cycles in software, AND removes a
hard-to-verify hot path from the interpreter (bounds-check bugs are
the #1 source of WASM sandbox escapes historically).

### 3.3 · Stack-machine micro-ops

```
zwari.push   rs1            # push rs1 onto WASM operand stack
zwari.pop    rd             # pop top of stack into rd
zwari.peek   rd, imm        # read stack[top - imm] into rd
zwari.dup    imm            # duplicate stack[top - imm]
```

The WASM operand stack is the second-hottest interpreter structure
(~20% of CPU time). Native instructions for push/pop/peek replace
explicit pointer arithmetic + load/store sequences.

**Hardware win**: 1 cycle each vs 2-4 cycles in software.

### 3.4 · Local access

```
zwari.local.get  rd, imm
zwari.local.set  rs1, imm
```

WASM `local.get` / `local.set` are extremely common (every function
prologue + body uses them). A dedicated instruction with the local
index baked in saves the indexed load.

### 3.5 · Conditional control flow

```
zwari.br.if   rs1, imm
```

WASM `br_if` semantics in one instruction: pop value from operand
stack, if non-zero branch by `imm` WASM bytecode bytes (and update
the WASM PC accordingly).

### 3.6 · Module CSRs

| CSR | Width | Writable from | Purpose |
|-----|-------|---------------|---------|
| `zwari.lmbase` | XLEN | Tier-0 only (S-mode write trap) | Base address of current WASM linear memory |
| `zwari.lmlen`  | XLEN | Tier-0 only | Length of current WASM linear memory |
| `zwari.spbase` | XLEN | Tier-0 only | Base of current WASM operand stack |
| `zwari.sptop`  | XLEN | Tier-0 + Zwari ops | Current top-of-stack pointer |
| `zwari.locals` | XLEN | Tier-0 only | Base of current WASM locals frame |

Tier-0 (the Wari kernel) updates these CSRs at WASM context switch.
Tier-1 / Tier-2 WASM code never sees the CSRs directly; the
interpreter reads them implicitly through the Zwari instructions.

---

## 4 · Estimated performance impact

Honest projection (will be measured, not assumed, when FPGA exists):

| Workload class | Wasm3 (sw) | Wasm3 + Zwari (sw fallback) | Wasm3 + Zwari (hw) |
|----------------|-----------:|----------------------------:|-------------------:|
| CPU-bound (compute) | 5-10× slower than native | 5-10× (no change — fallback ≈ existing) | **1.5-2.5× slower** |
| IO-bound (REST API + DB) | 1.5-2× slower than native | 1.5-2× (DB dominates) | **1.2-1.5× slower** |
| Cold load (validate + parse) | baseline | baseline | **20-40% faster** (CSR setup amortized) |

These are projections from analogous architectures (NVIDIA NVENC,
Apple Neural Engine, Google TPU all show 10-50× speedup on
specialized hot paths). RISC-V's relatively simple pipeline makes
custom-extension acceleration easier to reason about than CISC
analogs.

The point is not "Zwari beats JIT on raw throughput" — it is
**"Zwari approaches JIT throughput while preserving auditability,
without W^X violations, without growing the kernel TCB."**

---

## 5 · Implementation path

### 5.1 · Software-first (Phase 2, in progress)

- Wasm3 interpreter ported to no_std S-mode (this is the immediate
  Phase 2 work; see `docs/research/wasm3-port-evaluation.md`)
- Zwari instructions emitted by interpreter as **inline assembly**
  with software-fallback `#ifdef`. On silicon without Zwari, the
  fallback runs (no perf gain). On silicon with Zwari, the
  hardware accelerates it
- This means the interpreter can be developed and tested **today**
  on stock VF2 silicon with zero hardware work; the Zwari
  acceleration becomes a drop-in win when hardware lands

### 5.2 · FPGA prototype (Phase 3a, ~12 months)

- Target: LiteX-based open RISC-V SoC on Lattice ECP5 or Xilinx
  Artix-7. Estimated FPGA cost: $300-1500 USD
- RTL written in SpinalHDL or Chisel (both used by LiteX ecosystem)
- Initial v1 implements ~6 instructions (dispatch + bounds + push +
  pop + br.if + local.get) — the 80/20 of interpreter hot paths
- Validation: cycle-accurate simulation against software fallback,
  bit-identical results required

### 5.3 · ASIC tapeout (Phase 4, multi-year)

- Requires foundry partnership. SkyWater 130 nm (open PDK) is the
  cheapest entry path; TSMC / GlobalFoundries for production
- Realistic budget: $200K-2M USD for a small-area test chip,
  $5M-30M for a production part
- Likely funding: LATAM government partnership (Argentina's
  Fundación Sadosky, Brazil's CTI Renato Archer, Mexico's CINVESTAV
  all have nascent silicon-sovereignty initiatives)
- This is genuinely a 3-5 year horizon; framing as such avoids
  overpromising

---

## 6 · Verification and audit posture

Hardware verification is, paradoxically, easier than verifying a
JIT compiler. The reasons:

1. **The state space is bounded.** A CPU instruction has well-defined
   inputs (registers, memory) and outputs. A JIT compiler has
   unbounded compile-time state.
2. **Mature tooling exists.** Cadence JasperGold, Synopsys VC Formal,
   open-source Yosys + SymbiYosys can prove RTL properties
   exhaustively.
3. **Coq/Lean models of RISC-V exist.** sail-riscv is the official
   formal model; Zwari extensions can be added to the same
   formalism and properties proved against it.

The audit story is therefore: open RTL + formal model + bit-exact
software fallback = three independent ways to verify that the
silicon does what the spec says. None of these are available for a
JIT.

---

## 7 · Comparison with JIT — the table that matters

| Dimension | JIT (Cranelift in TCB) | Zwari hardware extension |
|-----------|------------------------|--------------------------|
| Native-comparable performance | Yes (1.5-2× slower than native) | Yes (1.5-2.5× projected) |
| Kernel TCB growth | +250,000 LOC | **0** |
| Runtime code generation | Yes (W^X required) | **No** |
| Attack surface | JIT compiler bugs → sandbox escape | Hardware errata only (rare, well-understood) |
| Formal verification feasibility | Hard (CompCert took 10+ years for C) | **Tractable** (sail-riscv + property proofs) |
| LATAM sovereignty story | Neutral (still depends on x86/ARM) | **Reinforced** (open silicon + open OS) |
| Differentiator vs other WASM runtimes | None (everyone has JIT) | **Unique** (no other WASM-OS has hardware) |
| Time to first working version | 6-12 months team | 12-18 months for FPGA (longer for ASIC) |
| Dependency on third parties | Cranelift project + LLVM ecosystem | Open RTL + foundry (only at ASIC stage) |

The trade-off is **time-to-perf vs ownership-of-perf**. JIT gets
you to good performance fast but you rent it from the Cranelift
project and inherit its attack surface. Zwari takes longer but you
own every transistor of the acceleration path.

For the Wari thesis (sovereignty, auditability), Zwari is correct.
For a hyperscaler clone (throughput at any cost), JIT would be
correct. We are not building a hyperscaler clone.

---

## 8 · Open research questions

These are honest gaps the design has not yet resolved:

1. **Context switch cost of Zwari CSRs.** Updating five CSRs on
   every WASM module switch may be expensive enough to dominate the
   savings on short-lived requests. Needs measurement.
2. **Multi-tenant silicon contention.** If multiple Tier-1 instances
   share one Zwari core, how do they share `zwari.sptop`? Options:
   per-hart Zwari state (more silicon), or trap-and-emulate on
   contention (complexity). Open.
3. **Validation of WASM modules at load.** Some validation must
   still happen in software (signature check, structural
   well-formedness). The Wasm3 port already moves spec validation
   to the signer; Zwari does not change this story.
4. **Interaction with WASM proposals.** WASM GC, WASM exception
   handling, WASM threads — all in flight in the upstream spec.
   Zwari v1 targets WASM 1.0 (MVP). v2 considerations are deferred.
5. **Patent landscape.** Hardware acceleration of bytecode VMs has
   prior art going back to picoJava (Sun, 1990s) and Jazelle (ARM,
   2001). A patent search is required before silicon spin. The
   open-RTL strategy mitigates this somewhat (defensive disclosure)
   but does not eliminate it.

---

## 9 · Why this name

`Zwari` follows the RISC-V naming convention for vendor extensions:
`Z` prefix = standard extension naming pattern (e.g., `Zicsr`,
`Zifencei`); `wari` = the project. Pronounced "zee-WAH-ree" or
"zwah-ree". The single-word form keeps it consistent with how the
extension would appear in `misa` / `marchid` reporting fields and
in `-march=rv64gc_zwari` compiler invocations.

---

## 10 · References and prior art

- RISC-V ISA Manual Vol 1, §27 — custom extension space allocation
- sail-riscv — official RISC-V formal model
  ([github.com/riscv/sail-riscv](https://github.com/riscv/sail-riscv))
- LiteX — open RISC-V SoC framework
  ([github.com/enjoy-digital/litex](https://github.com/enjoy-digital/litex))
- picoJava (Sun Microsystems, ~1997) — early hardware JVM, prior art
  for bytecode acceleration
- ARM Jazelle (2001) — hardware-assisted bytecode execution, prior art
- "WARP" and "Hardware Acceleration of WebAssembly" — academic
  papers from UC Davis / ETH Zürich (cite specific DOIs in v2 of
  this doc once verified)
- Cranelift / wasmtime project — the JIT path Wari deliberately
  declines

---

## Appendix A · Decision log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-04-26 | Reject JIT path | TCB explosion + W^X attack surface incompatible with Wari thesis |
| 2026-04-26 | Adopt interpreter-optimization + custom-extension trajectory | Preserves audit story; opens path to silicon moat |
| 2026-04-26 | Name extension `Zwari` | RISC-V naming convention compliance |
| TBD | Pick FPGA target board | LiteX ECP5 vs Artix-7 — pending cost / ecosystem comparison |
| TBD | Pick HDL (SpinalHDL vs Chisel) | Pending team familiarity assessment |

## Appendix B · How this slots into the roadmap

```
Phase 1b · 8 sem  →  Phase 2 · 6 mo   →  Phase 3a · 12 mo  →  Phase 3b · 12 mo →  Phase 4 · 36 mo
   caps + IPC         Wasm3 port        Zwari sw fallback     Zwari FPGA           Zwari ASIC
   net driver         Sign-time            (no perf gain        (perf real,         (production
   demo cluster       validation            yet)                 + paper)            silicon)
```

Each phase preserves backward compatibility: a Wari binary built for
Phase 4 silicon must still run on Phase 1b silicon (with software
fallback for Zwari ops). This is principle Z1 enforced operationally.
