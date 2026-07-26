---
sidebar_position: 3
sidebar_label: "Ch 3: The WASM-Only Bet"
title: "Chapter 3 — The WASM-Only Bet"
---

# Chapter 3 — The WASM-Only Bet

There is no `SYS_SPAWN_ELF`. There is no ELF loader in the ship kernel,
no code path that turns a Linux binary into a running process, and no
plan to add one. Rule R7 states it without softening — *no ELF in the
customer ABI, ever* — and Phase 0's exit criteria make the absence
*testable*: a native-ELF load attempt via any syscall must be rejected,
and it is rejected not by a check but because the code that would honor it
does not exist (`CLAUDE.md`, Phase 0 Exit Criteria). This chapter argues
that the absence is the architecture's single most important decision, not
its most inconvenient one — that WASM-only is a bet with a large, specific
payoff, and that the payoff is worth the things it costs.

## The comparison every cloud primitive loses in one column

Set the major compute primitives side by side on the three axes that
decide whether a sovereign cloud is possible: how many tenants fit on a
board (**density**), how fast a cold tenant starts serving
(**cold-start**), and how large a trusted base each tenant's isolation
depends on being correct (**cold TCB** — the code that must be right, from
the tenant's boundary down, or isolation fails).

| Primitive | Density (order) | Cold start | Cold TCB — what must be correct for isolation |
|---|---|---|---|
| Linux process | 100s–1000s | 10s of ms | The full Linux kernel — tens of millions of lines |
| OCI container | 100s–1000s | 100s of ms | Full Linux kernel + namespaces/cgroups + runtime |
| Firecracker microVM | ~100s / host | ~100+ ms | Guest kernel + a ~50 KLOC Rust VMM |
| V8 isolate | millions | sub-ms (warm) | ~50 MB of V8 C++ + the host |
| **WASM instance (`wasmi`)** | **10k–50k / board** | **< 10 ms target** | **`wasmi` + a 5–10 KLOC Rust Tier-0** |

The density and Firecracker figures are the project's own targets and the
Firecracker paper's own scale (`docs/prior-art.md`); the millisecond
columns are ordinal, not benchmarks, and the point does not depend on the
exact millisecond. The point is the last column. Every row above the last
buys its isolation with a trusted base measured in *tens of thousands to
tens of millions of lines* — a Linux kernel, or a VMM plus a guest kernel,
or fifty megabytes of a browser engine's C++. Wari's row buys its
isolation with `wasmi` and a Tier-0 held to 5–10 KLOC: a trusted base a
small team can read in a week. The other columns matter — density and
cold-start are why anyone would choose this at all — but the cold-TCB
column is the one that a government auditing its own infrastructure
actually cares about, and it is the column only WASM-only wins.

## The only way to beat Cloudflare on density is to not be Linux

Cloudflare already proved the density ceiling of the shared-runtime bet
(Chapter 2): millions of tenants, because there is no process and no page
table per tenant. Wari cannot beat that by being a better Linux. Every
Linux-shaped primitive — process, container, microVM — pays a per-tenant
cost that is structural: an address space, a set of kernel objects, a
scheduling entity the host kernel tracks. You can make the VM lighter, as
Firecracker did, and move from thousands of tenants to hundreds *per host
with strong isolation* — but you cannot make the per-tenant cost
*disappear*, because the isolation is coming from hardware and kernel
bookkeeping that scales with tenant count.

The way out is the move Cloudflare made and the move Wari makes: get the
isolation from the *language boundary* instead of from the hardware
boundary, so that adding a tenant adds a validated module and a linear
memory, not an address space and a kernel-object retinue. The slogan
compresses it: **the only way to beat Cloudflare on density is to not be
Linux.** Wari is not Linux from boot zero. It is not a stripped Linux, not
a Linux-compatible microkernel, not a unikernel that links glibc — it is
WASM-native from the kernel out, and that is what puts 10,000–50,000
tenants on a single board within reach where the Linux-shaped primitives
top out orders of magnitude lower.

## Type-safety is a security property, not a portability one

The usual reason to like WebAssembly is portability — one bytecode,
many targets. That is real and it is not why Wari bets on it. Wari bets
on WASM because its type system is a *security* mechanism that can be
*proven at load time, before the module runs a single instruction*.

A validated WASM module cannot generate a pointer outside its own linear
memory. This is not a runtime check the module might evade and not a
convention it is trusted to honor — it is a structural property the
validator establishes over the whole module at load, the way a type
checker establishes that a well-typed program cannot add a string to a
function. This is the top layer of Wari's three-layer security model, and
the model's rule is that *all three must hold* (`CLAUDE.md`, Security
Model): the **structural** layer (the WASM type system and validator, "no
module generates pointers outside its linear memory, proven at load
time"), the **hardware** layer (Sv39 paging, later RISC-V PMP, later CoVE),
and the **cryptographic** layer (Phase 2 hardware crypto). Isolation is
not one line of defense that a single bug takes down; it is three
independent lines, and the structural one is the reason the model can even
consider, at Phase 4, a variant that omits the hardware line entirely
(Chapter 7's immutable endpoint).

The security payoff is what makes the two-tier model possible at all.
Tier-2 drivers run *in the kernel's own ring*, with the WASM sandbox as
their only barrier and no MMU between them and Tier 0 (Chapter 4). That
arrangement is only sane because the structural property is a *proof*, not
a hope: a driver that has passed validation and signature verification
cannot forge a pointer into kernel memory, because the bytecode that would
do so does not type-check and never loaded. Portability is a nice
side-effect. The security property is the load-bearing one, and Chapter 12
shows it carrying weight — a Tier-1 tenant cannot forge a capability
precisely because it manipulates only integer indices and there is no code
path that turns tenant bytes into a kernel object.

## Density and cold-start, concretely

The density and cold-start numbers are not aspirations pulled from a
pitch; they are graded exit criteria. Phase 0's bar is that a signed
`.wasm` module boots as Tier-1 PID 1, prints via a WASI host function,
exits cleanly, and does so with **cold start under 50 ms and two
concurrent instances under 20 MB of RAM** (`CLAUDE.md`, Phase 0 Exit
Criteria) — measured on the target, not modeled. The Tier-1 *design*
target tightens the cold start to **under 10 ms** at the 10,000–50,000
instance density (`CLAUDE.md`, Two-Tier model). Those two facts together
are the argument: a memory footprint that lets tens of thousands of
tenants coexist on one board, and a cold start fast enough that
per-request instantiation — Fastly's fresh-instance-per-request model — is
on the table rather than a fantasy.

The honesty note is that Wari buys this with an *interpreter* first.
`wasmi` 0.32.3, the pinned `no_std` pure interpreter (`docs/architecture.md`),
gives fast cold start and a small, auditable engine and *slow steady-state
throughput* — it walks bytecode instruction by instruction. Chapter 21's
net driver is the concrete face of the trade: a `wasmi` interpreter polls
at roughly a hundred thousand times a second on a U74 core, which is fast
enough to answer a ping and nowhere near enough to saturate a gigabit
link. Wari chose the interpreter first on purpose — correctness and
smallness before speed — and the throughput ceiling is a known cost with a
known fix, which is the subject of the risk accounting below.

## The honest risks, and their mitigations

A bet stated without its downside is a sales pitch. WASM-only has four
real risks, and each has a mitigation that is itself part of the
architecture rather than a promise to fix it later.

**Risk 1 — `wasmi` correctness is now load-bearing.** Because Tier-2
drivers run with no MMU barrier, a soundness bug in the interpreter is a
kernel-integrity bug, not merely a wrong answer. This is named openly as
the two-tier model's central risk: it "requires `wasmi` to be highly
correct" (`docs/prior-art.md`). The mitigations are structural. The TCB is
deliberately tiny (5–10 KLOC of Tier 0) so the surface that must be correct
is small enough to audit and, at Phase 3–4, to formally verify — with a
`wasmi` correctness proof named as an explicit long-horizon goal. Tier 1
keeps the MMU as a *second* independent line, so a single structural
failure does not by itself breach a customer tenant (the three-layer model
again). And Tier 2 is not open to arbitrary code: only ~10–50 modules per
board, each ed25519-signed and verified against the kernel's compiled-in
key before instantiation (`docs/architecture.md`, INV-13). The correctness
bet is concentrated onto a small, signed, verified, formally-targeted core
rather than spread across everything that runs.

**Risk 2 — interpreter throughput.** The `wasmi` steady-state cost is real
and Chapter 21 measures it. The mitigation is the ahead-of-time engine of
Phase 2+ (Part 5): compile the same validated, signed WASM to native
RISC-V *before* it runs, paying the interpretation cost once at build time
instead of on every frame, while keeping the structural isolation the
validator already established. The trade is sequenced, not ignored —
correctness first with the interpreter, speed second with the AOT compiler,
and the isolation property preserved across the transition because it was
proven at validation, upstream of either engine.

**Risk 3 — customers cannot bring arbitrary Docker images.** This is the
flat cost of R7, and it is a genuine loss: a workload that needs an
unmodified Linux binary against the full POSIX and glibc surface will not
run on Wari. The mitigation is `tools/oci2wasm/` (Phase 2): the customer
brings a Docker image, host-side tooling compiles it to WASM, and Wari
runs the WASM (`docs/prior-art.md`). Compatibility becomes a build-time
translation on the host — paid once, off the critical path — rather than a
runtime obligation the kernel must carry, which is exactly the Kata trap
Chapter 2 refused. The compatibility story is "compile to our boundary,"
not "emulate their boundary."

**Risk 4 — WASM and WASI are still moving.** Betting the customer ABI on a
young standard risks churn. The mitigation is conservative sequencing: a
Phase 0–1 baseline on WASI Preview 1 — mature, widely implemented,
compatible with wasi-libc — with interface boundaries chosen to slot
cleanly into the Preview 2 Component Model as a Phase 2 migration target
(`docs/prior-art.md`). Wari explicitly rejects WASIX, the vendor-controlled
competing superset, as fragmentary. The standard will move; Wari tracks the
open, widely-implemented line of it and designs its boundaries to migrate.

## What we give up, and what we get

State the ledger plainly. What Wari **gives up**: arbitrary Docker images
at runtime, the full Linux/POSIX/glibc surface, native ELF binaries, and
the ability to run an unmodified third-party binary the customer will not
recompile. For a buyer who needs those, Wari is the wrong OS, and
Chapter 1 already said so.

What Wari **gets** in exchange is the entire thesis: 10,000–50,000 tenants
per board where the Linux-shaped primitives top out orders of magnitude
lower; a cold start fast enough to instantiate per request; structural,
load-time-proven isolation that lets drivers share the kernel's ring
safely; and — the property that no other primitive in the comparison table
delivers — a trusted computing base small enough that the government,
hospital, or bank depending on it can actually read the code that isolates
its data. The give-up column is a list of compatibility conveniences. The
get column is sovereignty. For the customer Wari is built for, that is not
a close trade.

## Closing hook

WASM-only settles *how* code enters the system and *why* the boundary is a
security property. It does not settle *where the privileged code lives* —
the code that touches the UART, drives the NIC, handles an interrupt.
Those jobs cannot run behind an MMU wall and still be fast, and they
cannot be native Rust in Tier 0 without bloating the very TCB this chapter
spent its length keeping small. Wari's answer is the two-tier model:
drivers are WASM too, signed and attested, running in the kernel's ring
with the structural sandbox as their barrier. Chapter 4 makes that case —
given WASM-only, where do drivers live, and why is running them in ring 0
the safe choice rather than the reckless one?
