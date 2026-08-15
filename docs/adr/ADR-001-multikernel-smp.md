# ADR-001 · SMP means multikernel — Wari will never be a shared-memory SMP kernel

**Status**: accepted 2026-08-15 (architect decision D4, recorded in
`../gapu-fit-review.md` §6). Direction is binding now; implementation
is deliberately deferred until after the Phase-2b AI capability layer.

## Context

Wari is a single-hart kernel by construction: INV-1 ("only one hart
executes kernel code at a time") is the soundness argument for every
`static mut` in Tier 0 — the scheduler table, the capability pools,
the bump allocator's cursor, the driver singletons. The 2026-08 memory
audit enumerated them all and found no lock, atomic, or per-hart
structure anywhere; INV-1 alone carries the kernel.

Meanwhile the hardware underneath us is multi-core: the VF2 has four
U74s (three parked via SBI HSM), and the Orange Pi R2S — the planned
second board — has eight. The tenant-density goal (10⁴–10⁵ instances
per board) will eventually want them. The GAPU v2.0 document names
"microkernel/multikernel" as the intended shape.

Two roads lead out of single-hart:

1. **Shared-memory SMP**: one kernel image, all harts inside it,
   every `static mut` converted to locks or per-CPU structures.
2. **Multikernel**: one kernel instance *per hart*, each
   single-threaded, no shared mutable kernel state; harts cooperate
   exclusively by message passing over explicitly shared, typed
   channels (Barrelfish, Baumann et al., SOSP '09).

## Decision

**Wari adopts the multikernel model as its only SMP path.** No shared-
memory SMP variant will be built, prototyped, or "temporarily"
tolerated. Until the multikernel lands, Wari remains single-hart and
INV-1 stands unmodified.

## Rationale

1. **It preserves the soundness story instead of replacing it.**
   Under a multikernel, INV-1 survives *per kernel instance* — every
   existing `static mut`, every SAFETY comment, and every audit
   conclusion remains valid within its instance. Shared-memory SMP
   invalidates all of them at once and replaces the argument with
   fine-grained lock discipline, which is precisely the argument that
   is hardest to formally verify: seL4's SMP verification remains
   partial after years of effort, while its unicore proof has stood
   since 2009. Wari's Phase-4 goal is a verified Tier 0; the
   multikernel is the only road where that goal survives SMP.

2. **The hard part is something Wari already has.** A multikernel's
   load-bearing component is its inter-kernel message-passing layer —
   Barrelfish had to build one from scratch. Wari's core primitive
   *is* seL4-style rendezvous IPC, proven cross-tenant on silicon.
   Extending IPC across harts (shared-memory rings + SBI IPIs) is an
   extension of the strongest subsystem, not a new discipline.

3. **Scheduling fits the existing model.** Tenants are already
   whole-WASM-instance units with per-instance CSpaces. Placing an
   instance on a hart at spawn time (no migration in v1) partitions
   cleanly; nothing in the current scheduler assumes global state
   beyond what each instance-owning kernel would hold.

4. **It matches the project's own thesis.** The manifesto's claim is
   that every node has I/O obligations to the network. A multikernel
   applies that claim to the harts themselves.

## Consequences

- **Binding now, before any code exists**: no kernel change may
  introduce a dependency on shared mutable state between harts. Any
  "we'll lock it later" design is rejected in review by citing this
  ADR. This sentence is the reason the ADR is written years before
  the implementation: the commitment is cheap today and prohibitively
  expensive to retrofit.
- Cross-hart communication will be typed message passing over
  explicitly shared rings — the INV-24 derived-descriptor discipline
  applies to those ring formats.
- Capability transfer between kernel instances needs its own protocol
  and invariants (seL4 handles this awkwardly; we get to design it
  deliberately). Expect an INV catalog section when implementation
  starts.
- Devices remain owned by exactly one kernel instance (the one whose
  hart takes the IRQ); other instances reach them by IPC, exactly as
  Tier-1 tenants reach drivers today.
- The R2S's eight harts become the natural first target; the VF2's
  four (one S7 excluded) are the development vehicle.

## Prior art

Barrelfish (ETH/MSR, SOSP '09) — the multikernel model itself.
seL4 (unicore proof vs partial SMP verification) — the verification
asymmetry this decision is built on. Hubris (Oxide) — evidence that
committed single-core-per-image kernels ship in production. Rejected:
Linux-style fine-grained-locking SMP (verification-hostile, and
retrofitting it would invalidate every INV-1 citation at once).
