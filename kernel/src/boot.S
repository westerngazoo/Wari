# SPDX-License-Identifier: AGPL-3.0-only
#
# Wari kernel boot entry — platform-aware (QEMU virt + StarFive VF2).
#
# Cherry-picked from goose-os/kernel/src/boot.S under the "only copy
# what makes sense" discipline. The hart-id mechanism is the
# load-bearing change vs. the previous Wari boot.S, which hardcoded
# `bnez a0, _park` (parked every non-hart-0 hart). VF2 boots on
# hart 1 — that hardcode silently parked the boot hart on real
# silicon; restoring the goose-os PC-relative `_boot_hart_id` load
# unifies QEMU (hart 0) and VF2 (hart 1) under one boot.S.
#
# Why a PC-relative `.dword _boot_hart_id` and not `la _boot_hart_id`:
# the linker scripts define `_boot_hart_id` as an absolute symbol
# (0 for QEMU, 1 for VF2). `la` expands to `auipc + addi`, which
# uses `R_RISCV_PCREL_HI20`/`R_RISCV_PCREL_LO12_I` relocations whose
# 32-bit reach cannot resolve a target at absolute address 0 or 1
# — the link fails with "relocation truncated to fit". Storing the
# absolute value in a `.dword` next to the boot code uses
# `R_RISCV_64`, which has no range limit, then loads it PC-relative.
#
# OpenSBI jumps here in S-mode with:
#   a0 = hart ID
#   a1 = device tree blob (DTB) pointer

.section .text.entry
.global _start

_start:
    # --- Park non-boot harts ---
    # Compute address of the inline _boot_hart_id_addr constant
    # PC-relative, then load the absolute hart-id value through it.
1:  auipc   t0, %pcrel_hi(_boot_hart_id_addr)
    ld      t1, %pcrel_lo(1b)(t0)
    bne     a0, t1, _park

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

# --- Boot configuration data ---
# Stored next to boot code so `auipc + ld` reaches it PC-relative.
# The .dword uses R_RISCV_64 (no range limit) to embed the absolute
# value of the linker-defined `_boot_hart_id` symbol.
.align 3
_boot_hart_id_addr:
    .dword  _boot_hart_id

# --- Secondary hart parking loop ---
_park:
    wfi
    j _park
