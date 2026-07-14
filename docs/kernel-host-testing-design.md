# Wari — Kernel Host-Testing Strategy (RFC)

> **Status**: PROPOSAL — awaiting architect decision. No code lands
> with this document (Co-Architect Protocol §1–2: options proposed,
> Gustavo picks, then execution).
> **Author**: Claude (probe + evidence from a clean worktree,
> 2026-07-13, post-build-142 tree).
> **Phase alignment**: closes a Phase-1c testing-gate gap; Option B
> is explicit groundwork for roadmap item **p4a** (functional-core /
> imperative-shell refactor of Tier 0).
> **Engineering principles**: this document is the P1 (*Think Before
> Coding*) step for the whole program; the options are scored against
> P2 (*Simplicity First*) and P3 (*Surgical Changes*).

---

## 1 · Problem

CLAUDE.md's Testing Strategy and `docs/testing.md` promise a unit
layer that runs on the host on every PR:

> **Run**: `cargo test --workspace`. Must pass on every PR.

and list six "unit-testable modules" that MUST remain host-testable:
`abi-shared/`, `kernel/src/mem/page_table.rs`,
`kernel/src/mem/page_alloc.rs`, `kernel/src/validate.rs`,
`kernel/src/cap/`, and the IPC rendezvous state machine.

Empirically (2026-07-13, clean worktree, pinned 1.95.0 toolchain,
aarch64-apple-darwin host), that promise does not hold:

1. **`cargo test -p wari-kernel` fails with 13 compile errors**
   (taxonomy in §2). It has never compiled for the host.
2. **`make test-unit` (= `cargo test --workspace`) and `make clippy`
   (`--all-targets`) fail before reaching the kernel** — the first
   casualty is `drivers/uart`, whose `#[panic_handler]` collides
   with std's under the host test harness (E0152). `make check` is
   therefore red on a fresh clone.
3. **`scripts/build.sh` — the canonical pipeline per
   `STATE-OF-PLAY.md` — contains no host-test step at all.**
4. **84 `#[test]` functions inside the kernel bin crate have never
   executed.** They type-check under target builds at most; nothing
   runs them:

   | Module | Dormant tests |
   |---|---|
   | `kernel/src/cap/types.rs` | 18 |
   | `kernel/src/cap/cspace.rs` | 16 |
   | `kernel/src/cap/pool.rs` | 14 |
   | `kernel/src/cap/objects.rs` | 11 |
   | `kernel/src/cap/syscall.rs` | 9 |
   | `kernel/src/cap/revoke.rs` | 4 |
   | `kernel/src/cap/static_caps.rs` | 4 |
   | `kernel/src/sched/process.rs` | 4 |
   | `kernel/src/validate.rs` | 4 |
   | **Total** | **84** |

   76 of the 84 guard the **capability system** — the subsystem the
   security model leans on hardest and the Phase-4b Kani target.

Meanwhile the host tests that *do* run live in the extracted pure
crates, and they are green today:

| Crate | Host tests | Kernel consumption |
|---|---|---|
| `wari-mem` | 47 | re-export shims (`kernel/src/mem/page_{alloc,table}.rs`) |
| `wari-ipc` | 6 | not yet wired (pure decision core, Lane B2) |
| `wari-policy` | 8 | executor design, not kernel |
| `wari-wnm` | 18 | AOT container format (Gemini lane) |

So the codebase already contains **both** candidate answers in
embryo: the kernel's inline `#[cfg(test)]` modules (which need the
crate to become host-buildable — Option A) and the extracted-crate
pattern (which sidesteps the kernel build entirely — Option B). This
RFC puts the choice in front of the architect explicitly instead of
letting it keep drifting.

---

## 2 · Blocker taxonomy — why the kernel won't compile for the host

All four classes reproduce with
`cargo test -p wari-kernel --no-run` from the workspace root (host
target, no features). Classes B1/B3/B4 abort compilation at name
resolution; the B2 errors are masked behind them and surface as soon
as B1 is patched (confirmed by the July probe).

### B1 — duplicate `panic_impl` (E0152)

`kernel/src/main.rs` declares `#![no_std]` unconditionally and
defines `#[panic_handler]` (main.rs:253). Under `cargo test` the
harness links std, which already provides the lang item. The same
structure exists in `drivers/uart`, `drivers/net`, and `apps/hello`
— which is why the *workspace* gate dies even before the kernel is
reached. Standard embedded-Rust idiom fixes this class:
`#![cfg_attr(not(test), no_std)]` plus `#[cfg(not(test))]` on the
panic handler and on the `global_asm!` includes (prior art: The
Embedded Rust Book; used by knurling-rs project templates).

### B2 — RISC-V inline `asm!` on a non-RISC-V host

Every site, by file:

| File | Sites | Instructions |
|---|---|---|
| `kernel/src/main.rs` | 10 + `global_asm!(boot.S)` | `wfi` idle/park loops |
| `kernel/src/trap.rs` | 3 + `global_asm!(trap.S)` | `csrw stvec`, `csrc sip`, `wfi` |
| `kernel/src/sbi.rs` | 2 | `ecall` (SBI SRST), `wfi` fallback |
| `kernel/src/mem/kvm.rs` | 1 | `csrw satp` + `sfence.vma` |
| `kernel/src/mmio/plic.rs` | 1 | `csrs sie` |
| `kernel/src/runtime/tier2_net.rs` | 1 | `rdtime` (smoltcp clock) |

None of these can compile for aarch64/x86_64. Each enclosing
function needs either a `#[cfg]` gate with a host stub (Option A) or
to stay kernel-side while the pure logic around it moves out
(Option B).

### B3 — the exactly-one-platform-feature invariant

The kernel requires exactly one of `qemu`/`vf2`. A bare host build
has neither, so these symbols vanish and E0425 fires:

- `NET_MMIO_BASE` / `NET_MMIO_LEN` (`validate.rs:44–57`)
- `HART_CONTEXT` (`mmio/plic.rs:81/83`)
- `TIMEBASE_HZ` (`runtime/tier2_net.rs:117/119`)
- `nic_kind` platform arms (`cap/boot.rs:193/195`)
- the per-platform driver-blob statics (see B4)

Any Option-A design must answer "which platform does the host test
build pretend to be?" — either `cargo test -p wari-kernel --features
qemu` (documented in the Makefile) or host-only fallback constants.
Note the interaction: picking `--features qemu` re-arms B4.

### B4 — embedded build artifacts + the stale-driver guard

`runtime/hello_blob.rs` unconditionally `include_bytes!`s
`build/apps/hello.wasm`; `uart_blob.rs`/`net_blob.rs` embed the
signed per-platform driver wasm behind the platform features. A
fresh clone has no `build/` outputs, so host compilation dies on a
missing file. On top of that, `kernel/build.rs` runs its
`WARI-DRV-BUILD-TAG-N` stale-driver guard on **every** kernel build
— including a host test build — and fails unless a `make`-produced,
tag-matched signed blob exists. Host testing the kernel crate as-is
therefore requires the full wasm32 build+sign pipeline as a
prerequisite, or a deliberate, narrowly-scoped escape (e.g. build.rs
skips the guard when `CARGO_CFG_TARGET_ARCH != "riscv64"`, blob
modules gated `#[cfg(not(test))]` along with their consumers).

The stale-driver guard exists because builds 107–114 shipped stale
driver blobs silently (CLAUDE.md, Build pipeline section). Any
escape hatch must provably never weaken the guard for
`riscv64gc-unknown-none-elf` builds.

---

## 3 · Option A — cfg-gate the kernel bin crate in place

Make `wari-kernel` itself compile under `cargo test` on the host by
gating every impure element, leaving all module boundaries where
they are.

### Mechanics (one PR, or two small ones)

1. **B1**: `#![cfg_attr(not(test), no_std)]` on `main.rs`;
   `#[cfg(not(test))]` on the panic handler and both `global_asm!`
   includes (`boot.S`, `trap.S`).
2. **B2**: gate each asm-bearing function `#[cfg(not(test))]` and
   provide a host stub twin under `#[cfg(test)]`. Stubs are
   compile-only scaffolding: `system_reset() -> !` can
   `unreachable!()`, `now_ms()` can return a monotonic fake. ~8
   functions across 6 files.
3. **B3**: pick the platform story. Two sub-options:
   - **A-3a**: host tests run `--features qemu`. No new constants;
     Makefile encodes the invocation. Cost: B4 must be solved for
     real (blobs must exist or be gated).
   - **A-3b**: `#[cfg(test)]` fallback constants (a third arm next
     to `qemu`/`vf2`). Cost: a fictional "test platform" appears in
     security-relevant code (`validate.rs` MMIO windows) — the
     window tests would validate fake windows.
4. **B4**: gate `hello_blob`/`uart_blob`/`net_blob` and their
   consumers (`runtime::loader` entry points, `sched` boot
   registration) `#[cfg(not(test))]`; teach `build.rs` to skip the
   stale-driver guard for non-riscv64 targets **only**.

### What it buys

All 84 dormant tests execute immediately — including the 76
capability-system tests — with zero code motion. `cargo test
--workspace` becomes meaningful for the kernel in a single PR.

### What it costs

- **Permanent dual personality for the verification target.** Tier 0
  is the crate we intend to hand to Kani/Prusti (Phase 4b) and
  eventually freeze as ROM (Phase 4d). Every `#[cfg(test)]` seam is
  a second configuration of that artifact — the thing R8 and the
  frozen-image spec push against. The seams are not one-time: every
  future impure function pays the same tax forever, and the count
  grows with Phase-2+ (timers, IPI, preemption all add asm).
- **Stub-divergence risk.** A host stub that *behaves* (rather than
  merely compiles) invites tests that accidentally test the stub.
  Mitigation is discipline ("stubs must be unreachable from tests"),
  which is exactly the kind of unenforced discipline Wari's rules
  usually replace with structure.
- **The bin crate becomes the test surface.** Cargo builds the whole
  bin (all modules, `runtime/`, `trap`, `mmio/plic`, …) to run even
  one `validate` test, so *every* future kernel module must stay
  host-compilable under `cfg(test)` — the gate spreads by
  construction (this is why B3 bit `cap/boot.rs`, which has nothing
  to do with any test).
- Contradicts the stated split in `docs/testing.md` ("pure stays
  host-testable; impure moves to a `_glue` file") in spirit: the
  impure code stays put and gets fenced instead.

---

## 4 · Option B — extract the pure state machines into workspace crates

Generalize the proven `wari-mem` / `wari-ipc` pattern: pure logic
moves to small no-std-by-default, host-tested workspace crates; the
kernel keeps thin re-export shims (call sites unchanged — the
`kernel/src/mem/page_alloc.rs` shim demonstrates this costs ~13
lines) plus the impure glue. The kernel bin crate ends with **zero**
inline `#[cfg(test)]` modules; `cargo test -p wari-kernel` stays
impossible *by design* and stops being a goal — the workspace gate
is the goal.

Prior art: functional core / imperative shell (Gary Bernhardt,
2012); sans-IO protocol design (the discipline smoltcp — already a
Wari dependency — is built on); Hubris (Oxide) splitting
host-testable logic crates out of firmware binaries.

### Extraction lanes (in proposed migration order)

**B-1 · `wari-sched`** — `sched/process.rs` (`Process`,
`ProcessState`, transition rules; 130 lines, no unsafe, no statics)
plus the pure pick-next policy currently interleaved in
`sched/mod.rs`. Kernel keeps the process table static, the wasmi
`Store` handling, and the run loop. Moves 4 dormant tests; small,
low-risk, establishes the template. ~1 PR, well inside size
discipline.

**B-2 · `wari-validate`** — `validate.rs` is already pure except
for the B3 platform constants. Extraction parameterizes the MMIO
windows as data: the crate exposes
`is_mmio_addr(addr, windows: &[MmioWindow])`-shaped validators, and
the *kernel* supplies the platform-selected window table (the
`#[cfg(feature)]` stays kernel-side where the platform features
live). This is strictly better than today for Phase 4: a
window-table-parameterized validator is directly Kani-provable over
arbitrary windows. Moves 4 dormant tests, plus enables the window
tests to cover *both* platforms' tables on the host (today's inline
tests could only ever see one platform per build). ~1 PR.

**B-3 · `wari-cap`** — the prize and the bulk: `cap/types.rs`,
`cspace.rs`, `pool.rs`, `objects.rs`, `revoke.rs` are pure (zero
unsafe, zero statics — verified by census) and carry 63 of the 84
dormant tests, plus `cap/proofs.rs` (the Kani harnesses, `#[cfg(kani)]`).
Kernel keeps `storage.rs` (the statics + 4 unsafe), `reg.rs`,
`ring_drain.rs`, `static_caps.rs`, `boot.rs`, and `syscall.rs` (9
unsafe) as the imperative shell. Running `cargo kani` against a
small pure crate instead of the kernel bin is a material
simplification of the Phase-4b plan. Size: needs slicing into 2–3
PRs (types+pool, cspace+objects, revoke+proofs) to stay inside
discipline; each slice is a file-move + shim, mechanical to review.

**Stays kernel-side forever** (imperative shell): `main.rs`,
`boot.rs`/`boot.S`, `trap.rs`/`trap.S`, `sbi.rs`, `mem/kvm.rs`,
`mmio/`, `runtime/`, `cap/storage.rs`, `cap/syscall.rs`, the
scheduler run loop. Its tests are the QEMU integration layer — as
`docs/testing.md` already prescribes.

### What it buys

- The kernel shrinks toward the 5–10 KLOC formally-verifiable Tier-0
  target; this *is* roadmap item p4a, started early and paid for in
  reviewable slices.
- No cfg seams, no stubs, no second personality for the ROM-image
  crate. R8 reproducibility story untouched.
- Pure crates are the natural Kani/Prusti unit (precedent:
  `cap/proofs.rs` is easier to run today against `wari-mem`-style
  crates than against the bin).
- Extraction moves **zero unsafe** (the lanes above are
  unsafe-free by census), so R1/INV audit burden per PR is nil.

### What it costs

- **Latency to green.** The 84 dormant tests activate lane by lane
  over ~5 PRs, not in one. `cap/syscall.rs`'s 9 dormant tests likely
  need their pure decision core split out first (it has unsafe), or
  they stay dormant until the Phase-4a shell refactor reaches it.
- **Churn + review load.** File moves across ~12 files, workspace
  member additions, `kernel/Cargo.toml` edits — mechanical but real,
  and it must not collide with the Gemini AOT lanes (§7).
- Two homes for cap code during the transition (pure crate + kernel
  glue) — the re-export shim keeps call sites stable, but readers
  must follow one indirection, as with `wari-mem` today.

---

## 5 · What each option unlocks — CLAUDE.md's "unit-testable modules" list

| CLAUDE.md list entry | Today | After A | After B |
|---|---|---|---|
| `abi-shared/` | ✅ host-testable | ✅ | ✅ |
| `mem/page_table.rs` | ✅ via `wari-mem` (47 tests incl. alloc) | ✅ | ✅ (already the B pattern) |
| `mem/page_alloc.rs` | ✅ via `wari-mem` | ✅ | ✅ |
| `validate.rs` | ❌ 4 tests dormant | ✅ in place (one platform's windows per run) | ✅ via `wari-validate` (both platforms' tables testable) |
| `cap/` | ❌ 76 tests dormant | ✅ all 76 in one PR | ✅ 63 across B-3 slices; `syscall.rs`'s 9 need a further pure/glue split; `static_caps`' 4 likewise |
| IPC rendezvous | ✅ decision core in `wari-ipc` (6 tests); kernel Endpoint queues in `cap/objects.rs` | ✅ (objects tests run) | ✅ (objects tests move with B-3) |
| *(sched/process — in `docs/testing.md`'s list)* | ❌ 4 tests dormant | ✅ in place | ✅ via `wari-sched` |

The honest asymmetry: **A unlocks everything at once; B unlocks it
progressively but leaves the kernel structurally better each step.**

---

## 6 · Cross-cutting repair needed under EITHER option

These are decision-independent and could land as a first, tiny PR:

1. **The workspace host gate is broken by `drivers/{uart,net}` and
   `apps/hello`**, not just the kernel. Either their panic handlers
   get the B1 idiom (`#![cfg_attr(not(test), no_std)]` — harmless,
   these crates have no inline tests today), **or** `make test-unit`
   stops saying `--workspace` and names the host-testable crates
   explicitly (`cargo test -p wari-abi -p wari-mem -p wari-ipc -p
   wari-policy -p wari-wnm -p wari-wasi -p wari-driver-iface`).
   The explicit list is the P2-simplest fix and makes the gate
   green *today*; the cfg idiom keeps `--workspace` literal.
2. **`make clippy` (`--all-targets`) has the same problem** — same
   two fixes apply.
3. **`scripts/build.sh` should run the host-test gate** it currently
   omits, whatever form the gate takes (it is the canonical
   pipeline; a gate that never runs is not a gate).

---

## 7 · Lane deconfliction (Gemini AOT track)

Per `docs/aot-parallel-roadmap.md`, the parallel implementer owns
the `wari-wnm` / loader / signing lanes and is fenced out of
`kernel/cap`, `kernel/sched`, and net. Option B's lanes (sched,
validate, cap) therefore do not overlap Gemini's files, but **both
tracks edit `Cargo.toml` workspace members and `kernel/Cargo.toml`**
— merge-order coordination on those two files is the only contact
point. Option A touches `main.rs`/`build.rs`, which Gemini's G-lanes
also read; same coordination note applies.

---

## 8 · Recommendation (Claude's, non-binding)

**Staged hybrid, B-first in spirit: land §6's gate repair
immediately; adopt Option B as the standing direction (it is p4a,
just early); optionally take a deliberately *minimal* Option A
subset only if the 76 dormant cap tests are wanted green before the
B-3 extraction can land.**

Reasoning, per the depth rule:

- The decision space is not really "A vs B" — it is "when do the 84
  dormant tests first run" vs "what shape is Tier 0 in when Phase 4
  arrives". A optimizes the first, B the second. Wari's stated
  priority order (correctness, security, size — before convenience)
  favors B: cfg-seams are a convenience purchase paid for in
  verification-target complexity.
- The strongest argument *for* A — 76 cap tests green in one PR — is
  weakened by how far the gate spreads (B3 bit `cap/boot.rs` and
  `runtime/tier2_net.rs`; B4 drags in `build.rs` policy). The
  "minimal" A is not actually small: it is ~6 files of gates + a
  platform-identity decision + a guard escape, all permanent.
- The strongest argument *against* B — latency — is bounded: B-1
  (`wari-sched`) and B-2 (`wari-validate`) are each one small PR,
  and B-3 slices are mechanical moves of unsafe-free files with
  their tests attached. The `wari-mem` precedent shows the shim
  pattern costs ~13 lines and zero call-site churn.
- If Gustavo wants the cap tests running *this month* without
  waiting for B-3: the narrowest A-subset that achieves it is worth
  pricing separately — but note it still requires B1 + B3 + B4
  answers, which is most of A's cost.

Gustavo may of course pick pure A, pure B, or a different staging;
this section is input, not a decision.

---

## 9 · Proposed migration order (if B or hybrid is chosen)

| # | PR | Content | Size | Unlocks |
|---|---|---|---|---|
| 0 | `make`/gate repair (§6) | Makefile `test-unit`/`clippy` explicit host-crate list; optionally `build.sh` gate step | XS | Workspace gate green on fresh clone |
| 1 | `wari-sched` | `Process`/`ProcessState` + pure policy, shim in kernel | S | 4 tests + future preemption state-machine tests (Phase 2) |
| 2 | `wari-validate` | window-table-parameterized validators, kernel supplies platform tables | S | 4 tests, both platforms' windows testable |
| 3 | `wari-cap` slice 1 | `types.rs` + `pool.rs` + shims | M | 32 tests |
| 4 | `wari-cap` slice 2 | `cspace.rs` + `objects.rs` | M | 27 tests |
| 5 | `wari-cap` slice 3 | `revoke.rs` + `proofs.rs` (Kani harnesses move) | S–M | 4 tests + host-side `cargo kani` |
| 6 | (Phase-4a proper) | `syscall.rs`/`static_caps.rs` pure-core split, shell refactor | — | remaining 13 tests |

Each PR: one conceptual change, within size discipline, no unsafe
moved, `docs/testing.md` updated in the PR that changes what the
unit-layer sentence means.

---

## 10 · Open questions for the architect

1. **Direction**: pure A, pure B, or the §8 staging?
2. **If any A**: platform identity for host tests — `--features
   qemu` (A-3a) or a test-constants third arm (A-3b)? And is a
   target-conditional escape in `build.rs`'s stale-driver guard
   acceptable at all?
3. **If B**: crate names `wari-sched` / `wari-validate` / `wari-cap`
   acceptable? (House pattern: one concern per crate, `wari-` prefix.)
4. **Goal definition**: does `cargo test -p wari-kernel` itself need
   to work (forces A), or is "every pure module host-tested +
   workspace gate green" the actual exit criterion (satisfied by B)?
5. **§6 gate repair**: explicit `-p` list, or panic-handler cfg
   idiom in the wasm crates, or both?
6. **Where does the host gate live** going forward — `make check`,
   `scripts/build.sh`, or both?
