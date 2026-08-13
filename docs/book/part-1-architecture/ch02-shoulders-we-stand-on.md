---
sidebar_position: 2
sidebar_label: "Ch 2: Shoulders We Stand On"
title: "Chapter 2 — The Shoulders We Stand On"
---

# Chapter 2 — The Shoulders We Stand On

Wari is not a research project inventing from scratch, and it says so in
its own rules: every architectural decision either inherits from a named
source with credit, deliberately rejects one with written justification,
or is labeled as an original bet with its risk stated — *no architectural
decision is unexplained* (`docs/prior-art.md`). That discipline is the
opposite of a bibliography. A bibliography lists what you read; this
chapter is an argument about what each piece of prior art *proved*, what
Wari took from the proof, and — just as load-bearing — where Wari looked
at a well-known solution and walked away on purpose. Read it as the case
that Wari's combination is defensible because every element of it is
either borrowed with attribution or refused with a reason.

## The commercial landscape

The clouds that already run planet-scale compute are the most valuable
prior art Wari has, because they are field tests. Their engineers have
already discovered, at enormous expense, which isolation primitives
survive contact with millions of tenants. Wari's job is to read the
results correctly.

**Cloudflare Workers (2017–)** is the density experiment. Kenton Varda's
"Cloud Computing Without Containers" (2018) makes the finding plainly:
process-per-tenant is too expensive for per-request isolation, so Workers
runs a *shared* V8 runtime with a separate heap per tenant and gets
isolation without the context-switch tax — millions of tenants on shared
infrastructure. Wari inherits exactly that finding and nothing else from
Cloudflare. The shared-runtime density model is *why* Wari runs its Tier-2
drivers with no MMU barrier between them and the kernel: density comes
from a shared runtime, not from a page table per tenant. But Wari rejects
V8 itself, and the rejection has three named reasons (`docs/prior-art.md`):
JavaScript is not Wari's primary language; V8 is on the order of fifty
megabytes of Google-controlled C++ that no one can audit at a
LATAM-sovereign-procurement standard; and its RISC-V support is young.
Customers who *want* to ship JavaScript still can — JS-to-WASM compilers
(Javy, AssemblyScript, Porffor) let them ship JS while Wari runs WASM.
Cloudflare's insight survives; Cloudflare's runtime does not.

**Fastly Compute@Edge (2019–)** is the proof that the boundary can be
WASM rather than a language VM. Fastly's move from its own Lucet compiler
to Wasmtime, under the Bytecode Alliance, established WASM as the process
boundary itself: a fresh instance per request, microsecond cold start when
a JIT is in play. Wari inherits the WASM-only user model directly. The
one place it diverges is conservatism about the runtime: Wari starts on
`wasmi`, a pure `no_std` interpreter with millisecond-class cold start,
and defers the JIT to a later phase — trading Fastly's peak speed for a
smaller, more auditable engine now, with the migration path held open.

**AWS Lambda + Firecracker (2018–)** is the discipline lesson. The
Firecracker paper (Agache et al., NSDI 2020, "Firecracker: Lightweight
Virtualization for Serverless Applications") shows a narrow-purpose Rust
VMM beating general-purpose hypervisors for serverless work precisely
because its scope is tight and therefore its attack surface is small.
Wari takes the narrow-Rust-kernel discipline and pushes it further:
Firecracker is roughly 50 KLOC because it still carries a virtual machine
monitor; Wari's Tier 0 aims at 5–10 KLOC because it carries no VMM at all.
And here Wari rejects the *mechanism* while keeping the discipline:
microVM-per-invocation is too heavy for the density target. Firecracker
counts tenants in the hundreds per host; Wari's target is 10,000–50,000
per board. You do not reach five orders of magnitude more density by
making the VM lighter. You reach it by not having a VM.

**AWS Nitro (2017–)** is the hardware-software co-design precedent:
offload network, storage, and security to purpose-built hardware and the
hypervisor's trusted base shrinks dramatically. Wari inherits co-design as
a *strategy* rather than a specific offload. Its analog is the GAPU FPGA
coprocessor of Phase 3, and — further out — the MMU-free custom silicon of
Phase 4. Nitro's lesson is that the cleanest way to shrink a TCB is
sometimes to move a job into hardware you designed; Wari plans to spend
that lesson on sovereignty rather than on margins.

**Google gVisor (2018–)** is the rejection that clarifies Wari's scope.
gVisor interposes on syscalls in userspace to shrink the kernel attack
surface a container can reach — a sophisticated answer to a real problem.
Wari rejects the interposition layer, and the reason is a statement of
what Wari *is*: gVisor exists to defend a legacy kernel it does not
control, and Wari controls the whole stack. There is no legacy kernel to
shim, and Tier 0 is small enough that it does not need a shim to be
defensible. A shim is debt you take on when you cannot change the thing
underneath; Wari can change everything underneath, so it does.

**Kata Containers (2017–)** is the rejection that names a trap. Kata wraps
each container in a lightweight VM to give Docker images real isolation.
The trap, for Wari, is OCI compatibility *as an architectural constraint*:
retrofitting strong isolation onto arbitrary Docker images defeats the
density advantage that is the whole point (`docs/prior-art.md`). Wari's
answer is to move compatibility off the critical path and into a build
step — `tools/oci2wasm/` (Phase 2): the customer brings a Docker image,
host tooling compiles it to WASM, and Wari runs the WASM. Compatibility
becomes something you pay for once, at build time, on the host — never a
constraint the kernel has to honor at runtime. Chapter 3 develops this
into the general shape of the WASM-only bet.

## The research inheritance

If the commercial clouds are field tests, the research operating systems
are existence proofs — each one demonstrated that a move Wari depends on
is *possible*, often decades before the ecosystem could use it.

**seL4 (Klein et al., SOSP 2009, "seL4: Formal Verification of an OS
Kernel")** is the deepest inheritance. It proved three things Wari builds
on: capability-based access control scales better than ambient authority;
synchronous rendezvous IPC needs no buffers and no kernel allocation and
is therefore verifiable; and formal verification of a ~10 KLOC kernel is
achievable with enough discipline. What "formally verified" actually
bought is the crucial part, because Wari does not get seL4's proof for
free. What it gets is the *shape*: by aligning Wari's capability
structures and IPC with seL4's, a future Phase-4 verification effort can
build on seL4's decades of proof engineering instead of starting from
zero. That is a concrete, cashable inheritance, and Chapter 12 shows it
paid — Wari's capability system is described in its own design docs as
"seL4 puro," condensed, with a table of every place it deliberately
simplifies and why.

**Singularity (Hunt & Larus, MSR-TR-2007-49, "Singularity: Rethinking the
Software Stack")** is the idea that was twenty years too early.
Singularity ran software-isolated processes in a single address space with
*no hardware page tables between them*, enforcing isolation with the type
system of a managed language (C#) instead of the MMU. It proved the
architectural move is sound. Wari inherits the endpoint, not the runtime:
Phase 4's MMU-free custom silicon is Singularity's dream with WASM and
`wasmi` as the 2026 enablers that C# and the CLR could not be — a smaller
runtime, cross-language, with a machine-verifiable type system. The reason
to study Singularity closely is the post-mortem: it did not become
mainstream for *business* reasons (Windows backward-compatibility, a heavy
C# runtime), not technical ones. Wari learns from the reason it failed to
ship, not from a failure in the idea.

**Tock OS (Levy et al., SOSP 2017, "The case for writing a kernel in
Rust")** is the production proof. Tock uses the Rust type system to
replace the MMU for process isolation in embedded systems, and it is
deployed in security-critical hardware — including Signal's secure
messaging devices. Wari inherits the confidence: if Rust's type system can
carry OS-scale isolation in shipped hardware, the Phase-4 MMU-free
direction is not wishful. Singularity argued the move was possible; Tock
shows it survives production.

**RedLeaf (Narayanan et al., 2020, "RedLeaf: Isolation and Communication
in a Safe OS Kernel")** is the closest academic sibling Wari has. RedLeaf
builds Rust-enforced "domains" with language-level isolation between
kernel components and zero-copy IPC carried by the type system. That is
Wari's two-tier model in a research kernel, and Wari's Phase-1 capability
design adopts RedLeaf's terminology directly (`docs/prior-art.md`). When a
reviewer asks whether language-enforced domain isolation is a fringe idea,
RedLeaf is the citation that says it is a published, peer-reviewed one.

Two more sit at the edges as adjacent inspirations rather than direct
ancestors. **MirageOS (Madhavapeddy et al., ASPLOS 2013, "Unikernels:
Library Operating Systems for the Cloud")** established the unikernel — an
OS compiled as a single-address-space binary, extreme specialization
buying extreme size reduction. Wari inherits the *direction* for Phase 4+:
a Wari module, a libc, and `wasmi` linked into one image for a
latency-critical workload is a MirageOS-flavored future, and the broader
unikernel movement it launched (Unikraft among the later entrants) is the
same specialization instinct at work. **Hubris (Oxide Computer, Cliff
Biffle, 2021–)** is the most visible production-Rust-kernel team of the
2020s: a static task set, no heap in the kernel, simple scheduling,
running on the Oxide rack's real hardware. Wari inherits Hubris's
"no heap in dispatch" rule directly — it is Wari's rule R2 — along with
the static-everywhere discipline that makes a kernel's resource footprint
provable by construction.

## The explicit rejections

Credit is only half the discipline. The other half is refusing popular
answers out loud, because a rejection with a written reason is an
architectural decision and an unexamined default is a liability waiting to
be discovered by an auditor. Wari refuses four things by name.

- **V8 / JavaScript as the runtime.** Fifty megabytes of
  Google-controlled C++ cannot be audited to a sovereign-procurement
  standard, JavaScript is not the primary language, and RISC-V support is
  immature. WASM is the boundary; JS reaches it through a compiler, not
  through a bundled engine.
- **OCI compatibility as a constraint.** Bending the kernel to run
  arbitrary Docker images unmodified would trade away the density that
  justifies the whole architecture. Compatibility is a host-side build
  step (`tools/oci2wasm/`), not a runtime obligation.
- **Syscall shimming.** The gVisor-style interposition layer defends a
  kernel you do not control. Wari controls the entire stack and keeps its
  TCB small enough that a defensive shim would be surface added, not
  surface removed.
- **Proprietary silicon isolation (the SGX lineage).** Intel SGX was
  proprietary enclave hardware that Intel itself later deprecated — a
  cautionary tale about betting a security model on closed silicon
  features a vendor can withdraw. Wari's confidential-compute path is
  RISC-V CoVE (the Confidential VM Extension, ratified 2024, silicon
  landing 2026–27), an open, ratified extension on an open ISA. It is the
  analog to Intel TDX and AMD SEV-SNP, chosen precisely because no single
  vendor owns the right to take it away.

## What is genuinely the bet

After all the credit is paid, four things are Wari's own, and honesty
requires flagging each as a bet with a risk rather than a settled result
(`docs/prior-art.md`). The **two-tier WASM model** — Tier 1 behind both an
MMU and the WASM sandbox, Tier 2 in the kernel's ring with the WASM
sandbox as its only barrier — is closer to Singularity than to any
commercial cloud, and it is the defensible moat *if it works*; its risk is
that it demands `wasmi` be extremely correct, because for Tier 2 there is
no page table underneath to catch a runtime bug. The **GAPU FPGA as an
architectural peer to the GPU** treats sovereign, inspectable inference
hardware as a first-class driver path rather than an afterthought. The
**LATAM-sovereign positioning** is a market bet, not a technical one: that
governments who cannot or will not audit x86-plus-Nvidia will pay for a
stack they can. And **formal verification from day one** — shaping the
code for proof now rather than retrofitting it — is the bet that seL4's
way is the right way and that most clouds are wrong to defer it
indefinitely. Naming these as bets is not hedging. It is the same
discipline as the citations: the borrowed parts are attributed, the
refused parts are justified, and the parts that are neither are marked as
wagers so no reader mistakes a hope for a proof.

## Closing hook

The single decision that runs through every inheritance and every
rejection in this chapter — the shared-runtime density from Cloudflare,
the WASM boundary from Fastly, the no-VMM discipline from Firecracker, the
OCI refusal from Kata, the language-enforced endpoint from Singularity and
RedLeaf — is the choice to make WASM the *only* way code enters the
system. There is no ELF path, and there never will be. Chapter 3 stops
surveying and starts arguing: given everything here, why is WASM-only the
right bet, what exactly does it buy in density and cold-start and
auditable surface, and what does it honestly cost?
