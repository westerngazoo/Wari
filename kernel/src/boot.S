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
#   - Section `.text.entry` matches the Wari linker script's
#     `KEEP(*(.text.entry))` (goose used `.text.boot`).
#   - Stack symbol is `_stack_top` (unchanged).
#   - Boot-hart selection (PR 8): both linker scripts now define
#     `_boot_hart_id` as an absolute symbol (0 on QEMU virt, 1 on
#     VF2 / JH7110 — hart 0 is the SiFive S7 monitor with no MMU).
#     The hart compare loads it PC-relative via an embedded data word
#     (`R_RISCV_64` reaches anywhere in the address space; a direct
#     `la` of an absolute symbol can overflow the relocation range
#     for far-from-PC values). Same pattern goose-os used in ~100
#     production builds.
#
# OpenSBI jumps here in S-mode with:
#   a0 = hart ID
#   a1 = device tree blob (DTB) pointer

.section .text.entry
.global _start

_start:
    # --- Park non-boot harts ---
    # Load boot hart ID from embedded data (see bottom of file).
    la      t0, _boot_hart_data
    ld      t0, 0(t0)
    bne     a0, t0, _park

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

# --- Boot configuration data ---
# Stored in .text.entry so `la` reaches it PC-relative; the .dword
# uses an absolute (R_RISCV_64) relocation that has no range limit.
.align 3
_boot_hart_data:
    .dword _boot_hart_id
