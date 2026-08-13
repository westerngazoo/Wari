---
sidebar_position: 5
sidebar_label: "Ch 5: Inheritance from Goose"
title: "Chapter 5 — Inheritance from Goose"
---

# Chapter 5 — Inheritance from Goose

Wari is not a fork of `goose-os`. That distinction is load-bearing, so
the project charter states it in the negative: goose-os is "reference
only," a separate repository, and "Wari is WASM-native from boot zero,
not a goose-os fork" (CLAUDE.md, Project Identity). A fork inherits by
default and subtracts what it must; you start with the whole thing and
argue your way out of the parts you dislike. Wari inherits by exception
and adds what it can defend; you start with an empty kernel and argue
each module's way *in*. The difference sounds like bookkeeping and is
actually the entire discipline of this chapter, because it decides who
carries the burden of proof. In a fork, dubious code stays until
someone justifies removing it. In Wari, every line from goose-os had to
earn its passage.

goose-os, then, is a quarry, not a foundation. It contains a working
page allocator, a proven Sv39 walker, a synchronous-IPC state machine,
a staged boot sequence, a trap dispatcher, and — most valuable of all —
a catalog that ties every `unsafe` block to the invariant that makes it
sound. Some of that is exactly what a WASM-native, two-tier kernel
needs, unchanged. Some of it is the right idea wearing the wrong
clothes — sound logic wrapped around ELF and native-process assumptions
the two-tier model just retired in Chapters 3 and 4. And some of it is
dead the moment you commit to WASM-only. This chapter is the audit that
sorts the quarry into three piles: keep, rewrite, delete.

## The test every module had to pass

There is exactly one question the audit asks of each goose-os module,
and it falls straight out of the architecture: *does this module's
design survive the WASM-only, two-tier constraint without depending on
anything the constraint removed?*

Three outcomes follow from that one question.

- **Keep** if the module's design makes no assumption Wari retired — if
  it is pure logic over data structures, or impure glue whose only
  premise is "there is a kernel in S-mode with an MMU." A bitmap
  allocator does not care whether the pages it hands out will hold an
  ELF segment or a WASM linear memory. It survives verbatim.
- **Rewrite** if the *idea* survives but the *implementation* is shaped
  by something removed — a driver that assumed native execution, a
  spawn path that assumed ELF. Keep the design intent; re-home it onto
  the two-tier structure.
- **Delete** if the module exists only to serve an assumption Wari
  rejects — the ELF loader, the hand-rolled interpreter, the native
  user programs. These do not get ported. They get retired, and their
  absence is a feature.

The criterion is worth stating so plainly because it is what keeps the
audit honest. "It compiles" is not a reason to keep code; "it's already
written" is not a reason either. The only passing grade is *the design
is still correct under Wari's constraints* — and where it is correct but
carries a known defect, Wari copies the design and fixes the defect in
the same move, which is where we start.

## What we keep

The largest pile, and the reason cherry-picking is worth the trouble at
all, is the pure-logic core. These modules made no ELF assumption and
no native-process assumption; they are logic over data, and logic over
data does not know or care what runs above it.

**The page allocator** (`page_alloc.rs`) is a pure bitmap allocator: a
region of physical memory, one bit per frame, `alloc` finds a clear bit
and sets it. Nothing in that algorithm depends on what the frame will
hold. It survives unchanged in design, and in Wari it has since been
lifted into its own host-testable crate (`wari-mem`) so its logic can
be exercised on a laptop, off the target — but the algorithm is
goose-os's.

**The page-table walker** (`page_table.rs`) is the pure Sv39 walker and
its PTE data structures: given a root and a virtual address, descend the
three-level tree and yield the leaf. This is RISC-V's Sv39 as specified,
not a Wari invention, and the walker realizes the spec. It, too, moves
to `wari-mem`. And it carries one of the clearest illustrations of what
disciplined cherry-picking means, which we return to below: the walker
takes a `read: FnMut(usize) -> u64` closure rather than reinterpreting a
byte slice as a `&Pte`, which structurally sidesteps a defect goose-os
had.

**The MMU glue** (`kvm.rs`) is the impure counterpart to those two pure
modules — the code that actually writes `satp`, issues the `sfence.vma`,
and installs the kernel's identity map. It is inherited because its only
premise is "an S-mode kernel with an Sv39 MMU," which Wari is. It stays
impure by nature (privileged CSR writes, volatile PTE stores), and so it
stays honest by citation: every `unsafe` in it points at INV-4, INV-5,
INV-7, or INV-12. Part 2, Chapter 9 walks it in detail.

**The synchronous-IPC rendezvous** (`ipc.rs`) is a state machine, and a
famous one: seL4's synchronous rendezvous, where a send and a receive
meet directly with no kernel buffer and no allocation on the path
(Klein et al., SOSP 2009). goose-os implemented that design; Wari
inherits the design and re-homes it. In Wari the rendezvous no longer
stands alone — it is realized through capability `Endpoint` objects, so
"who may send to whom" is a capability question rather than an ambient
one (Part 2, Ch 14). The *shape* is goose's inheritance of seL4's; the
*naming* is Wari's capability layer. That the buffers-free, allocation-
free rendezvous survived the move intact is a small proof that the
design was right to begin with.

**The process and scheduler tables** (`process.rs`, `sched.rs`) are
inherited in their already-split form — after goose-os's "Debt-3" split
separated the process table from the scheduling policy. Wari would have
demanded that split anyway; the per-module rule is "one concern per
file," and a combined proc-and-sched file fails it. Inheriting the
post-split version means inheriting code that already obeys Wari's own
standards, which is the ideal case: no cleanup on entry.

**The staged boot** (`boot.rs`) is the pre/post-condition boot sequence
— each stage stating what it assumes and what it guarantees, so a
failure halts at the stage that owns it rather than three stages later
with the address already lost. That structure is exactly the kind of
"reads as if it will be proven next quarter" discipline Wari wants
everywhere, so it comes across unchanged. Part 2, Chapter 8.

**The trap dispatcher** (`trap.rs`) arrives in its dispatch-table form —
the shape goose-os reached at Build 88, where `scause` classification
drives a table rather than a ladder of `if`s. Its own header is an
artifact of the audit: `trap.S` "records what was *removed* relative to
the goose-os original" (Part 2, Ch 10) — the U-mode entry path, the
`sscratch` swap, the IPC fast-path, the syscall jump table, all excised
because Phase 0 has no userspace to enter from. The file documents its
own subtraction, which is exactly how a cherry-pick should leave a trail.

**The ABI** (`abi.rs`) — syscall numbers, opcode tables, typed error
codes — is extracted into a shared crate (`abi-shared`, now `wari-abi`)
so the kernel and the WASM tooling read from one source of truth. It
carries no `unsafe` and no MMIO, which earns it a place on the
invariant catalog's "non-contributing crates" list: pure data, host-
testable, audit-exempt for unsafe coverage. The extraction is Wari's,
but the constants and the typed-error discipline are goose's.

**The validators** (`security.rs`, renamed `validate.rs`) are the pure
argument-checkers — the functions that decide whether a syscall's
arguments are well-formed before any effect happens. The rename is not
cosmetic: `validate.rs` says what the file *does*, where `security.rs`
said what the file was *for*, and a file named by its function is a file
you can write a one-sentence purpose for. The logic is goose's; the name
is Wari holding to its own "one concern per file" rule.

**The invariant catalog** (`unsafe-audit.md`, renamed `invariants.md`)
is the most valuable inheritance of all, and it gets its own chapter
next. goose-os's format survives verbatim — `invariants.md` opens by
saying so, "Format inherited from `../goose-os/docs/unsafe-audit.md`" —
because the format is the point: every `unsafe` block cites an INV-N,
every INV-N names the condition that makes the unsafe sound, and when a
condition changes every citing site is found by grep. That framework is
Wari's whole formalization-staging bet, inherited wholesale.

### The INV-9 fix: cherry-picking is not copying

One entry in the catalog shows better than any argument what the audit
actually does when it says "keep with a fix." INV-9 governs
reinterpreting a byte slice as a struct: the read must be preceded by a
length check *and* an alignment check (or an unaligned read). goose-os
followed this for length but not for alignment — recorded, in its own
catalog, as follow-up #1. Wari does not inherit the bug along with the
rule. The invariant is copied with the alignment fix folded in, and the
page-table walker is written so the caveat cannot bite: it takes a
`read` closure and never reinterprets a slice as a `&Pte` at all, so
"the slice-to-struct alignment caveat is structurally avoided"
(`docs/invariants.md`, per-file sites).

That is the difference between forking and auditing in one example. A
fork inherits follow-up #1 as latent debt and hopes someone circles
back. The audit reads the predecessor's own record of its known
defects, and pays them off at the moment of crossing. You inherit the
design *and* the list of what was wrong with it, and you fix the second
thing while you copy the first.

## What we rewrite

Some modules carry a sound idea inside an implementation that assumed
something Wari removed. The idea crosses; the implementation does not.

**The device drivers** — UART, PLIC, VirtIO — existed in goose-os as
native code, poking registers directly in ring 0. The *idea* of each
driver survives: Wari still needs to talk to an NS16550, still needs the
PLIC's claim/complete handshake, still needs a network path. But native
in-kernel drivers are precisely what Chapter 4 argued *against* for
anything above Tier 0 — a driver belongs in Tier 2, a signed WASM module
in S-mode reaching hardware through capability-gated host functions. So
the drivers are rewritten, not ported: the register-level knowledge
carries forward, the execution model changes from "native ring-0 code"
to "signed WASM under wasmi." The PLIC is a partial exception — it is
kernel machinery that *routes* interrupts to Tier-2 notifications, so it
stays in Tier 0 (Part 2, Ch 10) — but the UART and network drivers
become the Tier-2 modules the two-tier model calls for, and Part 4 is
the book on how they are written.

**The spawn path** (`syscall.rs::sys_spawn`) assumed the thing Wari most
firmly rejects: it loaded an ELF binary. The idea — "turn a stored image
into a running process" — is essential and survives. The implementation
is rewritten from an ELF loader into a WASM module loader: verify a
signature, hand the bytes to wasmi, instantiate, run `_start` (Part 2,
Ch 11, `load_tier1`/`load_tier2`). Same intent, incompatible mechanism.
There is no `SYS_SPAWN_ELF` on the far side of the rewrite, and by R7
there never will be.

## What we delete

The last pile does not get ported at all, and its emptiness on the Wari
side is the clearest single win of the whole exercise.

**The ELF loader** (`elf.rs`) is deleted outright. It is a direct R7
violation — "No ELF in the customer ABI. Ever." — and there is no
softened version of it that survives. The customer ABI has no ELF path
because the code that would parse ELF does not exist in the ship kernel.
Deleting a loader is also deleting an attack surface: every byte of ELF-
parsing code is a byte an adversary could probe, and Wari simply does
not carry it.

**The hand-rolled interpreter** (`wasm.rs`, `interp.rs`, `wasi.rs`) is
the largest deletion, and the most instructive. goose-os had begun
writing its *own* WASM interpreter — 3,556 lines of it — and Wari throws
all of it away in favor of embedding `wasmi`. This looks like discarding
a heroic amount of finished work, and it is; it is also unambiguously
correct. A hand-rolled interpreter would be 3,556 lines of the most
security-critical code in the system — the thing that stands between
untrusted tenant bytecode and the kernel — that Wari alone would have to
audit, fuzz, and eventually formally verify. Replacing it with a single
pinned dependency the wider ecosystem already fuzzes is not a loss; it
is retiring 3,556 lines of trusted-computing-base debt and inheriting a
verification target that is at least *shared*. The architecture states
the rule that makes this non-negotiable: "No third-party code except
`wasmi` itself." wasmi earns the one exception precisely so that Wari
never has to own an interpreter of its own.

**The native user programs** — the `_user_init` and `_uart_server`
assembly blocks — are deleted because they are the last residue of a
world with native userspace processes. In the two-tier model a "user
program" is a Tier-1 WASM module and a "UART server" is a Tier-2 WASM
driver; hand-written user-mode assembly has no home in either. They go,
and nothing WASM-native misses them.

## The retire rationale: why deleting ~4,000 lines is a win

Add up the deletions and the number is arresting: on the order of 4,000
lines of working, tested goose-os code — a complete ELF loader, a
3,556-line interpreter, native user programs — retired rather than
carried. Measured as "progress," that is a step backward; the goose-os
line count was higher and some of that code *ran*. Measured as Wari
measures — "make it correct, make it secure, make it small, in that
order" — it is one of the best trades in the project.

The reason is the trusted computing base. Every one of those deleted
lines would have lived at the center of Wari's TCB: the ELF loader
parses untrusted input, the interpreter executes untrusted bytecode, the
user programs run at privilege. Wari's entire value proposition is a
Tier 0 small enough for a modest team to audit in a week and, in time,
to formally verify. Four thousand lines of security-critical inheritance
is four thousand lines of that audit that Wari would own alone, forever.
Deleting them buys three things at once: a smaller attack surface (no
ELF parser to probe, no bespoke interpreter to escape), a smaller audit
(fewer lines between a tenant and the kernel), and a *shared* burden on
the one piece that remains (wasmi, fuzzed by an ecosystem rather than by
Wari in isolation).

What Wari gives up is short-term velocity and the sunk cost of finished
work. What it gets is the thing the whole book is organized around:
long-term correctness bought by smallness. The cherry-pick is not
nostalgia for goose-os and it is not a shortcut past writing code. It is
an audit with a single passing grade — *the design is still correct
under Wari's constraints* — applied module by module, keeping what
survives, rewriting what carries the right idea in the wrong clothes,
and deleting what only ever served an assumption Wari made in order to
reject.

## Closing hook

The audit sorted the quarry, but sorting is a one-time act and
correctness is a standing obligation. The single most valuable thing
carried across from goose-os was not a module at all — it was the
catalog that ties every `unsafe` block to the condition that makes it
sound, the mechanism by which the kernel stays honest as it grows.
Chapter 6 — the invariants: why they are first-class documentation, how
they gate every unsafe block, and how they become, in Phase 4, the
proof obligations themselves.
