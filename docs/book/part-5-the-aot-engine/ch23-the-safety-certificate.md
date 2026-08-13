---
sidebar_position: 23
sidebar_label: "Ch 23: The Safety Certificate"
title: "Chapter 23 — The Safety Certificate"
---

# Chapter 23 — The Safety Certificate

The previous chapter ended at the edge of a cliff. If Wari compiles WASM
to native RISC-V, then at some point the kernel maps a page of
compiler-generated machine code as executable and jumps into it. And a
compiler is exactly the kind of thing Wari's trust model exists to keep
out of Tier 0: large, fast-moving, third-party, and — for a code
generator like Cranelift — far too big to read line by line or prove
correct. The interpreter's isolation was self-enforcing; a compiler's
isolation is only as good as the compiler. Emit one memory load without
its bounds check and the escaping module is not malformed input a
validator can reject. It is *correct native code*, and nothing downstream
will question it.

This chapter is the answer to that, and it is the crux of the entire AOT
bet. The answer is not "use a better compiler" or "audit Cranelift." It
is to change what the device trusts. The device will not trust the
compiler at all. It will trust a **signature** and a **certificate** — a
compact proof, shipped alongside the native code, that a **small on-device
checker** can verify — establishing that this specific machine code
provably stays inside its own sandbox. A compiler bug cannot ship an
escaping module, because the checker re-establishes isolation from the
*output*, independently, and refuses to map anything it cannot certify.

Like the rest of Part 5, this is a **design under construction**, and the
hardest, longest-lead part of it. The build plan calls the certificate
and its checker "the long pole" without embarrassment
(`docs/aot-build-plan.md:104`): research-grade, plausibly months of work,
the natural home for an external or academic collaboration. What follows
is the shape of the design and the honest map of what is decided versus
what is still open.

## Trust the output, not the compiler

Start with the property that has to hold. For an AOT-compiled module — a
blob of native RISC-V `.text` inside a WNM — the device must establish,
*before* it maps that `.text` executable, that the code is **software-
fault-isolated (SFI)**. The design document `docs/aot-safety-cert-design.md`
decomposes this into four claims:

1. **Memory isolation.** Every load and store the native code performs
   lands inside the module's own linear memory — never in kernel memory,
   never in another tenant's. This is the load-bearing one. With it,
   compiled code is exactly as confined as interpreted code was.
2. **Control-flow safety.** Indirect branches and calls land only on a
   verified set of targets — a checked jump table, real function entries —
   never in the middle of an instruction and never outside `.text`.
3. **Bounded host transitions.** The only exit to the kernel is the
   sanctioned, capability-checked host-call trampoline. No raw `ecall`,
   no jump to an arbitrary kernel address.
4. **Stack confinement.** Stack accesses stay within the instance's own
   stack region.

If those four hold, a compiler bug cannot produce a module that escapes
its sandbox — because *the device verifies the output*, not the compiler.
That single inversion is the whole idea. The compiler can be as large and
untrusted as it likes, because it has been demoted from a trusted
component to an untrusted one whose work is independently re-checked. It
is the same move Wari already makes with WASM at load time — Chapter 11's
"verify the signature before the parser ever sees the bytes" — extended
one layer down, to the native code the parser's descendants produce.

## The precedent that makes it tractable: VeriWasm

This would be a wild research bet if it had never been done. It has. The
key precedent is **VeriWasm** (Johnson et al., NDSS 2021), and Wari's
situation is almost exactly the one VeriWasm was built for.

VeriWasm is a **static, offline verifier of the native binary** produced
by a WASM-to-native compiler — and the compiler it was built against was
**Lucet**, the same AOT model Wari adopts. It proves SFI memory
isolation *after* compilation by lifting the machine code into a small
intermediate representation and running iterative dataflow analysis —
abstract interpretation over an analysis lattice — function by function.
Crucially, it operates on the compiler's *output* and therefore does not
trust the compiler: it independently re-establishes the isolation
property, and its soundness is proven, with no false positives reported
on real WASM-compiled binaries (`docs/aot-safety-cert-design.md:40`).

So the property Wari needs is not hypothetical. It is known-checkable on
exactly the class of binary Wari intends to produce. That reduces the
research question from "can this be done?" to a narrower and more
tractable one: **where does the check run, and what artifact does it
consume?** — a question that trades device-TCB size against how much the
device has to trust.

There is even a piece of unexpected good luck in the target. VeriWasm
does its hardest work fighting x86-64: variable-length instructions mean
"jump into the middle of an instruction" is a live attack the verifier
must prove impossible, which forces a complex disassembly lattice. RISC-V
instructions are fixed-width — 32-bit, or 16-bit with the compressed
extension — so decode safety collapses to a simple alignment check, and
the lattice largely evaporates
(`docs/aot-safety-cert-design.md:222`). Stack operations that x86 hides
inside `push`/`pop`/`call`/`ret` become explicit loads and stores
relative to `sp` on RISC-V, which the checker can track directly. And
indirect branches, which x86 spreads across arbitrary registers, funnel
through `jalr` — so if the compiler is disciplined about emitting
indirect calls through a bounds-checked jump table, the checker only has
to verify the masking logic immediately preceding each `jalr`. The move
from x86 to RV64 makes the verifier *smaller*, which for a component
destined for the kernel TCB is precisely the direction you want.

## Three models, and the real fork

Knowing the property is checkable, the design lays out three ways to make
the device believe it — and the choice between them is the genuine open
decision, gate **DG-2**, that the previous chapter flagged.

- **Model A — offline-verify + sign.** A VeriWasm-style verifier runs
  *offline*, in the signing pipeline. The device does no analysis at all;
  it trusts the **signature**, which asserts "this passed verification."
  Device cost: tiny — just a signature check. It trusts the offline
  verifier and the signer, but *not* the compiler's code generator.
- **Model B — on-device re-verify.** The full verifier — lifter,
  dataflow, lattice — runs *on the device* at load time. Trusts nothing
  but itself. Device cost: **large** — a whole abstract interpreter in the
  kernel TCB, paid at every module load.
- **Model C — proof-carrying code (PCC).** The compiler emits
  **witnesses** — the facts its offline analysis already established — and
  the device runs a *small* checker that validates those witnesses against
  the `.text` in a single linear pass. Trusts neither the compiler nor a
  large on-device analyzer.

Model C rests on the classic proof-carrying-code insight, due to Necula
and Lee in the mid-1990s: **checking a proof is far cheaper and simpler
than finding it.** The compiler does the expensive fixpoint analysis
once, offline, and ships the answer; the device only re-checks the answer,
which needs no fixpoint iteration and no lattice — a small, auditable
checker that can itself become a formal-verification target.

Model B is **rejected** for Wari, and the reasoning is the same thesis
that rejected the JIT. Putting a full abstract interpreter in the kernel
means putting a large, complex analyzer into the exact trusted base Wari
is trying to shrink toward a ROM-burnable core — and paying its runtime
cost on every load. Model C gets the identical guarantee with a fraction
of the trusted code (`docs/aot-safety-cert-design.md:98`). So the real
fork is A versus C, and the design's recommendation is not to choose one
but to **phase them to the hardware line**.

## Phasing the trust to the hardware: A now, C for the endgame

The recommendation in `docs/aot-safety-cert-design.md:76` is to match the
trust model to whether the MMU is standing behind it.

**Phase 2/3, while the MMU is present: Model A.** Run the SFI verifier
offline; record a signed "verified-offline" attestation in the WNM's
`SafetyCert` section; the device checks the signature, checks that the
hashes bind the certificate to *this* `.text` and *this* source WASM, and
maps the code RX-only. The Sv39 MMU and PMP are still the hardware
backstop underneath — so trusting the offline verifier and the signer is
acceptable, the device side stays tiny, and this is the fastest path to
running AOT code safely at all.

**Phase 4, the MMU-free endpoint: Model C.** This is where the whole
chapter connects back to Chapter 7. Wari's long-term endgame is an
immutable kernel on custom silicon that may omit paging hardware
entirely — where structural WASM isolation, not the MMU, is the primary
line. Chapter 7 named the four properties of that immutable kernel; the
third was *no self-modification — kernel text RO, no JIT, no dynamic
loading*. AOT is what lets that kernel run fast native code without
violating that property, and the safety certificate is what lets it do so
**without the MMU as a safety net**.

The logic is stark. When you remove the MMU, you remove the hardware that
would have caught an escaping load with a page fault. There is no longer a
backstop, so trusting an offline verifier is no longer good enough — the
device must re-establish isolation *itself*, or nothing does. But it must
do so cheaply, because it is a frozen ROM kernel, not a workstation. That
is exactly the shape PCC provides: the compiler emits witnesses, the small
on-device checker validates them before mapping, and **the verified output
becomes the isolation** (`docs/aot-safety-cert-design.md:88`). The checker
joins Tier 0 and `wasmi` in the Phase-4 formal-verification scope — a
small enough thing that proving it correct is a stated goal, not a fantasy.

The elegance is that both models ride the **same carrier**. The WNM
`SafetyCert` section already exists in the format; only its *contents*
differ. Model A fills it with a signed attestation record; Model C fills
it with per-function witnesses. Choosing A first does not foreclose C —
same format, richer payload later (`docs/aot-safety-cert-design.md:94`).
Wari can ship AOT under the MMU's protection now and grow into the
MMU-free line without a format break.

## What the on-device checker actually does

To make Model C concrete, picture the checker's single linear pass over
`.text` plus its witnesses. The design sketch
(`docs/aot-safety-cert-design.md:125`) has it confirm, and reject the
whole module the instant any of them fails:

1. every memory access is preceded by a bounds check, or uses an index
   provably masked within `[0, linmem_len)`;
2. every indirect branch is masked or checked into the verified target
   table;
3. every call site targets the host-call trampoline entry or a verified
   function entry — never a raw kernel address, never a bare `ecall`;
4. stack-pointer adjustments keep every stack access inside the instance's
   stack region;
5. the witnesses actually correspond to *this* `.text`, hash-bound, so a
   valid certificate cannot be transplanted onto different code.

It **fails closed**: any check that does not pass means the module is
never mapped. And the witness format is designed for exactly this pass —
a dense per-function record of the masked memory-access offsets, the
bounds-checked `jalr` offsets, and the proven maximum stack depth, laid
out so the checker consumes one witness per relevant instruction with no
backtracking (`docs/aot-safety-cert-design.md:227`). The design even works
a trivial module by hand — a two-instruction `add; ret` with no memory
access and no indirect branch, whose certificate records zero of each and
whose checker scans the text, finds nothing to bound, verifies the stack
depth is zero and the `ret` is a clean `jalr x0, 0(x1)`, and accepts
(`docs/aot-safety-cert-design.md:255`). The worked example is deliberately
small; the point is that the *checking* is small too.

## The honest boundary of the guarantee

A certificate that promised too much would be worse than none, so the
design is explicit about what it does and does not catch
(`docs/aot-safety-cert-design.md:244`).

It **catches** the escapes that matter for isolation: any load or store
reaching outside the sandbox, any control-flow hijack — jumping into the
middle of an instruction, a ROP chain, a jump into kernel code — and any
attempt to leave through a raw `ecall` instead of the sanctioned
trampoline.

It **does not catch** functional bugs or side channels. If the compiler
miscompiles an `add` into a `sub`, the certificate passes, because the
wrong arithmetic never leaves the sandbox — SFI guarantees *isolation*,
not *correctness*. And timing or cache side channels inside the sanctioned
boundary are out of scope here; they belong to the confidential-compute
work of Part 6, not the safety certificate. Naming the boundary precisely
is what makes the guarantee trustworthy: the certificate says the module
cannot escape its box, and says nothing about whether the module computes
the right answer inside it. That is the correct division of labor — the
differential oracle from Chapter 22 is what watches for the miscompiled
`add`, by proving the native output is observably identical to the
interpreter; the certificate watches only for the escape.

## Where the state of play honestly sits

It bears repeating, because the subject invites overclaiming: none of
this is built. The `SafetyCert` section exists in the WNM format as a
place to put a certificate; the certificate format, the offline verifier,
and the on-device checker do not yet exist as running code.

The **decided** parts are the trust architecture and its sequencing. The
trust anchor is the SFI check over the native binary, not the compiler
(decision D1). The model is phased — A while the MMU backstops, C for the
MMU-free endpoint (D2). Model B, on-device full re-verify, is rejected
(D3). The `SafetyCert` section carries both models (D4). And the whole
track runs *in parallel* with the M0/M1 work of the previous chapter,
precisely because it dominates the schedule (D5) — the cert-format RFC
(roadmap task G7a) is meant to start immediately, before the compiler it
will certify even exists.

The **open** parts are the ones that need the architect and, frankly, the
research. DG-2 — guard pages versus explicit-bounds-plus-certificate — is
a live fork, though the MMU-free endgame argues hard for the second. DG-3
— the concrete certificate wire format, whether adapted from VeriWasm or
specified fresh for RV64 — is open by design, awaiting the G7a proposal.
And the single largest unknown is the verifier itself: adapting VeriWasm's
analysis (its lattice transfers from x86 to RV64; its lifter and backend
do not) or commissioning a fresh RV64 SFI verifier is, either way, months
of work and the reason this track is where an outside collaboration would
land (`docs/aot-safety-cert-design.md:158`).

The first concrete step is characteristically modest: once the M0 oracle
and a throwaway Cranelift spike exist, run a *prototype* SFI check —
even a hand-checked property list — over one spike-compiled module, just
to confirm that Wari's own code generation emits analyzable, isolatable
code: bounds checks present, no wild indirect branches. That de-risks the
entire verifier before a line of it is committed
(`docs/aot-safety-cert-design.md:176`). It is the same instinct as the M0
gate — prove the premise cheaply before investing in the conclusion.

## Closing hook

Step back and see what the certificate buys. It is the hinge that lets a
kernel obsessed with a small, provable trusted base run native code at
all — because it converts "trust a large compiler" into "check a small
proof," and a small proof-checker is the kind of thing that belongs in a
ROM-burned kernel next to a formally verified `wasmi`. Without it, AOT
would be a quiet abandonment of the correctness-first thesis, native
speed bought by admitting a code generator into Tier 0. With it, AOT is
the thesis *extended*: the same "verify, don't trust" move that gated the
WASM parser, now gating the machine code that WASM becomes.

And it is the piece that makes the ending of Part 1 reachable. When the
MMU is gone and the kernel is frozen in silicon, the verified output *is*
the isolation — there is nothing else left to be it. Everything after this
is the vision that machinery was built to keep within reach: confidential
compute that hides a tenant's memory even from the kernel, the GAPU
coprocessor that does for acceleration what Nitro did for AWS, and finally
the kernel burned into ROM that this entire book has been walking toward.
That is Part 6 — the road ahead.
