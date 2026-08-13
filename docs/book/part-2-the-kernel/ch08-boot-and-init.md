---
sidebar_position: 8
sidebar_label: "Ch 8: Boot & Init"
title: "Chapter 8 — Boot & Init"
---

# Chapter 8 — Boot & Init

Every operating system has a first instruction. On Wari it is the
label `_start` in [`kernel/src/boot.S`](../../../kernel/src/boot.S),
and by the time you reach it a surprising amount of work has already
happened — none of it ours. This chapter follows the current running
from that label to the moment the scheduler takes over, and it tries
to be honest about a recurring shape in the code: at every stage where
something can go wrong, the kernel does not try to recover. It prints
what it knows and parks the hart forever. That is a design decision,
not an oversight, and understanding *why* is most of understanding
Wari's boot.

## The chain above us

Wari is an S-mode kernel. It never runs in machine mode. The boot
chain that CLAUDE.md names —

```
OpenSBI (M) → U-Boot → Wari (S)
```

— means that by the time `_start` executes, OpenSBI owns machine mode
underneath us (timers, inter-hart interrupts, the `ecall` ABI we lean
on for reset in [`kernel/src/sbi.rs`](../../../kernel/src/sbi.rs)), and
U-Boot has already loaded our image into RAM and jumped to it. This is
the seL4 and Firecracker posture, not the Linux one: we inherit a small
verified M-mode firmware rather than reimplementing SBI ourselves. The
only C code anywhere in Wari's trust story lives up there in OpenSBI,
and we treat it as given (see Part 1, Ch 2).

OpenSBI hands off in S-mode with a documented register contract, spelled
out in the [`boot.S`](../../../kernel/src/boot.S) header:

```
a0 = hart ID
a1 = device tree blob (DTB) pointer
```

Hold onto `a0`. It is about to cause trouble.

## `boot.S` — the smallest possible assembly

The philosophy line from CLAUDE.md is "make it correct, make it secure,
make it small." Assembly is the least verifiable, least testable code in
the kernel, so there is almost none of it. [`boot.S`](../../../kernel/src/boot.S)
does exactly four things before it reaches Rust, and not one thing more:

1. **Select the boot hart, park the rest.**
   ([`boot.S:33-35`](../../../kernel/src/boot.S))

   ```asm
   1:  auipc   t0, %pcrel_hi(_boot_hart_id_addr)
       ld      t1, %pcrel_lo(1b)(t0)
       bne     a0, t1, _park
   ```

   RISC-V brings every hart up at reset; SMP is not a "later" feature we
   have to opt into, it is the default we have to opt *out* of. Wari is a
   single-hart kernel today — that is INV-1, the load-bearing invariant
   behind nearly every `unsafe` block in the tree — so every hart whose
   `a0` does not match the designated boot hart falls into `_park`
   ([`boot.S:65-67`](../../../kernel/src/boot.S)), an unadorned
   `wfi; j _park` loop, and never touches kernel state again.

   The subtlety is *how* the boot hart id is compared. It is not a literal.
   The linker scripts define `_boot_hart_id` as an **absolute** symbol —
   `0` on QEMU `virt` ([`linker.ld:98`](../../../kernel/linker.ld)), `1` on
   the VisionFive 2 ([`linker-vf2.ld:85`](../../../kernel/linker-vf2.ld)),
   because the JH7110 brings U-Boot up on hart 1. You cannot reach an
   absolute address of `0` or `1` with `la` in the `medany` code model:
   `la` expands to `auipc + addi`, whose PC-relative relocations have a
   32-bit reach anchored at the kernel's load address, and "relocation
   truncated to fit" is the linker's way of saying it. So `boot.S` stores
   the absolute value in a `.dword` sitting right next to the boot code
   ([`boot.S:60-62`](../../../kernel/src/boot.S)) — that uses `R_RISCV_64`,
   which has no range limit — and then loads *that* PC-relatively. The
   header comment on `boot.S` records that this exact hardcode is what
   silently parked the boot hart on real silicon before it was fixed:
   the previous Wari `boot.S` did `bnez a0, _park`, which parked every
   hart but hart 0, and VF2 boots on hart 1.

2. **Zero `.bss`.** ([`boot.S:37-45`](../../../kernel/src/boot.S)) A
   plain word loop from `_bss_start` to `_bss_end`. Rust statics assume
   zeroed BSS; the global page allocator, for instance, lives in BSS as
   a zeroed `BitmapAllocator` until boot constructs the real one (Ch 9).

3. **Set the stack pointer.** ([`boot.S:48`](../../../kernel/src/boot.S))
   `la sp, _stack_top`. The stack grows down from the top of a 1 MiB
   region the linker script carves out just past `_end`; that same region
   doubles as the page-allocator pool, a detail Ch 9 returns to.

4. **Call Rust.** ([`boot.S:51`](../../../kernel/src/boot.S)) `call kmain`,
   with `a0`/`a1` preserved. If `kmain` ever returns — it is typed `-> !`,
   so it should not — execution falls through into `_park`.

That is the whole file. No trap vector yet, no paging, no device setup.
All of that is Rust, where it can carry contracts and, where the logic
is pure, host tests.

## `kmain` — the boot spine

[`kernel/src/main.rs:101`](../../../kernel/src/main.rs) is the spine of
the boot. Read top to bottom it is a checklist, and each item either
succeeds and prints a terse confirmation or fails and parks the hart.
The signature is worth pausing on:

```rust
pub extern "C" fn kmain(_hart_id: usize, _dtb_addr: usize) -> !
```

Both arguments are prefixed with an underscore. `kmain` is handed the
hart id and the DTB pointer that `boot.S` faithfully preserved, and it
uses neither. The DTB is future work — Wari's MMIO bases are still
compiled-in constants, not parsed from the device tree. The hart id
is a more interesting refusal.

### The junk-`a0` story

You would expect the banner to print the hart id it was handed. It does
not; it prints a compile-time constant, `BOOT_HART_ID`, selected by the
`vf2` feature ([`main.rs:87-90`](../../../kernel/src/main.rs)). The
doc comment above it ([`main.rs:77-86`](../../../kernel/src/main.rs))
explains the two-sided reason, and both sides are instructive:

- **We do not trust the runtime `a0`.** Some OpenSBI ports — the comment
  names the StarFive VF2 build specifically — leave `a0` holding junk by
  the time the Rust prologue would save it for printing, producing a
  banner that reads `hart 100000` and undermining the one job the banner
  has: telling you the boot got far enough to print.
- **We cannot read the linker symbol from Rust either.** Same `medany`
  reach problem `boot.S` hit: an absolute symbol at value `0`/`1` is
  unreachable by PC-relative addressing from the kernel base.

So the hart id exists in two forms for two audiences. `boot.S` needs the
real value to make the parking decision, and gets it through the
`R_RISCV_64` `.dword` trick. `kmain` only needs a value to *display*, and
takes the honest compile-time constant instead of laundering an unreliable
register through a print path. Both truths come from the same
`--features vf2` switch, so keeping them consistent is a build-time
concern, not a runtime one. It is a small thing, but it is the kind of
small thing — a nonsense number in a boot log — that eats an afternoon
on real hardware, and the code carries the scar tissue as a comment.

### The staged sequence

With the banner printed ([`main.rs:103`](../../../kernel/src/main.rs),
calling `stage_banner` in [`boot.rs`](../../../kernel/src/boot.rs) —
a pure-ASCII chakana, the Andean stepped cross, because ASCII art
renders on every 8N1 serial terminal), `kmain` walks its checklist:

| Order | Call | Brings up |
|-------|------|-----------|
| 1 | `mmio::uart_ns16550::init()` ([`:102`](../../../kernel/src/main.rs)) | The early console, so every later stage can speak |
| 2 | `mem::kvm::init()` ([`:105`](../../../kernel/src/main.rs)) | Page allocator + Sv39 identity map + `satp` on (Ch 9) |
| 3 | `trap::install()` ([`:114`](../../../kernel/src/main.rs)) | Trap vector into `stvec` (Ch 10) |
| 4 | `mmio::plic::init()` ([`:115`](../../../kernel/src/main.rs)) | PLIC threshold + `sie.SEIE` (Ch 10) |
| 5 | `cap::boot::init_root_caps()` ([`:118`](../../../kernel/src/main.rs)) | The capability pools and root CSpaces (Ch 12) |
| 6 | `runtime::run_tier2_uart()` ([`:129`](../../../kernel/src/main.rs)) | Signature-checks and loads the Tier-2 UART driver (Ch 11) |
| 7 | `runtime::run_tier2_net()` ([`:140`](../../../kernel/src/main.rs)) | Loads the Tier-2 net driver |
| 8 | `sched::register_library` ×2 ([`:156`, `:169`](../../../kernel/src/main.rs)) | Registers the two Tier-2 drivers as library processes |
| 9 | `sched::register_tenant` ×2 ([`:182`, `:195`](../../../kernel/src/main.rs)) | Registers the two Tier-1 `hello` instances as `Ready` |
| 10 | `sched::run()` ([`:208`](../../../kernel/src/main.rs)) | Hands off; runs each Tier-1 in `proc_id` order |

Two things about this list contradict the map you might expect from
elsewhere in the tree, and in both cases the code is the truth.

**The order is not the `boot.rs` order.** The module docstring in
[`boot.rs:8-19`](../../../kernel/src/boot.rs) lays out an aspirational
stage list — `stage_interrupts` (item 3) *before* `stage_memory` and
`stage_mmu` (items 4–5). The real `kmain` does the opposite: it enables
paging (`kvm::init`) *before* it installs the trap vector or the PLIC.
That is the correct order — you want the MMU up and the kernel window
mapped before you start taking traps against it — and it is what ships.
`boot.rs` today contains exactly one live function, `stage_banner`; the
rest of its staging is a documentary table of contents that history has
partly overtaken. When the doc and the code disagree, the code wins, and
here the code is right.

**"Interrupts on" overstates it.** The same `boot.rs` comment says its
interrupt stage turns "SIE on." What `plic::init` actually sets is
`sie.SEIE` — bit 9 of the `sie` *mask* register, the per-source enable
for external interrupts — and nothing anywhere in the tree sets
`sstatus.SIE`, the global supervisor interrupt-enable. The consequence is
the whole subject of Ch 10: interrupts can become *pending* and can wake
a `wfi`, but with `sstatus.SIE` clear the kernel never *takes* one while
it is running. Preemption is a seam, not a feature, today.

## Why every failure parks

Look at the shape repeated ten times in `kmain`
([`main.rs:105-216`](../../../kernel/src/main.rs)):

```rust
if let Err(e) = mem::kvm::init() {
    kprintln!("MMU init failed: {:?}", e);
    loop {
        // SAFETY: INV-7 — wfi is an S-mode instruction in S-mode.
        unsafe { core::arch::asm!("wfi"); }
    }
}
```

Print the error, then spin forever in `wfi`. No retry, no fallback, no
degraded mode. This is deliberate, and it follows from what Wari is for.
CLAUDE.md's mission names the users — "governments, hospitals, banks,
citizens." For that audience a kernel that limps forward in an unknown
state after its MMU failed to initialize is far more dangerous than one
that stops. Every one of these failures is, by construction, a bug in
Tier 0 or a violation of an environmental assumption (the signed driver
did not verify; the capability pools would not initialize). There is no
correct recovery from "the thing I proved could not happen, happened."
So we halt, having first emitted one diagnostic line so an operator on
the serial console knows *which* stage died.

The `wfi` in each loop is the reason INV-7 exists: `wfi` is a privileged
S-mode instruction, and the `unsafe` is there only because Rust demands
it around inline assembly, not because the operation is unsound — we are
in S-mode, where `wfi` is legal. The [invariants catalog](../../invariants.md)
records both the `kmain` halt loops and the panic handler as INV-7 sites.

The panic handler ([`main.rs:278-286`](../../../kernel/src/main.rs)) is
the same idea taken to its limit: it prints *nothing* and parks. Per
absolute rule R5, panics in Wari are last-resort assertions only —
`unwrap`/`expect` are banned in `kernel/src/` — and by the time one
fires the system is in an undefined state where even trying to format a
message could make things worse. Halt, and let a human decide.

## The handoff, and the idle loop after it

`sched::run()` ([`main.rs:208`](../../../kernel/src/main.rs)) is where
boot ends and the system begins. It runs each `Ready` Tier-1 tenant in
ascending `proc_id` order to completion, isolating faults so one
tenant's crash does not take down another
([`sched/mod.rs:145`](../../../kernel/src/sched/mod.rs)) — the mechanics
are Ch 13's subject. What is worth noticing here is that `run()`
*returns*. When the last tenant has exited (or the survivors are
permanently blocked on each other), control comes back to `kmain`, which
falls into a deliberate idle loop ([`main.rs:237`](../../../kernel/src/main.rs)).

That loop has two jobs, and both are honest about the interrupt story:
it polls smoltcp for the net driver if one is installed, and it
busy-polls the UART receive register for a Ctrl-R (`0x12`), which
triggers an SBI cold reboot ([`main.rs:259-263`](../../../kernel/src/main.rs))
— goose-os's old reset-key affordance, carried forward. The comment on
that loop is candid: it is busy-polled "because UART RX isn't yet routed
through the PLIC; a future PR can wire IRQ-driven `wfi` to drop the busy
cost." The kernel *could* sleep here and let an interrupt wake it. It
does not, yet, for exactly the reason Ch 10 will make precise.

## Closing hook

Notice what item 2 in `kmain`'s checklist quietly assumed: that by the
time it runs, there is memory to allocate and an address space to map
things into. There is not, yet — `boot.S` set a stack pointer and zeroed
BSS, and that is all. Before Wari can load a driver, take a trap against
a mapped kernel window, or hand a page to wasmi, it has to build the
world those things live in. Ch 9 — memory, the bitmap allocator, and
turning the Sv39 MMU on without faulting on the instruction right after
you do.
