<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Wari — Parallel Worklist (AOT engine + agentic layer)

> **Status:** Live worklist (Phase 2 horizon). Granular, dependency-lane
> tasks that can run in parallel *while the network bring-up proceeds
> independently*. Companion to [`aot-build-plan.md`](aot-build-plan.md),
> [`aot-safety-cert-design.md`](aot-safety-cert-design.md), and
> [`ai-os-assistant-design.md`](ai-os-assistant-design.md).
>
> **Terminology:** what's colloquially the "JIT" here is the **AOT
> engine** — the decided direction (`wasm-jit-design.md`): native speed,
> compiler kept **off-device** in the signing pipeline. Same goal (run
> WASM at native speed), the safe shape.

---

## The thesis this serves

The OS is a **minimal hardware custodian**: Tier 0 owns boot, MMU, sched,
caps, and the runtime — nothing more. **Agents + cloud-native WASM do
everything else.** So every task below either (a) makes WASM execute at
native speed (AOT engine), or (b) makes agentic work faster and safer
(the assistant + fast-path layer). The first AI-driven OS: hardware
management below, an agent-and-cloud world above.

---

## Lanes (run in parallel)

```
NETWORK (you, on the board) ── independent ── GMAC1 RGMII → HTTP demo
        │
        ▼ (no dependency on the lanes below)
LANE A · AOT engine        LANE B · agentic layer        LANE C · foundations
  (native-speed WASM)        (faster agent work)            (unblocks A & B)
```

Legend: **[P]** = parallel-safe now · **[D:x]** = depends on task x ·
**[board]** = needs hardware · **[you]** = architect decision/curation.

---

## Lane C — Foundations (do first / in parallel; unblock everything)

- **C1 [you]** Decide AOT backend: Cranelift-offline (recommended) vs
  bespoke vs wasm2c. Gates A2–A5. One decision.
- **C2 [you]** Decide memory-safety model: guard-pages vs
  explicit-bounds-checks + verified output. Gates A3 + the safety-cert.
  (MMU-free endgame argues for explicit-checks.)
- **C3 [P]** M0 oracle: benchmark harness — representative WASM timed
  under `wasmi`, `wasmi` as the differential correctness reference.
  Answers "do we even need AOT?" and is reused to validate AOT output.
- **C4 [you]** Curate representative workloads for C3 (the agent
  orchestration loop, cloud-native apps). Oracle is only as honest as
  its inputs.
- **C5 [P]** Fix the build ergonomics (see §Build refactor below) —
  removes the `PATH=...` friction that slows every parallel build.

## Lane A — AOT engine (native-speed WASM)

- **A1 [P]** WNM format polish: it exists (`wari-wnm` + `load_plan`).
  Add a golden-file test corpus (hand-built WNMs) + a `wnm-dump` CLI for
  eyeballing artifacts. Pure, host-testable.
- **A2 [D:C1]** Target ABI spec for compiled code: linear-memory
  addressing (base+bounds vs guard), the host-call trampoline ABI, and
  trap/fuel/OOB → kernel mapping. The contract the compiler emits *to*.
- **A3 [D:A2]** `tools/wari-aot` driver skeleton: `.wasm` → (stub) →
  WNM → sign. Deterministic/reproducible (R8). Start with a pass-through
  that packs the `.wasm` unchanged so the loader path is exercised end to
  end before real codegen.
- **A4 [D:A3]** Cranelift spike: compile ONE trivial module to RV64,
  pack into a WNM, run under the QEMU harness. De-risks the backend with
  real numbers before committing to A5.
- **A5 [D:A4, C1]** Real AOT codegen for the common instruction set;
  differential-equal vs `wasmi` (uses C3 oracle).
- **A6 [D:C2]** Safety-cert track (the long pole; see
  `aot-safety-cert-design.md`): Model A (offline-verify + sign) first.
  Sub-tasks A6a read VeriWasm, A6b RV64 SFI lattice, A6c the on-device
  checker. **[you]** line up the academic collaboration — biggest
  schedule win, run it *now*.
- **A7 [D:A5, A6]** Kernel loader: verify sig + cert → map `.text`
  **RX-only** → relocs → enter. `load_plan` is done; this consumes it.

## Lane B — Agentic layer (faster agent work)

- **B1 [P]** Cap-fastpath ring drain (PR-2b): `SYS_RING_SETUP` +
  `SYS_RING_SUBMIT`, per-entry `validate_handle` (INV-α/γ go live),
  delegate to existing host fns by handle. Pure format (`wari_abi::ring`)
  already merged (#37). QEMU-verifiable, no board.
- **B2 [D:B1]** seL4 fastpath: register-only synchronous IPC for the
  single-latency-critical call. Extends the Endpoint mechanism.
- **B3 [P]** Executor policy engine (the non-LLM trusted gate,
  `ai-os-assistant-design.md` §5): allow-list → cap-scope → taint →
  rate/budget → irreversibility-confirm → audit. Pure logic, host-
  testable — the deterministic core that bounds a prompt-injected planner.
- **B4 [D:B3, you]** Bounded-attenuation cap primitive (count/time/
  target-boxed mint) — the one new cap-system primitive the assistant
  needs. Design with the architect.
- **B5 [P]** WASI-NN host-fn surface sketch: how the planner offloads
  inference to GPU/GAPU (the WASM stays orchestration). Spec first;
  hardware path is Phase 2/3.

---

## Build refactor — make it ergonomic for parallel work

**The current weakness:** every build needs
`PATH="$HOME/.cargo/bin:$PATH" make ...` because Homebrew `cargo` (no
wasm32/riscv targets) shadows the rustup toolchain. That friction
compounds when working two machines + multiple branches. Proposed fixes,
smallest first:

- **R1 [P]** Makefile self-heals PATH: prepend `$(HOME)/.cargo/bin` to
  `PATH` inside the Makefile (via `export PATH := ...`) so bare `make`
  always finds the right `cargo` regardless of shell. Removes the manual
  prefix entirely. *(Highest ergonomics-per-line.)*
- **R2 [P]** `make doctor`: one target that checks toolchain, targets
  (`wasm32-unknown-unknown`, `riscv64gc-...`), QEMU, and the
  `cargo`-resolution gotcha — prints exactly what's wrong. New-machine
  onboarding + parallel-machine sanity in one command.
- **R3 [P]** `make ci`: fmt + clippy(-D warnings) + `cargo test
  --workspace` + QEMU smoke, one target, matching the review checklist —
  so "is the repo green?" is a single command on either machine.
- **R4 [D:R1]** Split build vs deploy: `make kernel` (build only) stays
  local + fast; `make deploy` (commit/push/flash) stays explicit. Ensure
  `make` never bumps `.build_number` unless actually deploying, so a
  parallel `cargo test` doesn't create dirty-tree noise.
- **R5 [P]** `wari` script: keep the branch-following `go` (done), add
  `wari sync` (fetch + fast-forward current branch, report divergence)
  for the two-machine flow, and `wari status` showing branch + build +
  dirty state at a glance.

---

## Ordering guidance

1. **C1, C2, C4, A6-collab** are architect decisions/kickoffs — do them
   whenever; they unblock the most.
2. **R1 + C3** are the highest-leverage engineering starts: R1 kills the
   build friction for every subsequent task; C3 tells us if AOT is even
   worth it.
3. **B1, B3, A1** are parallel-safe code tasks with no cross-dependency —
   ideal for filling capacity while decisions settle.
4. **A6 (safety cert)** dominates the schedule — start the reading +
   collaboration in parallel with everything, even though the code lands
   late.

---

## Decision log

- **D1 — Lanes are dependency-partitioned so network work never blocks
  AOT/agentic work** and vice versa.
- **D2 — "JIT" == the AOT engine** (off-device compiler); no runtime
  codegen ever (W^X, ROM, MMU-free endgame).
- **D3 — Fix build ergonomics (R1) early** — it's a force multiplier for
  every parallel task and every machine.
- **D4 — Start the safety-cert collaboration now** even though A7 lands
  last; it's the long pole.
