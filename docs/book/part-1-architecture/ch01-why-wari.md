---
sidebar_position: 1
sidebar_label: "Ch 1: Why Wari"
title: "Chapter 1 — Why Wari"
---

# Chapter 1 — Why Wari

A hospital in Oaxaca runs its patient records on a virtual machine it
does not control, in a datacenter it cannot enter, on a processor whose
microcode is a trade secret, under an operating system of thirty million
lines that no one on the continent has read end to end. The bill clears
every month. The arrangement works, in the sense that the records load.
It is also, in every dimension that matters for a sovereign institution,
a lease on someone else's territory. When the terms change — the price,
the export license, the jurisdiction the data is subpoenaed under — the
hospital finds out at renewal.

This chapter argues that the arrangement is not inevitable, that the gap
it exposes is large and specific, and that closing it is worth building
an operating system from boot zero to do. It also explains the name,
because the name is not decoration: it is the shortest available
statement of the thesis.

## The sovereign-cloud thesis

Start with who needs a sovereign cloud, because the answer is narrower
and sharper than the marketing category suggests. A sovereign cloud is
not "a cloud that happens to be in your country." It is a compute stack
a polity can *inspect, audit, fork, and refuse* — one where the people
who depend on it can, in principle and with a small enough team, read
the code that governs their data and prove to themselves what it does.
The customers for that property are the institutions that cannot treat
their infrastructure as a disposable contract: governments running
identity and tax systems, hospitals holding records that outlive any
vendor relationship, banks whose regulators demand an auditable chain of
custody, and — behind all of them — citizens who never signed anything
but bear the consequences when the stack is captured.

Now the second question, the one that turns a need into a market: who
*doesn't* build this? The hyperscalers don't, and not out of malice.
Their architecture is the wrong shape. A stack optimized to rent compute
to millions of tenants at maximum throughput is optimized for exactly
the properties that make sovereignty impossible: a trusted computing base
too large to audit, silicon whose security features are proprietary and
occasionally deprecated out from under you (Chapter 2 returns to Intel
SGX as the cautionary case), and a business model in which the provider
"sets the terms, sets the laws, sets the chip supply, sets the price"
(`docs/manifesto.md` §III) and the customer signs. The incumbents cannot
build the sovereign stack because it would negate the leverage that makes
them incumbents. That is the market gap: a genuine need, structurally
unmet by the parties best resourced to meet it. Gaps of that shape are
where new infrastructure comes from.

The 2020s sharpened the need into a threat model. **The supply chain is
the threat model** (`docs/manifesto.md` §IV): whoever controls the
silicon, firmware, OS, and toolchain controls the polity that runs on
them. That is no longer a hypothetical for anyone who watched a decade of
export controls, firmware backdoors, and sanctioned deplatforming. Two
regions have responded with the most concrete infrastructure work —
Latin America and East Asia — and they share more than a moment: they
share, this project argues, a cultural substrate that makes the response
coherent. That substrate is the subject of the name.

## What "sovereign" means concretely

"Sovereign" is a word that invites hand-waving, so pin it to five
verifiable properties. Wari is sovereign infrastructure only to the
degree it delivers all five:

- **Open ISA.** The instruction set is RISC-V, ratified in the open, with
  no license gate and no single national chokepoint on who may fabricate
  a core. The Phase-0 target — the StarFive VisionFive 2, a JH7110 with
  four U74 RV64GC cores — is a board a procurement office can buy without
  a US export license.
- **Open drivers.** The code that touches the hardware is not a signed
  binary blob from a vendor. Wari's drivers are WASM source built in this
  tree (`drivers/`), signed by a key the operator controls, and readable
  by anyone. There is no closed firmware layer between the kernel and the
  silicon that the operator cannot inspect.
- **An auditable TCB.** The trusted computing base — the native Rust
  kernel, Tier 0 — is deliberately held to roughly 5–10 KLOC (`CLAUDE.md`,
  Two-Tier model). That number is not an accident of scope; it is a hard
  design constraint, because "auditable" means *auditable by a small team
  in bounded time*, and thirty million lines is not that at any team size.
- **LATAM jurisdiction.** The physical chips sit in physical datacenters
  under the laws of the states that depend on them. "Land," in the
  collective's tagline *soberanía tecnológica, tierra y libertad*, is
  literal: computing infrastructure is territory
  (`docs/manifesto.md` §VI).
- **No US-controlled silicon in the trust path.** Confidential-compute
  features, when they arrive (Phase 3), come from RISC-V CoVE — an open,
  ratified extension — not from a proprietary enclave whose vendor can
  deprecate it. The sovereignty of the stack does not depend on trusting a
  chip vendor's roadmap.

None of the five is a slogan. Each is a property you can check against the
tree, and the rest of this book is largely the work of making each one
true down to the register write.

## The name

*Wari* is the Wari Empire — the Andean state that, from roughly 600 to
1000 CE, ran the highlands on an administrative-first model: roads,
agricultural terracing, and a storehouse network, its accounts kept on
*quipus*, knotted-string ledgers that recorded not merely inventory but
the standing balance of who owed what to whom across mountain valleys and
generations (`docs/manifesto.md` §I). Two facts about that empire are the
reason it lends its name to a kernel. First, it was **infrastructure that
outlasted its builders**: the roads and terraces and administrative logic
were inherited and extended by the Inca centuries later. The Wari built
for a time horizon longer than their own dynasty. Second, its power was
*informational* — a quipu is a data structure, and an empire coordinated
by knotted ledgers is an empire that understood administration as the
management of verifiable obligations. Wari-the-OS takes both: build for a
horizon longer than the current vendor cycle, and treat the system as a
ledger of explicit, checkable obligations rather than a web of ambient
trust.

The characters 和力 — *Hé Lì*, "harmonious force" — name the thesis behind
the empire's name. The claim (`docs/manifesto.md` §I–II) is that three
civilizations on opposite sides of the Pacific independently encoded one
operating principle for human networks:

- Andean **ayni** — sacred reciprocity; every gift creates an obligation,
  every discharged obligation renews the network. Ayni was not ethics; it
  was the infrastructure that fed an empire without coinage.
- Mesoamerican **tequio** — communal labor as structural duty; you build
  the road, the school, the water system by mandatory contribution, and
  skipping it does not incur a fine, it severs your standing.
- Chinese **和 hé** — structural harmony; order emerges from properly
  aligned reciprocal relationships, not from imposed force. "Two gears
  mesh harmoniously when their teeth match; not when they pretend to be
  the same gear."

The unifying statement is one sentence: **every node has I/O obligations
to the network, and the network's health is cosmic law.** What makes this
more than comparative anthropology is that it maps, clause for clause,
onto the architecture. Capability tokens are ayni — every relationship
between caller and callee *is* a capability, a quipu-knot encoding the
standing balance (Chapter 12 builds it). The two-tier sandbox is tequio —
a driver or app earns its place in the system not by living in the OS but
by being signed, manifested, and verified on every load; membership is
paid in audit. Explicit capability-gated IPC is hé — there is no shared
memory and no ambient broker, only labeled, typed, declared exchanges,
because order emerges from explicit relationships. We are, the manifesto
insists, not inventing this principle. We are recovering it, and
compiling it into 5–10 KLOC of Rust.

## Correctness, then security, then size

Every architecture is a filter: it makes some things easy and some things
impossible, and the ordering of its priorities decides which. Wari's
ordering is stated flatly and is not negotiable — *make it correct, make
it secure, make it small, in that order* (`CLAUDE.md`, Philosophy). The
parenthetical that follows is the whole performance strategy:
**performance comes from smallness.** Wari does not have a fourth priority
called speed. It has the belief that a kernel small enough to audit is
also small enough to be fast, and that a system which chases throughput
first will sacrifice exactly the correctness and security that are the
entire reason to build it.

The order is a strict tie-breaker, and the point is what it does under
conflict. When correctness and convenience collide, correctness wins and
convenience loses — every fallible operation returns a `Result` rather
than panicking (rule R5), every `unsafe` block cites a written invariant
(R1), every public API carries a contract as if a prover will check it
next release (R4). When security and performance collide, security wins:
the interpreter is slower than a JIT, and Wari shipped the interpreter
first (Chapter 21's net driver answers a ping at a hundred thousand polls
per second precisely because it walks bytecode rather than trusting
generated native code). When size and features collide, size wins — the
capability system is seL4's, deliberately condensed, and the two-tier
model exists so the auditable core stays small while functionality moves
outward into sandboxed WASM.

Read the ordering as an admissions test. A proposed feature that improves
throughput but enlarges the TCB, or adds a fast path that cannot be shown
correct, or requires an unauditable dependency, fails the filter no matter
how attractive its benchmark. This is unusual, and it is unusual on
purpose. Most infrastructure is built with the priorities inverted —
speed first, then features, then security as a compliance checkbox, then
correctness as whatever the tests happen to catch. Wari inverts the
inversion because of who is downstream.

## The stakes

Who is downstream is the reason the ordering is worth its cost. The people
who will depend on this kernel — governments, hospitals, banks, citizens —
do not get to opt out when it fails. A throughput regression on a social
network is a graph that dips. A correctness failure in the system holding
a nation's identity records, or a hospital's patients, or a bank's ledger,
is not a graph. The standard the project holds itself to is written into
its philosophy: code you would be proud to submit to the seL4 team for
review — *every line, every commit, every PR, every time*
(`CLAUDE.md`, Philosophy). That is not a flourish. It is the direct
consequence of building infrastructure for people who cannot audit it
themselves and must be able to trust that someone did.

## Who Wari is not for

An architecture defined by what it refuses is only honest if it names who
it is not for, because a system that claims to serve everyone serves its
priorities to no one. Wari is not for:

- **Throughput-maximizing hyperscalers.** If the goal is the last
  percentage point of packets-per-second across a fleet, a kernel that
  ranks correctness and auditability above raw speed is the wrong tool.
  Wari trades peak throughput for a TCB you can read, and that trade is
  wrong for a workload where the TCB is nobody's concern.
- **Pure cost plays.** Wari's per-TOPS or per-request cost is not the
  pitch, and it will sometimes lose the spreadsheet to an incumbent whose
  scale Wari cannot match. The value is sovereignty and auditability. A
  buyer for whom those are worth nothing is a buyer Wari cannot win, and
  should not try to.
- **Workloads requiring full Linux compatibility.** There is no ELF path
  in the customer ABI — none, ever (rule R7). A workload that must run an
  arbitrary Linux binary, unmodified, against the full POSIX and glibc
  surface is a workload for Linux. Wari offers a WASM boundary and, in
  Phase 2, a Docker-to-WASM compiler (`tools/oci2wasm/`) for the images
  that *can* cross it — but it will not retrofit itself into a Linux
  emulator to capture that market, because doing so would dissolve every
  property the first half of this chapter defined.

Each refusal is a design decision, and by this project's rules no
architectural decision goes unexplained. Chapter 3 makes the ELF refusal
concrete, with the density-and-cold-start evidence that turns "no Linux"
from a limitation into the load-bearing bet.

## Closing hook

Before explaining what Wari *is*, honesty demands explaining whose work it
is built on. Almost nothing here is novel in isolation: capabilities came
from seL4, the WASM-as-process-boundary bet from Fastly and Cloudflare,
the narrow-Rust-kernel discipline from Firecracker and Hubris, the
language-enforced-isolation endpoint from Singularity and RedLeaf. What is
Wari's own is the *combination*, and the combination is only defensible if
each borrowed piece is credited and each rejected alternative is refused
with a reason. Chapter 2 is that accounting — the shoulders we stand on,
and the ones we deliberately step off of.
