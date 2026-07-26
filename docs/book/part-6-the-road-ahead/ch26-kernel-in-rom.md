---
sidebar_position: 26
sidebar_label: "Ch 26: Kernel in ROM"
title: "Chapter 26 — Kernel in ROM"
---

# Chapter 26 — Kernel in ROM

Chapter 7 named the endpoint and left it as a sketch: four properties of
an immutable kernel, Singularity's dream twenty years on, an MMU-free
custom SoC as the place the architecture logically leads. This chapter is
the fuller telling. It is also the last chapter, so it owes the reader
two things — a precise account of what "kernel in ROM" would mean, and an
honest account of how far away it is — and then it owes the book an
ending.

Start with the sentence the whole project has been ordered by:

> Make it correct. Make it secure. Make it small. In that order.
> (Performance comes from smallness.)

That ordering has looked, at times, like an aesthetic preference — a
tasteful minimalism, the sort of thing a careful engineer says. It is
not. It is a specification for a destination. A kernel small enough to
*prove*, frozen enough to *trust*, and finally reduced to a hash burned
into mask ROM is only reachable if smallness came before speed at every
fork in the road. This chapter argues that the destination was always
that literal, and walks the three properties — small, frozen, burned —
that would get there.

## Small enough to prove

Tier-0 is scoped at five to ten thousand lines and named, from the first
page of `CLAUDE.md`, as a formal-verification target. The number is not a
boast about frugality; it is the precondition for a proof. Formal
verification cost scales viciously with size, and the reason "make it
small" outranks "make it fast" is that a fast kernel you cannot prove is
worth less, to the tenants Wari serves, than a slow kernel you can.

The proof lineage is explicit and cited. **seL4** (Klein et al., SOSP
2009, `docs/prior-art.md:94`) is the existence proof: roughly ten
thousand lines of C, verified in Isabelle/HOL down to the machine code,
demonstrating that a real microkernel's functional correctness can be
mechanically established if — and only if — the kernel was disciplined
for it from the start. seL4 is also the honest cost signal. It took on
the order of twenty-five person-years to reach its first verified
release, funded by a national research agency, a defense grant, and a
commercial spinout (`docs/research/heli-adversarial-review.md:72`). The
proof is the endpoint of an institution, not of a weekend.

Against that bar, here is exactly where Wari stands, stated plainly so no
one mistakes the aim for the achievement. The capability system carries
seven Kani harnesses — but they prove the *pure-logic mint primitive*,
and the hard cases are unproved: revocation cascades, IPC capability
transfer, and generation-counter monotonicity across the combined
mint-and-delete state machine are unit-tested or argued in review, not
mechanically verified (`docs/research/heli-adversarial-review.md:30`).
The Phase-4 roadmap names the two proof targets that matter next —
capability monotonicity and scheduler invariants (`CLAUDE.md`, Phase 4) —
and they are targets, not results. What exists is a handful of proofs
about the easiest module and a discipline that keeps the rest
provable-in-principle. That is a real head start and it is not a verified
kernel, and the gap between those two sentences is measured in
person-years.

Then there is the proof obligation that dwarfs the rest: **`wasmi`**. The
security model's load-bearing caveat is that the interpreter runs in
S-mode inside the kernel address space, so a host-side soundness bug in
wasmi corrupts kernel memory with no privilege boundary to stop it — the
one place where Layers 1 and 2 collapse into a single layer
(`docs/security-model.md:30`). Every isolation claim in this book has an
asterisk pointing at that fact. Formally verifying the wasmi interpreter
core is the only thing that removes the asterisk, and the roadmap is
candid that it is *speculative*, depending on an external academic
collaboration that does not yet exist (`docs/security-model.md:48`). This
is not a footnote to the ROM endpoint; it is the endpoint's central
precondition. A kernel-in-ROM whose interpreter is unverified is a frozen
image of an unproven TCB — the worst of both, immutability without
assurance. The existence proof that a real language runtime *can* be
verified is **CompCert**, the formally verified C compiler; it is the
reason "verify wasmi" is an ambitious research program rather than a
category error. But it is a program, and it has not started.

## Frozen enough to trust

Provability is necessary and not sufficient. A proof is a statement about
a specific artifact; it is worthless if the artifact running on the board
is not the artifact that was proved. The second property is therefore
immutability — a chain of evidence from silicon up to the running kernel
that forecloses substitution.

Chapter 7's four properties are the mechanism, and each one traces back to
an absolute rule that looked, in Phase 0, like ordinary hygiene:

1. **Functionally pure state transitions.** The functional-core /
   imperative-shell refactor of Tier-0 (Phase 4a on the roadmap) pushes
   the kernel's logic into pure functions whose outputs depend only on
   their inputs — the form a prover can reason about. The no-heap-in-
   dispatch rule (R2) and the pure-before-impure module discipline were
   the down payment on this; a kernel that allocated in its trap handler
   would not have a pure core to lift out.
2. **Hash-attested boot.** An open, small equivalent of Secure Boot:
   OpenSBI measures the kernel, the kernel's hash is the root of a chain
   that continues into driver signatures and then into per-module
   attestation. The chain is *ROM hash → kernel → driver signatures →
   module attestation* — the same attestation grammar Chapters 24 and 25
   leaned on, now anchored in immutable silicon at its root.
3. **No self-modification.** Read-only kernel `.text`, no JIT, no dynamic
   loading. This is why Part 5's AOT engine compiles *ahead* of time, in
   the signing pipeline, and why R7's absolute refusal of an ELF loader
   was never negotiable: a kernel that could load and run new native code
   at runtime is a kernel that cannot be frozen. The AOT bet and the
   immutability bet are the same bet seen from two ends.
4. **Burnable to mask ROM.** Reproducible builds (R8) — committed
   lockfile, pinned toolchain, bitwise-identical output — are what make a
   burned hash meaningful. If the build is not reproducible, "the ROM
   contains the audited kernel" is unprovable, and the whole attestation
   chain dangles from an unverifiable root.

Seen this way, R2, R7, and R8 were not three independent rules. They were
three faces of a single commitment to a kernel that could eventually stop
changing. The discipline was the plan.

## Burned into silicon: the MMU-free variant

The third property is the one that sounds like science fiction and is
actually the oldest idea in the book. If wasmi and Tier-0 are formally
verified and the kernel is a hash-attested read-only image, then the WASM
validator's structural guarantee — no module can construct a pointer
outside its own linear memory, *proven at load time* — becomes the
primary isolation mechanism, and the MMU drops to defense-in-depth. The
verified WASM output *is* the wall. At that point the paging hardware is
redundant, and a custom SoC can omit it: the kernel-in-ROM tapeout, a
Tier-0 burned into silicon that does language-enforced isolation without
a memory-management unit at all.

This is **Singularity's dream** (Hunt & Larus, MSR 2003–08,
`docs/prior-art.md:109`) — an operating system whose processes are
isolated by a checked type system rather than by page tables — reachable
now because the enablers Singularity lacked have arrived. WASM plus wasmi
is a smaller, cross-language, machine-verifiable runtime than the CLR was
in 2003 (`docs/prior-art.md:119`), and Singularity was cancelled for
business reasons, not technical ones (`docs/prior-art.md:123`). The idea was sound; the
substrate was not ready. Two other citations close the argument that it
ships. **Tock OS** (Levy et al., SOSP 2017, `docs/prior-art.md:127`)
proves language-enforced isolation runs in production security-critical
hardware today, with a Rust type system standing in for the MMU — and
Wari's delta over Tock is precisely that its isolated units are *WASM
binaries with a signature gate*, auditable and language-agnostic, rather
than Rust source. **RedLeaf** (Narayanan et al., SOSP 2020,
`docs/prior-art.md:140`) is the academic path from a general-purpose
kernel to language-isolated domains, the closest sibling to Wari's
two-tier model. The MMU-free endpoint is not a leap off the edge of the
prior art. It is the prior art, followed to its conclusion.

## Why not now — the honest distance

This is the furthest thing on the map, and the book would betray its own
standard if it let the vision blur the distance.

Every dependency in the previous sections is unmet. The functional-core
refactor of Tier-0 has not started. The scheduler and capability
invariants are named Phase-4 targets, not proofs. The wasmi verification
is speculative and needs an academic partner who has not been found.
CoVE-class silicon — the confidentiality layer a sovereign ROM kernel
would want beside it — is not yet shipping (Chapter 24). And a tapeout is
a capital event, not a commit. The comparable-projects ledger is
sobering on purpose: seL4's twenty-five person-years, Singularity killed
despite technical soundness, Genode and HelenOS at fifty-plus
person-years each and still niche or hobbyist
(`docs/research/heli-adversarial-review.md:70`). Reaching the endpoints
this part describes is a fifteen-to-forty person-year undertaking with no
funding on the page (`docs/research/heli-adversarial-review.md:87`), begun
by one architect with an LLM collaborator, on a kernel that as of this
writing is still calibrating a PHY delay to get a stable ping.

So the honest claim is not "we will burn a proven kernel into ROM." The
honest claim is narrower and, in its way, larger: *every decision so far
was made so that we could, if the proofs and the institution arrive.* The
MMU stays the primary isolation line through Phase 3. Phase 4 only opens
the option — and the option stays open only because smallness came before
speed, because the ELF loader was refused, because the build was made
reproducible, because the kernel was written as if a prover would read it
next quarter. Nothing above is promised. Everything above is *reachable*,
and keeping it reachable is what the discipline in this book has been for.

## Closing hook

The book opened on a name. The Wari — the Andean empire of roughly 600 to
1000 CE — built roads and ran an administrative state on a quipu
information system, and the infrastructure they laid down outlasted them:
the later Inca did not start from nothing, they inherited a network. That
was the thesis of Chapter 1. Sovereign infrastructure is not the thing
you use; it is the thing that is still standing, still trustworthy, after
the people who built it are gone.

A kernel burned into ROM, small enough to have been proven, is the most
literal form that thesis can take. Its trust does not rest on the goodwill
of whoever operates the machine this year, or on a promise that the vendor
will not push a compromised update next year, or on the hope that the
build the auditors read is the build in the field. It rests on a hash in
silicon and a proof on paper — infrastructure whose integrity is a
property of physics and mathematics rather than of institutions and their
continued good behavior. That is what "make it correct, make it secure,
make it small" was always building toward. Not a fast kernel. A durable
one — the kind of thing you could hand to a hospital, a ministry, a bank,
and say: you do not have to trust us, and you do not have to trust whoever
runs this after us, because the machine cannot betray you and we can prove
it.

None of it is built. The road is drawn, and the drawing is honest about
being only a drawing. But the Wari did not lay their roads for themselves
either. They laid them for whoever would still be walking them a thousand
years on — and someone was. That is the standard this project set for
itself on its first page, and it is the one it will be measured against on
its last: not whether the demo boots, but whether, a very long time from
now, the thing is still worth trusting.
