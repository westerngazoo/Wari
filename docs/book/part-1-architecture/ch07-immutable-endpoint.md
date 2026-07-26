---
sidebar_position: 7
sidebar_label: "Ch 7: The Immutable Endpoint"
title: "Chapter 7 — The Immutable Endpoint"
---

# Chapter 7 — The Immutable Endpoint

Chapter 6 ended on a question that sounds heretical for an operating
system to ask: if the interpreter is proven correct and the kernel is
proven sound, what is the MMU still *for*? Every chapter so far has
treated the Sv39 page tables as a load-bearing wall — Layer 2 of the
security model, the hardware boundary under untrusted Tier-1 code. This
chapter follows the architecture past the point where that wall is
structurally necessary, to the endpoint it has been pointing at since
Chapter 4: a Wari that could run on custom silicon with no paging
hardware at all, where the verified WASM output *is* the isolation and
the MMU, if it is present, is belt-and-suspenders.

This is Phase 4, and Phase 4 is years out. Everything in this chapter is
labeled honestly as vision, and the project charter is careful about the
verb: Phase 0 through 3 "keeps the MMU as the primary line"; Phase 4
"opens the *option* of shipping a ROM-burned Tier-0 kernel on custom
silicon that omits paging hardware entirely" (CLAUDE.md, Long-term
endpoint). We build toward this without locking into it. The chapter's
purpose is not to promise the endpoint but to show that the whole
architecture has been *shaped* for it — that a hundred small Phase-0
decisions only make sense once you see where they were aiming.

## Four properties of an immutable kernel

An "immutable kernel" is not a slogan; it is four concrete properties, and
each is a design commitment Wari can start honoring long before the
silicon exists.

**1. Functionally pure state transitions.** The Phase-4 roadmap opens with
a "functional-core / imperative-shell refactor of Tier 0" — the FC-IS
pattern, and it is exactly the pattern Wari's per-module rules have been
pushing toward all along. The idea: the kernel's *decisions* — what the
next process state is, whether a capability derivation is legal, which
process runs next — are pure functions from old state to new state, with
no I/O and no mutation inside them. The *effects* — writing the register,
touching the MMIO, flipping the static — live in a thin imperative shell
around that pure core. This is what "pure before impure" (module rule 6)
has been staging since the first commit: `Cap::derive` is already a pure
function, the page-table walker is already pure logic over a `read`
closure, the validators are already pure argument-checkers. FC-IS matters
here because a pure function is a verifiable function — Kani reasons about
`old_state -> new_state` cleanly and cannot reason about a tangle of
effects. The immutable kernel is, first, a kernel whose every important
decision is a theorem.

**2. Hash-attested boot.** An immutable kernel must be able to prove it is
the kernel it claims to be. The roadmap's "hash-attested boot + RO kernel
`.text`" is the open, small equivalent of Secure Boot: the kernel image
hashes to a known value, and the boot chain refuses to run anything whose
hash does not match. Wari can do this *because of a Phase-0 rule that
looks unrelated* — R8, reproducible builds. A bitwise-reproducible build
produces a stable hash; a build that varied run to run could not be
attested at all. R8 was written for supply-chain integrity, but it is also
the precondition for attestation: you cannot hash-lock an image you cannot
reproduce.

**3. No self-modification.** An immutable kernel does not rewrite itself at
runtime — kernel `.text` is read-only, there is no JIT generating code
into executable pages, and there is no dynamic loading of native code. Wari
already lives this way, and Chapter 11's interpreter choice is why. wasmi
is an *interpreter, not a JIT* — the roadmap defers JIT to a later phase
"only behind a proof obligation" precisely because runtime code generation
is "a W^X nightmare, a much larger attack surface, and a much harder
formal-verification target" (Part 2, Ch 11). A kernel that never generates
code can mark its text pages RO and mean it. The no-ELF rule (R7)
completes the property: there is no native-code loader anywhere in the
customer ABI, so there is no path by which new privileged code enters the
running system.

**4. Burnable to mask ROM.** The final property is the literal one. A
kernel with the first three — pure transitions, a stable attestable hash,
no self-modification — is a kernel that never changes after it ships, which
means it can be *burned into mask ROM* on custom silicon. The roadmap's
last two milestones are "Tier-0 frozen-image spec" and, past it,
"kernel-in-ROM tapeout." A kernel in ROM is immutable in the strongest
possible sense: not "we mark it read-only" but "there is no circuit that
could write it." That is the endpoint the word *immutable* is reaching for,
and it is the subject of Part 6.

## Singularity's dream, twenty years on

None of this is a new dream. It is a twenty-year-old one, and naming the
lineage is both intellectual honesty and the strongest argument that the
endpoint is reachable.

Microsoft Research's **Singularity** (Hunt & Larus, 2003–08) built exactly
this: an OS where processes were isolated by a verified type system rather
than by hardware page tables, running in a single address space, with the
language as the protection boundary. Singularity *proved the move is
sound* — that you can take the MMU out of the primary isolation role and
put a machine-checkable type system in its place, and the system still
holds. That is the whole thesis Wari's Phase 4 rests on, demonstrated in a
research OS two decades ago.

So why did Singularity never ship into the world? The prior-art record is
specific, and the reason is the encouraging kind: "business (Windows
backward compat, heavy C# runtime), not technical" (`docs/prior-art.md`).
Singularity's isolation depended on the CLR — the .NET runtime — which was
tens of megabytes of managed-code machinery, tied to one language, and
commercially yoked to a Windows ecosystem that could not abandon backward
compatibility. The idea was right; the *enabler* was too heavy and too
proprietary to carry it to production. Wari learns from the post-mortem,
not the failure mode.

What changed by 2026 is the enabler. **WASM + wasmi is the enabler the CLR
was not.** WASM's type system is machine-verifiable and standardized, not
owned by one vendor. It is cross-language — a customer writes Rust, Go, C,
or compiled JavaScript and ships WASM — where the CLR was C# and its
cousins. And wasmi is *small*: a `no_std` interpreter of a scale Wari can
count in its TCB and put on a formal-verification roadmap, not tens of
megabytes of runtime. Singularity needed a heavyweight managed runtime to
enforce isolation; Wari needs a compact, verifiable one. The dream did not
change. The tool finally exists to build it in a form small enough to
prove and open enough to ship sovereign.

Two more prior-art anchors keep the bet grounded. **Tock OS** (Levy et al.,
SOSP 2017) is the existence proof at production scale: a Rust kernel that
uses the type system to isolate processes in embedded systems, deployed in
Signal's secure-messaging hardware. Tock demonstrates that language-
enforced isolation is not a lab curiosity — it ships in security-critical
devices today. **RedLeaf** (Narayanan et al., SOSP 2020) is the academic
path from general-purpose toward specialized: Rust-enforced domains with
language-level isolation between kernel components, the closest sibling to
Wari's two-tier model. Between Singularity's proof of concept, Tock's
production evidence, and RedLeaf's recent academic refinement, the Phase-4
endpoint is not Wari inventing something unprecedented. It is Wari
assembling a well-attested idea from named sources, with a 2026 enabler its
predecessors lacked — which is exactly the posture the whole book insists
on: inherit with credit, and label the genuinely new part a bet.

And the genuinely new part *is* a bet. The prior-art record says so
plainly: two-tier WASM is "our defensible moat if it works; requires
`wasmi` to be highly correct." The MMU-free endpoint inherits that bet and
raises it. It is an option Wari keeps open by construction, not a
correctness claim it makes today.

## What "MMU-free" means concretely

Strip the vision to mechanism. In an MMU-free Wari variant, isolation
between a tenant and the kernel does not come from page tables trapping a
stray dereference. It comes from the fact that the tenant's code, as
actually executed, provably cannot *form* a stray dereference in the first
place — because it was produced by a verified translation of validated
WASM.

There are two routes to that guarantee, and Wari is building both. The
first is a **formally verified interpreter**: if wasmi's interpreter core
is proven correct, then a validated WASM module running under it cannot
escape its linear memory, by theorem rather than by hardware trap. The
Phase-4 roadmap names this directly — "wasmi correctness proof (external
academic collaboration)" — and the security model is candid that it is
"speculative; depends on academic partner." The second route is Part 5's
subject: an **ahead-of-time engine** that compiles WASM to native machine
code *and emits a machine-checkable safety certificate* alongside it — a
proof, in the proof-carrying-code tradition, that the generated code
respects the WASM memory model. When the certificate checks, the native
code is known to be sandbox-respecting without anything watching it at
runtime. The verified WASM output *is* the isolation. This is why Part 5 is
load-bearing for Phase 4 and not a side quest: the safety certificate is
the artifact that makes dropping the MMU defensible, because it moves the
isolation guarantee from "the hardware will catch an escape" to "an escape
cannot be generated."

Given either route, **the MMU becomes defense-in-depth rather than the
primary line.** Nothing forces its removal — a Phase-4 board could keep
Sv39 as a redundant Layer 2, and on general-purpose silicon it would. What
changes is that the MMU is no longer the thing the isolation *depends* on.
And that shift is what makes the radical option available: a custom SoC
that omits paging hardware entirely, smaller and simpler and cheaper in
silicon, relying on the verified translation for isolation and reserving
hardware protection for wherever it remains worth the transistors. The
roadmap's "SoC RTL: MMU-free variant" milestone is exactly this — hardware
that trusts the proof.

## The verification obligations and the attestation chain

Two things have to be true for the MMU to safely step back, and they are
the two halves of the trusted computing base the whole book has been
narrowing toward.

The **verification obligations** are: formal `wasmi` and formal Tier 0.
The interpreter (or the AOT engine plus its certificate checker) must be
proven to enforce the WASM memory model, and the native kernel under it
must be proven sound. Chapter 6 showed how the second obligation is already
being assembled — the invariant catalog is the proof skeleton, INV-10 is
already a Kani target, and the Phase-4 gate is "pre-tapeout formal
verification of kernel + wasmi." The immutable endpoint is where those two
proofs meet: a proven interpreter running proven-sound kernel logic needs
no third mechanism to be safe.

The **attestation chain** replaces the MMU's continuous runtime
enforcement with a chain of load-time and silicon-rooted guarantees, link
by link:

- **ROM hash → kernel.** The SoC boots from mask ROM whose contents hash to
  a value fixed in silicon — a hardware root of trust. That ROM *is* the
  immutable Tier-0 kernel (property 4), so the root of the chain is not a
  key a signer holds but a circuit that cannot be rewritten.
- **kernel → driver signatures.** The ROM kernel verifies every Tier-2
  driver's ed25519 signature before instantiation — INV-13 today,
  generalized to INV-11's signed manifests. The trusted kernel vouches for
  the drivers.
- **driver signatures → module attestation.** Signed drivers and attested
  Tier-1 modules extend the chain to the tenant edge, each link
  authorized by the one below it.

Read top to bottom, the chain roots isolation in silicon and carries it up
through the tiers by verified translation and signature rather than by a
page-table check on every access. It is the same trust structure the
two-tier model drew in Chapter 4 — Tier 0 trusted, Tier 2 signed, Tier 1
attested — with its root moved from a boot-time software check down into
the mask ROM itself.

## What this already means for Phase 0

Here is the payoff for a reader who has followed Part 1 and might have
wondered why a Phase-0 kernel is so fussy. Almost every disciplined,
seemingly over-careful decision in the early kernel is a Phase-4
commitment paid early, and the endpoint is why they are not negotiable.

**Phase 0 avoids future-locking patterns on purpose.** No ELF path (R7),
so there is no native loader to rip out before the kernel can be frozen. An
interpreter, not a JIT, so there is no runtime code generation to eliminate
before `.text` can go read-only. Reproducible builds (R8), so the image has
a stable hash to attest. Typed MMIO behind the `mmio` module (R3) with the
bases marked to move behind a `platform::` layer (INV-3's expiry), so a
custom SoC's different memory map is a localized change, not a rewrite. The
pure/impure split enforced file by file, so the FC-IS refactor is a
consolidation of a discipline already in force rather than a ground-up
rework. None of these cost much in Phase 0. All of them would be
enormously expensive to retrofit, and the immutable endpoint is why they
were paid for up front.

**Some Phase-0 invariants transfer directly to the MMU-free model, and
knowing which ones reveals where Wari has been investing.** The invariants
that carry the isolation load *without the MMU* are the structural and
capability ones — forgery prevention (INV-15), monotonicity (INV-10),
derivation integrity (INV-16), the anti-ABA counter (INV-17), signed
loading (INV-11/INV-13). These are exactly the invariants Chapter 6 noted
are closest to being proofs, and that is not a coincidence: they are the
ones that must hold when the MMU is gone, so they are the ones shaped
hardest for verification. The MMU-specific invariants — the page allocator
returning kernel-writable PAs (INV-5), the walker returning installed PAs
(INV-6) — are the ones that change form in an MMU-free variant, because the
mechanism they describe is precisely the one being retired. The catalog, in
other words, already sorts itself along the Phase-4 fault line: the
structural invariants are permanent, the hardware-memory invariants are
contingent, and Wari has spent its rigor accordingly.

The architectural commitments that make Phase 4 possible were therefore all
made in Phase 0. Not because Phase 0 needed them — a first kernel could
have shipped an ELF loader and a JIT and a non-reproducible build and
booted just as well. They were made because an architecture is a set of
options you keep open, and every one of these decisions keeps open the
option of the immutable endpoint. That is what it means to say Part 1 was
written before the code: the code has been executing an argument, and the
argument's conclusion is this chapter.

## Closing hook

That is the end of the argument. Part 1 has claimed a great deal — a WASM-
only kernel, split into two tiers by trust, cherry-picked from a
predecessor under a single honest test, kept sound by a catalog of dated
invariants, aimed at an immutable endpoint on custom silicon. It has argued
all of it and built almost none of it in front of you, because Part 1 was
written before the code, to say what the code is *for*.

Now the code answers. Part 2 opens the kernel and reads it line by line —
boot, the MMU, traps, the wasmi runtime, capabilities, the scheduler, IPC —
and every claim Part 1 made becomes a file you can open and a test you can
run. The arguing is over. Part 2 onward is the build log.
