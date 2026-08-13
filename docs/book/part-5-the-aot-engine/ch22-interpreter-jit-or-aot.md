---
sidebar_position: 22
sidebar_label: "Ch 22: Interpreter, JIT, or AOT"
title: "Chapter 22 — Interpreter, JIT, or AOT"
---

# Chapter 22 — Interpreter, JIT, or AOT

The last chapter ended on a boast and a limit. The network driver
answers a ping, and the thing answering it is a `wasmi` interpreter
walking WASM bytecode one instruction at a time, at roughly a hundred
thousand polls per second on a single U74 core. That is fast enough for
a ping and nowhere near fast enough to saturate the gigabit link the
board is wired to. The gap is not a bug to be fixed with a golden-
reference diff. It is the interpreter loop itself — the same loop this
whole book has leaned on for its safety story — meeting arithmetic it
cannot win.

This part is about what you do at that fork, and the honest state of the
answer. Wari's answer is an **ahead-of-time compiler**: turn the
validated, signed WASM into native RISC-V *before* it runs, in the
off-device signing pipeline, and keep every isolation guarantee the
interpreter gave away for free. That answer is a **plan under
construction**, not a shipping subsystem. The container format exists;
the compiler does not yet. Most of this chapter is the reasoning that
got the design to this shape, and the one measurement that could still
tell us not to build it at all.

## Where interpretation actually costs

Chapter 11 made the case for the interpreter and made it well. `wasmi`
pinned at `=0.32.3`, `default-features = false`, compiling against
`core` + `alloc` so it runs in a `no_std` kernel with no OS beneath it —
one of the very few WASM runtimes that will do this at all. It is the
single third-party dependency Wari admits into Tier 0, and it earns the
exception by being an interpreter: a body of code you can read, count,
and eventually prove, rather than a black box that manufactures machine
code while you are not looking.

The cost of that clarity is per-instruction dispatch. Every WASM opcode
the module executes becomes, in the interpreter, a decode step and a
branch through the instruction handler before any real work happens. For
a workload that spends its time waiting on I/O — polling a network
descriptor ring, marshalling a few bytes across the host-call boundary —
the dispatch overhead disappears into the wait. That is why the net
driver works. But a workload that spends its time *computing* pays the
dispatch tax on every operation, and the tax does not amortize.

It is worth being precise about the number from Chapter 21, because it
is easy to misread. The 11-millisecond ping floor that chapter
diagnosed was **not** the interpreter — it was per-frame trace lines on
the always-on UART, each costing about 3.6 ms of blocking serial, since
moved to a compiled-out debug channel. The interpreter's own ceiling is
the quieter figure in that chapter's closing hook: on the order of a
hundred thousand poll iterations per second on a U74. A gigabit link
delivering minimum-size frames asks for well over a million packets per
second. You cannot orchestrate a million-plus events per second through
a loop that manages a hundred thousand, and no amount of driver
cleverness closes a gap that large. When the WASM *is* the hot path,
interpretation is the wall.

The escape hatch, in principle, is to stop interpreting and start
running native code. There are two ways to get native code, and the
difference between them is the whole security argument of this part.

## Why not a JIT — and why that is not a performance decision

The obvious move, the one every high-performance WASM runtime makes, is
a **just-in-time compiler**: at load time, on the device, translate the
module's bytecode into native instructions, write them into a page, mark
that page executable, and jump to it. V8 does it. Wasmtime does it.
It is the standard answer, and it is fast.

Wari rejects it, and the rejection is categorical. Chapter 11 already
stated the shape of it — "an interpreter, not a JIT… a JIT would mean
generating executable code at runtime inside the kernel, a W^X
nightmare." The AOT design documents make it a hard rule. Decision D4 in
`docs/aot-build-plan.md`: *"RX-only, never W+X. The loader maps compiled
`.text` RX-only; no runtime codegen, ever."* The parallel roadmap repeats
it in the terminology note at `docs/aot-parallel-roadmap.md:14` — the
track is colloquially "the JIT," but *"per decision D4 there is no
runtime codegen, ever — the kernel never maps a page W+X."*

The reason this is not a performance trade is worth dwelling on, because
"we chose the slower option for safety" undersells it. A JIT requires
the kernel to hold, simultaneously, a page it can **write** and a page
it can **execute**. The moment those are the same page — writable and
executable at once, W+X — you have handed any memory-corruption bug in
the kernel a direct path to arbitrary code execution: overwrite the live
code, and the CPU runs your bytes next. Decades of exploit mitigation,
from OpenBSD's W^X to hardware NX bits, exist to make that page pair
impossible. A JIT reintroduces it *on purpose*, in the most privileged
address space on the machine, because it must.

For an OS whose ordering is correctness, then security, then size —
whose long-term endpoint is a formally verified Tier 0 with the kernel
text burned into read-only ROM (Chapter 7) — a runtime code generator is
not a fast path with a caveat. It is a contradiction of the thesis. You
cannot hash-attest a kernel image and freeze its `.text`, then have that
same kernel manufacture and execute fresh code on every module load. The
two are mutually exclusive. So the JIT is not deferred pending
optimization; it is ruled out by construction. Whatever gives Wari native
speed has to do it **without the kernel ever generating code**.

## The AOT bet: interpret once, at build time

That constraint has exactly one shape of solution, and it is an old and
respected one. If you cannot compile on the device, compile *off* it.
Do the expensive translation from WASM to native RISC-V once, ahead of
time, in the signing pipeline that already exists to sign Tier-2
drivers — the same pipeline Chapter 19 built. Ship the native code as a
signed artifact. On the device, the kernel verifies the signature, maps
the code **read-execute-only**, and runs it. No page is ever both
writable and executable. The compiler never touches the device.

This is the **ahead-of-time (AOT)** model, and its lineage is direct.
Fastly's **Lucet** (2019) compiled WASM to native ahead of time
specifically so that Compute@Edge could run untrusted modules at native
speed without a JIT in the request path — the exact trade Wari is making,
cited as "the model" in `docs/aot-build-plan.md:197`. The compiler
backend under consideration, **Cranelift** (the code generator behind
Wasmtime), is the same one Lucet used; here it runs as an offline
library that emits a Wari artifact rather than a live JIT.

The elegance of the bet is that it pays the interpretation cost exactly
once and moves it entirely off the critical path. The interpreter's tax
was per-instruction, per-execution, forever. The compiler's tax is
per-instruction, per-*build*, once, on a beefy host machine that is not
resource-constrained and not in anyone's request latency. What the
device runs is straight-line native RISC-V. The dispatch loop is gone
because there is nothing left to dispatch.

Two properties have to survive the move, and the design is built around
keeping both.

**R7 — no ELF in the customer ABI — is not violated.** This is the trap
the AOT design is most careful to avoid, and it is easy to get wrong.
Native code sounds like an executable, and "ship a native executable"
sounds exactly like the ELF-loading path R7 forbids forever. It is not.
The customer still writes and ships **WASM**. The native artifact is
produced *downstream*, inside the trusted signing pipeline, from WASM
that has already been validated — and it is packaged not as an ELF but
as a **WNM** (Wari Native Module): a Wari-native container the kernel
knows how to verify and map. There is no `SYS_SPAWN_ELF`. There is no
customer-facing native-load path. The customer ABI stays WASM from boot
zero; the native code is an *internal representation* of an
already-trusted module, the way a cached compilation is an internal
representation of source.

**Structural isolation must not be discarded.** The interpreter gave
memory isolation for free: a WASM module simply cannot express a pointer
outside its own linear memory, and `wasmi` enforces that on every access.
Native code has no such built-in humility — a compiled load is just an
address and a load, and a buggy compiler could emit one that reaches into
the kernel. Preserving isolation *after* compilation, without trusting
the compiler that did the compiling, is the entire subject of the next
chapter. It is the load-bearing piece, and it is the reason AOT is
compatible with Wari's thesis at all rather than a quiet surrender of it.

## What already exists — the easy ten percent

It would overstate the state of things to imply the AOT engine is close.
What exists today is the *output contract* and the loader's *parser* —
what `docs/aot-build-plan.md:56` frankly calls "the easy ~10%."

The **WNM container format** is built, in the `wari-wnm` crate: a header
plus a section table carrying `Text` (the native code), `Relocs`
(relocations the loader applies per instance), `SafetyCert` (the
certificate of the next chapter), and `Wasm` (the original module, kept
for fallback and audit). It has `validate_header` and duplicate-section
rejection. And the loader's front half — `wari_wnm::load_plan` —
validates the header and resolves the byte-ranges the kernel loader will
need.

So Wari knows what an AOT artifact *looks like* and can parse one. What
it cannot yet do is *produce* one — there is no compiler driving
Cranelift, no relocation emitter, no on-device loader that maps `.text`
and enters it. Those are the milestones M1 through M4 in the build plan,
and every one of them is ahead, not behind.

## The gate before the work: measure first

Here is the part of the plan that is most against the grain of how
ambitious systems get built, and the part the team is proudest of.
Before writing a single line of compiler, the plan demands a
measurement: **do we even need it?**

The reasoning is in `docs/aot-build-plan.md:20` and it is disciplined
almost to the point of self-sabotage. AOT only pays for itself if WASM
execution is genuinely the bottleneck. But Wari's heavy Phase-2 compute —
LLM inference — does not run in WASM at all. It runs on the GPU or the
GAPU coprocessor, reached through the WASI-NN host-function surface. The
WASM in that world is **orchestration**, not arithmetic: it sets up the
call, hands the tensor to the accelerator, and waits. And orchestration
is precisely the I/O-bound shape the interpreter is already fast enough
for. If the compute-heavy path has already been offloaded to silicon
built for it, the WASM core may never *be* the hot path — in which case
the entire AOT engine is an elaborate answer to a question no workload is
asking.

So the first milestone, **M0**, is not a compiler. It is a **benchmark
harness plus a differential-testing oracle**. The benchmark
(`tools/wari-bench`) runs representative modules under the pinned `wasmi`
and reports where the time actually goes. The oracle (`tools/wari-oracle`)
captures the full observable trace of a module's execution — its exit
value, its exact sequence of host calls, a hash of its linear memory at
exit — so that two executors can be compared for exact equivalence.
Together they answer the gate question with numbers instead of ambition.

The reason this is the right first move, and not merely a cautious one,
is that M0 is a **no-regret** step. If the numbers say interpretation is
fine — that tuning `wasmi` is enough, or that real workloads are all
orchestration — the project stops there, and stopping is recorded as a
*success*, not a failure (`docs/aot-parallel-roadmap.md:360`). And if the
numbers say AOT is warranted, the oracle you built to answer the question
becomes the exact instrument you need next: the reference that proves the
compiler's native output is observably identical to the interpreter it
replaces. Every future compiler bug is caught, or missed, in that oracle.
You build it either way. So you build it first.

This honesty extends to what the corpus contains. The oracle is only as
truthful as its inputs, which is why the workload fixtures
(`tests/fixtures/aot/`) deliberately span shapes — an integer hot loop,
linear-memory churn, a deep call graph, and crucially a **host-call-dense**
fixture modelling the AI-assistant orchestration loop that is the actual
target. A benchmark stacked with tight arithmetic loops would "prove" AOT
is essential by measuring a workload Wari may not run. The gate is only
meaningful if the corpus tells the truth about the real shapes.

## What is decided, and what is still open

The design has a spine of settled decisions and a set of forks still
genuinely open. Confusing the two would be the easiest way to mislead a
reader, so they are worth stating flatly.

**Decided.** No JIT, ever — D4, the RX-only / no-runtime-codegen rule,
which is a security invariant and not up for revision. AOT-off-device, in
the signing pipeline, as the shape of the answer if the answer is yes.
The WNM container format and its `load_plan` parser, which are built. The
measure-first M0 gate as the precondition for any compiler work at all.
And the **backend**: decision gate DG-1 lands on **Cranelift, run
offline** — mature, deterministic, with a real RV64 target, and driven as
a library so that its size and trust cost live in the host pipeline and
never reach the device. Cranelift's alternatives (a bespoke `no_std`
codegen, or a `wasm2c` → C → `riscv64-gcc` lowering) stay on the shelf as
fallbacks, the bespoke path revisited only if artifact size eventually
demands it.

> **A note on the record.** The AOT design documents still formally mark
> DG-1 as *"pending confirmation"* (`docs/aot-parallel-roadmap.md:57`,
> `docs/aot-build-plan.md` D2), because under the Co-Architect Protocol a
> recommendation is not a ratification until the architect signs it. This
> book treats Cranelift-offline as the settled *direction* — it is
> unambiguously the recommendation and the shape the rest of the plan is
> built against — while noting that its formal sign-off is a checkbox, not
> an open question, in a way DG-2 and DG-3 below are not.

**Still open.** The **memory-safety model** — DG-2 — is a real fork with
real consequences, and the next chapter is largely about it: does
compiled code lean on **guard pages** (let the MMU catch an out-of-bounds
access with a fault) or on **explicit bounds checks plus a verified
certificate** (prove, in software, that no access can escape)? The two
lead to different endgames, and Wari's MMU-free ambition pulls hard toward
the second. And the **safety-certificate format** — DG-3 — the wire
format the compiler emits and the on-device checker consumes, is open by
design: the roadmap has it produced as a proposal (task G7a) for the
architect to decide, not assumed.

Above all, the **M0 verdict itself is still open**. The gate has not been
run. Until the benchmark and oracle put numbers on the table, the correct
statement about the entire AOT engine is not "it is fast" or even "it is
coming." It is: *the design is ready, the measurement that justifies
building it has not yet been taken, and the team has committed in advance
to believe the measurement even if it says stop.*

## Closing hook

Suppose the gate opens. Suppose the numbers say the WASM core really is
the hot path, the interpreter really is the wall, and native code is
worth it. You now face the problem the whole strategy has been quietly
deferring: you are about to run compiler-generated machine code, in
S-mode, in the kernel's own address space — and a compiler is a large,
fast-moving, third-party program that Wari's trust model has spent
eleven chapters refusing to admit into Tier 0.

The interpreter never had this problem. `wasmi` enforces isolation on
every access, so a bug in `wasmi` is bounded by `wasmi`'s own logic. A
compiler bug is different in kind: emit one load without its bounds
check, and the escaping module is *correct native code* that no runtime
will second-guess. The next chapter is the answer to that — the safety
certificate, and the small on-device checker that lets Wari run native
code while trusting the signature and the certificate, and *not* the
compiler that produced them. It is the crux of the whole bet, the long
pole of the schedule, and the single thing that keeps AOT compatible with
a kernel meant one day to run with no MMU at all.
