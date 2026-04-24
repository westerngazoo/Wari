# SPDX-License-Identifier: AGPL-3.0-only
#
# Wari kernel boot entry — QEMU virt RV64 (Phase 0).
#
# Cherry-picked from goose-os/kernel/src/boot.S at commit
# 69d9908b6956315684c567fb95cec542062a61a5 under the "only copy what
# makes sense" discipline.
#
# Adapter edits vs. goose-os:
#   - Entry symbol is `kmain` (was `kmain` there too; kept as-is).
#   - Hart-0-only check hardcodes `a0 == 0` (QEMU boot hart) instead
#     of reading a linker-defined `_boot_hart_id`. Phase 0 is
#     QEMU-only; the VF2 variant (hart 1) returns in Phase 1 alongside
#     platform selection and its own linker script.
#   - Section `.text.entry` matches the Wari linker script's
#     `KEEP(*(.text.entry))` (goose used `.text.boot`).
#   - Stack symbol is `_stack_top` (unchanged).
#
# OpenSBI jumps here in S-mode with:
#   a0 = hart ID
#   a1 = device tree blob (DTB) pointer

.section .text.entry
.global _start

_start:
    # --- Park non-boot harts (QEMU boot hart = 0) ---
    bnez    a0, _park

    # --- Zero the .bss section ---
    la      t0, _bss_start
    la      t1, _bss_end
_zero_bss:
    beq     t0, t1, _bss_done
    sd      zero, 0(t0)
    addi    t0, t0, 8
    j       _zero_bss
_bss_done:

    # --- Set up stack ---
    la      sp, _stack_top

    # --- Jump to Rust (a0 = hart_id, a1 = dtb_addr preserved) ---
    call    kmain

    # kmain never returns; fall through to park just in case.
    j       _park

# --- Secondary hart parking loop ---
_park:
    wfi
    j       _park
