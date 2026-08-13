# Part 2 — The Kernel, From Reset

Seven chapters tracing the kernel from the first instruction after
OpenSBI to a running Tier-1 WASM tenant. This is the *how it works*
counterpart to Part 1's *why*. Each chapter narrates real code
(`kernel/src/**`) against the design docs it implements.

| Ch | Title | Narrates |
|----|-------|----------|
| 8  | Boot & Init | `main.rs::kmain`, `boot.rs`, `boot.S` |
| 9  | Memory & the Sv39 MMU | `mem/page_alloc.rs`, `mem/page_table.rs`, `mem/kvm.rs` |
| 10 | Traps & the PLIC | `trap.rs`, `trap.S`, `mmio/plic.rs` |
| 11 | The wasmi Runtime & WASI | `runtime/{loader,wasi,host_fns}.rs` |
| 12 | Capabilities | `cap/**`, `docs/cap-system-design.md` |
| 13 | The Scheduler & Processes | `sched/**`, `runtime/tier1_pool.rs` |
| 14 | Synchronous IPC | `ipc.rs`, `wari-ipc`, `docs/ipc-design.md` |

The chapters land as drafts; the architect approves each before it is
final.
