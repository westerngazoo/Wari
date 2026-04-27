/* SPDX-License-Identifier: AGPL-3.0-only */
/* Wari kernel linker script — StarFive VisionFive 2 (JH7110) target.
 *
 * Loads at 0x40200000 — U-Boot's `bootm` jumps here after OpenSBI.
 * Symbol layout matches kernel/linker.ld so kvm.rs's identity-map
 * arithmetic and the page allocator's `[_end, _heap_end)` window
 * work unchanged across both platforms.
 *
 * Cherry-picked + adapted from goose-os/kernel/linker-vf2.ld at
 * goose-os HEAD (PR 8 cherry-pick window).
 */

OUTPUT_ARCH(riscv)
ENTRY(_start)

MEMORY {
    /* JH7110 has 4–8 GB DDR; reserve 256 MB for the kernel image.
     * Boot stack + heap + runtime arena live inside this window. */
    RAM (rwx) : ORIGIN = 0x40200000, LENGTH = 256M
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

    /* Boot stack — 64 KiB, grows down. */
    _stack_bottom = .;
    . += 0x10000;
    _stack_top = .;

    . = ALIGN(4K);
    _heap_end = .;

    /* Runtime bump-allocator arena — 16 MiB, parallels QEMU layout
     * (was 4 MiB pre-Net-2). Sized for UART driver + net driver
     * (~4 MiB smoltcp buffers) + 2 Tier-1 instances per
     * docs/net-driver-design.md §7.5. */
    . = ALIGN(4K);
    _runtime_heap_start = .;
    . += 0x1000000;
    _runtime_heap_end = .;

    /* VF2 boots on hart 1 (not hart 0 like QEMU). PR 8's boot.S loads
     * this symbol and parks every hart whose id != _boot_hart_id. */
    _boot_hart_id = 1;

    /DISCARD/ : {
        *(.eh_frame)
        *(.comment)
    }
}
