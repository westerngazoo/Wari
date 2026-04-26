# Wari — Net Driver Design (Phase 1b PR 0)

> **Status**: Design draft v1. **No code yet.** This document is the
> contract that the net-driver implementation PRs will follow.
> Same shape as `docs/cap-system-design.md`: read once, sign off,
> then we write Rust + WASM.
>
> **Authors**: Gustavo Delgadillo + Wari project. Best-engineering-
> first mandate (no timeline pressure).
>
> **Audience**: Wari maintainer (Gustavo), future external auditor
> reviewing the Tier-2 driver TCB, formal-verification reviewer
> (Phase 4+ Verus track).

---

## 1 · Goals and non-goals

### Goals (what this design must achieve)

1. **Add network I/O to Wari** as a signed Tier-2 driver, in the
   same shape as the Phase 0/1a UART driver. The net driver is
   WASM, signed, capability-gated, and runs in the WASM-only
   sandbox.
2. **Two hardware targets**: VirtIO-net (QEMU virt) and JH7110
   GMAC (VisionFive 2 silicon). Same Tier-2 driver source tree
   produces two signed blobs via cargo features, mirroring the
   per-platform UART driver pattern.
3. **TCP/IP stack stays out of the kernel.** Per the Wari thesis
   (small auditable TCB), the TCP/IP stack lives **in the Tier-2
   driver**, not Tier 0. The kernel never gains TCP/IP code.
4. **Tier-1 apps reach sockets via cap-mediated IPC** to the net
   driver, using the cap system from Phase 1b PR 1-3b. New cap
   kind `ObjectKind::Net` plus per-socket Endpoint caps gate every
   send/recv.
5. **Phase-1b demo workload**: a Tier-1 app that listens on a TCP
   port and echoes received bytes back. Visible from a host on
   the same QEMU network or the same LAN as the VF2.
6. **Preserve auditability**: every line of code in the net path
   (Tier-0 host fns + Tier-2 driver + smoltcp port) must be
   inspectable in a single audit pass. The Tier-0 surface stays
   small (~200 LOC of new host fns); the Tier-2 driver and
   smoltcp port are signed and reviewed as a unit.

### Non-goals (deferred to later phases)

| Item | Why deferred |
|---|---|
| TLS / HTTPS termination | Phase 2 (depends on hardware crypto Zkn/Zks) |
| Multi-NIC bonding / failover | Phase 2 (no production workload yet) |
| Kernel-bypass DPDK-style fast path | Phase 3 (Wari thesis is auditability over throughput) |
| IPv6 | Phase 2 (smoltcp supports it; Phase 1b just enables IPv4 to keep the demo simple) |
| DHCP client | Phase 1c (use static IP for the Phase-1b demo) |
| BGP / OSPF / dynamic routing | Out of scope forever (this is an OS, not a router) |
| Userspace network namespaces | Phase 2 (Phase 1b's tenant-per-CSpace already isolates apps, but no namespace-style network virtualization) |
| Persistent socket state across reboot | Phase 4 (immutable kernel) |
| eBPF-style packet filtering | Phase 2+ if a workload needs it |

This list is the explicit **scope fence**. Anything not on the
goals list and not in the host-fn / cap-kind catalog (§5, §6) is
out of scope and gets rejected at review with reference to this
section.

---

## 2 · Background and motivation

### Why a net driver now

Phase 1b's cap-system sprint (PR 1-3b + scheduler) gave Wari a
real multi-tenant kernel: two Tier-1 instances run with isolated
CSpaces, the cap layer is load-bearing on the existing path, and
sequential scheduling works. The next bottleneck for "demonstrate
sovereign LATAM cloud" is **network I/O**: a workload that does
nothing but UART output is interesting as architecture but
unconvincing to a CTO who needs to see HTTP requests served.

Per the EPAM-Garage trajectory discussed earlier in this project
(`docs/pitch/jaime-v0.md`): the demo target is a cluster of 2-3
VF2 boards serving a REST API workload from a Tier-1 WASM
container, surviving a chaos test where one node loses power. To
get there:

1. **A net driver** (this design) — the prerequisite.
2. **A Tier-1 HTTP service in WASM** — the workload, built on top
   of the net driver's socket host fns.
3. **A simple cluster front-end** (haproxy on the demo laptop, no
   Wari-side load balancer for Phase 1b) — the visible scale.
4. **k6 stress test + chaos** — the proof.

The net driver is the foundation for everything between Phase 1b
and the EPAM Garage demo.

### Why TCP/IP in Tier-2 (not Tier-0, not Tier-1)

Three places the TCP/IP stack could live:

- **Tier-0 (kernel)**: smoltcp compiled into the Wari kernel.
  - **Pros**: simplest integration; every Tier-1 just calls
    `wari::socket_send` and the kernel handles everything.
  - **Cons**: smoltcp is ~30 KLOC of Rust. Adding it to Tier 0
    pushes the kernel from ~8 KLOC to ~40 KLOC overnight,
    destroying the audit-in-a-week thesis. **Rejected.**

- **Tier-2 (driver)**: smoltcp inside the net driver WASM blob,
  signed, cap-gated.
  - **Pros**: kernel TCB stays small; smoltcp is reviewed as
    part of the driver signing process; multiple Tier-1 tenants
    share the driver's stack via IPC; the cap system gates every
    socket op.
  - **Cons**: every socket op crosses a tier boundary (Tier-1 →
    cap-IPC → Tier-2). Phase-1b's interpreted wasmi pays
    interpreter overhead twice per syscall, plus the cap check.
    Acceptable for the EPAM demo workload (DB-bound APIs); not
    for line-rate forwarding.
  - **Verdict**: ✓ **picked**.

- **Tier-1 (per-app library)**: each Tier-1 app links its own
  copy of smoltcp; the kernel/driver delivers raw Ethernet frames
  via cap-gated IPC.
  - **Pros**: tightest isolation (every app's TCP state is its
    own).
  - **Cons**: every app pays the smoltcp cost (binary size,
    review surface, version skew); ARP responses get duplicated;
    port assignment becomes a multi-app coordination problem.
    **Rejected** as poor ergonomics for Phase 1b.

The Tier-2 choice keeps Wari consistent with the UART pattern:
"hardware access is the driver's job; tenants talk to the driver
via cap-mediated IPC." Networking just adds a non-trivial
protocol stack to that driver.

### Why smoltcp and not a custom stack

`smoltcp` (https://github.com/smoltcp-rs/smoltcp) is a no_std,
allocation-free TCP/IP stack written in Rust. ~30 KLOC. Used in
production by RustyHermit, Tock, embedded-net stacks for STM32
family, Redox-OS.

Alternatives considered:

- **Hand-rolled minimal stack** (ARP + ICMP + UDP + TCP, ~3-5
  KLOC): smaller TCB, but TCP is famously hard to get right —
  every TCP corner case in RFC 793 / 7414 / 9293 has been a
  source of CVEs in mature stacks. Writing TCP correctly from
  scratch in 5 KLOC is a research project, not a Phase-1b PR.
- **No TCP, just UDP**: viable for telemetry but the EPAM
  demo workload (HTTP REST API) needs TCP. Hard veto.
- **lwIP** (C, well-known embedded TCP/IP): C in our TCB,
  defeats the Rust-everywhere thesis; same audit-burden
  problem as smoltcp without smoltcp's safety properties.

smoltcp wins on: (a) Rust no_std, (b) production-tested, (c) zero
heap allocations (uses caller-provided buffers), (d) actively
maintained, (e) WASM-portable. The 30 KLOC cost is paid in the
Tier-2 driver, not in the kernel TCB — that's the right place.

---

## 3 · Conceptual model

A reader who has never seen a Wari net driver should be able to
build a correct mental model from this section alone.

### 3.1 Network interface card (NIC)

A **NIC** is a piece of physical hardware that moves Ethernet
frames between the host and a wire. Wari Phase-1b targets two:

- **VirtIO-net** (QEMU): a paravirtualized NIC. The "wire" is
  whatever QEMU's `-netdev` setting connects to (slirp, tap, …).
  Register set is the VirtIO PCI/MMIO transport plus the net-
  device-class queues.
- **JH7110 GMAC** (VisionFive 2): a real Ethernet MAC + DMA
  block. Synopsys DesignWare IP, same as the `stmmac` Linux
  driver targets. Two ports on the VF2 board (`eth0` / `eth1` on
  the running silicon — Gustavo's PCB shows MAC0 6c:cf:39:00:40:84
  and MAC1 6c:cf:39:00:40:85).

Both expose a similar abstract interface to the driver:
**descriptor rings + IRQ on completion.** The Tier-2 driver
implements both behind a `Nic` trait so smoltcp's `phy::Device`
implementation is platform-agnostic.

### 3.2 Tier-2 net driver

A signed WASM module loaded by the kernel at boot, the same way
the UART driver is. Its responsibility:

1. **Drive the NIC hardware** through `wari::net_mmio_*` host fns
   (cap-gated like UART).
2. **Run smoltcp** internally to handle ARP / IP / ICMP / TCP /
   UDP.
3. **Expose a socket API to Tier-1 tenants** via the
   `wari::net_socket_*` host-fn family + cap-mediated IPC on
   per-socket Endpoint caps.
4. **Handle IRQs** delivered by the kernel as Notifications on a
   driver-held Notification cap.

The driver is a **library** in scheduler terms (per
`sched::ProcessState::Library`) — never picked for `run`, only
called into via host fns. The driver's `_start` initializes the
NIC and smoltcp, installs the socket API, then exits the
initialization phase. After that, every socket operation re-
enters the driver via host fn dispatch.

### 3.3 Socket

A **socket** is a TCP/UDP endpoint owned by a Tier-1 tenant. In
Wari Phase 1b, a socket has:

- A protocol (`Tcp` or `Udp`)
- A local 5-tuple (or 4-tuple for unbound listeners): proto,
  local IP, local port, peer IP, peer port
- A pair of buffers (rx + tx ring) inside the driver's memory
- A **socket cap** held by the owning tenant — an Endpoint cap
  badged with the socket id, granting the tenant the right to
  send/recv on that specific socket

Socket caps are minted by the driver in response to
`wari::net_socket_create` calls and granted to the calling Tier-1
tenant via the cap-IPC path (PR 4 endpoint_send/recv). Revoking
the socket cap (e.g., on Tier-1 exit) tears down the socket and
returns its ring buffers to the driver's pool.

### 3.4 Cap kinds: `Net` and `Socket`

Phase 1b's cap system has 4 object kinds: Endpoint, Notification,
Untyped, Frame. The net driver introduces **two new kinds**:

| Kind | Object | Phase-1b role |
|---|---|---|
| `Net` | A NIC handle | Held by the Tier-2 net driver (root cap, minted at boot from the driver's signed manifest); grants permission to use a specific NIC |
| `Socket` | A TCP/UDP socket | Minted from a `Net` cap (READ rights → recv-only socket; WRITE rights → send-only; READ+WRITE → bidirectional); held by the Tier-1 tenant that opened the socket |

This extends `ObjectKind` from 4 variants to 6. INV-19 (Tier-Shape
Compatibility — currently reserved) becomes load-bearing: a
Tier-1 process cannot mint a `Net` cap (only the driver's
`init_root_caps` path produces them); Tier-1 tenants can hold
`Socket` caps but not `Net` caps.

### 3.5 IRQ delivery

Net is async: the NIC raises an IRQ when packets arrive in the rx
ring or tx descriptors complete. Wari's existing kernel handles
**only timer interrupts** (Phase 0 trap dispatcher). Phase 1b net
requires:

- **PLIC driver** (Platform-Level Interrupt Controller, RV64
  standard): claim/complete cycle for external IRQs
- **IRQ → Notification routing**: when IRQ N fires, the kernel
  signals the Notification cap that has been bound to IRQ N at
  boot
- **Driver wait-for-IRQ host fn**: the net driver calls
  `wari::notification_wait(slot)` and blocks until the kernel
  signals the bound Notification

The PLIC driver is **a prerequisite** to the net driver and lives
in its own PR (see §10).

---

## 4 · Per-platform NIC differences

The same Tier-2 driver source tree produces two signed blobs:

```
drivers/net/
├── Cargo.toml           features = ["qemu", "vf2"]
├── build.rs             pulls platform-specific MMIO bases
└── src/
    ├── lib.rs           main driver entry
    ├── nic.rs           Nic trait
    ├── virtio_net.rs    cfg(qemu) impl
    ├── gmac.rs          cfg(vf2) impl
    └── smoltcp_glue.rs  smoltcp::phy::Device adapter
```

### 4.1 QEMU VirtIO-net

- **MMIO base**: `0x10008000` on QEMU virt machine
- **Discovery**: VirtIO MMIO transport (`virtio-mmio` register
  layout); the QEMU command line declares
  `-device virtio-net-device,netdev=...`
- **Queues**: receive (queue 0) + transmit (queue 1), each a
  classic VirtIO virtqueue (descriptor ring + available ring +
  used ring)
- **IRQ**: PLIC-routed interrupt source 8 on QEMU virt
- **MAC address**: assigned by QEMU at boot, read from the
  device's `config` space
- **Implementation effort**: ~600-1000 LOC of WASM-targeted
  Rust; well-documented protocol; `virtio-drivers` crate exists
  in no_std Rust as reference (do NOT pull as dep — vendor a
  minimal subset for review)

### 4.2 JH7110 GMAC (VisionFive 2)

- **MMIO base**: `0x16030000` (eth0) / `0x16040000` (eth1) per
  the VF2 EEPROM dump and `ip a` output
- **Discovery**: the kernel knows the MAC is at the fixed MMIO
  base from the device tree (or hardcoded in `build.rs` per the
  UART precedent)
- **Queues**: a pair of DMA rings (rx + tx) of 8-byte descriptors
  pointing at packet buffers in the driver's WASM linear memory.
  DMA-coherent buffers needed (cache flush ops)
- **IRQ**: PLIC source TBD (need to read the JH7110 TRM); the
  number is fixed per platform and lives in `drivers/net/src/gmac.rs`
- **MAC address**: read from EEPROM at boot (per the existing VF2
  bringup output: MAC0 = 6c:cf:39:00:40:84, MAC1 = 6c:cf:39:00:40:85)
- **PHY layer**: GMAC needs MDIO bus initialization to bring up
  the PHY (auto-negotiation, link state). Synopsys ref-driver
  pattern; ~200-400 LOC
- **Implementation effort**: ~1500-2500 LOC of WASM-targeted
  Rust; substantially more complex than VirtIO; benefit from
  reading `stmmac` in Linux as a sketch (don't copy code — GPL
  conflict with our AGPL dual-license posture)

### 4.3 Phase-1b scope decision: VirtIO first

Per Simplicity First, **Phase 1b ships VirtIO-net only** (QEMU
target). The `vf2` feature on `drivers/net/` produces a
**stub blob** that fails to init with `KernelError::DriverError`
on real silicon — the kernel boots, but the net driver does not.
The Phase-1b QEMU demo runs to completion; the VF2 silicon demo
gets net in **Phase-1c PR** (a follow-up sprint after the EPAM
Garage demo lands on QEMU).

This honest scoping prevents a Phase-1b sprint from sprawling into
both VirtIO + GMAC implementation. The architectural shape is
identical (the `Nic` trait abstracts the difference); the driver
source tree is ready to receive the GMAC impl in a follow-up.

---

## 5 · Host function surface

Phase 1b adds two host-fn families: NIC-side (Tier-2 only) and
socket-side (Tier-1 + Tier-2). All follow the existing Wari
host-fn pattern: `wari::*` import module name, `i32` return
(0 = success, negative = errno).

### 5.1 Tier-2 NIC host fns (driver only)

Registered for `Tier2HostState`-typed linkers; gated by the
driver's `Net` cap at slot 0.

| Host fn | Args | Returns | Purpose |
|---|---|---|---|
| `wari::net_mmio_read32(addr)` | `u32` | `u32` | Read a 32-bit NIC register; sentinel `u32::MAX` on cap denial |
| `wari::net_mmio_write32(addr, val)` | `u32, u32` | `i32` | Write a 32-bit NIC register; 0 on success, negative on denial |
| `wari::net_dma_alloc(size)` | `u32` | `i32` | Allocate a DMA-coherent buffer; returns the physical address (driver's perspective) or negative errno |
| `wari::net_dma_free(pa)` | `u32` | `i32` | Free a previously-allocated DMA buffer |
| `wari::notification_wait(slot)` | `u32` | `i32` | Block until the Notification at `slot` is signaled (used for IRQ wait) |
| `wari::notification_ack(slot)` | `u32` | `i32` | Acknowledge processing the notification (re-arm the IRQ source) |

**MMIO addresses are bounds-checked** by a new validator in
`kernel/src/validate.rs::is_net_mmio_addr` (sister to the existing
`is_uart_mmio_addr`). The validator's range is per-platform:
`[0x10008000, 0x10008100)` for VirtIO on QEMU,
`[0x16030000, 0x16040100)` for GMAC on VF2.

**DMA host fns** are Phase-1b stubs for QEMU (VirtIO doesn't
need DMA-coherent buffers in the kernel sense; the device walks
guest physical memory directly). They become real on VF2 in
Phase 1c. The signatures land now so the driver code is
platform-portable.

### 5.2 Tier-1 socket host fns (tenant-facing)

Registered for `Tier1HostState`-typed linkers (each Tier-1
instance gets its own registration with `proc_id` baked in,
matching the scheduler PR's pattern); cap-gated by the per-socket
Endpoint cap at the tenant-supplied slot.

| Host fn | Args | Returns | Purpose |
|---|---|---|---|
| `wari::net_socket_create(proto, slot_for_cap)` | `u32, u32` | `i32` | Open a new socket; the driver mints a Socket cap and grants it to the calling tenant at `slot_for_cap`; returns 0 on success or negative errno |
| `wari::net_socket_bind(slot, ip_be, port)` | `u32, u32, u32` | `i32` | Bind a socket to a local 4-tuple; `ip_be` is big-endian IPv4 |
| `wari::net_socket_listen(slot, backlog)` | `u32, u32` | `i32` | Mark TCP socket as listening |
| `wari::net_socket_accept(slot, peer_slot_for_cap)` | `u32, u32` | `i32` | Block waiting for an incoming TCP connection; on accept, mint a new Socket cap for the peer connection at `peer_slot_for_cap` |
| `wari::net_socket_connect(slot, ip_be, port)` | `u32, u32, u32` | `i32` | Initiate a TCP connect or set the UDP peer |
| `wari::net_socket_send(slot, buf_ptr, buf_len)` | `u32, u32, u32` | `i32` | Send bytes; returns bytes written or negative errno; non-blocking — partial writes possible |
| `wari::net_socket_recv(slot, buf_ptr, max_len)` | `u32, u32, u32` | `i32` | Receive bytes; blocks until ≥1 byte available; returns byte count or negative errno |
| `wari::net_socket_close(slot)` | `u32` | `i32` | Tear down the socket; deletes the Socket cap (cap delete cascade frees the smoltcp socket state) |

**Errnos** match the existing cap-host-fn convention plus a few
net-specific ones added to `cap/syscall.rs`:

| Errno | Value | Meaning |
|---|---|---|
| `E_PERM` | -1 | Cap denial (existing) |
| `E_INVAL` | -2 | Bad arguments (existing) |
| `E_NOMEM` | -3 | Pool exhaustion (existing) |
| `E_AGAIN` | -4 | Would block (NEW; for non-blocking partial writes) |
| `E_NOTCONN` | -5 | TCP socket not connected (NEW) |
| `E_REFUSED` | -6 | Connection refused by peer (NEW) |

### 5.3 Cap-mediated IPC under the host fns

The Tier-1 socket host fns are thin wrappers that perform cap
checks and then dispatch into the Tier-2 driver via the cap-IPC
path (Phase 1b PR 4). They do **not** implement TCP themselves —
all protocol logic lives in the Tier-2 driver.

The dispatch shape: Tier-1 calls `wari::net_socket_send(slot,
buf, len)` → Tier-0 host fn checks the per-socket cap → if OK,
copies bytes from Tier-1 lin-mem to Tier-2 driver lin-mem → calls
into the driver's exported `socket_send` function → driver
returns; Tier-0 returns the result to Tier-1.

This is the same shape as the existing `host_fd_write` flow that
crosses Tier-1 → Tier-2 UART driver (`tier2_uart::write`).

---

## 6 · Cap kinds + invariants

### 6.1 New `ObjectKind` variants

```rust
#[repr(u8)]
pub enum ObjectKind {
    Empty = 0,
    Endpoint = 1,
    Notification = 2,
    Untyped = 3,
    Frame = 4,
    Net = 5,           // NEW: NIC handle (driver-only)
    Socket = 6,        // NEW: per-socket Endpoint badged by socket id
    // Phase 2+: Tcb = 7, AsidPool = 8, IrqHandler = 9, ...
}
```

### 6.2 `Net` object

```rust
#[repr(C)]
pub struct Net {
    /// Hardware target: 0 = VirtIO-net (QEMU), 1 = JH7110 GMAC eth0,
    /// 2 = JH7110 GMAC eth1.
    pub nic_kind: u8,
    /// Whether this NIC has been initialized (link up, smoltcp
    /// interface attached). Set by the driver after NIC bring-up.
    pub initialized: bool,
    /// Number of Sockets currently held under this Net.
    pub socket_count: u16,
    /// Refcount of caps pointing here. Phase 1b: 1 (driver's
    /// root cap). Future expansion may include monitoring caps.
    pub refcount: u16,
}
```

Pool capacity: `NET_POOL_CAPACITY = 4` (room for 2 NICs × 2
generations of cap reuse). Sized small because Net objects are
per-NIC, not per-flow.

### 6.3 `Socket` object

```rust
#[repr(C)]
pub struct Socket {
    /// Index of the parent Net pool entry this socket lives on.
    pub net_idx: u16,
    /// Socket id within the driver's smoltcp stack (the
    /// `SocketHandle` smoltcp returns; an opaque u32 to the
    /// kernel).
    pub smoltcp_handle: u32,
    /// Local 4-tuple. `0` for unbound fields.
    pub local_ip: u32,    // big-endian IPv4
    pub local_port: u16,
    pub peer_ip: u32,
    pub peer_port: u16,
    /// Refcount.
    pub refcount: u16,
}
```

Pool capacity: `SOCKET_POOL_CAPACITY = 256` (Phase 1b's per-NIC
socket cap). Sized to allow a meaningful concurrent-connection
demo (256 simultaneous TCP connections is a respectable HTTP echo
demo).

### 6.4 New invariants

| INV | Name | Status |
|---|---|---|
| INV-19 | Tier-Shape Compatibility | **Promoted from reserved to enforced.** A `Net` cap can be minted only by `init_root_caps` for the driver's CSpace. A Tier-1 mint of a Net cap returns `E_PERM`. |
| INV-20 | NIC MMIO Window Validity | New. Every `net_mmio_*` call validates the address against `is_net_mmio_addr` for the active platform. Sister to INV-3 (UART MMIO). |
| INV-21 | Socket Cap Implies Net Cap Existence | New. Every Socket cap's `net_idx` field references a live `Net` pool entry; revoking a Net cap cascades through every Socket minted from it (existing INV-16 derivation chain handles this). |
| INV-22 | NIC Initialization Is Boot-Once | New. The Tier-2 net driver's NIC-init path runs exactly once per boot (the driver's `_start` triggers it). Subsequent Tier-1 socket calls assume the NIC is initialized. |

### 6.5 New errnos in `cap/syscall.rs`

```rust
pub const E_AGAIN:    i32 = -4;
pub const E_NOTCONN:  i32 = -5;
pub const E_REFUSED:  i32 = -6;
```

---

## 7 · Tier-2 net driver internals

### 7.1 Driver structure

```
drivers/net/
├── Cargo.toml
├── build.rs               # platform-specific MMIO bases as env vars
└── src/
    ├── lib.rs             # _start: init NIC, init smoltcp, register socket API
    ├── nic.rs             # trait Nic { fn rx_pop(&mut self) -> Option<&[u8]>;
    │                      #              fn tx_push(&mut self, frame: &[u8]) -> Result<(), ()>; }
    ├── virtio_net.rs      # cfg(feature="qemu") impl Nic
    ├── gmac.rs            # cfg(feature="vf2") impl Nic — Phase-1c stub
    ├── smoltcp_glue.rs    # impl smoltcp::phy::Device for our Nic wrapper
    ├── socket_api.rs      # functions exported to the kernel for socket ops
    └── ipc.rs             # IPC entry-point glue for cap-mediated Tier-1 calls
```

### 7.2 Initialization sequence

1. Driver's `_start` runs once at boot (after the kernel's
   `cap::boot::init_root_caps` has installed the driver's `Net`
   cap at slot 0).
2. Driver checks the platform feature flag and selects the NIC
   impl (VirtIO or GMAC).
3. Driver calls `wari::net_mmio_*` to bring up the NIC: read
   MAC, configure rings, enable IRQ generation.
4. Driver constructs an in-driver smoltcp `Interface` with a
   static IP (Phase 1b) configured at build time.
5. Driver registers its socket-API exports for the kernel to
   call into (these are WASM exported functions; the kernel
   resolves them via `Instance::get_typed_func`, same pattern as
   the UART driver's `write` export).
6. Driver returns from `_start` (it doesn't loop — kernel
   re-enters via host-fn dispatch on each Tier-1 socket call).

### 7.3 IRQ-driven receive path

1. NIC raises IRQ → PLIC claims → kernel signals the
   Notification cap bound to that IRQ source.
2. Driver's main loop (running in a kernel-driven dispatch
   context — actually the driver is single-threaded, so this is
   "next time a socket op fires") reads the Notification:
   `wari::notification_wait(slot=2)` returns 0.
3. Driver calls `nic.rx_pop()` to drain the rx ring into smoltcp
   buffers.
4. smoltcp processes received frames: ARP replies, ICMP echoes,
   TCP segment delivery to the matching socket.
5. Driver acknowledges the IRQ: `wari::notification_ack(slot=2)`.

This polling-on-Tier-1-call shape avoids needing a real driver
worker process in Phase 1b. Phase 2+ when there's a worker model
in the scheduler, the driver gets its own scheduling context and
polls IRQs without waiting for Tier-1 calls.

### 7.4 Socket send path

1. Tier-1 calls `wari::net_socket_send(slot, buf_ptr, len)`.
2. Tier-0 host fn checks the cap at `slot` is a `Socket` with
   WRITE rights.
3. Tier-0 reads the Socket pool entry to get the `smoltcp_handle`.
4. Tier-0 marshals `len` bytes from Tier-1 lin-mem into the
   driver's lin-mem at a known scratch offset (same pattern as
   `runtime::tier2_uart::write` — INV-14 lineage).
5. Tier-0 calls into the driver's exported
   `socket_send(smoltcp_handle, len)` function.
6. Driver runs smoltcp: writes bytes into the TCP send buffer,
   triggers any pending tx ring writes via `nic.tx_push`.
7. Returns the bytes-written count.

### 7.5 Linear memory layout

Smoltcp needs buffers (per-socket rx + tx). Driver lin-mem layout:

```
0x000000 - 0x07FFFF   driver code + smoltcp + statics      (~512 KB)
0x080000 - 0x0FFFFF   smoltcp interface + sockets          (~512 KB)
0x100000 - 0x1FFFFF   per-socket send/recv buffers         (~1 MB)
0x200000 - 0x2FFFFF   tx scratch (Tier-1 → driver marshal) (~1 MB)
0x300000 - 0x3FFFFF   rx scratch (driver → Tier-1 marshal) (~1 MB)
```

Total driver lin-mem: 4 MiB. Phase 1b's bump allocator has 4 MiB
total kernel heap; the driver's lin-mem comes out of that (wasmi
allocates lin-mem from the kernel's `#[global_allocator]`).

A 4 MiB driver eats the entire bump heap, leaving nothing for
Tier-1 lin-mem. **Action**: bump the kernel heap to 16 MiB before
the net driver lands (a one-line change in `mem/kvm.rs` —
documented as a Phase-1b PR prerequisite).

---

## 8 · Test plan

### 8.1 Unit tests (host-side)

Per the Phase-1b cap-system pattern: tests live with the code,
need a `wari-cap` / `wari-net` crate split to actually run on the
host (currently kernel binary can't be built host-side). Defer
the crate split to a follow-up; tests are valid Rust today.

- `validate::is_net_mmio_addr` — bounds check correctness for
  both QEMU + VF2 platform ranges
- `Net::new`, `Socket::new` constructors
- Socket pool alloc/dealloc with refcount tracking
- IPC marshal: bytes-from-Tier1-to-Tier2 round-trip

### 8.2 QEMU integration tests

Add `tests/integration/tests/net_*.rs` files that drive QEMU
with the VirtIO-net device and assert observable behaviour:

- `net_arp_reply.rs` — kernel boots with net driver, host pings
  a known IP, observes ARP request → ARP reply on the QEMU tap
- `net_icmp_echo.rs` — host sends ICMP echo, driver replies via
  smoltcp
- `net_tcp_echo.rs` — Tier-1 echo app accepts TCP connection,
  echoes ≥1 KiB of bytes back; host verifies via `nc`
- `net_socket_isolation.rs` — two Tier-1 instances each open
  socket on different ports; verify cap isolation prevents cross-
  tenant access

### 8.3 Adversarial security tests

`tests/security/tests/cap_net_*.rs`:

- `tier1_forge_net_cap.rs` — Tier-1 attempts `cap_mint` of a Net
  cap; expect `E_PERM` (INV-19)
- `tier1_socket_send_no_write.rs` — Tier-1 holds READ-only
  Socket cap; calls `net_socket_send`; expect `E_PERM`
- `tier1_mmio_outside_window.rs` — Tier-1 attempts
  `net_mmio_write32` with addr outside NIC window; expect
  `E_INVAL` (INV-20)
- `tier1_socket_after_revoke.rs` — Tier-1 opens socket, revokes
  the cap, attempts send; expect `E_PERM`; also verify driver's
  socket pool entry was returned

### 8.4 Demo workload (Phase 1b exit gate)

A standalone Tier-1 app at `apps/echo/`: TCP server on port
8080, accepts connections, reads up to 4 KiB, echoes back, closes.
Built into a signed `.wasm` blob and run via the scheduler as
PROC_ID 4 (alongside the existing two hello instances).

Phase 1b net exit criteria:
1. `make test-net` (new target) boots Wari in QEMU with VirtIO-
   net + tap, runs `nc 192.168.x.y 8080`, sees the echoed bytes
2. The boot trace shows `[t1:4] echo listening on :8080` and
   per-connection log lines
3. Stress test: 10 concurrent connections × 1 MiB each via
   `wrk` from the host complete without driver crash
4. Adversarial tests in §8.3 pass

---

## 9 · Migration from no-net to networked Tier-1

### 9.1 Phase 1b PR sequence

| PR | Title | Scope |
|---|---|---|
| **PR Net-0 (this doc)** | Net driver design draft v1 | Doc only |
| **PR Net-1** | PLIC driver + IRQ → Notification routing | Kernel-only; prerequisite |
| **PR Net-2** | Heap bump (4 MiB → 16 MiB) + cap-kind extension (`Net`, `Socket`) + new errnos | Cap-system extension; no driver yet |
| **PR Net-3** | Net MMIO host fns + validator + cap-gated dispatch | Kernel-only host fn surface |
| **PR Net-4** | VirtIO-net Tier-2 driver (no socket API yet) | Driver crate scaffold + NIC init + IRQ wait + ARP/ICMP working |
| **PR Net-5** | smoltcp port + socket API in driver | Driver gains TCP/UDP via smoltcp |
| **PR Net-6** | Tier-1 socket host fns + IPC integration | Tier-1 can call `net_socket_*` |
| **PR Net-7** | Echo demo Tier-1 app + integration tests | The Phase 1b net exit gate |
| **PR Net-8** | Adversarial security tests | Per §8.3 |
| **PR Net-9** | _(deferred to Phase 1c)_ JH7110 GMAC driver + VF2 silicon demo | Real network on real silicon |

PR Net-1 through PR Net-8 are the Phase 1b net sprint:
**~8 PRs, cumulative ~6,000-9,000 LOC.** Realistic 4-8 weeks of
focused work.

### 9.2 Cap-system integration touchpoints

The net driver is the first user of the cap system beyond the
existing Tier-2 UART driver. Several Phase-1b cap gaps surface
for the first time:

- **Cap mint across tier boundaries**: Tier-1 calls
  `net_socket_create` → driver mints a Socket cap → cap must be
  delivered to Tier-1's CSpace. This requires the cap-IPC path
  (Phase 1b PR 4 / endpoint_send). PR Net-6 depends on PR 4.
- **Cap revoke on Tier-1 exit**: when a Tier-1 with open sockets
  exits, all its Socket caps must be revoked, which cascades
  back to the driver's smoltcp socket cleanup (driver listens on
  a Notification bound to "child cap revoked" — a new mechanism
  needed). Document this gap; defer the implementation to Phase
  1c (until then, sockets leak when their owner exits and only
  reclaim on full reboot).
- **IRQ handler caps**: PR Net-1 introduces `IrqHandler` as an
  optional cap kind for routing PLIC IRQs to Notifications. Or
  alternatively, model IRQs as Notifications directly (one
  Notification per IRQ source, bound at boot). The latter is
  simpler; pick that for Phase 1b unless review surfaces a
  reason to introduce IrqHandler kind.

---

## 10 · Open questions

Items where the design is not yet final. Resolved in review with
Gustavo before PR Net-1 starts.

1. **Static IP for Phase-1b demo**: hardcode `192.168.122.10` in
   the driver build? Or read from a build env var? Suggest:
   build env var `WARI_NET_STATIC_IP` defaulting to
   `192.168.122.10`. Easy to change per deploy.
2. **Single NIC or both ports on VF2 (eth0+eth1)?** Phase 1b
   defers VF2 entirely; Phase 1c picks. Suggest: eth0 only at
   first; eth1 is bonded/spare.
3. **TCP MSS and window sizes?** smoltcp defaults are
   conservative (~1460 MSS, ~64 KiB window). Defer tuning to
   when stress tests show specific bottlenecks.
4. **What happens to in-flight TCP connections on driver
   reload?** Phase 1b: connections drop, Tier-1 reconnects.
   Phase 4 (immutable kernel) revisits.
5. **Per-tenant rate limiting?** Out of scope for Phase 1b.
   Phase 2+ if a workload requires it.
6. **Should the kernel know the IP address?** No — the IP lives
   entirely inside the driver. Tier-1 tenants discover their IP
   via a `net_socket_local_addr(slot)` host fn (defer to PR
   Net-6 if needed; the echo demo doesn't strictly need it).
7. **DNS resolution?** Out of scope for Phase 1b. Tier-1 echo
   listens on a numeric IP; clients connect by IP. DNS can be a
   Tier-1 library in Phase 2 (build on top of UDP socket).

---

## 11 · Prior art consulted

This section is the **citations the auditor will check**.

### Primary

- **smoltcp** (Bergmann, Brown, et al.) —
  https://github.com/smoltcp-rs/smoltcp. The TCP/IP stack we
  port. ~30 KLOC Rust no_std. Used in Tock, Redox, RustyHermit.
- **VirtIO 1.2 Specification** —
  https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html.
  Section 5.1 (network device) is the protocol our QEMU driver
  implements.
- **Synopsys DesignWare Ethernet MAC IP** — used in the JH7110;
  the Linux `stmmac` driver
  (https://elixir.bootlin.com/linux/latest/source/drivers/net/ethernet/stmicro/stmmac/)
  is the reference for VF2 GMAC behaviour. **Do not copy code**
  (Linux is GPLv2-only; Wari is AGPL-3.0; license incompatible
  for direct lift). Used as a reading reference only.

### Secondary

- **Bytecode Alliance WASI sockets** —
  https://github.com/WebAssembly/wasi-sockets. The standardized
  WASI socket API. Wari Phase 1b uses custom `wari::net_*` host
  fns rather than wasi-sockets because: (a) wasi-sockets is a
  draft proposal not yet stable, (b) our cap model differs from
  wasi-sockets's resource model. Phase 2+ may add a wasi-sockets
  shim on top of `wari::net_*` for portability of upstream WASM
  apps.
- **virtio-drivers (Rust)** —
  https://github.com/rcore-os/virtio-drivers. no_std VirtIO
  driver in Rust. Used for code-shape reference; vendor a
  minimal subset for review (don't pull as a dep — keeps the
  Tier-2 driver TCB self-contained).
- **Tock OS net stack** — Tock pioneered the "TCP/IP stack in
  WASM-equivalent capsule" pattern that Wari's Tier-2-driver-
  hosts-smoltcp shape inherits.

### Internal

- `docs/cap-system-design.md` — the cap system this design
  extends (new ObjectKinds, new INVs)
- `docs/invariants.md` — INV catalog (we add INV-19 promotion +
  INV-20/21/22)
- `docs/architecture.md` — Tier 0/1/2 model
- `kernel/src/runtime/tier2_uart.rs` — the existing Tier-2
  driver pattern this design extends
- `kernel/src/runtime/host_fns.rs` — the existing host-fn
  registration pattern (cap-gated, errno-returning)
- `kernel/src/cap/objects.rs` — the kernel-object pool pattern
  we extend with `Net` and `Socket`

---

## 12 · Decision log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-04-26 | TCP/IP stack lives in Tier-2 driver, not Tier 0 | Preserves small-TCB thesis; smoltcp's 30 KLOC stays out of the kernel |
| 2026-04-26 | smoltcp as the TCP/IP stack | Production-tested, no_std, allocation-free, Rust-native; alternatives (custom, lwIP) rejected |
| 2026-04-26 | Two NIC kinds (VirtIO + GMAC) but ship VirtIO only in Phase 1b | Simplicity First; VF2 GMAC is Phase 1c; the `Nic` trait keeps the driver source platform-portable |
| 2026-04-26 | Two new ObjectKinds: `Net` (driver-only) + `Socket` (per-tenant) | Maps cleanly to the cap-system derivation tree; INV-19 enforcement promotes from reserved |
| 2026-04-26 | Static IP for Phase 1b demo (no DHCP) | Removes a dependency for the QEMU demo; DHCP is a Phase 2+ Tier-1 library |
| 2026-04-26 | IRQs as Notifications (not a separate IrqHandler kind) | Simplicity First; one fewer ObjectKind; reconsider only if a workload needs richer IRQ semantics |
| 2026-04-26 | Heap bump from 4 MiB to 16 MiB before net driver lands | The net driver's lin-mem is ~4 MiB; current heap leaves no room for Tier-1 |
| 2026-04-26 | 9-PR Phase-1b sprint (PR Net-1 through PR Net-8) + Phase-1c GMAC | Honest scoping; each PR independently reviewable |
| TBD | Static IP value | Pending §10 question 1 |
| TBD | Demo workload shape (TCP echo confirmed; HTTP echo / REST API for Garage demo) | Pending the Garage timeline |

---

## Appendix A · Glossary

| Term | Definition |
|------|------------|
| **NIC** | Network Interface Card — the physical Ethernet device |
| **VirtIO-net** | Paravirtualized NIC used by QEMU |
| **GMAC** | Synopsys DesignWare Gigabit MAC; the JH7110 NIC |
| **PLIC** | Platform-Level Interrupt Controller (RV64 standard) |
| **smoltcp** | The Rust no_std TCP/IP stack we port to Tier-2 |
| **DMA** | Direct Memory Access; how the NIC moves packets in/out of memory |
| **Descriptor ring** | A circular buffer of (address, length, flags) entries the NIC consumes to find packets |
| **Net cap** | An `ObjectKind::Net` capability granting NIC use; held by the driver |
| **Socket cap** | An `ObjectKind::Socket` capability granting per-socket send/recv; held by the tenant that opened the socket |
| **MAC** | Media Access Control address (Ethernet hardware address) |
| **PHY** | Physical-layer chip on the NIC; talks to the MAC over MDIO bus |
| **MDIO** | Management Data Input/Output bus, the way MAC and PHY communicate |

---

## Appendix B · Why this is the right shape (the elevator pitch)

A reviewer skimming this doc looking for the "why" should land
here:

> **Wari's net driver is a Tier-2 WASM module that drives a NIC
> and runs smoltcp internally, exposing a socket API to Tier-1
> tenants via cap-mediated IPC.** The TCP/IP stack stays out of
> the kernel (Wari's small-TCB thesis); per-platform NIC
> implementations (VirtIO for QEMU, GMAC for VF2) live behind a
> common `Nic` trait so the driver source tree produces two
> signed blobs from one codebase. Two new cap kinds (`Net` and
> `Socket`) extend the Phase-1b cap system; INV-19 (Tier-Shape
> Compatibility) promotes from reserved to enforced.
>
> Phase 1b ships VirtIO-net + a TCP echo demo over the QEMU
> network (PR Net-1 through PR Net-8, ~6-9 KLOC over 4-8 weeks).
> Phase 1c lands the JH7110 GMAC driver and the same demo on VF2
> silicon. The EPAM Garage REST-API workload lives downstream:
> once Tier-1 sockets work, an HTTP service is just a Tier-1 app
> that loops on `net_socket_accept`/`recv`/`send`.
>
> The design's success criterion is: a Phase 4 external auditor
> reads this doc + the Tier-2 driver source + smoltcp + the cap
> additions, and can sign off on the net subsystem's soundness
> without reading every line of the kernel — because the kernel
> didn't grow by 30 KLOC of TCP code. That is what "auditable
> network stack on a small kernel" means in practice.

---

*End of design draft v1. Review and sign-off needed before PR
Net-1 starts.*
