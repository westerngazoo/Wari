<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Wari — AOT Engine: Parallel-Implementer Roadmap

> **Status:** Execution roadmap for a **parallel implementer** (the
> Gemini track). Companion to [`aot-build-plan.md`](aot-build-plan.md)
> (the dependency-ordered plan this decomposes) and
> [`aot-safety-cert-design.md`](aot-safety-cert-design.md) (the M2 long
> pole). The architect (Gustavo) reviews and merges every PR; nothing in
> this document delegates that authority.
>
> **Terminology:** this track is colloquially "the JIT" — technically it
> is the **AOT engine** (compiler runs off-device in the signing
> pipeline; the device runs signed, certified native code). Per decision
> D4 there is **no runtime codegen, ever** — the kernel never maps a
> page W+X.

---

## 1 · Ground rules (non-negotiable)

1. **Co-Architect Protocol applies.** Propose → Gustavo decides →
   implement. Tasks below marked `RFC` produce a *document* for
   approval, not code. Tasks marked `GATED` must not start before the
   named decision.
2. **PR discipline** per `CLAUDE.md`: one task = one branch = one PR,
   template mandatory, 100–400 changed lines preferred (a new fixture
   corpus or generated file may exceed this with pre-approval).
   Branch naming: `phase-2/aot-<kebab-summary>`.
3. **Build via `make` / `scripts/build.sh` only.** Never
   `cd kernel && cargo build` (stale-driver hazard — see `CLAUDE.md`).
4. **R1–R8 hold.** In particular R8: everything this track produces
   must be deterministic/reproducible — it feeds a signing pipeline.
5. **Lane boundaries.** The AOT track owns **new** code only:
   - ✅ new crates: `tools/wari-bench`, `tools/wari-oracle`,
     `tools/wari-aot`, `wari-cert`
   - ✅ `wari-wnm` — **append-only** extensions (new sections/APIs;
     never change existing byte-layout semantics without an RFC)
   - ✅ `tests/fixtures/aot/`, `docs/aot-*.md`
   - ❌ `kernel/src/**` — the M3 loader is **explicitly out of scope**
     for this track (kernel security surface; it lands via the kernel
     lane once M2's cert format is approved)
   - ❌ `drivers/**`, `abi-shared` sysnums, `cap`/`ipc`/`net` code —
     active lanes of the other tracks; collisions waste everyone's time
6. **Every task's acceptance criteria are mechanical** — commands with
   expected output. A task is done when the commands pass on a clean
   checkout, `cargo fmt --check` and `cargo clippy -p <crate> -- -D
   warnings` are clean for the crates it adds, and the PR template is
   complete. New crates join `[workspace.members]` and inherit
   `[lints] workspace = true`.

---

## 2 · Decision gates (architect)

| Gate | Decision | Status | Blocks |
|------|----------|--------|--------|
| **DG-1** | Compiler backend: Cranelift-offline / bespoke / wasm2c | **CONFIRMED (2026-07): Cranelift-offline.** Validated by the G4 spike — see `docs/aot-spike-results.md` | ~~G4~~, G6 |
| **DG-2** | Memory-safety model: guard-pages vs explicit-bounds + cert | pending | G5 (recommendation), G7b |
| **DG-3** | Safety-cert format (adapt VeriWasm vs bespoke) | pending — G7a produces the proposal | G7b, M3 |

Tasks G1, G2, G3 need **no** decision and can start immediately, in
parallel.

---

## 3 · Task overview

```
       (no gate)                (DG-1)            (DG-1 + G5 approved)
 G1 bench ──┐                     │                      │
 G2 oracle ─┼── M0 gate verdict   G4 spike ──────────── G6 wari-aot driver ── G8 CI
 G3 corpus ─┘                     │                      │
                                  └── G5 ABI RFC ────────┘
 G7a cert RFC (start now, long pole) ── (DG-3) ── G7b checker skeleton
```

| id | task | deliverable | gate | size |
|----|------|-------------|------|------|
| G1 | benchmark harness | `tools/wari-bench` | none | S |
| G2 | differential oracle | `tools/wari-oracle` | none | M |
| G3 | workload corpus | `tests/fixtures/aot/` | none | S |
| G4 | Cranelift spike | `tools/wari-aot-spike` (throwaway) | DG-1 | M |
| G5 | target-ABI RFC | `docs/aot-target-abi.md` | none (RFC) | S |
| G6 | compiler driver | `tools/wari-aot` | DG-1 + G5 approval | L |
| G7a | cert-format RFC | extension of `aot-safety-cert-design.md` | none (RFC) | M |
| G7b | cert checker skeleton | `wari-cert` (no_std) | DG-3 | L |
| G8 | differential CI target | `make test-aot` | G6 | S |

---

## 4 · Task specifications

### G1 — Benchmark harness (`tools/wari-bench`) — M0a

**Goal.** Answer "is the interpreter actually a bottleneck?" with
numbers, and provide the measurement side of the M0 gate.

**Spec.**
- New host-side binary crate `tools/wari-bench` (std allowed — it never
  runs on device).
- Input: one or more `.wasm` files (Tier-1-shaped: `_start` export,
  `wari`-module imports may be stubbed no-ops).
- Executes each module under `wasmi` v0.32.x (same version the kernel
  pins — read it from the workspace, do not float) with fuel metering
  on; reports per-module: wall-time, instructions-retired proxy (fuel
  consumed), peak linear memory, and a stable machine-readable JSON
  output alongside the human table.
- Deterministic: `--runs N` (default 5) reports min/median; the JSON
  orders keys and omits timestamps so two runs of the same corpus
  diff-compare on everything except the timing values themselves.

**Acceptance criteria.**
```bash
cargo run --release -p wari-bench -- tests/fixtures/aot/*.wasm --runs 5 --json out.json
# → table on stdout: one row per module, columns
#   module | fuel | wall_ms_min | wall_ms_median | peak_linmem_pages
# → out.json parses; contains the same rows
cargo test -p wari-bench    # ≥ 3 unit tests, incl. a fixture that
                            # exercises fuel accounting deterministically
```
Fuel consumed for the same module + input is **bit-identical across
runs** (assert in a test).

**Out of scope.** No RV64/QEMU execution (host-only); no AOT anything.

---

### G2 — Differential oracle (`tools/wari-oracle`) — M0b

**Goal.** The reference harness that will later prove AOT output is
observably identical to `wasmi`. This is the single most load-bearing
tool in the track: every future compiler bug is caught (or missed)
here.

**Spec.**
- New host-side crate `tools/wari-oracle`.
- Define `trait Executor { fn run(&mut self, wasm: &[u8], input: &Input)
  -> ObservableTrace; }` where `ObservableTrace` captures, in order:
  return/exit value, the full sequence of host calls (function name +
  arguments), and a BLAKE3 (or SHA-256) hash of linear memory at exit.
- Reference implementation: `WasmiExecutor` (same pinned wasmi).
- `oracle diff <a.wasm> --lhs wasmi --rhs wasmi` runs both sides and
  reports EQUAL / DIVERGED with the first differing trace event.
- **Mutation self-test** (the oracle must be able to fail): a test
  executor that deliberately perturbs one host-call argument must be
  reported DIVERGED at the exact event index.

**Acceptance criteria.**
```bash
cargo run -p wari-oracle -- diff tests/fixtures/aot/arith.wasm --lhs wasmi --rhs wasmi
# → "EQUAL (N trace events)"
cargo test -p wari-oracle
# → includes: identity_is_equal, mutation_is_detected_at_event,
#   trace_is_deterministic (two runs, identical ObservableTrace)
```

**Out of scope.** No second real executor yet — the AOT side arrives
with G6/G8. The `Executor` trait is the handoff seam.

---

### G3 — Workload corpus (`tests/fixtures/aot/`) — M0c

**Goal.** The oracle and bench are only as honest as their inputs.
Curate a small, documented corpus of representative modules.

**Spec.**
- `tests/fixtures/aot/` with **both** the `.wat` source and the built
  `.wasm` checked in (reproducible: a `build-fixtures.sh` regenerates
  the `.wasm` via `wat2wasm` and a byte-compare check keeps them in
  sync).
- Minimum set: `arith.wat` (integer hot loop), `memory.wat` (linmem
  load/store churn), `calls.wat` (deep call graph / indirect calls),
  `hostcall.wat` (host-fn round-trip density — the AI-assistant
  orchestration shape), `fuel_bomb.wat` (infinite loop — for fuel-path
  parity later).
- Each fixture has a header comment: what it represents, provenance,
  expected observable behavior.
- Architect input requested (non-blocking): 1–2 real workload shapes
  from the sovereign-cloud target set to add later.

**Acceptance criteria.**
```bash
./tests/fixtures/aot/build-fixtures.sh   # regenerates; git diff is empty
cargo run -p wari-oracle -- diff tests/fixtures/aot/arith.wasm --lhs wasmi --rhs wasmi  # EQUAL
# every fixture runs under wari-bench without error
```

---

### G4 — Cranelift spike (`tools/wari-aot-spike`) — GATED on DG-1

**Goal.** De-risk the backend with real numbers before committing to
the driver. **Throwaway code** — explicitly allowed to be ugly; lands
in-repo so the numbers are reproducible, deleted when G6 lands.

**Spec.**
- Drive Cranelift (as a library, `cranelift-codegen` +
  `cranelift-wasm` or `wasmtime-cranelift`'s translator) to compile
  `arith.wasm` to RV64 machine code.
- Execute it as a **Linux user-space binary under `qemu-riscv64`
  (user-mode emulation)** — no Wari kernel involvement — wrapped in a
  minimal ELF harness that calls the compiled function and prints the
  result.
- Report side-by-side: wasmi fuel/wall vs native wall for the corpus's
  compute-bound fixtures.

**Acceptance criteria.**
```bash
cargo run -p wari-aot-spike -- tests/fixtures/aot/arith.wasm --out /tmp/arith.elf
qemu-riscv64 /tmp/arith.elf      # prints the same result the oracle
                                 # records for wasmi on arith.wasm
```
A short `docs/aot-spike-results.md` with the measured table (this file,
plus the M0 numbers from G1, is the **M0 gate evidence** the architect
uses to green-light or kill the track).

---

### G5 — Target-ABI RFC (`docs/aot-target-abi.md`) — D1

**Goal.** Pin the contract compiled code is emitted against, for
architect approval. Document, not code.

**Spec.** The RFC must cover, each with ≥2 options and a recommendation:
1. **Linear-memory addressing**: reserved base register + explicit
   bounds checks vs guard pages (must present the MMU-free-endpoint
   implication of each — ties to DG-2).
2. **Host-call trampoline**: how native code invokes kernel host fns
   (register convention, who saves what, how the wasm↔native boundary
   cost compares to wasmi's).
3. **Trap and fuel mapping**: how OOB, unreachable, div-by-zero, and
   fuel exhaustion surface to the kernel with semantics identical to
   wasmi's (the oracle will enforce this).
4. **Relocation model**: what the WNM `Relocs` section must express for
   the M3 loader's per-instance arena.

**Acceptance criteria.** Doc covers all four sections; each ends with a
boxed recommendation; prior art cited per claim (Lucet, Wasmtime,
VeriWasm as applicable); the architect can answer DG-2 from this doc
alone.

---

### G6 — Compiler driver (`tools/wari-aot`) — M1 — GATED on DG-1 + G5 approval

**Goal.** The real pipeline tool: `.wasm → native .text + relocs → WNM
→ signed`. Deterministic (R8).

**Spec.**
- CLI: `wari-aot compile <in.wasm> --out <out.wnm>` then the existing
  `sign-module` signs the WNM (extend `scripts/` invocation only if
  needed — coordinate, don't fork the signing flow).
- Packs sections via `wari-wnm` (`Text`, `Relocs`, `Wasm` [the source
  module, for fallback/audit], `SafetyCert` placeholder until G7b).
- **Bitwise-reproducible**: same input + same tool version → identical
  bytes. No timestamps, no host paths, sorted everything.
- `wari_wnm::load_plan` must accept every produced artifact.

**Acceptance criteria.**
```bash
cargo run -p wari-aot -- compile tests/fixtures/aot/arith.wasm --out /tmp/a.wnm
cargo run -p wari-aot -- compile tests/fixtures/aot/arith.wasm --out /tmp/b.wnm
sha256sum /tmp/a.wnm /tmp/b.wnm   # identical
cargo test -p wari-aot            # incl. load_plan-accepts-output test
```

---

### G7a — Safety-cert format proposal — M2 prep — RFC, start immediately

**Goal.** The long pole starts now. Concrete cert-format proposal
extending `aot-safety-cert-design.md` §design into something a checker
can be written against.

**Spec.**
- Read VeriWasm (Johnson et al., USENIX Security '21) closely; document
  which of its checks transfer to RV64 + our ABI (G5) and which don't.
- Propose the cert wire format (goes in the WNM `SafetyCert` section):
  what the compiler asserts (bounds-check placement, indirect-branch
  target sets, stack discipline) and what the on-device checker
  verifies per assertion.
- State the trust claim precisely: *what class of compiler bug can this
  cert catch, and what class can it not.*

**Acceptance criteria.** RFC lands in `docs/`; DG-3 is decidable from
it; includes a worked example — the cert content for `arith.wasm`'s
compiled text, by hand.

---

### G7b — Cert checker skeleton (`wari-cert`) — GATED on DG-3

**Goal.** The on-device checker, built host-first: pure, `no_std`,
allocation-free — written as if Kani will prove it (because eventually
it will).

**Spec.**
- New workspace crate `wari-cert`: `#![cfg_attr(not(test), no_std)]`,
  zero deps, pure functions only (the kernel will call it at load
  time; it must satisfy R2/R5 by construction).
- API sketch: `check(text: &[u8], cert: &Cert, abi: &AbiParams) ->
  Result<(), CertViolation>` — precise, enumerated `CertViolation`
  taxonomy, no panics.
- Test corpus: hand-written good certs (accepted) and **adversarial
  certs** — every violation variant has a fixture that triggers exactly
  it. This is the security-test discipline applied to M2 from day one.

**Acceptance criteria.**
```bash
cargo test -p wari-cert      # every CertViolation variant covered by
                             # at least one rejecting fixture
cargo build -p wari-cert --no-default-features  # no_std build clean
```

---

### G8 — Differential CI target (`make test-aot`) — M4 seed — GATED on G6

**Goal.** Wire it all together: every corpus module compiled by G6 must
be observably identical to wasmi under the G2 oracle.

**Spec.**
- `AotExecutor` implementing G2's `Executor` trait (runs the compiled
  artifact under `qemu-riscv64` user-mode, same harness as G4 matured).
- `make test-aot`: for each fixture — compile, run oracle diff, fail on
  any DIVERGED; plus reproducibility check (double-compile, compare).

**Acceptance criteria.**
```bash
make test-aot
# → one line per fixture: "arith.wasm ... EQUAL (reproducible)"
# → non-zero exit if any fixture diverges or fails to reproduce
```

---

## 5 · What this track hands off (and to whom)

| Artifact | Consumer | Contract |
|----------|----------|----------|
| M0 evidence (G1 + G4 numbers) | architect | go/kill decision for the whole track |
| `Executor` trait + oracle | kernel lane (M3/M4) | the acceptance test for the on-device loader |
| approved ABI (G5) | compiler (G6) **and** kernel loader (M3) | the shared contract |
| `wari-cert` (G7b) | kernel loader (M3) | called at module-load; no_std/pure by construction |
| signed `.wnm` artifacts (G6) | kernel loader (M3) | `load_plan`-valid, cert-carrying, reproducible |

**M3 (kernel loader: verify → map RX-only → relocate → enter) is not in
this roadmap.** It is kernel security surface and lands through the
kernel lane after DG-2/DG-3, using the artifacts above.

---

## 6 · Suggested execution order

1. **Week 1:** G1 + G3 (small, independent), G7a reading begins.
2. **Week 2:** G2 (the oracle), G5 RFC drafted. Architect confirms DG-1.
3. **Week 3:** G4 spike → **M0 gate review with the architect** (the
   numbers may say "stop here — interpreter tuning suffices." That is a
   success outcome, not a failure).
4. **After the gate + approvals:** G6 → G8; G7a → DG-3 → G7b in
   parallel (it is the schedule-dominant path).

---

## 7 · Prior art (inherited)

See `aot-build-plan.md` §9. Key anchors for this track: **Lucet**
(AOT-not-JIT model), **VeriWasm** (G7 checker), **Cranelift/Wasmtime**
(G4/G6 backend), **wasmi** (the reference semantics the oracle
enforces).
