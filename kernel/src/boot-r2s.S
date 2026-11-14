# SPDX-License-Identifier: AGPL-3.0-only
#
# Wari kernel boot entry — Orange Pi R2S (Ky X1) BRING-UP variant.
#
# Differs from boot.S in exactly one way: the boot hart is ELECTED AT
# RUNTIME instead of compared against a compile-time `_boot_hart_id`.
#
# Why: during first-light we do not yet know which hart OpenSBI hands
# S-mode control to. The JH7110, for instance, boots on hart 1 because
# hart 0 is a monitor core with no S-mode; the Ky X1's boot hart is
# unknown until we read it. If boot.S's `_boot_hart_id` guesses wrong it
# parks the ACTUAL boot hart — a silent board with no banner, the worst
# bring-up failure. Electing at runtime removes the guess.
#
# Election: the first hart to atomically claim `_boot_claim` proceeds;
# every other hart parks. Robust whether OpenSBI releases a single hart
# (the standard S-mode handoff, in which case the claim is uncontended)
# or several. It does not read a0 for the decision, so it works even if
# this OpenSBI port leaves a0 with junk (a known quirk on some boards).
# a0 (hart id) and a1 (DTB) are preserved for kmain / r2s_first_light so
# the elected hart is reported for the operator to record.
#
# RETIREMENT: once first-light prints the real boot hart, set
# `_boot_hart_id` in linker-r2s.ld and switch the r2s build back to the
# shared boot.S — the stateless id-compare park, which is also warm-
# reboot-safe. This file's claim flag is NOT reset across a warm reboot;
# that is acceptable only because first-light has no reboot path (it
# halts in wfi; the sole exit is a cold reset that reloads the image and
# re-zeros `_boot_claim`).
#
# OpenSBI enters in S-mode with:
#   a0 = hart ID
#   a1 = device tree blob (DTB) pointer

.section .text.entry
.global _start

_start:
    # --- Elect the boot hart ---
    # First hart to swap 1 into _boot_claim wins (old value 0). Every
    # other hart reads back 1 and parks. amoswap.w is a single atomic
    # RMW; _boot_claim is 4-byte aligned DRAM, which supports atomics.
    la        t0, _boot_claim
    li        t1, 1
    amoswap.w t2, t1, (t0)      # t2 = previous value; 0 == we won
    bnez      t2, _park          # already claimed -> park this hart

    # --- Zero the .bss section (winner only) ---
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

# --- Boot-hart election flag ---
# Lives in the boot section (writable RAM before the MMU is enabled);
# claimed exactly once via amoswap.w. `.align 3` over-aligns it to 8
# bytes, more than the 4-byte atomic requires. Initialized to 0 in the
# loaded image, so the first cold boot starts uncontended.
.align 3
_boot_claim:
    .word   0

# --- Secondary / losing hart parking loop ---
_park:
    wfi
    j       _park
