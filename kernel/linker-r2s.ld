/* SPDX-License-Identifier: AGPL-3.0-only */
/* Wari kernel linker script — Orange Pi R2S (Ky X1 / SpacemiT K1).
 *
 * ⚠ PROVISIONAL. ORIGIN below is a DT-blocked bring-up value, not
 * confirmed against this board's OpenSBI/U-Boot handoff yet — see
 * docs/r2s-bringup.md. It is correct enough to LINK (which is what the
 * first-light scaffold proves); the value that makes it actually BOOT
 * comes from the board's boot.log + board.dts.
 *
 *   ORIGIN 0x00200000 — DRAM base 0x0 + 2 MiB, mirroring the VF2/QEMU
 *   "+2 MiB above DRAM base" convention. The K1's DRAM base is 0x0
 *   (U-Boot CFG_SYS_SDRAM_BASE); 0xC0000000 is the DDR *controller*
 *   register block, NOT DRAM — do not load there. Cross-check the real
 *   handoff address from the U-Boot banner in boot.log before trusting.
 *
 *   Boot hart: NOT a linker symbol here — boot-r2s.S elects it at
 *   runtime (first hart to claim wins), because the R2S boot hart is
 *   unknown and guessing wrong would make the shared boot.S silently
 *   park the real one (no banner). r2s_first_light prints the elected
 *   hart so it can be recorded.
 *
 * Symbol layout is identical to linker.ld / linker-vf2.ld so kvm.rs's
 * identity-map arithmetic and the page allocator's [_end, _heap_end)
 * window work unchanged across all platforms.
 */

OUTPUT_ARCH(riscv)
ENTRY(_start)

MEMORY {
    /* Ky X1 boards ship 2–8 GB LPDDR; reserve 256 MB for the kernel. */
    RAM (rwx) : ORIGIN = 0x00200000, LENGTH = 256M
}

SECTIONS {
    .text : {
        _text_start = .;
        KEEP(*(.text.entry))
        *(.text .text.*)
        . = ALIGN(4K);
        _text_end = .;
    } > RAM

    .rodata : ALIGN(4K) {
        _rodata_start = .;
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
        . = ALIGN(4K);
        _rodata_end = .;
    } > RAM

    .data : ALIGN(4K) {
        _data_start = .;
        __global_pointer$ = . + 0x800;
        *(.data .data.*)
        *(.sdata .sdata.*)
        . = ALIGN(4K);
        _data_end = .;
    } > RAM

    .bss : ALIGN(4K) {
        _bss_start = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        . = ALIGN(4K);
        _bss_end = .;
    } > RAM

    . = ALIGN(4K);
    _end = .;

    /* Boot stack + page-allocator pool — share this 1 MiB region.
     * See linker-vf2.ld for the sizing rationale (256 pages covers
     * every MMIO region; first-light maps only UART + PLIC). */
    _stack_bottom = .;
    . += 0x100000;
    _stack_top = .;

    . = ALIGN(4K);
    _heap_end = .;

    /* Runtime bump-allocator arena — 16 MiB, parallels the other
     * platforms. First-light does not instantiate any wasm, so this is
     * headroom for when the Tier-2 stack lands post-DT. */
    . = ALIGN(4K);
    _runtime_heap_start = .;
    . += 0x1000000;
    _runtime_heap_end = .;

    /* No `_boot_hart_id`: boot-r2s.S elects the boot hart at runtime
     * (first-hart-wins), so there is no compile-time hart to park on —
     * that is the whole point of the bring-up boot. If this build is
     * later switched back to the shared boot.S (once the boot hart is
     * known), restore `_boot_hart_id = <hart>` here. */

    /DISCARD/ : {
        *(.eh_frame)
        *(.comment)
    }
}
