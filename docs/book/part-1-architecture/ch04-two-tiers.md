---
sidebar_position: 4
sidebar_label: "Ch 4: Two Tiers"
title: "Chapter 4 — Two Tiers"
---

# Chapter 4 — Two Tiers

Chapter 3 spent a WASM-only bet and walked away holding a problem. If
every process on Wari is a WASM module and there is no ELF path — no
`SYS_SPAWN_ELF`, ever (R7) — then the driver that talks to the UART is
a WASM module too. So is the network stack. So is whatever eventually
drives the GPU. And a driver is not like a customer's request handler:
it pokes hardware registers, it fields interrupts, it holds the only
copy of every tenant's connection state. Where, exactly, does a WASM
module like that *run*?

That is the question this chapter answers, and the answer is the single
most important structural decision in Wari. It is not "WASM everywhere,
uniformly." It is a split — two tiers of WASM sitting on one native
kernel — and the split is drawn along the axis that actually matters,
which is *trust*, not *language*. Getting there means first walking
into the two tempting dead ends on either side of it, because the
two-tier model only looks obvious once you have felt why the simpler
answers hurt.

## The maximalist temptation: everything in ring 0

Here is the first tempting answer. WASM is already a sandbox. The
validator proves, at load time, that a well-typed module cannot
fabricate a pointer outside its own linear memory — that is a
structural property of the type system, not a runtime check that might
be forgotten (this is Layer 1, and we return to it below). So why pay
for a hardware MMU at all? Run *everything* — customer code, drivers,
services — as WASM in a single privilege level, ring 0, with the
language as the only isolation mechanism. No page tables to switch. No
mode transitions. No TLB shootdowns. One address space, many sandboxes,
maximal density.

This is not a strawman. It is very nearly Singularity, and Singularity
was real research that worked. Microsoft Research's Singularity OS
(Hunt & Larus, MSR-TR-2007-49) ran every process as a *Software
Isolated Process* — a sealed, verified C# assembly — in a single
address space with no hardware protection between processes at all. The
type system was the isolation boundary. It was fast precisely because a
cross-process call did not cross a hardware wall; it was a verified
transfer inside one ring. Singularity proved the architectural move is
sound: language-enforced isolation *can* replace the MMU.

So why not do that for all of Wari? Because of one word Singularity
never had to reckon with the way a sovereign cloud does: *untrusted*.

Singularity's SIPs were compiled by a trusted toolchain from a sealed
source. Wari's Tier-1 tenant is a hostile stranger. It is a `.wasm`
blob a customer uploaded, and the entire product thesis — 10,000 to
50,000 tenants on one board — means the machine is *full* of them,
each one potentially trying to break out. The isolation between that
blob and the kernel is, in the ring-0 model, exactly one thing: the
correctness of the WASM interpreter. If `wasmi` has a single host-side
soundness bug — a type confusion, an out-of-bounds write in its own
Rust — a customer who finds it owns kernel memory outright, because in
ring 0 there is no second wall behind the first.

The security model states this liability in as many words. The wasmi
interpreter runs in S-mode inside the kernel's address space; its
`Store` and `Engine` are identity-mapped kernel memory; "from the
page-table's perspective, wasmi *is* the kernel" (`docs/security-model.md`,
"Load-bearing caveat"). We accept that liability for *one* interpreter
we pin, fuzz, count in the TCB, and aim to formally verify — because we
must run WASM somehow. What we refuse to accept is that the interpreter
be the *only* thing standing between untrusted customer code and the
kernel. For code we did not write and have every reason to distrust,
one bug should not be catastrophe. That is what defense in depth *is*,
and the maximalist answer throws it away for everyone at once. It is
the right architecture for code you trust and the wrong one for code
you do not.

## The minimalist temptation: everything in userspace

Chastened, swing the other way. If the danger is untrusted code near
the kernel, then put *all* WASM behind the hardware wall — every
module, drivers included, runs in U-mode with the MMU between it and
Tier 0. Now a soundness bug in the interpreter is caught by a second,
independent mechanism: even if a module escapes the WASM sandbox, the
page tables stop its raw dereference cold. Maximum paranoia, uniformly
applied. Isn't more isolation always better?

For a customer request handler, yes. For a driver, it is a performance
catastrophe, and the reason is the trap.

A WASM module in U-mode cannot touch a hardware register. It has no
MMIO; it has only host functions, and every host function call from
U-mode is a trap into S-mode — the mode switch that is the whole point
of the U/S boundary. That cost is fine, even cheap, amortized over a
customer request that does a little compute and one `fd_write`. It is
ruinous for a driver, because a driver's inner loop *is* MMIO. Bringing
up a NIC, draining a receive ring, kicking a descriptor queue — these
are hundreds to thousands of register reads and writes per packet. Put
that driver in U-mode and every single one of those register pokes
becomes a U→S trap: save the frame, switch mode, dispatch, switch back.
You would be paying a syscall's worth of overhead per hardware register
access, thousands of times per packet, on the hottest path in the
system. No amount of "isolation is good" survives contact with that
arithmetic.

So the minimalist answer is correct about *customers* and wrong about
*drivers* — which is the exact mirror image of what the maximalist
answer got wrong. One says "trust the language for everyone"; the other
says "distrust the hardware boundary for everyone." Both errors come
from treating all WASM as the same kind of thing. It is not. A
customer's request handler and a signed network driver differ in
exactly the dimension that should decide where they run: how much we
trust them, and how much it costs to isolate them.

## The two-tier answer: split by trust

Draw the line there. Not one tier of WASM, not a uniform policy, but
two tiers distinguished by trust and cost, sitting on a native kernel:

```
┌─────────────────────────────────────────────────────────────┐
│  Tier 1 — Customer WASM   (U-mode, double-sandboxed)          │
│   • untrusted; validator + MMU both enforce isolation         │
│   • target: 10 000 – 50 000 instances per board               │
│   • reaches the kernel only through WASI host functions       │
├─────────────────────────────────────────────────────────────┤
│  Tier 2 — System WASM     (S-mode, WASM-only sandbox)         │
│   • drivers and system services; signed + attested at load    │
│   • ~10–50 modules per board                                  │
│   • direct MMIO and IRQ handling via capability grants        │
├─────────────────────────────────────────────────────────────┤
│  Tier 0 — Native Kernel   (S-mode Rust, ~5–10 KLOC)           │
│   • boot · trap · MMU · scheduler · wasmi runtime             │
│   • no third-party code except wasmi; formal-verification tgt │
└─────────────────────────────────────────────────────────────┘
```

**Tier 1 is the maximally-distrusted tenant, so it gets both walls.**
Customer WASM runs in U-mode, double-sandboxed: the WASM validator is
the structural boundary, and the Sv39 MMU is the hardware boundary
underneath it. A validator bug that lets a module attempt a raw pointer
dereference does not reach kernel memory, because the page tables are a
second, independent line — and independence is the whole value of depth.
This is the case the minimalist answer got right, kept only for the
tier that needs it. Density is not sacrificed: U-mode tenants share the
one wasmi runtime rather than each carrying a process image, which is
the Cloudflare Workers density bet (Kenton Varda, 2017–) applied to
WASM instead of V8.

**Tier 2 is the trusted, signed driver, so it gets one wall and full
speed.** System WASM — the UART driver, the network stack — runs in
S-mode, inside a WASM sandbox but without the U/S wall beneath it.
There is no trap per MMIO, because the driver's host-function calls do
not cross a privilege boundary; they are ordinary calls from wasmi into
the kernel, both already in S-mode. The driver still cannot fabricate a
pointer into kernel memory — the WASM sandbox holds structurally — but
it pays no mode-switch tax on its inner loop. This is the case the
maximalist answer got right, kept only for the tier that has earned it:
Tier-2 modules are ed25519-signed and verified against a compiled-in
public key before instantiation (INV-13), so "trusted" is a checkable
claim, not a hope.

**Tier 0 is native Rust and stays small.** Boot, traps, the MMU, the
scheduler, the wasmi embedding, the capability table — five to ten
thousand lines, no third-party code except wasmi itself, shaped from
day one as a formal-verification target. Everything else is WASM in one
tier or the other. Privilege in Wari is not a property of a *language*
and not even, cleanly, a property of a *ring*; it is a per-module
grant, and the tier a module lands in decides both where it executes
and how it is contained.

The payoff of the split is that a single interpreter bug is no longer
uniformly catastrophic. Against a Tier-1 tenant it is caught by the MMU;
that tenant is double-sandboxed by design. Against a Tier-2 driver it is
contained by the sandbox and the driver's signature, and the blast
radius is bounded to that driver's own linear memory rather than the
kernel. Different threats, different containment, because we stopped
pretending customer code and driver code are the same animal.

> **Built-vs-planned: U-mode is the intended posture, not today's.**
> The diagram places Tier 1 in U-mode, and that is the design. It is not
> yet what runs. Today Tier-1 modules are *interpreted in S-mode*: the
> wasmi runtime executes tenant bytecode in the kernel's own privilege
> level (Part 2, Ch 11), and the scheduler resumes them cooperatively
> rather than preempting them (Part 2, Ch 13 — "no preemption here, no
> timer, no fuel"). The structural WASM sandbox is real and load-bearing
> now; the Sv39 MMU is up and bounds each module's linear memory, so a
> *wasm-level* escape that turns into a raw dereference is trapped
> (`docs/security-model.md`, Layer 2). What is not yet wired is the U/S
> privilege wall that would put the *tenant itself* in U-mode with the
> kernel unmapped. Until it is, the second wall protects against a
> wasm-runtime escape but not against a host-side soundness bug in wasmi,
> which is single-sandboxed today. Part 1 argues the endpoint; Part 2
> is honest about the distance still to cover. The two-tier *shape* —
> trust-split, capability-gated — is what ships now; U-mode execution
> is the completion of it.

## The crossing: capabilities, not ambient authority

A tier boundary is only a boundary if crossing it is controlled.
Wari's tiers do not trust each other by position; a Tier-1 module does
not get to call the kernel simply because it is a Tier-1 module. Every
crossing is gated on an unforgeable *capability* — a token the kernel
alone constructs, held in a per-process table, naming exactly one
object with exactly the rights the holder was granted.

Concretely: when a Tier-1 tenant calls `fd_write`, the host function's
first act is to ask whether the caller holds an `Endpoint` capability
with `WRITE` rights at the slot it named. When the Tier-2 UART driver
does an MMIO store, the runtime checks that the driver holds the
capability that authorizes MMIO. There is no ambient authority anywhere
— no "userspace can always write to stdout," no "drivers can always do
MMIO." The permission to cross is a value, granted at boot, checkable
in one function, revocable transitively. This is seL4's capability
model (Klein et al., SOSP 2009), condensed to Wari's scale, and it is
what turns three static tiers into a system whose *actual* authority
graph is auditable. Part 2, Chapter 12 shows the whole mechanism —
what a `Cap` is, how `check_cap` refuses five ways, why a tenant cannot
forge or reach a capability it was not given. For now the only claim is
architectural: the tier crossings are the trust boundaries, and every
one of them is capability-gated by construction.

## Three layers, and which of them ship

The two-tier split organizes *where* code runs. Orthogonal to it is the
question of *what stops an escape*, and Wari's answer is three
independent layers, each of which would have to fail for isolation to
break:

| Layer | Mechanism | Guarantee | Status |
|---|---|---|---|
| **Structural** | WASM validator + type system (wasmi) | A module cannot generate a pointer outside its linear memory. Proven at load, not checked at runtime. | Phase 0 — **shipped** |
| **Hardware** | Sv39 MMU today; RISC-V PMP (Phase 1); RISC-V CoVE (Phase 3) | Tier-1 cannot read or write kernel or other-tenant memory. CoVE adds per-tenant ciphertext RAM, so a kernel dump leaks nothing. | MMU **shipped**; PMP/CoVE **planned** |
| **Cryptographic** | Zkn/Zks hardware crypto (Phase 2) | Data at rest is AES-256-GCM; inter-module traffic is BLAKE3-authenticated. Exfiltrated bytes are ciphertext. | Phase 2 — **planned** |

The discipline this book insists on is to read that "Status" column
literally. Two layers ship — the WASM sandbox and the MMU. Three are on
the roadmap and are named as such. A sympathetic skim of the layered
model can leave the impression all of it is in production; it is not,
and `docs/security-model.md` is blunt about the gap because a
procurement auditor forming a sovereignty claim deserves the honest
version. The layers that ship are genuine defense in depth for the
common case — malformed wasm, out-of-bounds linear-memory access, a
missing trap handler are all double-caught. The rare case — a
host-side memory-safety bug in wasmi's own Rust — collapses Layers 1
and 2 into one, for the reason the maximalist section already gave: in
S-mode there is no wall between the interpreter and the kernel. That
single honest caveat is why Tier 0 stays small enough to verify, why
wasmi is fuzzed continuously, and why formal verification of the
interpreter core sits on the Phase-4 roadmap rather than a wish list.

## Why not Singularity, RedLeaf, or MirageOS

Wari's two-tier model has three close relatives in the literature, and
naming the differences sharpens what the split is *for*.

**Singularity (MSR, 2003–08)** is the maximalist answer taken all the
way, and taken seriously: SIPs, sealed and verified, isolated by the
C# type system in a single address space with no MMU between processes.
It proved language isolation can carry an OS. Wari's disagreement is not
with the proof but with the population. Singularity's processes came
from a trusted toolchain; Wari's Tier-1 tenants are adversaries by
assumption, and for adversaries one verified runtime is not enough
walls. So Wari keeps Singularity's model for the tier it fits — Tier 2,
the signed, trusted driver in ring 0 with language isolation only — and
refuses it for the tier it does not. Chapter 7 argues that Singularity's
full vision is Wari's *endpoint*, once the runtime is verified enough to
retire the MMU; it is a destination, not a starting posture.

**RedLeaf (UCI, SOSP 2020)** is the closest academic sibling: Rust-
enforced "domains," language-level isolation between kernel components,
zero-copy IPC through the type system. Wari borrows the vocabulary and
the confidence that Rust can carry OS-scale isolation. Where it differs
is the *trust gradient*. RedLeaf's domains are peers — mutually
distrusting, uniformly isolated by the language. Wari deliberately does
*not* treat all modules as peers: a customer tenant and a signed driver
sit at different trust levels and therefore behind different numbers of
walls. The two-tier split *is* that trust gradient made structural,
which a flat field of language-isolated domains does not express.

**MirageOS (Cambridge, ASPLOS 2013)** goes the other direction into
specialization: a unikernel is one application, its libraries, and just
enough OS, linked into a single-address-space binary. Extreme
specialization, extreme size reduction — and exactly the wrong shape
for a machine meant to hold 50,000 mutually-distrusting tenants at once.
MirageOS collapses the multi-tenant problem by refusing to be
multi-tenant. Wari cannot; density across untrusted tenants is the
whole product. The single-address-space idea returns, though, in
Chapter 7 and in Part 5's ahead-of-time work — a Wari module plus wasmi
linked into one specialized image is a MirageOS-flavored future for
latency-critical workloads, once the safety certificate makes it safe
to drop the shared runtime.

Three neighbors, three borrowings, three refusals — and none of them
arbitrary. Singularity's language-only isolation is right for trusted
code and wrong for untrusted; Wari splits the tiers accordingly.
RedLeaf's domains are right about Rust and flat about trust; Wari adds
the gradient. MirageOS is right about specialization and wrong about
multi-tenancy; Wari defers the idea to a phase that can afford it. That
is the pattern this whole book tries to hold to: no architectural
decision unexplained, every one of them either inherited with credit,
rejected with reason, or labeled a bet.

## Closing hook

The two-tier model is the shape of the thing we intend to build. But
Wari does not begin from an empty editor. Its predecessor, `goose-os`,
already contains a working page allocator, an Sv39 walker, a
synchronous-IPC state machine, a trap dispatcher, and an invariant
catalog — some of it exactly what a WASM-native two-tier kernel needs,
some of it shaped by ELF and native-process assumptions this
architecture just retired. Before we write a line, we have to decide
what survives the crossing. Chapter 5 — the cherry-pick audit: what we
inherit from goose, what we rewrite, and what we delete.
