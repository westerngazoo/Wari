/* Wari kernel linker script — QEMU virt target.
 *
 * Loads at 0x80200000 (S-mode entry after OpenSBI hands off).
 * The `boot.S` assembly scaffold sets up the stack and jumps into
 * Rust `_start`. That assembly is a Phase-0a cherry-pick from
 * goose-os/kernel/src/boot.S — see PR plan.
 *
 * Invariants this script establishes (see docs/invariants.md):
 *   - .text at fixed PA 0x80200000 (INV-3-candidate: MMIO map depends
 *     on this)
 *   - Section-boundary symbols (_text_start etc.) exported so kvm.rs
 *     can identity-map each section with the right permissions (INV-4)
 *   - _end and _heap_end exported for the page allocator (INV-4)
 */

OUTPUT_ARCH(riscv)
ENTRY(_start)

MEMORY {
    RAM (rwx) : ORIGIN = 0x80200000, LENGTH = 128M
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

    /* Boot stack — 64KB, grows down. _stack_bottom is the low end,
     * _stack_top is the high end (initial sp). */
    _stack_bottom = .;
    . += 0x10000;
    _stack_top = .;

    . = ALIGN(4K);
    _heap_end = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.comment)
    }
}
