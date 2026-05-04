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

    /* Boot stack + page-allocator pool — share this 1 MiB region.
     * The stack pointer starts at _stack_top and grows DOWN; the
     * page allocator hands out frames from _end (== _stack_bottom)
     * UP. Bumped from 64 KiB → 1 MiB in PR Phase-1c-1.5 because
     * adding MMIO mappings (PLIC + GMAC0) ran out of L3 page-table
     * pages with the smaller region; the kernel rebooted in a loop
     * mid-kvm::init. 1 MiB = 256 pages, enough for every MMIO
     * region in the foreseeable future. */
    _stack_bottom = .;
    . += 0x100000;
    _stack_top = .;

    . = ALIGN(4K);
    _heap_end = .;

    /* Runtime bump-allocator arena (Phase 0b, PR 4).
     *
     * Distinct from the page-allocator pool `[_end, _heap_end)`. The
     * bump allocator backs `#[global_allocator]` so wasmi can land its
     * internal `Vec`/`Box`/`String` somewhere during boot-time
     * `Module::new` + `Linker::instantiate`. Phase 0 is arena-per-boot
     * (no free); Phase 1's real allocator replaces this region.
     *
     * Size: 256 KiB (64 pages). Comfortable headroom for wasmi 0.32's
     * Module + Engine + Store state for one tiny module. Adjust if
     * boot-time OOM is observed during the gate.
     */
    . = ALIGN(4K);
    _runtime_heap_start = .;
    . += 0x1000000;  /* 16 MiB — wasmi linear memory for UART driver +
                      * net driver (~4 MiB smoltcp buffers) + 2 Tier-1
                      * hello instances. Sized per net-driver-design.md
                      * §7.5 prerequisite. Was 4 MiB pre-Net-2. */
    _runtime_heap_end = .;

    /* QEMU boots on hart 0. boot.S compares a0 against this symbol
     * (loaded via PC-relative .dword) and parks every non-matching
     * hart. Parallel to linker-vf2.ld's `_boot_hart_id = 1;`. */
    _boot_hart_id = 0;

    /DISCARD/ : {
        *(.eh_frame)
        *(.comment)
    }
}
