---
sidebar_position: 24
sidebar_label: "Ch 24: Confidential Compute"
title: "Chapter 24 — Confidential Compute"
---

# Chapter 24 — Confidential Compute

The security-model table has five rows. Two of them ship today. The
last three are promises, and the doc says so in plain language —
"planned," "planned," "planned, hardware-dependent"
(`docs/security-model.md:22`). This chapter is about the bottom promise:
Layer 3c, RISC-V CoVE, ciphertext RAM per tenant. It is the furthest a
memory-isolation story can be pushed — the point past which there is no
software mechanism left to add, only silicon — and it is not built. What
follows is the argument for why the architecture already leans toward it,
what CoVE would give a sovereign tenant that nothing above it can, and
the honest list of things that have to become true before the promise
turns into a shipped row.

## The threat that survives everything we shipped

Walk down the table and watch the adversaries fall away. The WASM
validator (Layer 1) stops a tenant from forging a pointer outside its
own linear memory. The Sv39 MMU (Layer 2) stops the raw dereference that
a validator escape would attempt. PMP (Layer 3a) adds a redundant region
check. Hardware crypto (Layer 3b) turns exfiltrated disk blocks into
ciphertext. Each layer removes a class of attacker, and the double
sandbox — validator plus MMU — is the genuinely-shipped core of the whole
isolation claim.

There is one adversary none of them removes: the operator.

Every layer above 3c defends the tenant against *other tenants* and
against *remote attackers*. None of them defends the tenant against the
person who runs the machine. A host with root can attach a debugger to
the kernel, dump physical RAM, and read a tenant's plaintext straight out
of its linear memory — because that memory is identity-mapped and
readable by the S-mode kernel by construction. A cold-boot attack against
unpowered DRAM does the same without any software at all. The MMU is a
wall between tenants; it is not a wall between a tenant and the operator,
because the operator *is* the thing the MMU serves.

For most cloud workloads this is fine — you trust your provider, or you
don't run there. For the workloads Wari exists for, it is the entire
problem. A hospital in Oaxaca putting patient records on shared
infrastructure, a finance ministry running citizen data on hardware it
does not physically hold, a bank whose regulator forbids plaintext from
leaving a jurisdiction — these tenants cannot answer "who can read my
memory?" with "we trust the operator not to." Sovereignty that reduces to
a promise of good behavior is not sovereignty. It is a contract, and
contracts are read by the same courts a sovereign tenant is trying to
route around.

## What CoVE is, and the family it belongs to

The RISC-V Confidential VM Extension (CoVE) was ratified in 2024
(`docs/prior-art.md:180`). Its core move is the one the confidential-
computing family has converged on: the memory controller encrypts a
confidential domain's pages under a key held in hardware that privileged
software — the hypervisor, and in Wari's framing the S-mode kernel —
cannot read. Inside the domain, execution is ordinary. From the outside,
a memory dump yields ciphertext. The kernel can still schedule the
domain, still route its I/O, still account its fuel; what it can no
longer do is *look inside*.

Wari does not invent this. CoVE is the open-ISA member of a well-populated
lineage, and the honest way to describe Wari's bet is "the auditable one,"
not "the first one":

- **AMD SEV / SEV-SNP** and **Intel TDX** — the x86 confidential-VM
  extensions CoVE is a deliberate analog of (`docs/prior-art.md:185`).
  They are the proof the mechanism works at production scale, and — TDX's
  first-generation implementation bugs in particular — the proof that a
  new confidential-computing silicon generation ships with sharp edges.
- **AWS Nitro Enclaves** (2017–, `docs/prior-art.md:57`) — the commercial
  demonstration that operators will sell "we cannot see your memory" as a
  product, and that customers with real sovereignty constraints will pay
  for it.
- **Intel SGX** — the cautionary tale. Wari's prior-art file rejects the
  SGX lineage outright: proprietary silicon isolation, a long CVE history,
  and ultimately deprecated by Intel itself (`docs/prior-art.md:188`).
  Betting sovereign infrastructure on a closed, vendor-controlled
  enclave feature is exactly the dependency the whole project is built to
  avoid. CoVE is the answer because it is *open* — auditable by the same
  governments that cannot audit x86.

## Layer 2, taken to its conclusion

The cleanest way to see where CoVE fits is to read it as the hardware
layer's terminus. Layer 2 keeps a tenant out of another tenant's memory
*as addressable by software*: the page tables simply do not map the
neighbor's pages into your address space. CoVE keeps a tenant out of
another tenant's memory *as readable at all* — including by the kernel
that owns every page table on the board. The MMU answers "can this
instruction reach that address?" CoVE answers "if it reaches the address,
is there anything there worth reading?" and makes the answer no for
everyone outside the domain, operator included.

Two honesties keep this from becoming a slogan.

First, CoVE is a confidentiality mechanism, not an integrity cure for the
kernel's own soundness. The security model's load-bearing caveat still
holds: `wasmi` runs in S-mode inside the kernel address space, and a
host-side soundness bug in the interpreter corrupts kernel state directly
(`docs/security-model.md:30`). CoVE encrypts a tenant's RAM against an
*outside* observer; it does not make the kernel that schedules the tenant
correct. If wasmi is subverted from within a tenant, the attacker is
already executing as the thing CoVE trusts. CoVE narrows the operator
threat and the physical-access threat. It does not narrow the wasmi-in-
TCB threat — that one is answered, if at all, by Chapter 26's proofs, not
by this chapter's silicon.

Second, the confidentiality guarantee is only as good as the silicon's
side-channel resistance. The table names this in its "broken by" column:
"Hardware side-channel; CoVE-silicon availability and correctness"
(`docs/security-model.md:22`). Ciphertext RAM does nothing against a
timing oracle or a cache-contention leak between tenants sharing a hart —
a leak Wari already carries as the price of shared-runtime density. CoVE
raises the floor on *bulk* exfiltration (dumps, cold-boot, operator
introspection). It does not raise the floor on *incremental* leakage
through microarchitecture. An honest sovereign-cloud pitch says both.

## Why the architecture already bends this way

The reason CoVE reads as a *row you add* rather than a *rebuild you
undertake* is that Wari already thinks in the vocabulary CoVE speaks:
attested units and capability-named identity.

The confidential-computing family runs on attestation — a domain proves,
cryptographically, that it is running the code it claims to be running
before anyone trusts it with a secret. Wari has been minting attested
units since Phase 1: every Tier-2 driver is signed and measured before
load, and the WASI-NN design pushes the same idea up to *models as
attested capabilities* (Chapter 25). The Phase-3 WASI surface already
reserves the two host functions the attestation flow needs —
`wari_cove_attest()` to produce a report and `wari_cove_seal(data)` to
bind a secret to a tenant's confidential context
(`docs/wasi-surface.md:74`). The capability system already keys every
object to a tenant identity, and the cap-system design already reserves
a slot for "confidential caps (CoVE-encrypted)" at Phase 4
(`docs/cap-system-design.md:41`). None of this is CoVE. All of it is the
grammar CoVE plugs into.

That is the payoff of a discipline that looked pedantic in Phase 0. When
every trust boundary is already an attested, capability-gated crossing,
adding a hardware confidentiality domain is a matter of binding an
existing tenant identity to a new hardware key — not of inventing a
tenant model from scratch.

## What has to be true for it to land

This is a promise, and the promise has preconditions. Read them before
forming an isolation claim.

**The silicon has to exist.** CoVE was ratified in 2024, but ratification
is a specification, not a shipment. Production RISC-V silicon
implementing CoVE is roadmapped from JH7110-class vendors for 2026–27 and
is not broadly available as this is written. Wari's own target board, the
VisionFive 2, does not have it. Phase 3's confidentiality story therefore
depends on hardware that has not yet manifested at the volume — or the
price — a sovereign deployment needs. If CoVE silicon slips, or arrives
with first-generation bugs the way TDX did, Phase 3's Layer 3c slips with
it. This is why the table's status column reads "planned,
**hardware-dependent**" and not merely "planned."

**The tenant model has to map onto CoVE's domain model.** CoVE isolates
confidential *VMs*. Wari's tenants are WASM instances — tens of thousands
of them, sharing one S-mode address space, which is the density bet the
whole architecture is built on (`docs/prior-art.md:22`). A confidential
domain per VM and a confidential domain per WASM instance are not the same
granularity, and reconciling CoVE's coarse, VM-shaped confidentiality
with Wari's fine-grained, many-tenants-per-address-space model is
unsolved design work, not a configuration flag. It may mean grouping
tenants into confidential pools; it may mean a hybrid where only
sovereignty-critical tenants get their own domain and pay the density
cost. That trade is a Phase-3 design question with no answer on disk yet.

**The mechanism has to earn the same audit the rest of the stack gets.**
A confidential-compute claim is a large security claim, and Wari's whole
posture is that large security claims get external review. The roadmap
puts a CoVE attestation-chain verification inside the Phase-3 gate
(`docs/testing.md:125`), behind an external security firm. Until that
audit exists and passes, "ciphertext RAM per tenant" is an architectural
intention, not a shipped guarantee — and this book will keep saying so.

## The sovereign stake, restated at the hardware line

Strip away the mechanism and the point is a single change in who has to
be trusted. Every isolation layer above CoVE still asks a sovereign
tenant to trust the operator not to look. CoVE is the first layer that
removes the operator from the trusted set — not by promising restraint
but by foreclosing capability. A government can run citizen records on
infrastructure it does not own, operated by a party it does not fully
trust, possibly under a foreign flag, and the operator's root, the
operator's memory dump, the operator's cold-boot attack against the DRAM
all yield ciphertext.

That is the concrete meaning of "sovereign" once you follow it to the
hardware: not *we promise not to look*, but *we cannot look*. It is the
strongest form of the guarantee the project is named for, and — read the
preconditions again — it is the one furthest from being real. Naming it
honestly, as the endpoint the architecture bends toward rather than a
feature it has, is the only way to earn the word.

## Closing hook

Ciphertext RAM hides a tenant's memory from everyone outside its domain,
the operator included. But the workload that most makes a sovereign
tenant want that opacity — running its own models, on its own data, on
hardware in its own jurisdiction — needs more than a place to hide. It
needs compute the four U74 cores on a VisionFive 2 cannot give it, and it
wants that compute to live behind the same attestation-and-capability
discipline as everything else. The next chapter is the accelerator that
would provide it: the GAPU, and why an FPGA over PCIe is the natural place
for the math to go once the WASM is only orchestrating it.
