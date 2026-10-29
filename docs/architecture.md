# Wari — Architecture (living document)

> **Scope**: the current architecture. Not the vision (see
> `book/part-1-architecture/` for that), not the roadmap (see
> `../CLAUDE.md` §Roadmap). Only what's true *right now*.

**Status**: Phases 0, 1a, 1b and 1c shipped (1c closed 2026-08-15);
Phase 2 open. Booting on QEMU `virt` and on the StarFive VisionFive 2.
What isolates tenants today, and what does not yet:
[`security-model.md`](security-model.md).

---

## Component overview

```mermaid
graph TB
    subgraph T1["Tier 1 — Customer WASM  (S-mode inside wasmi, WASM sandbox only; U-mode + MMU planned)"]
        A1[app A<br/>.wasm]
        A2[app B<br/>.wasm]
        AN[... 50k instances target]
    end

    subgraph T2["Tier 2 — System WASM  (S-mode, WASM sandbox only)"]
        D1[uart driver<br/>.wasm signed]
        D2[net driver<br/>.wasm signed]
        D3[gpu / ai driver<br/>.wasm signed]
        D4[gapu driver<br/>.wasm signed]
    end

    subgraph T0["Tier 0 — Native Rust Kernel  (S-mode)"]
        K1[boot &middot; trap &middot; MMU]
        K2[wasmi runtime]
        K3[capability table]
        K4[IPC + host-fn dispatch]
        K5[scheduler]
    end

    subgraph HW["Hardware — JH7110 (Phase 0) / + GAPU FPGA (Phase 3)"]
        H1[U74 cores]
        H2[Sv39 MMU + PMP]
        H3[Zkn / Zks crypto]
        H4[CoVE confidential mem - P3]
        H5[PCIe &middot; GPU &middot; FPGA]
    end

    A1 -- "WASI host fn" --> K4
    A2 -- "WASI host fn" --> K4
    AN -- "WASI host fn" --> K4

    K4 -- "cap-gated host fn calls driver export" --> D1
    K4 -- "cap-gated host fn calls driver export" --> D2
    K4 -- "cap-gated host fn calls driver export" --> D3
    K4 -- "cap-gated host fn calls driver export" --> D4

    D1 -- "typed MMIO" --> H1
    D2 -- "typed MMIO" --> H1
    D3 -- "PCIe / MMIO" --> H5
    D4 -- "PCIe / MMIO" --> H5

    K1 -.-> H1
    K1 -.-> H2
    K2 -.-> K4
    K3 -.-> K4
    K5 -.-> K1
```

## Control flow — Tier-1 syscall

A Tier-1 app calls `fd_write(stdout, "Hello")`; this is the full path
through the system.

```mermaid
sequenceDiagram
    autonumber
    participant App as Tier-1 app (.wasm)
    participant WR as wasmi runtime (Tier 0)
    participant K as Kernel dispatch
    participant D as Tier-2 UART driver (.wasm)
    participant HW as UART MMIO

    App->>WR: fd_write(1, buf, len)
    WR->>K: host_fn_fd_write(stdout, buf, len)
    K->>K: validate caller's stdout cap
    K->>D: call driver `write` export (buf_copy, len)
    D->>D: wasmi executes driver module
    D->>HW: mmio_write8 host fn → typed volatile store to THR
    HW-->>D: (bytes out the wire)
    D-->>K: return bytes_written
    K-->>WR: return n
    WR-->>App: a0 = n
```

Two WASM sandbox crossings (Tier 1 → Tier 2), zero process-level
context switches, every crossing capability-gated. The Tier-1 → Tier-2
hop is **not** IPC: `host_fd_write` (`runtime/wasi.rs`) calls the
driver's export directly through `tier2_uart::write`, synchronously,
on the same kernel stack. Drivers are loaded as libraries, not
scheduled, so a driver that stalls stalls its caller and the kernel
with it. Synchronous IPC between Tier-1 instances
(`cap/objects.rs` + `runtime/tier1_pool.rs`) is a separate mechanism.

All of this runs in S-mode in one identity-mapped address space: there
is no U-mode and no per-instance page table yet, so the MMU separates
nothing between tenants (see `security-model.md` §"What Layer 2 is
today").

## Subsystem state

| Subsystem        | Status | Where                                                     |
|------------------|--------|-----------------------------------------------------------|
| Workspace layout | Done   | Cargo workspace                                           |
| ABI (syscalls/errors) | Done | `kernel/src/abi.rs`, `abi-shared/`                       |
| Tier 0 memory    | Done   | `kernel/src/mem/{kvm,page_alloc,page_table}.rs`           |
| Tier 0 scheduler | Done (Phase 1b) | `kernel/src/sched/`                              |
| Tier 0 IPC       | Done (Phase 1b) | Capability Endpoint/Notification objects — `kernel/src/cap/objects.rs` |
| Tier 0 trap      | Done   | `kernel/src/trap.rs`, `trap.S`                            |
| Typed MMIO (R3)  | Done   | `kernel/src/mmio/volatile.rs`                             |
| wasmi embedding  | Done   | `kernel/src/runtime/engine.rs` (wasmi 0.32.3)             |
| WASI host fns    | Done   | `kernel/src/runtime/{wasi,host_fns}.rs`                   |
| Tier 1 hello     | Done (Phase 1a) | `apps/hello/` — runs on VF2 silicon                |
| Capability system | Done (Phase 1b) | `kernel/src/cap/`                                |
| Tier-2 UART driver | Done | `drivers/uart/`, `kernel/src/runtime/tier2_uart.rs`       |
| Tier-2 net driver | Done (Phase 1c) | `drivers/net/`, `kernel/src/runtime/tier2_net.rs` — JH7110 GMAC1 on the VF2, virtio-net on QEMU, smoltcp, TCP sockets; 0% ping loss on silicon (build 162). DMA addresses are driver-computed and unchecked — see `security-model.md` §"DMA" |
| Tier-1 U-mode + per-instance page tables | **Not implemented** | One S-mode, identity-mapped address space (`kernel/src/mem/kvm.rs`, `kernel/src/trap.S`) |
| Fuel / preemption | **Not implemented** | Run-to-completion (`kernel/src/sched/mod.rs`); the wasmi `Engine` is built without fuel |
| Kernel heap | Bump allocator, no free (Phase-0 design, INV-12) | `kernel/src/runtime/heap.rs` — memory from every instantiation is kept until reset |

## Design decisions settled since Phase 0

1. **wasmi version + feature set.** Pinned to `wasmi` 0.32.3, `no_std`
   pure interpreter. JIT deferred to Phase 2+.
2. **`.wasm` signing + boot verification.** Tier-2 bytecode is ed25519
   signature-verified against the kernel's compiled-in pubkey before
   instantiation — `kernel/src/runtime/sign.rs` (INV-13).
3. **PID allocation.** PID 1 = first Tier-1, PID 2+ = Tier-1 pool,
   PIDs from 16 up are Tier-2 drivers.

See `book/part-1-architecture/` for the narrative derivation of this
architecture and why it looks like this.
