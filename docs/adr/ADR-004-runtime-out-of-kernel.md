# ADR-004 · Everything that is not Tier 0 runs in U-mode — the runtime is not part of the kernel

**Status**: accepted by the architect 2026-11-14; binding on the merge
of this document. Implementation is sequenced below, one step per PR,
and goes **ahead of** the dynamic module lifecycle and the MCP layer.

Numbering note: the research report this ADR comes from called it
"ADR-002". That number went to the network-layering ADR in the
meantime, and ADR-003 is reserved for the wasmi upgrade, so this is
ADR-004.

## Context

Wari's isolation story has three layers (`security-model.md`): the
WASM validator, the MMU, and later cryptography. The second one has
never existed.

1. **There is no U-mode.** `kernel/src/trap.S` removed the U-mode
   entry path when it was cherry-picked ("every trap originates in
   S-mode"). `satp` is written once, at boot, with one kernel address
   space (`kernel/src/mem/kvm.rs`). `wari-mem` defines `PteFlags::USER`
   and nobody uses it.
2. **The interpreter runs inside the kernel, for all three tiers.**
   `wasmi` 0.32.3 — 65 624 lines of upstream code with 81 `unsafe`
   sites, pinned since June 2024 — executes in S-mode in the kernel's
   address space, next to the page tables, the capability tables and
   every other tenant's linear memory. `kernel/src` itself is about
   10 000 lines. The trusted base the documents describe ("5–10 KLOC
   of Tier 0, no third-party code except wasmi") is in practice more
   than 80 KLOC, four fifths of it not ours.
3. **So one memory-safety bug in the interpreter is a total
   compromise**: the kernel and every tenant on the board, at once.
   The written mitigations — fuzzing the validator, counting wasmi in
   the TCB, a formal proof in Phase 4 — lower the probability of that
   bug. None of them changes its consequence.

What has changed since the report (2026-10-29), and what has not:

- Tier-1 instances are now fuel-metered per scheduling step and a
  tenant that never yields is faulted (`wari_sched::TIER1_FUEL_PER_STEP`).
  That is cooperative accounting inside the interpreter: it stops a
  `loop {}`, it does not stop a compromised interpreter. There is
  still no timer interrupt.
- A tenant waiting on the network blocks instead of spinning, and a
  tenant that dies gives its sockets back (INV-26).
- Signed modules are checked against a production key whose secret is
  outside the repository.
- Kernel-owned DMA addressing (ADR-002, part 1) is in place for
  VirtIO only. On the VF2 the GMAC driver still computes the physical
  addresses it hands to the DMA engine (INV-25, "GMAC: not yet").

None of these puts a hardware boundary between the runtime and the
kernel. That boundary is what this ADR decides.

## Options considered

**(A) Stay in S-mode and mitigate.** Fuzz, audit, eventually prove
wasmi. This is the current state and what `aot-target-abi.md` §3.1
recommended as the default for compiled code (A2). It requires the
interpreter to be correct forever. The one direct precedent, Nebulet
(WASM in ring 0, single address space), never left proof of concept
and was archived in 2023.

**(B) The runtime in U-mode, one address space per tenant and per
driver.** A runtime bug compromises the instance that triggered it.
Costs an `ecall` per host function that needs the kernel and a full
TLB flush per tenant switch on the JH7110, which reports no ASID
bits.

**(C) S-mode plus the AOT safety certificate only.** The certificate
shrinks what runs; it leaves the interpreter, which still exists,
with no boundary, and makes the checker a single point of failure.
Complementary to (B), not a substitute.

**(D) Hybrid: Tier 1 in U-mode, Tier 2 drivers stay in S-mode.**
Rejected outright: the network driver is the remotely reachable
surface — any host on the LAN reaches the TCP/IP parsers — so this
keeps the shortest attack chain open and pays most of (B)'s cost.

## Decision

Option **(B)**. Tier 0 is the only code that executes in S-mode.

1. **A Wari process is an Sv39 address space.** It holds a U-mode
   runtime image, the tenant's WASM module, its linear memory and its
   stack. Every Tier-1 tenant and every Tier-2 driver is such a
   process, each with its **own** wasmi instance. The kernel is mapped
   in every address space with U=0, so an `ecall` does not switch
   `satp`; only a change of tenant does.
2. **The runtime lives in its own crate, `wari-rt`**: wasmi (later,
   certified AOT code), the `ecall` shims for host functions, and a
   per-process allocator. It is untrusted by definition and does not
   share a module with Tier 0. **The kernel stops linking wasmi.**
3. **What crosses by `ecall`**: everything that touches kernel state
   — capabilities, IPC, `proc_exit`, console output, queue attachment,
   DMA. **What does not**: a driver's MMIO. The device window is
   mapped into the driver's address space (U=1, uncached) and the
   driver reads and writes it directly, emitting its own `fence`.
   Today each MMIO access is a host call into the kernel, so this is
   cheaper than what it replaces.
4. **A device window is mapped by capability, into exactly one
   address space**, and never includes DMA address registers or
   clock/reset controllers. Where such a register shares a page with
   one the driver needs, the kernel exposes it by `ecall` instead.
   Drivers never program physical DMA addresses; the kernel does
   (ADR-002, part 1).
5. **Interrupts stay in the kernel.** The PLIC is Tier 0; an IRQ
   becomes a signal on the driver's Notification capability and the
   scheduler resumes the driver process with `sret`. The idle loop's
   direct call into the network driver goes away.
6. **Preemption comes from a hardware timer** (Sstc if the JH7110 has
   it, otherwise the SBI timer call). A tenant is interrupted wherever
   it is, with no cooperation from the interpreter. Fuel stays, for
   accounting and quotas; it is no longer the mechanism that keeps the
   board alive.
7. **wasmi stays at 0.32.3 during the migration** and is upgraded to
   2.x afterwards, in U-mode, where a regression costs one process.
   One variable at a time.
8. **Compiled (AOT) code runs in U-mode by default.** The A2
   recommendation of `aot-target-abi.md` §3.1 is inverted; the
   S-mode shim it described is not built. The compiler emits the same
   bytes either way, as that document already anticipated.

### Sequence

Each step is one PR with host tests, QEMU, an adversarial test at any
new boundary, and a run on the VF2 — every one of them touches traps,
the MMU or a driver.

| # | Step | What lands | Gate |
|-|-|-|-|
| 1 | U-mode trap entry | U→S path in `trap.S` (`sscratch`, per-process kernel stack), `sret` to U, a U-mode exception ends the process instead of halting the kernel, a first `ecall`. A ten-instruction stub, no wasmi | Stub `ecall`s and returns on QEMU and VF2; a stub that reads a kernel address is terminated and the kernel carries on |
| 2 | Address spaces | Create and destroy per-process Sv39 spaces, `satp` switch and `sfence.vma`, page allocator with free in real use | Process A reading B's memory is terminated. **Measurement gate, below** |
| 3 | `wari-rt` | The U-mode runtime image; first Tier-1 tenant running there, `fd_write` and `proc_exit` by `ecall` | `Hello from Wari` from U-mode on QEMU and VF2; out-of-bounds and `memory.grow` abuse kill only that process |
| 4 | Scheduler and timer | The scheduler `sret`s to processes instead of calling exports; IPC rendezvous between U-mode processes; timer preemption | PING→PONG between two U-mode tenants; a `loop {}` tenant does not stall the board, with fuel disabled |
| 5 | UART driver in U-mode | The driver as a process with its window mapped; console output by IPC | Banner and `Hello` through the driver process on the VF2; a process without the capability cannot map the UART |
| 6 | Network driver in U-mode | Driver process, IRQ by Notification, MMIO in-process, kernel-owned DMA | `ping -c 80` at 0 % and the HTTP demo on the VF2; a driver write to a DMA address register falls outside its window |
| 7 | Close-out | `security-model.md` Layer 2 marked shipped, `prior-art.md`, `CLAUDE.md`, the invariants below, roadmap | Documents checked against the code |

The order is strict. Steps 1 and 2 need no wasmi, which is what makes
the gate below cheap. UART goes before the network because it has no
DMA and no real interrupt. **Step 6 has a precondition outside this
sequence**: kernel-owned DMA addressing for the GMAC (the half of
ADR-002 part 1 that is not done). Until it lands, the DMA address
registers cannot be kept out of the driver's window.

Until step 4 lands, the current S-mode path keeps working beside the
new one, so every step can be reverted on its own.

### Measurement gate after step 2

Measured on the VF2, before any work on step 3:

1. The real `ASIDLEN` of the JH7110 (write ones to `satp.ASID`, read
   back).
2. The cost of an `ecall` round trip with a full trap frame.
3. The cost of a tenant switch (`satp` write plus `sfence.vma`).
4. Whether Sstc exists; if not, the latency of the SBI timer call.

**If a tenant switch costs more than 20 µs the design is reviewed**
before wasmi is moved. Network throughput with the driver in U-mode
is measured at step 6 against today's figures.

**Result (VF2, build 193, `scripts/build.sh bench`, 4 MHz timebase):**

| Measurement | VF2 | QEMU `virt` (reference only) |
|-|-|-|
| `ASIDLEN` | **0** (`satp.ASID` reads back 0) | 16 |
| `ecall` round trip, full trap frame | 142 ns (10 000 iterations) | ~3 µs |
| Tenant switch, `csrw satp` + `sfence.vma zero, zero` | 35 ns (20 000 switches) | ~5.9 µs |
| Sstc | **absent** (OpenSBI: `Boot HART ISA Extensions : none`) | present |
| SBI `set_timer` | 307 ns (1 000 calls) | ~4 µs |

The gate passes: 35 ns against a 20 µs budget. The switch figure is a
lower bound — the loop's working set is a few pages, so it does not
include refilling the TLB for a real tenant; step 4 measures that
under load. Consequences: every address space uses ASID 0 and every
switch flushes the whole TLB, which is what step 2 already does, now
as the permanent design on this silicon rather than an interim one;
preemption (decision 6) goes through the SBI timer call.

### Invariants this introduces

Drafts; each is added to `docs/invariants.md` by the step that makes
it true, with its test. (The report numbered these from INV-26; that
number has since been taken by the socket-lifetime invariant.)

- **INV-27 · Nothing but Tier 0 executes in S-mode.** Every `sret`
  out of the kernel goes to U-mode; `sstatus.SPP` is forced to 0 on
  every exit. *(step 4)*
- **INV-28 · One address space per process.** No U=1 page is mapped
  in two spaces, except shared memory granted by an explicit `Frame`
  capability. *(step 2)*
- **INV-29 · A device's MMIO window is mapped in exactly one address
  space**, that of the process holding its capability, and never
  contains DMA address registers or clock/reset controllers.
  *(steps 5 and 6)* This is INV-20 enforced by hardware.
- **INV-30 · Every physical address a DMA engine sees was written by
  the kernel.** Absorbs the unenforced half of INV-25. *(step 6)*

INV-2 (trap frame exclusivity) gains the U→S case, with `sscratch`
holding the kernel stack. INV-12 (the bump-allocator arena is
boot-only) is revisited once the kernel no longer instantiates wasmi.

## Rationale

- **Change the consequence, not the probability.** Every mitigation
  on the table for option (A) makes a bug in 65 KLOC of someone
  else's interpreter less likely. Only a hardware boundary between
  the runtime and the kernel makes it survivable. Formally verifying
  an upstream project that ships a major version a year is not a plan.
- **This goes before dynamic module loading.** The next roadmap item
  lets a customer's module arrive over the wire. Opening that door
  while any module can take the kernel with it has the order wrong.
- **Drivers too, and the network driver above all.** It holds the
  only remotely reachable parser and the only device authority. In
  U-mode with a mapped window, "a driver cannot touch another
  device" stops being a check in a host function and becomes the
  absence of a page-table entry.
- **A separate crate for the runtime** because the point of the
  change is that this code is not trusted. Sharing a module with
  Tier 0 would blur exactly the line being drawn, and the kernel
  dropping the wasmi dependency is the visible proof it was drawn.
- **Measure before moving wasmi.** The two real unknowns are the cost
  of crossing the boundary and of switching tenants without ASID.
  Both can be measured with a stub after two small PRs; the large PR
  (the runtime image) waits for the numbers.
- **Preemption falls out.** With tenants in U-mode a timer interrupt
  reaches them whatever they are doing. Building preemption against
  the interpreter instead — resumable out-of-fuel, which wasmi 0.32.3
  does not support — is what made the wasmi upgrade look urgent. It
  no longer is.

## Consequences

- (+) A runtime or compiler bug compromises one instance.
- (+) The MMU is a real layer for all three tiers; `security-model.md`
  can say "shipped" and mean it.
- (+) The trusted base to verify is Tier 0 plus the certificate
  checker. The Phase 4 item "wasmi correctness proof" is dropped.
- (+) Timer preemption; INV-20 and the open half of INV-25 hold by
  hardware.
- (+) MMIO from a driver gets cheaper, not dearer.
- (−) An `ecall` and a trap frame per host function that needs the
  kernel. The request ring of `aot-target-abi.md` §3 remains the
  answer for host-call-dense workloads.
- (−) A full TLB flush per tenant switch if the JH7110 has no ASID.
  Irrelevant at 16 processes; a real question for the density target,
  which the multikernel direction (ADR-001, per-hart kernels) spreads
  across harts. Measured at the gate.
- (−) About 110 hours across seven PRs, each with a silicon run,
  before the dynamic lifecycle and the MCP layer resume. It is the
  largest change since the capability system and it touches the
  network path that took longest to get right — hence UART first and
  the network last.
- (−) The bet "Tier-2 WASM-only in ring 0" in `prior-art.md` is
  withdrawn. It becomes "signed WASM drivers in user mode with a
  capability-mapped MMIO window", which has precedent.
- The immutable-kernel endpoint (Phase 4, a ROM kernel on silicon
  without paging) stays an **option**, conditional on a verified
  checker. It stops being a reason to design without the MMU today.
- ADR-002 stands. Its stack-as-a-separate-module split composes with
  this: the stack becomes one more U-mode process. Its part 4 (fuel
  and preemption) is half done by fuel metering and finished here by
  the timer.

## Prior art

| Source | What it does | What we take |
|-|-|-|
| seL4 and the seL4 Device Driver Framework (Trustworthy Systems, UNSW) | All drivers in user mode; shared-memory queues recover the performance; an Ethernet driver under 600 lines | The model for Tier 2: IRQ as notification, MMIO by capability, performance through queues rather than privilege |
| Fastly Compute | Wasmtime in a user process, plus VeriWasm checking the generated code | Runtime in user space **and** verification of what it runs — our certificate on top of U-mode |
| Cloudflare Workers | V8 isolates for density, process isolation for sensitive tenants; V8 is treated as untrusted | A third-party runtime is hostile code, not kernel |
| Fuchsia DFv2, Genode | Drivers in user space over a microkernel | The industry converged: the driver does not live in the kernel |
| Nebulet (2018, archived 2023) | WASM in ring 0, one address space, "syscalls are function calls" | The model we are leaving, and what became of it |
| Singularity (MSR) | Software-isolated processes in one address space, trusted compiler | The same dilemma; our Phase 4 is its "with proof" version and must remain optional |

Hardware facts relied on, with their status: Sv39 U bit — available
and already encoded in `wari-mem`; ASID on the JH7110 — zero bits,
confirmed on the board at the gate; Sstc — absent, confirmed at the
gate, SBI timer it is; IOMMU in front of the GMAC
— none found, assumed absent; cache coherence between the CPU and the
GMAC DMA master — coherent on the JH7110, to be checked separately
for the Orange Pi R2S.
