// SPDX-License-Identifier: AGPL-3.0-only
//! Kernel Virtual Memory — impure glue between `wari_mem::page_alloc`,
//! `wari_mem::page_table`, and the hardware MMU.
//!
//! Cherry-picked and adapted from goose-os/kernel/src/kvm.rs at
//! 69d9908b6956315684c567fb95cec542062a61a5 under the "only copy what
//! makes sense" discipline.
//!
//! This is the ONLY module that writes to page table memory or the
//! `satp` CSR. Everything above it uses typed helpers; everything below
//! it is pure data.
//!
//! Phase 0 scope:
//!   - One identity-mapped kernel address space, built once at boot
//!   - No process / per-process page tables (those land in Phase 1)
//!   - No PLIC / VirtIO mappings (no IRQs in Phase 0)
//!   - No user-buffer copy helpers (no userspace in Phase 0)

use crate::error::KernelError;
use crate::kprintln;
use wari_mem::page_alloc::{self, AllocError, BitmapAllocator, PAGE_SIZE};
use wari_mem::page_table::{
    make_satp, va_parts, KERNEL_RW, KERNEL_RX, KERNEL_RO, Pte, PteFlags, PT_ENTRIES,
};

/// QEMU `virt` NS16550 UART base. Matches `mmio::uart_ns16550::UART_BASE`.
/// Hardcoded here because Phase 0 has no `platform::` module yet (lands in
/// Phase 1 alongside VF2 support).
const UART_MMIO_BASE: usize = 0x1000_0000;

/// PLIC MMIO base — Platform-Level Interrupt Controller. Standard
/// RV64 layout at `0x0c000000`; covers 0x400000 bytes (4 MiB) of
/// register space (priority + pending + enable + threshold + claim
/// for up to 1024 IRQ sources × 16K hart contexts). Phase-1b uses
/// only the first hart's contexts but maps the full window so PLIC
/// register access from `mmio::plic::*` doesn't fault.
/// Added in PR Net-1 fix.
const PLIC_MMIO_BASE: usize = 0x0c00_0000;
const PLIC_MMIO_LEN:  usize = 0x40_0000;

/// VirtIO MMIO transport range on QEMU virt — `0x10001000..0x10009000`,
/// 8 transport slots × 0x1000 each. The Phase-1b net driver uses the
/// 4th slot at `0x10008000`. Cfg-gated since VF2 has no VirtIO MMIO.
/// Added in PR Net-3 fix (deferred mapping).
#[cfg(feature = "qemu")]
const VIRTIO_MMIO_BASE: usize = 0x1000_1000;
#[cfg(feature = "qemu")]
const VIRTIO_MMIO_LEN:  usize = 0x8000;

/// JH7110 GMAC register window — 128 KiB at 0x16030000 covers
/// both GMAC0 (0x16030000) and GMAC1 (0x16040000). Phase-1c-11
/// widened from 64 KiB so the `gmac1` cargo feature path can read
/// `GMAC1_BASE + 0x110` (version) without a Load Page Fault.
/// Each MAC's 64 KiB register window covers MAC config, MMC
/// counters, MTL queues, and DMA channel-N descriptors. vf2-only.
#[cfg(feature = "vf2")]
const GMAC0_MMIO_BASE: usize = 0x1603_0000;
#[cfg(feature = "vf2")]
const GMAC0_MMIO_LEN:  usize = 0x2_0000;

/// JH7110 STG clock + reset generator (STGCRG). 64 KiB at
/// 0x10230000. Owns GMAC0_AHB / _AXI / _PTP / _TX / _RX clocks
/// and the GMAC0 reset bit. Phase-1c-3 deasserts the reset and
/// enables these clocks before reading the GMAC version register.
#[cfg(feature = "vf2")]
const STGCRG_MMIO_BASE: usize = 0x1023_0000;
#[cfg(feature = "vf2")]
const STGCRG_MMIO_LEN:  usize = 0x1_0000;

/// JH7110 SYS clock + reset generator (SYSCRG). 64 KiB at
/// 0x13020000. Owns the NOC_BUS_STG_AXI clock that the GMAC0
/// AXI port depends on; without it the GMAC's MMIO is alive but
/// register reads return zeros (the bus to the IP block is gated).
/// Phase-1c-11: also owns the GMAC1_* clock+gate cluster
/// (+0x184..+0x1AC) and the GMAC1 reset register (+0x300).
#[cfg(feature = "vf2")]
const SYSCRG_MMIO_BASE: usize = 0x1302_0000;
#[cfg(feature = "vf2")]
const SYSCRG_MMIO_LEN:  usize = 0x1_0000;

/// JH7110 SYS syscon. 4 KiB at 0x13030000 — separate from SYSCRG.
/// Holds the GMAC1 phy-interface-mode select field (offset 0x90,
/// bits 4:2), the SYS-side equivalent of AON_SYSCON +0x0C for
/// GMAC0. Required by Phase-1c-11 (gmac1 cargo feature).
#[cfg(feature = "vf2")]
const SYS_SYSCON_MMIO_BASE: usize = 0x1303_0000;
#[cfg(feature = "vf2")]
const SYS_SYSCON_MMIO_LEN:  usize = 0x1000;

/// JH7110 always-on clock + reset generator (AONCRG). 64 KiB at
/// 0x17000000. Phase-1c maps this for completeness; the actual
/// AON-domain clocks the GMAC needs are minimal (most are STG/SYS),
/// but the driver may eventually want to read the AON syscon for
/// chip-state diagnostics.
#[cfg(feature = "vf2")]
const AONCRG_MMIO_BASE: usize = 0x1700_0000;
#[cfg(feature = "vf2")]
const AONCRG_MMIO_LEN:  usize = 0x1_0000;

/// JH7110 always-on syscon. 4 KiB at 0x17010000 — separate from
/// AON CRG. Holds the GMAC0 phy-interface-mode select field
/// (offset 0x0C, bits 20:18), which routes the RGMII RX clock
/// from the PHY pad into the AON CRG. Without this set, the
/// gmac0_rx gate at AONCRG+0x1C silently rejects the enable
/// bit because its parent (gmac0_rgmii_rxin) is not toggling.
#[cfg(feature = "vf2")]
const AON_SYSCON_MMIO_BASE: usize = 0x1701_0000;
#[cfg(feature = "vf2")]
const AON_SYSCON_MMIO_LEN:  usize = 0x1000;

// ── Linker symbol accessors ─────────────────────────────────────

extern "C" {
    static _text_start: u8;
    static _text_end: u8;
    static _rodata_start: u8;
    static _rodata_end: u8;
    static _data_start: u8;
    static _data_end: u8;
    static _bss_start: u8;
    static _bss_end: u8;
    static _stack_bottom: u8;
    static _stack_top: u8;
    static _end: u8;
    static _heap_end: u8;
    static _runtime_heap_start: u8;
    static _runtime_heap_end: u8;
}

/// Read a linker-defined symbol's address as a `usize`.
///
/// Soundness: INV-4 — linker symbols' values are their addresses; taking
/// `&X as *const u8 as usize` is the standard idiom and does not deref.
#[inline]
fn sym_addr(s: &'static u8) -> usize {
    s as *const u8 as usize
}

// ── Public surface ──────────────────────────────────────────────

/// Initialize the page allocator, build the kernel identity map, and
/// enable Sv39 paging.
///
/// Called exactly once from `kmain` before any code that depends on
/// virtual memory. Steps:
///   1. Compute the heap range `[_end, _heap_end)` from linker symbols
///   2. Install the global `BitmapAllocator` over that range
///   3. Allocate + zero a root page table (one 4 KiB page)
///   4. Identity-map kernel sections + stack + UART MMIO
///   5. Write `satp` and issue `sfence.vma`
///
/// On any failure (allocator overflow, OOM during table walk) returns
/// a `KernelError`; per R5 we never panic in this path.
pub fn init() -> Result<(), KernelError> {
    // SAFETY: INV-4. Reading linker symbol addresses; no deref.
    let heap_start = unsafe { sym_addr(&_end) };
    // SAFETY: INV-4.
    let heap_end = unsafe { sym_addr(&_heap_end) };

    if heap_end <= heap_start || (heap_start % PAGE_SIZE) != 0 {
        return Err(KernelError::InvalidArgument);
    }
    let num_pages = (heap_end - heap_start) / PAGE_SIZE;
    if num_pages == 0 {
        return Err(KernelError::OutOfPages);
    }

    // Install the global allocator. INV-4 (linker addrs) + INV-5 (the
    // `[_end, _heap_end)` range is kernel-writable RAM) + INV-8 (this
    // is the one-time post-init install).
    let alloc = BitmapAllocator::new(heap_start, num_pages);
    alloc.init();
    // SAFETY: INV-1, INV-8. Single-hart, called exactly once during boot
    // before any `page_alloc::get()` reader runs.
    unsafe { page_alloc::install(alloc); }

    kprintln!("  [kvm] heap {:#x} - {:#x} ({} pages)", heap_start, heap_end, num_pages);

    // Allocate the root page table.
    let root = alloc_zeroed_page()?;

    // Identity-map kernel sections.
    // SAFETY: INV-4. Reading linker symbol addresses.
    let text_start   = unsafe { sym_addr(&_text_start) };
    // SAFETY: INV-4.
    let text_end     = unsafe { sym_addr(&_text_end) };
    // SAFETY: INV-4.
    let rodata_start = unsafe { sym_addr(&_rodata_start) };
    // SAFETY: INV-4.
    let rodata_end   = unsafe { sym_addr(&_rodata_end) };
    // SAFETY: INV-4.
    let data_start   = unsafe { sym_addr(&_data_start) };
    // SAFETY: INV-4.
    let data_end     = unsafe { sym_addr(&_data_end) };
    // SAFETY: INV-4.
    let bss_start    = unsafe { sym_addr(&_bss_start) };
    // SAFETY: INV-4.
    let bss_end      = unsafe { sym_addr(&_bss_end) };
    // SAFETY: INV-4.
    let stack_bottom = unsafe { sym_addr(&_stack_bottom) };
    // SAFETY: INV-4.
    let stack_top    = unsafe { sym_addr(&_stack_top) };

    map_range(root, text_start,   text_end,   KERNEL_RX)?;
    map_range(root, rodata_start, rodata_end, KERNEL_RO)?;
    map_range(root, data_start,   data_end,   KERNEL_RW)?;
    map_range(root, bss_start,    bss_end,    KERNEL_RW)?;
    map_range(root, stack_bottom, stack_top,  KERNEL_RW)?;

    // The heap (allocator pool) must itself be identity-mapped RW so that
    // every allocator-returned PA is reachable post-MMU.
    map_range(root, heap_start, heap_end, KERNEL_RW)?;

    // Runtime bump-allocator arena (Phase 0b, PR 4) — distinct from the
    // page-allocator pool above. Identity-map RW so wasmi's allocations
    // are reachable post-MMU.
    // SAFETY: INV-4. Reading linker symbol addresses; no deref.
    let runtime_heap_start = unsafe { sym_addr(&_runtime_heap_start) };
    // SAFETY: INV-4.
    let runtime_heap_end   = unsafe { sym_addr(&_runtime_heap_end) };
    map_range(root, runtime_heap_start, runtime_heap_end, KERNEL_RW)?;

    // UART MMIO — one page, RW. RISC-V Sv39 has no cache-disable PTE bit;
    // cacheability is a PMA property, not a PTE property. Identity-map
    // the page RW and trust the platform PMA configuration.
    map_range(root, UART_MMIO_BASE, UART_MMIO_BASE + PAGE_SIZE, KERNEL_RW)?;

    map_range(root, PLIC_MMIO_BASE, PLIC_MMIO_BASE + PLIC_MMIO_LEN, KERNEL_RW)?;
    #[cfg(feature = "qemu")]
    map_range(root, VIRTIO_MMIO_BASE, VIRTIO_MMIO_BASE + VIRTIO_MMIO_LEN, KERNEL_RW)?;
    #[cfg(feature = "vf2")]
    {
        map_range(root, GMAC0_MMIO_BASE, GMAC0_MMIO_BASE + GMAC0_MMIO_LEN, KERNEL_RW)?;
        map_range(root, STGCRG_MMIO_BASE, STGCRG_MMIO_BASE + STGCRG_MMIO_LEN, KERNEL_RW)?;
        map_range(root, SYSCRG_MMIO_BASE, SYSCRG_MMIO_BASE + SYSCRG_MMIO_LEN, KERNEL_RW)?;
        map_range(root, SYS_SYSCON_MMIO_BASE, SYS_SYSCON_MMIO_BASE + SYS_SYSCON_MMIO_LEN, KERNEL_RW)?;
        map_range(root, AONCRG_MMIO_BASE, AONCRG_MMIO_BASE + AONCRG_MMIO_LEN, KERNEL_RW)?;
        map_range(root, AON_SYSCON_MMIO_BASE, AON_SYSCON_MMIO_BASE + AON_SYSCON_MMIO_LEN, KERNEL_RW)?;
    }

    kprintln!("  [kvm] root pt at {:#x}", root);

    // Enable the MMU.
    let satp = make_satp(root, 0);
    // SAFETY: INV-7. `csrw satp` and `sfence.vma` are S-mode privileged
    // instructions; we are in S-mode. The identity map covers every PA
    // the kernel will touch after this fence (text, rodata, data, bss,
    // stack, heap, UART), so the next instruction fetch and every
    // subsequent load/store resolves through the freshly-installed root.
    //
    // Memory ordering (R6): `sfence.vma zero, zero` flushes the entire
    // TLB on this hart and orders the satp write before any subsequent
    // implicit memory reference. Without it, prefetched translations
    // from the bare-metal pre-MMU state could persist and cause faults.
    unsafe {
        core::arch::asm!(
            "csrw satp, {0}",
            "sfence.vma zero, zero",
            in(reg) satp,
        );
    }

    // Seed the runtime bump allocator. Done after MMU enable so wasmi
    // sees identity-mapped RW pages from the first allocation.
    // SAFETY: INV-1, INV-12. Single-hart boot context, called exactly
    // once before any allocator user runs (`runtime::run_noop` is the
    // first consumer, invoked from `kmain` after this returns).
    unsafe {
        crate::runtime::heap::init(runtime_heap_start, runtime_heap_end);
    }

    Ok(())
}

// ── Internal helpers ────────────────────────────────────────────

/// Identity-map `[start, end)` (rounded out to page boundaries) at `flags`.
fn map_range(
    root: usize,
    start: usize,
    end: usize,
    flags: PteFlags,
) -> Result<(), KernelError> {
    let start_aligned = start & !(PAGE_SIZE - 1);
    let end_aligned = (end + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let mut addr = start_aligned;
    while addr < end_aligned {
        map_page(root, addr, addr, flags)?;
        addr += PAGE_SIZE;
    }
    Ok(())
}

/// Map a single 4 KiB page (VA → PA) walking/allocating intermediate
/// tables as needed.
fn map_page(
    root: usize,
    va: usize,
    pa: usize,
    flags: PteFlags,
) -> Result<(), KernelError> {
    let (vpn2, vpn1, vpn0, _) = va_parts(va);
    let l1 = walk_or_create(root, vpn2)?;
    let l0 = walk_or_create(l1, vpn1)?;
    write_pte(l0, vpn0, Pte::new(pa, flags));
    Ok(())
}

/// Read PTE at `(table, index)`. If valid+branch, return its child PA.
/// Otherwise allocate a new zeroed table and install a branch PTE.
fn walk_or_create(table: usize, index: usize) -> Result<usize, KernelError> {
    let existing = read_pte(table, index);
    if existing.is_valid() {
        if !existing.is_branch() {
            // A leaf where we expected a branch — table is malformed.
            return Err(KernelError::InvalidArgument);
        }
        Ok(existing.phys_addr())
    } else {
        let new_table = alloc_zeroed_page()?;
        write_pte(table, index, Pte::branch(new_table));
        Ok(new_table)
    }
}

/// Read a PTE from a page table page.
///
/// Index is always in-range here (callers pass `va_parts` outputs which
/// are masked to 9 bits = 0..512), so we treat an out-of-range value as
/// an internal logic error.
fn read_pte(table: usize, index: usize) -> Pte {
    debug_assert!(index < PT_ENTRIES);
    let addr = table + index * 8;
    // SAFETY: INV-5. `table` was returned by the allocator (or is the
    // allocator-returned root) and lives in the identity-mapped heap
    // range; `index < 512` so `addr` stays inside that 4 KiB page.
    let bits = unsafe { core::ptr::read_volatile(addr as *const u64) };
    Pte::new_from_bits(bits)
}

/// Write a PTE into a page table page.
fn write_pte(table: usize, index: usize, pte: Pte) {
    debug_assert!(index < PT_ENTRIES);
    let addr = table + index * 8;
    // SAFETY: INV-5. Same argument as `read_pte`: `addr` is inside an
    // allocator-owned, identity-mapped, kernel-writable page.
    unsafe { core::ptr::write_volatile(addr as *mut u64, pte.bits()); }
}

/// Allocate one zeroed page from the global allocator.
fn alloc_zeroed_page() -> Result<usize, KernelError> {
    // SAFETY: INV-1, INV-8. Single-hart, `init()` runs `page_alloc::install`
    // before any caller hits `alloc_zeroed_page`.
    let alloc = unsafe { page_alloc::get() };
    let page = alloc.alloc().map_err(|e| match e {
        AllocError::OutOfMemory => KernelError::OutOfPages,
        _ => KernelError::InvalidArgument,
    })?;
    // SAFETY: INV-5. `page` came from the bitmap allocator whose pool is
    // `[_end, _heap_end)`; that range is kernel-writable.
    unsafe { BitmapAllocator::zero_page(page); }
    Ok(page)
}
