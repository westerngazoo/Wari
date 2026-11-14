# ADR-002 · Network layering — the NIC driver moves frames, the stack is a separate Tier-2 service, DMA addressing belongs to the kernel

**Status**: proposed 2026-10-30, pending the architect's sign-off (the
merge of this document). The decision is binding on merge; the
implementation is sequenced below and lands one step per PR.

## Context

`drivers/net/src/lib.rs` is 3 866 lines in one file and does five
jobs: the hardware (GMAC/VirtIO, PHY, DMA rings, RX recovery), the
TCP/IP stack (smoltcp 0.11), the socket table, the HTTP demo
(`socket_send_canned`, one accept per instance) and the diagnostics.
It is a `cdylib` `no_std` crate with its own `panic_handler`, so none
of it compiles as a host test.

Three facts about that module decide this ADR:

1. **It holds the only device authority in the system.** The net
   driver owns the `Net` capability. `wari::net_mmio_write32` checks
   that capability and that the *register* lies inside the GMAC
   window; it does not look at the *value*. The driver computes the
   physical addresses it hands to the DMA engine itself
   (`lin_mem_base()` + offset) and writes them into descriptors in
   its own linear memory and into the DMA list-address registers. The
   JH7110 GMAC sits behind no IOMMU. INV-25's address-containment
   half ("every physical address handed to a DMA engine lies inside
   the driver's own linear memory") is written down and enforced by
   nothing (`invariants.md` INV-25, `security-model.md` §DMA).
2. **It also holds the only remotely reachable parser.** smoltcp's
   Ethernet/ARP/IP/ICMP/TCP parsers run inside the same WASM instance.
   The WASM sandbox bounds what a compromised driver *loads and
   stores*; it does not bound what the driver tells a bus master to
   load and store. Today the chain "smoltcp bug → code execution in
   the driver sandbox → arbitrary DMA → kernel memory" has no gate in
   it. It is the shortest remote attack path in the system.
3. **Mixing the layers produces bugs that look like hardware.** Build
   165 lost 25–33 % of pings on the VF2 because the demo's accept
   loop fed smoltcp a second clock (a per-call counter) next to the
   kernel's rdtime clock, so ARP cache entries expired within one or
   two polls and the ARP rate limit stuck. Weeks of RX-wedge hunting
   before that were spent inside the same file. Nothing in the
   structure stopped a demo from reaching into the stack's timers.

Two further gaps sit next to these and are decided here because the
same sequence closes them:

- **No fuel or preemption on Tier-1 instances.** `Engine::default`
  runs without fuel (`events.rs`, `MAX_RING_WAITERS` note). A tenant
  in an infinite loop holds the hart; the kernel's idle loop, which is
  the only thing that polls the network outside a socket call, never
  runs while tenants are alive.
- **Tier-1 exposes 23 host functions** (2 WASI P1, 21 `wari::*`:
  capabilities, rings, sockets, IPC, events). `tests/security/` has 8
  adversarial files; there is no per-function negative test, and no
  fuzz target at the frame boundary.

## Options considered

**(a) Stack as a separate Tier-2 module, driver moves frames only.**
The NIC driver keeps the `Net` capability and does nothing but
hardware: rings, PHY, RX/TX of raw Ethernet frames. The stack
(smoltcp, sockets) is a second signed Tier-2 module with *no* device
capability at all. Frames cross between them over kernel-mediated
rings, using the SQ/CQ discipline the capability fast path already
has (`cap/ring_drain.rs`: every entry copied out of linear memory
before use, INV-β; generation re-checked, INV-γ). Tier-1 reaches the
stack through the kernel's socket host functions, as today.

**(b) Stack stays in the driver, split into files, demo removed.**
Same authority in one instance, better source layout. Fixes fact 3,
leaves facts 1 and 2 untouched.

**(c) Stack in the kernel (Tier 0).** Rejected a priori: Tier 0 takes
no third-party code except `wasmi` (CLAUDE.md, module structure), and
it would put the remotely reachable parser in the only place where a
bug is not contained by anything.

**DMA containment: kernel-owned addressing vs. PMP/IOPMP.** RISC-V PMP
governs hart accesses, not bus masters; the JH7110 has no IOPMP and
no IOMMU. Hardware containment is therefore not available on the
Phase-0 board. It stays on the table for the second silicon (R2S) as
defence in depth, never as the only line.

## Decision

Option **(a)**, with kernel-owned DMA addressing first. The decision
has five parts, in the order they are built; the order is part of the
decision because step 1 is the actual hole and does not depend on any
of the others.

1. **DMA addressing belongs to the kernel.** The kernel owns the
   descriptor rings and the DMA address registers. A driver hands the
   kernel *offsets into its own linear memory*; the kernel validates
   each one against `[lin_base, lin_base + len)` and the transfer
   length, translates it, and writes the descriptor and the register
   itself. `lin_mem_base` is withdrawn from the driver ABI and
   `net_mmio_write32` stops accepting the DMA address registers. This
   turns INV-25's address-containment half from a sentence into a
   check, and it holds even if the driver is compromised. Gate: an
   adversarial test where a driver submits an offset outside its
   memory and is refused, on the host, in QEMU and on the VF2.
2. **The driver is split by responsibility, then the stack leaves
   it.** First a behaviour-preserving split of the file into
   hardware, stack, sockets, demo and diagnostics, so each part has a
   one-sentence purpose and the stack and socket code compile as host
   tests. Then the stack and socket table move into their own Tier-2
   module (`drivers/netstack/` or similar; final name in the PR) with
   no `Net` capability, fed by kernel-mediated frame rings. A smoltcp
   fault then lands in a sandbox whose only authority is "send and
   receive frames through the kernel".
3. **One clock.** Time comes from the kernel's rdtime-derived
   `next_tick()` and from nowhere else; no module keeps a clock of
   its own, and the stack module's poll takes the kernel timestamp as
   its only argument. This is the contract build 166 restored and the
   rule the split must not re-break.
4. **Fuel and preemption for Tier-1.** Tier-1 instances run with
   wasmi fuel and are preempted on exhaustion; the scheduler regains
   the hart on a fixed budget, so an idle-loop network poll and other
   tenants always run. Tier-2 modules get a larger budget and a
   watchdog rather than preemption; the exact numbers are set in that
   step's PR.
5. **Adversarial coverage at the two boundaries.** A host-side fuzz
   target feeds frames to the stack module's ingress (smoltcp has its
   own `fuzz/` targets to start from), and every one of the 23 Tier-1
   host functions gets a negative test that proves a malformed call
   returns an errno and never panics. "Later" is not an option for
   these (CLAUDE.md, security test rule).

Two consequences of (a) that are decided here as well, because they
follow directly:

6. **The stack module is a daemon.** It is spawned through the module
   registry with a restart budget (`modreg` already has this for
   guard agents), so a trap in smoltcp is an outage of the network
   service, not of the board. On restart the driver re-attaches its
   rings; sockets are lost and Tier-1 sees errors on them.
7. **Tier-1 gets a generic socket ABI, and HTTP becomes a service.**
   `net_socket_send_canned` is retired in favour of
   accept/recv/send/close with a capability per socket and buffer
   validation against the caller's linear memory, with adversarial
   tests. A long-lived HTTP service built on `wari-http` replaces the
   one-shot demo in `apps/hello`.

## Rationale

- **It makes the two-tier promise true instead of restating it.**
  The architecture's claim is that a Tier-2 fault is contained by the
  WASM sandbox plus capabilities. For the net driver that claim is
  false today because the sandbox does not reach the bus master.
  Steps 1 and 2 are the two independent ways to make it true: step 1
  bounds what *any* driver can make the device do, step 2 removes the
  device authority from the code an attacker can reach. Either alone
  is a real improvement; together they give defence in depth without
  an IOMMU.
- **Containment of a parser-layer CVE becomes a property, not a
  hope.** smoltcp is Rust `no_std` compiled to WASM: the likely
  failure is a trap, not code execution. After step 2 even the worst
  case, code execution inside the stack module, yields frames and
  sockets, nothing else. After step 6 the likely case, a trap, yields
  a restart.
- **Host-testability falls out of the split.** The regression test
  for the two-clock bug could not be written because the driver does
  not compile on the host. The stack module is pure logic over byte
  buffers; it compiles on the host by construction, and the fuzz
  target in step 5 is the same harness.
- **The cost is a frame copy and a second module.** Frames cross one
  kernel-mediated ring between driver and stack. On a Gigabit NIC in
  a WASM interpreter the copy is not the bottleneck; the interpreter
  is. The AOT lane handles that, and the ring format is the same
  typed, derived-descriptor format ADR-001 requires for cross-hart
  channels, so the multikernel inherits it unchanged.

## Consequences

- INV-25's address-containment half becomes enforced (step 1); the
  invariant text is updated in the same PR and gains a test.
- A new invariant states that the stack module holds no device
  capability and that the only device authority reachable from the
  network is the frame rings (step 2); `docs/invariants.md` gets the
  entry and `security-model.md` §"smoltcp containment" is rewritten
  from "not contained" to the actual guarantee.
- `lin_mem_base`, `nic_attach_queue`'s buffer addresses and the
  DMA-register writes leave the Tier-2 ABI; `docs/wasi-surface.md`
  and the driver manifest change accordingly.
- `net_socket_send_canned` and the accept-window demo are removed
  (step 7). `apps/hello` keeps the IPC demo.
- Any PR that puts a clock, a timer or a socket table anywhere but
  the stack module is rejected in review by citing this ADR.
- The review checklist's "silicon run required" line applies to steps
  1, 2 and 6: they touch DMA and module lifecycle, and QEMU models
  neither the GMAC nor the U74 store buffer.

## Prior art

MINIX 3 (Herder et al., 2006) — network stack and drivers as separate,
restartable user-level servers; the restart budget in step 6 is its
reincarnation server in miniature. Xen grant tables — the guest hands
the hypervisor references, the hypervisor validates and installs the
mapping; step 1 is the same shape with offsets into linear memory.
seL4 / sDDF (Trustworthy Systems, 2022–) — device drivers that move
frames over shared rings to a separate protocol component, with the
driver holding the device and the stack holding nothing. Redox and
Fuchsia — `netstack` as a process distinct from the NIC driver.
wasmtime/wasmi fuel metering — step 4. smoltcp's own `fuzz/` crate —
the seed for step 5. Rejected: Linux-style in-kernel stack (option
c), and "modularize in place" (option b) because it leaves the
authority where the parser is.
