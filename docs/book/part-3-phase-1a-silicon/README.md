---
sidebar_position: 0
sidebar_label: "Part 3 — Overview"
title: "Part 3 — The Phase 1a Silicon Sprint"
---

Part 2 closed with Wari running a Tier-1 WASM module to `proc_exit(0)`
inside QEMU's `virt` machine. That demo was the Phase-0 exit gate —
real, verifiable, and entirely virtual. Part 3 is the three PRs that
took us from "QEMU-only" to **"Hello from Wari prints on a VisionFive 2
you can buy for eighty dollars."** Three PRs, one weekend, one
sentence of UART output that justifies every prior chapter of this
book.

The sprint:

- **Chapter 15 — VF2 Cross-Compile (PR 8).** The linker script, the
  platform module, the build-script switch, and the `_boot_hart_id`
  symbol that lets one `boot.S` cover two boards.
- **Chapter 16 — Per-Platform Drivers (PR 9).** Two signed Tier-2 UART
  blobs instead of one — and the new `mmio_read8` host fn the LSR
  poll loop needed.
- **Chapter 17 — Hello from Silicon (PR 10).** The deploy harness, the
  `init()` writes the JH7110 actually requires, and the moment the
  COM7 console prints `Hello from Wari`.

By the end of Part 3, sovereign WASM-on-RISC-V is no longer a thesis.
It boots on silicon you can audit byte-for-byte from boot ROM up.
