---
sidebar_position: 9
sidebar_label: "Ch 9: Memory & the MMU"
title: "Chapter 9 — Memory & the Sv39 MMU"
---

# Chapter 9 — Memory & the Sv39 MMU

Enabling paging is the single most alarming thing a kernel does to
itself. You spend a few hundred instructions building a data structure
that describes where memory *is*, and then, in one CSR write, you tell
the hardware to route every future memory reference — including the fetch
of the very next instruction — through that structure. If the page the
program counter is sitting on is not mapped, or is mapped without execute
permission, the machine faults on the instruction after the one that
turned the MMU on, and it faults before there is any trap vector able to
say so. This chapter is about how Wari builds that structure so the write
is safe, and about the discipline — pure logic below, impure glue above —
that lets most of the risky reasoning happen in a host test instead of on
a serial cable at 3 a.m.

## Pure before impure

Absolute rule number six in CLAUDE.md: *pure before impure.* Separate the
logic that has no `unsafe`, no MMIO, and no statics into files that
compile and test on your laptop; put the hardware-touching glue in
adjacent files with explicit banners. Memory is where this rule earns its
keep, because the two hardest-to-get-right things — the bitmap allocator's
arithmetic and the Sv39 page-table walk — are *pure functions*. They were
cherry-picked from goose-os (rev `69d9908…`), and in Wari they live in the
`wari-mem` workspace crate, host-tested, with the kernel-side modules
([`kernel/src/mem/page_alloc.rs`](../../../kernel/src/mem/page_alloc.rs),
[`page_table.rs`](../../../kernel/src/mem/page_table.rs)) reduced to thin
re-exports. The one file that actually writes page-table memory and the
`satp` register is [`kernel/src/mem/kvm.rs`](../../../kernel/src/mem/kvm.rs),
and its module docstring states the boundary flatly: "This is the ONLY
module that writes to page table memory or the `satp` CSR."

That is not tidiness for its own sake. It is a formalization-staging
move (rule 3): the pure crate reads as if Kani will prove its invariants
next quarter — and for the page-table primitives, Kani already does. Below
the tests in [`page_table.rs:768`](../../../wari-mem/src/page_table.rs)
sits a `#[cfg(kani)]` proof module asserting, among other things, that
virtual-address decomposition round-trips and that no kernel permission
set contains both `WRITE` and `EXECUTE`. You can prove those things
precisely *because* the code is pure.

## The bitmap allocator

[`wari-mem/src/page_alloc.rs`](../../../wari-mem/src/page_alloc.rs) is a
`BitmapAllocator`: one bit per 4 KiB physical page, set means allocated.
That is the entire idea. Its design comment names the reason it is shaped
this way — "State is a bitvector; transitions are set/clear, both monoid
ops" — which is another way of saying it was written to be reasoned about.
`PAGE_SIZE` is 4096 ([`:25`](../../../wari-mem/src/page_alloc.rs)); the
bitmap is sized for a worst case of 32,768 pages, i.e. 128 MiB, which
costs 4 KiB of static bitmap ([`:28-31`](../../../wari-mem/src/page_alloc.rs)).

`alloc()` ([`:118`](../../../wari-mem/src/page_alloc.rs)) scans words for
the first one that is not all-ones, finds the first zero bit with
`(!word).trailing_zeros()`, sets it, and returns `base + index *
PAGE_SIZE`. `free()` ([`:142`](../../../wari-mem/src/page_alloc.rs))
clears the bit and — this is the part that matters for a kernel — returns
a typed `AllocError` rather than trusting the caller: `DoubleFree`,
`InvalidAddress`, `NotAligned`. There are no silent failures and no
panics on the hot path. The invariant the whole thing turns on is spelled
out in its own doc comment and echoed as INV-5 in the catalog: a returned
address is a page-aligned PA inside `[base, base + total_pages *
PAGE_SIZE)`, and the kernel identity-maps that entire range read-write, so
writing through an allocator-returned pointer can never clobber kernel
text.

Everything above is pure. The one `unsafe` in the file is `zero_page`
([`:238`](../../../wari-mem/src/page_alloc.rs)), which volatile-writes
4096 zero bytes through a returned PA — sound *by* INV-5, cited in the
`// SAFETY` comment, and the reason `zero_page` is `unsafe fn` rather than
safe: it trusts its caller to hand it an allocator-owned address. The
global singleton `ALLOC` lives in BSS as a zeroed `BitmapAllocator::new(0,
0)` ([`:50`](../../../wari-mem/src/page_alloc.rs)) — which is why `boot.S`
had to zero BSS — and the `get()`/`install()` accessors
([`:57`, `:70`](../../../wari-mem/src/page_alloc.rs)) are `unsafe` under
INV-1 (single-hart) and INV-8 (called only post-init). The pure allocator
does not know it is a singleton; the kernel decides that.

## Sv39, briefly

RISC-V Sv39 gives you a 39-bit virtual address resolved through three
levels of page table. The ASCII diagram at the top of
[`page_table.rs:8-21`](../../../wari-mem/src/page_table.rs) is the whole
format:

```
Virtual address (39 bits):
  VPN[2] (9) │ VPN[1] (9) │ VPN[0] (9) │ offset (12)
    38-30        29-21        20-12        11-0
```

Each 9-bit VPN indexes one 512-entry table (`PT_ENTRIES = 512`,
[`:41`](../../../wari-mem/src/page_table.rs)); each table is exactly one
4 KiB page. `va_parts()` ([`:277`](../../../wari-mem/src/page_table.rs))
is the pure shift-and-mask that splits a VA into `(vpn2, vpn1, vpn0,
offset)`, and its inverse `va_from_parts` is proven to round-trip it.

A page table entry is a newtype over `u64`
([`Pte`, `:180`](../../../wari-mem/src/page_table.rs)) — "a pure value;
two PTEs with the same bits are equal, no hidden state." The low ten bits
are flags (`V R W X U G A D`); bits 10–53 hold the 44-bit physical page
number. `Pte::new(pa, flags)` ([`:195`](../../../wari-mem/src/page_table.rs))
builds a leaf; `Pte::branch(table_pa)` ([`:204`](../../../wari-mem/src/page_table.rs))
builds an interior pointer with only `V` set. A PTE is a *leaf* when it
carries any of R/W/X and a *branch* otherwise — the distinction the walker
lives and dies on.

The permission sets are named constants, not magic numbers (rule 3):
`KERNEL_RX` ([`:103`](../../../wari-mem/src/page_table.rs)) is
read+execute, no write; `KERNEL_RO` read-only; `KERNEL_RW`
([`:116`](../../../wari-mem/src/page_table.rs)) read+write, no execute.
The W^X separation those constants encode is not a convention you have to
trust a reviewer to have checked — `proof_kernel_wx_separation`
([`:848`](../../../wari-mem/src/page_table.rs)) makes it a machine-checked
fact.

### The walker takes a closure

Here is the elegant part. The pure walker `walk()`
([`:330`](../../../wari-mem/src/page_table.rs)) does not dereference
anything. It takes a closure `read: FnMut(usize) -> u64` and asks *it* for
the PTE at each physical address:

```rust
pub fn walk<F: FnMut(usize) -> u64>(root: usize, va: usize, mut read: F)
    -> Option<WalkResult>
```

In the kernel, under identity mapping, that closure is a volatile pointer
read. In the host tests it is a `HashMap` lookup
([`:620-632`](../../../wari-mem/src/page_table.rs)), which is exactly how
a laptop with no MMU can nonetheless test a three-level Sv39 walk against
a fake tree. This is why the [invariants catalog](../../invariants.md)
records `page_table.rs` as having *no* `unsafe` blocks at all, and why
INV-9 (the slice-to-struct reinterpretation caveat that bit goose-os's
ELF loader) simply does not apply here: there is no `&[u8]`-to-`&Pte`
cast in the file, because the walker never touches memory directly. The
caveat was engineered out of existence.

The walker is also deliberately narrow. It rejects superpages — a leaf at
level 2 or level 1 returns `None`
([`:340-345`](../../../wari-mem/src/page_table.rs)) — because the Phase 0
kernel only ever emits 4 KiB leaves, and a walker that refuses to resolve
a shape the kernel never produces is a walker with fewer ways to be wrong.

## `kvm.rs` — the glue that turns it on

[`kvm.rs::init()`](../../../kernel/src/mem/kvm.rs) is the impure
orchestrator, called exactly once from `kmain`. Its doc comment lists the
five steps ([`:141-152`](../../../kernel/src/mem/kvm.rs)); the interesting
ones are what it maps and the write that ends it.

**The pool comes from the linker.** `init` reads `_end` and `_heap_end`
([`:156-158`](../../../kernel/src/mem/kvm.rs)) — under INV-4, taking a
linker symbol's address as a `usize` is sound and does not dereference —
computes the page count, constructs the `BitmapAllocator` over that range,
and installs it ([`:171-177`](../../../kernel/src/mem/kvm.rs)). The pool
`[_end, _heap_end)` is the 1 MiB region `boot.S`'s stack pointer sits atop:
the [linker script](../../../kernel/linker.ld) deliberately overlaps them
([`linker.ld:60-73`](../../../kernel/linker.ld)), stack growing *down* from
`_stack_top`, allocator handing frames *up* from `_end`.

**The kernel maps itself.** `init` then identity-maps each section at its
correct permissions — text `KERNEL_RX`, rodata `KERNEL_RO`, data/bss/stack
`KERNEL_RW` ([`:211-215`](../../../kernel/src/mem/kvm.rs)) — plus the
allocator pool itself ([`:219`](../../../kernel/src/mem/kvm.rs)), because
every PA the allocator will ever return has to be reachable *after* paging
is on, and the runtime bump arena wasmi allocates from
([`:228`](../../../kernel/src/mem/kvm.rs)). "Identity-mapped" means VA
equals PA everywhere; Wari runs the kernel in the physical address space,
which keeps `kvm.rs`'s reasoning — and INV-5's — a single sentence long.

**And it maps the devices.** This is the part the module's own Phase 0
docstring no longer describes. The header still says "No PLIC / VirtIO
mappings (no IRQs in Phase 0)" ([`:16`](../../../kernel/src/mem/kvm.rs)),
but the body has long since grown past that scope: it maps the UART page
([`:233`](../../../kernel/src/mem/kvm.rs)), the full 4 MiB PLIC window
([`:235-240`](../../../kernel/src/mem/kvm.rs)), the VirtIO transport range
on QEMU ([`:241-247`](../../../kernel/src/mem/kvm.rs)), and on the VF2 a
whole cluster of JH7110 windows — GMAC0's registers, and the STG/SYS/AON
clock-and-reset generators the Ethernet MAC depends on
([`:248-286`](../../../kernel/src/mem/kvm.rs)). When the docstring and the
code disagree, the code is the truth; the header is a stale scope note
from when this file really was Phase 0.

There is no separate cache-disable bit to set for these MMIO pages.
RISC-V Sv39 has no such PTE flag — cacheability is a PMA property, not a
page-table property — so the MMIO ranges are mapped plain `KERNEL_RW` and
the platform's physical-memory attributes are trusted to mark them
non-cacheable ([`:230-233`](../../../kernel/src/mem/kvm.rs)). (The
`page_table.rs` crate defines a `KERNEL_MMIO` constant for documentation,
but its bits are identical to `KERNEL_RW`; `kvm.rs` uses the latter.)

### Two scars worth reading

The MMIO mappings carry two pieces of hard-won history that a reader
should not have to rediscover.

The first is in the linker script. That 1 MiB stack-and-pool region was
64 KiB until PR Phase-1c-1.5 ([`linker.ld:60-67`](../../../kernel/linker.ld)):
"adding MMIO mappings (PLIC + GMAC0) ran out of L3 page-table pages with
the smaller region; the kernel rebooted in a loop mid-`kvm::init`." Every
device window you identity-map costs *interior* page-table pages, and
those pages come from the very pool you are mapping. Map enough MMIO and
the allocator that builds the tables runs dry before the tables are
finished — and because this happens during `kvm::init`, before the trap
vector is installed, the failure is a silent reboot loop rather than a
diagnosable fault. The fix was not cleverness; it was giving the pool room.

The second is the GMAC1 comment ([`:50-54`](../../../kernel/src/mem/kvm.rs)):
the GMAC register window was widened from 64 KiB to 128 KiB in
Phase-1c-11 so the `gmac1` feature path could read `GMAC1_BASE + 0x110`
(the version register) "without a Load Page Fault." A driver that reads
one register past the edge of a mapped window does not get a friendly
error — it gets a page fault, which in the current kernel means a printed
`scause`/`stval` and a parked hart (Ch 10). The map window and the driver
reach have to agree to the byte.

### The write

The last step turns paging on ([`:291-308`](../../../kernel/src/mem/kvm.rs)):

```rust
let satp = make_satp(root, 0);
// SAFETY: INV-7 ...
unsafe {
    core::arch::asm!(
        "csrw satp, {0}",
        "sfence.vma zero, zero",
        in(reg) satp,
    );
}
```

`make_satp` ([`page_table.rs:299`](../../../wari-mem/src/page_table.rs)) —
pure, and proven to round-trip — packs the Sv39 mode field and the root
table's PPN into the `satp` value. The `csrw` installs it; from the next
instruction on, every fetch and every load/store resolves through the root
we just built. That next instruction does not fault only because the
identity map covers every PA the kernel will touch afterward: its own
text, so the fetch resolves; its stack, so the return works; the heap, the
UART, everything. The `SAFETY` comment on this block spells that argument
out in full, and it is the single most important safety comment in the
memory subsystem.

The `sfence.vma zero, zero` is not decoration. Absolute rule R6 requires
every barrier to document what it orders and why, and this one does: it
flushes the entire TLB on the hart and orders the `satp` write ahead of
any later implicit memory reference, so a translation prefetched during
the bare-metal pre-MMU state cannot survive to cause a fault after the
switch. A `satp` write without the following `sfence.vma` is a classic,
maddening bug; Wari writes them as one inseparable `asm!` block so they
cannot drift apart.

## Closing hook

The memory subsystem builds the world and leaves one thing conspicuously
unbuilt: a way to survive touching an address that world does not
describe. `kvm::init` returns, `kmain` prints "mmu OK," and the very next
thing it does is install a trap vector — because from here on, a stray
load, a driver reading one register too far, a timer OpenSBI delivers
unbidden, are all things the kernel has to *catch* rather than crash on.
Ch 10 — the trap vector, the `scause` dispatch, and the PLIC claim cycle,
plus the honest reason none of it preempts anything yet.
