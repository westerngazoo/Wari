---
sidebar_position: 6
sidebar_label: "Ch 6: The Invariants"
title: "Chapter 6 — The Invariants"
---

# Chapter 6 — The Invariants

Open any file in the Wari kernel and you will find the same shape
recurring, an `unsafe` block with a comment bolted to its roof:

```rust
// SAFETY: INV-1 (single-hart). PROCS is scheduler-owned and the
// scheduler only runs in trap context with interrupts disabled.
unsafe { PROCS[pid].state = ProcessState::Ready; }
```

That comment is not decoration and it is not a wish. It is a citation,
and it points at a numbered entry in a single document, `docs/invariants.md`,
that states the exact condition under which this particular `unsafe` is
sound. Absolute Rule R1 makes the citation mandatory: "Every `unsafe`
block must have a `// SAFETY:` comment citing which invariant in
`docs/invariants.md` makes the operation sound. No exceptions. No 'trust
me.'" This chapter is about why that rule exists, what the invariant
catalog actually is, and why it is the reason the whole codebase reads
the way it does.

The short version is this: Wari treats its unsafe code as a set of
theorems, each with a stated hypothesis. The `unsafe` block is the
theorem — "this pointer write is memory-safe." The invariant it cites is
the hypothesis — "provided only one hart runs kernel code." The catalog
is the list of hypotheses the whole kernel is allowed to assume, and R1
is the rule that no theorem may quietly assume a hypothesis that is not
on the list. It is a proof system enforced by convention and grep,
staged so that a real prover can take it over later without rewriting
the argument — only checking it.

## Why a catalog is the right formalization-staging artifact

Wari's endgame is formal verification: Phase 3 and 4 aim to *prove*, in
Kani or Prusti, that the kernel's core properties hold. You do not get
to that endgame by writing ordinary code for four years and then
attempting to verify it in one. Verifiable code has to be *shaped* for
verification from the first commit — the code-quality standard says as
much: "Every module should read as if Kani or Prusti will prove its
invariants next quarter — because eventually one of them will." The
question is what artifact carries that intent forward across the years
between "we wrote it carefully" and "we proved it."

The invariant catalog is that artifact, and it is the right one for a
specific reason: it is the exact object a proof needs, written in prose
first. A machine proof of memory safety is, at bottom, a discharge of
obligations — for every unsafe operation, a demonstration that its
precondition holds. An invariant is precisely one such precondition,
named and stated. By requiring every `unsafe` to cite an invariant *now*,
in English, Wari is assembling the proof's obligation list continuously,
as the code is written, by the person who best understands why the
operation is sound — the author, at the moment of authorship, when the
reasoning is fresh. A verification effort that begins in Phase 3 does not
have to reverse-engineer why each `unsafe` is safe. The reasons are
already written down, one per block, cross-indexed to the sites that
depend on them. The prover's job shrinks from *discover the argument* to
*check the argument*.

That is why the catalog, and not line coverage, is Wari's measure of
rigor. The testing standard is blunt about rejecting the usual metric:
coverage here means "every invariant has at least one test that would
fail if the invariant were violated," not a percentage that "rewards
testing easy code." The catalog defines what must be true; the tests
defend each entry; the `unsafe` blocks consume the entries; and R1
keeps the three in sync. Line coverage is a side effect of that
discipline, never its target.

## The shape of an invariant

Before the tour, look at the anatomy, because the format is inherited
deliberately from goose-os's `unsafe-audit.md` and every entry obeys it.
An invariant has three parts, and the third is the one most catalogs
forget.

First, the **guarantee** — the condition itself, stated as something
true of the system. INV-1: "Only one hart executes kernel code at a
time. Interrupts are disabled on entry to the trap vector and not
re-enabled until sret." A flat, checkable claim about how the kernel
runs.

Second, the **consequence** — what the guarantee licenses. INV-1's
consequence: "`static mut` access without synchronization is sound for
scheduler-owned state (`PROCS`, `CURRENT_PID`, `TICKS`)." This is the
bridge from the condition to the `unsafe` blocks that lean on it. A block
citing INV-1 is asserting "my soundness follows from this consequence."

Third — and this is the part that makes the catalog a living document
rather than a monument — the **expiry**: *when this breaks*. INV-1:
"SMP. Every INV-1 citation needs per-hart or locked access." Every
invariant names the future change that will invalidate it and tells you
what to do when that change arrives. An invariant in Wari is *dated*; it
carries its own obsolescence clause. That single habit is what turns a
pile of assumptions into a migration plan, and we return to it below
because it is the mechanism by which the kernel survives its own growth.

## The Phase 0 baseline: INV-1 through INV-9

The nine baseline invariants are the hypotheses the earliest kernel was
allowed to assume. Read as a set, they describe a very particular
machine: one hart, interrupts off, in S-mode, with a linker and an MMU
it trusts.

**INV-1 (Single-Hart Kernel)** is the keystone, and half the kernel
leans on it. One hart, interrupts masked from trap entry to `sret`. Its
consequence is enormous: unsynchronized `static mut` is *sound*, because
there is no second thread of execution to race with. Almost every static
in the kernel — the process table, the current PID, the page allocator
singleton — is safe to touch without a lock only because INV-1 holds.
That is also why INV-1's expiry clause is the most consequential in the
document: the day SMP lands, this single invariant fails, and every
citation of it — dozens — must convert to per-hart or locked access. INV-1
is the cheapest invariant to state and the most expensive to lose.

**INV-2 (Trap Frame Exclusivity)** is INV-1's near relative and the best
illustration of an invariant being *purchased* rather than assumed. A
trap handler takes `&mut TrapFrame`; in a preemptible kernel that
reference would be a hazard, because a nested trap could hand out a second
`&mut` to the same frame. INV-2 says it does not alias, and the reason it
does not is that S-mode traps run with interrupts masked, so no second
trap arrives mid-handler. Part 2, Chapter 10 traces exactly how this
invariant is bought: it is the same masked-interrupt posture that means
the kernel cannot be preempted at all. INV-2 is not free — it is paid for
by giving up preemption, and its expiry clause names the bill: "nested
interrupts. Prevented by SIE=0 during S-mode trap service." The day
preemption arrives, INV-2 and the whole non-nesting story are re-audited
together.

**INV-3 (MMIO Address Validity)** licenses the typed volatile wrappers:
hardcoded MMIO bases are fixed by hardware spec, so a read or write to
one is a register operation, not arbitrary memory access. It expires on a
port to a different SoC layout, at which point the bases move behind a
`platform::` module. **INV-7 (Privileged ASM Is Privileged)** is its
cousin for CSR and instruction-level unsafe: `sret`, `wfi`, `sfence.vma`,
CSR writes are sound because the kernel runs in S-mode; the `unsafe` is
only there because Rust requires it around inline asm, not because the
instruction is in doubt. Between them, INV-3 and INV-7 cover most of the
kernel's hardware-facing unsafe, and R3 confines raw volatile access to
`kernel/src/mmio/` so INV-3's surface stays small enough to enumerate.

**INV-4, INV-5, and INV-6** form the memory-soundness chain. INV-4
(Linker Symbol Addresses Are Valid): symbols like `_end` and `_heap_end`
have link-time addresses, so reading them as `usize` is sound with no
dereference. INV-5 (Page Allocator Returns Kernel-Writable PAs): the
allocator only hands out addresses in `[_end, _heap_end)`, which the
kernel identity-maps RW, so writes through them cannot clobber kernel
text. INV-6 (Page-Table Walker Returns Installed PAs): the walker invokes
its callback only for a present leaf whose PA came from a validated
mapping. Chained, they let the memory subsystem's unsafe blocks each cite
one link and stay locally checkable.

**INV-8 (Static-Mut Singleton Accessors Are Called Post-Init)** governs
the `&'static mut` accessors — `page_alloc::get()`, the runtime and driver
singletons — that hand out references to statics initialized once at boot.
The invariant is simply that callers obtain the reference only after
`init()` has run. It pairs with INV-1 constantly: INV-1 says "no aliasing
because single-hart," INV-8 says "and it's initialized because post-init,"
and together they make a singleton accessor sound.

**INV-9 (Bytewise Struct Reinterpretation Is Bounds-Checked)** is the one
we already met in Chapter 5. Reinterpreting `&[u8]` as `&StructT` must be
preceded by a length check *and* an alignment check. It is the invariant
Wari inherited *with a fix* — goose-os enforced length but not alignment,
and Wari folds the alignment requirement in and then, in the page-table
walker, structurally avoids the reinterpretation entirely. INV-9 is the
proof that the catalog is not copied blindly: an inherited invariant
arrived carrying a known follow-up, and the cherry-pick paid it off.

Read together, these nine are a portrait of a deliberately simple
machine. That simplicity is not an accident of an early kernel; it is the
thing that makes every unsafe block locally checkable. You can hold INV-1
through INV-9 in your head, and so you can read any single `unsafe` block
and verify its citation without loading the rest of the kernel into
memory. The catalog is short on purpose, because a hypothesis list you
cannot memorize is a hypothesis list you cannot check.

## How a new unsafe block lands

The catalog is only trustworthy if it stays complete, and completeness is
a process, not a wish. When a change introduces an `unsafe` block, R1 and
the PR workflow prescribe an exact loop:

1. **Identify** the invariant that makes the operation sound. Is the
   block safe because only one hart runs? Because the MMIO base is fixed?
   Because the accessor is post-init? Name it.
2. **If no invariant covers it, write one.** A genuinely new kind of
   unsafe demands a new INV-N entry — guarantee, consequence, expiry — in
   *the same PR* that introduces the block. You do not get to add unsafe
   now and document the invariant later; "we'll add tests in a follow-up"
   is explicitly not acceptable for the security-critical layer, and the
   same holds for invariants.
3. **Cite** it in the `// SAFETY:` comment on the block, and add the
   per-file row to the catalog's site table so the block is discoverable
   from the invariant and vice versa.
4. **Test** it — add at least one test that would fail if the invariant
   were violated, so the guarantee is defended by something executable and
   not only by prose.

Enforcement backs the loop. `cargo clippy -- -D warnings` runs with
`undocumented_unsafe_blocks` on, so a block with no SAFETY comment fails
the build. And every phase-gate audit runs the cross-check in both
directions: for every `unsafe` in the tree, is there a matching row in
`invariants.md`? For every invariant, are its citing sites still valid?
The catalog and the code are kept in a bijection, mechanically, so that
the document can never quietly drift out of date while the kernel evolves
under it.

## The Phase 1b additions: invariants that already prove themselves

Phase 1b's capability system added a second cluster of invariants, and
they show the formalization-staging bet beginning to pay real interest.
Two are worth holding up.

**INV-10 (Capability Monotonicity)** states that for any successful
`Cap::derive`, `child.rights & !parent.rights == 0` — the kernel never
mints a child with rights its parent does not hold. This is the algebraic
form of "you cannot grant what you do not have," and it is what makes
capability revocation sound: rights cannot be amplified through a chain of
mints. Here is the payoff. INV-10 is enforced by a *pure* function with
no `unsafe` and no statics, which means it is not merely documented — it
is unit-tested exhaustively and it is a **Kani proof target**. The catalog
records the proof names directly: `derive_preserves_rights_monotonicity`
and `derive_rejects_rights_amplification`. This is the staging arriving at
its destination for one invariant ahead of the rest: the property that
was written as English prose in the catalog is, for INV-10, already a
machine-checkable theorem. The design "ships the proofs *as* the
specification" (Part 2, Ch 12).

**INV-11 (Tier-2 Grants Are Signed)** connects the capability system back
to the trust boundary Chapter 4 drew. A Tier-2 module's CSpace is
populated only from caps its signed manifest declares; every capability a
Tier-2 instance can reach traces, through its parent chain or an IPC
delegation, to a kernel-issued root cap that a signature authorized. It
generalizes INV-13, the Phase-0 invariant that a Tier-2 blob is ed25519-
verified before wasmi ever parses it. INV-11 is where "signed and
attested" stops being a slogan from the architecture diagram and becomes
a stated property with an enforcement site: boot-time root-cap
construction is the only producer of root caps, and it consults the
signed manifest as its input.

The rest of the Phase-1b cluster — forgery prevention (INV-15), derivation
integrity (INV-16), the anti-ABA generation counter (INV-17), slot-index
bounds (INV-18) — fills out the capability system's soundness argument,
and Part 2, Chapter 12 walks each where it is realized. The point for Part
1 is the trajectory: the newest invariants are the ones closest to being
proofs, because the code that carries them was shaped, from its first
line, to be proven.

## When an invariant breaks

Now the expiry clause pays off, because invariants in Wari are designed to
be outlived. Consider the three most instructive expiries in the catalog.

**INV-1 meets SMP.** The day a second hart runs kernel code, "only one
hart executes kernel code at a time" is simply false. Nothing subtle
happens; the invariant is void. What saves the kernel from silent
corruption is that the failure is not silent: every site that leaned on
INV-1 cited it by name, so a grep for `INV-1` produces the exact list of
blocks that must convert to per-hart or locked access. The migration is
bounded and enumerable *because* the citations were mandatory. An
undocumented `static mut` in an SMP kernel is a landmine; an INV-1-cited
`static mut` is a checklist item.

**INV-23 retires on schedule.** INV-23 (IRQ Routing Determinism) governs
the boot-time interrupt-to-notification table, and it holds only because
nothing writes the table after init. Its expiry clause names the exact
future feature that ends it: "Phase 1c when a `sys_irq_bind` syscall lands
to allow drivers to register IRQs at runtime. INV-23 is then replaced by
INV-1 covering the binding write path." The invariant was drafted knowing
its own expiration date. When the syscall lands, the audit does not have
to *discover* that a read-only assumption was violated; the catalog told
it in advance which invariant to retire and which to promote in its place.

**INV-12 hands off to a real allocator.** The boot-only bump allocator's
arena is valid only because it is initialized once and never freed; INV-12
says so, and its expiry says "Phase 1's real allocator lands. INV-12
retires; a new INV covers the replacement allocator's invariants (free-
list integrity, etc.)." The invariant is scaffolding that knows it is
scaffolding.

This is the deepest reason the catalog is worth the discipline. A kernel
that grows will, repeatedly, invalidate assumptions it made when it was
smaller. The difference between a codebase that survives that and one that
rots is whether the invalidated assumptions can be *found*. Wari's answer
is that every assumption is named, cited at every site that depends on it,
and stamped with the change that will end it. Growth becomes a series of
reviewable migrations instead of a slow accumulation of latent unsoundness.

## From prose to proof

Follow the trajectory to its end and the chapter's thesis resolves. The
Phase-0 invariants are prose — carefully stated hypotheses, defended by
tests, cited at every site. The Phase-1b invariants are prose that has
begun to compile: INV-10 is already a Kani target, its English guarantee
mirrored by a machine-checked theorem. Phase 3 and Phase 4 finish the
arc. The audit cadence names it directly — the Phase-4 gate is "pre-
tapeout formal verification of kernel + wasmi" — and when it arrives, the
proof obligations are not a new artifact anyone has to invent. They are
the invariant catalog, promoted. Every INV-N that was written as a
hypothesis becomes a lemma the prover discharges; every SAFETY comment
that cited an invariant becomes a use of a proven fact; and the "when this
breaks" clauses become the boundary conditions the proof is stated under.

The codebase reads the way it does — SAFETY comments on every unsafe
block, a numbered catalog behind them, tests that defend each entry, and
expiry clauses that read like a migration guide — because it is a proof
being written in slow motion, by hand, in a form a machine will one day
check. R1 is the rule that keeps the proof honest while it is still made
of English. The catalog is the proof's skeleton. And the discipline of
never adding an `unsafe` without a cited, tested, dated invariant is the
whole difference between a kernel that hopes it is correct and one that is
assembling, commit by commit, the argument that it is.

## Closing hook

An invariant catalog that grows toward proof, a Tier 0 small enough to
verify, a WASM sandbox that holds structurally — line these up and they
point somewhere specific. If the interpreter is proven correct and the
kernel is proven sound, what is the MMU still *for*? Chapter 7 follows
the architecture to its logical endpoint: the immutable kernel, hash-
attested and eventually burned into ROM, where the verified WASM output
*is* the isolation and the hardware page tables become defense in depth
rather than the primary line.
