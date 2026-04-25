/* SPDX-License-Identifier: AGPL-3.0-only */
/*
 * Wari kernel linker script — StarFive VisionFive 2 (JH7110).
 *
 * Cherry-picked + adapted from goose-os/kernel/linker-vf2.ld
 * (goose-os HEAD as of PR 8 cherry-pick).
 *
 * Key differences from QEMU virt (`linker.ld`):
 *   - RAM ORIGIN at 0x40200000 (DRAM_BASE 0x40000000 + 2 MiB OpenSBI window)
 *   - OpenSBI lives at 0x40000000 - 0x401FFFFF — DO NOT overwrite
 *   - Boot hart is hart 1 (hart 0 is the SiFive S7 monitor with no S-mode);
 *     `_boot_hart_id` is defined as an absolute symbol that boot.S loads
 *     PC-relative — same pattern as goose-os.
 *
 * Adapter edits vs. goose-os/linker-vf2.ld:
 *   - `KEEP(*(.text.entry))` matches Wari's boot.S section name
 *     (goose-os used `.text.boot`).
 *   - All section-boundary symbols Wari's `kvm.rs` consumes are exported
 *     with the same names as `linker.ld` (`_text_start`, `_text_end`,
 *     `_rodata_start/_end`, `_data_start/_end`, `_bss_start/_end`,
 *     `_stack_bottom`, `_stack_top`, `_end`, `_heap_end`,
 *     `_runtime_heap_start`, `_runtime_heap_end`).
 *   - Runtime bump-allocator arena is 4 MiB to match `linker.ld`
 *     (goose-os had no Tier-2 wasmi arena since it pre-dates Wari's
 *     two-tier WASM model).
 *   - `__global_pointer$` defined inside `.data` for the `gp` register
 *     ABI, mirroring `linker.ld`.
 *
 * Invariants this script establishes (see docs/invariants.md):
 *   - .text at fixed PA 0x40200000 (INV-3-candidate)
 *   - Section-boundary symbols exported for INV-4 consumers in kvm.rs
 *   - _end and _heap_end exported for the page allocator (INV-4)
 *   - _boot_hart_id = 1 — consumed by boot.S hart-selection check
 */

OUTPUT_ARCH(riscv)
ENTRY(_start)

MEMORY {
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

    /* Boot stack — 64 KiB, grows down. Same layout as linker.ld. */
    _stack_bottom = .;
    . += 0x10000;
    _stack_top = .;

    . = ALIGN(4K);
    _heap_end = .;

    /* Runtime bump-allocator arena (PR 4 / linker.ld parity, 4 MiB).
     * See linker.ld for the rationale; the size is identical so wasmi
     * sees the same headroom on both targets. */
    . = ALIGN(4K);
    _runtime_heap_start = .;
    . += 0x400000;
    _runtime_heap_end = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.comment)
    }
}

/* Boot hart ID — consumed PC-relative by boot.S. JH7110: hart 0 is the
 * S7 monitor (no MMU / no S-mode), harts 1-4 are U74 application cores;
 * hart 1 is the conventional boot hart per OpenSBI. */
_boot_hart_id = 1;
