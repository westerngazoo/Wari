# SPDX-License-Identifier: AGPL-3.0-only
#
# Wari trap vector — S-mode traps, plus the U-mode entry path
# (ADR-004 step 1).
#
# Cherry-picked and adapted from goose-os/kernel/src/trap.S at
# 69d9908b6956315684c567fb95cec542062a61a5 under the "only copy what
# makes sense" discipline. Removed: IPC fast-path, scheduler-tick
# branch, syscall jump table.
#
# Which mode trapped is told by `sscratch` (INV-2):
#   - while the kernel runs, sscratch == 0;
#   - while U-mode runs, sscratch == the kernel stack pointer to use.
# Swapping sp with sscratch on entry therefore yields 0 for a trap from
# S-mode (swap back, keep the current stack) and the kernel stack for a
# trap from U-mode. This is the convention of Linux's
# arch/riscv/kernel/entry.S (`csrrw tp, CSR_SCRATCH, tp; bnez tp, ...`),
# with sp in place of tp because Wari keeps no per-hart data in tp.
#
# The kernel runs with interrupts disabled outside the trap vector, so
# nested traps don't occur (INV-2).
#
# TrapFrame layout — must match `trap.rs`:
#   x0..x31  at offsets 0x00..0xF8     (x0 stored as 0)
#   sepc     at offset  0x100
#   sstatus  at offset  0x108
#   scause   at offset  0x110
#   stval    at offset  0x118
# Total: 288 bytes.
#
# Kernel context saved by `__umode_enter` (UCTX_*): ra, s0..s11,
# sstatus, gp and tp — 16 words, 128 bytes. The U-mode trap frame sits
# directly below it, which is how the U-mode entry and leave paths find
# it.
#
# U-mode never sees kernel state: every GPR is zeroed before `sret`,
# and `sstatus.FS` is Off while U-mode runs, so the FP registers are
# neither readable nor writable from U-mode (an FP instruction there is
# an illegal-instruction fault, which ends the process). The kernel's
# gp/tp are reloaded from the context before any Rust runs on a U-mode
# trap, so kernel code never executes with U-mode's gp/tp.

.equ FRAME_SIZE, 288
.equ UCTX_SIZE, 128
.equ UCTX_SSTATUS, 104
.equ UCTX_GP, 112
.equ UCTX_TP, 120
.equ SSTATUS_SIE, 1 << 1
.equ SSTATUS_SPIE, 1 << 5
.equ SSTATUS_SPP, 1 << 8
.equ SSTATUS_FS, 3 << 13

# Save every GPR except x0 and x2 (sp) into the frame at sp.
.macro SAVE_GPRS
    sd      x0,   0x00(sp)
    sd      x1,   0x08(sp)
    sd      x3,   0x18(sp)
    sd      x4,   0x20(sp)
    sd      x5,   0x28(sp)
    sd      x6,   0x30(sp)
    sd      x7,   0x38(sp)
    sd      x8,   0x40(sp)
    sd      x9,   0x48(sp)
    sd      x10,  0x50(sp)
    sd      x11,  0x58(sp)
    sd      x12,  0x60(sp)
    sd      x13,  0x68(sp)
    sd      x14,  0x70(sp)
    sd      x15,  0x78(sp)
    sd      x16,  0x80(sp)
    sd      x17,  0x88(sp)
    sd      x18,  0x90(sp)
    sd      x19,  0x98(sp)
    sd      x20,  0xA0(sp)
    sd      x21,  0xA8(sp)
    sd      x22,  0xB0(sp)
    sd      x23,  0xB8(sp)
    sd      x24,  0xC0(sp)
    sd      x25,  0xC8(sp)
    sd      x26,  0xD0(sp)
    sd      x27,  0xD8(sp)
    sd      x28,  0xE0(sp)
    sd      x29,  0xE8(sp)
    sd      x30,  0xF0(sp)
    sd      x31,  0xF8(sp)
.endm

# Save sepc, sstatus, scause, stval into the frame at sp (clobbers t0).
.macro SAVE_CSRS
    csrr    t0, sepc
    sd      t0,   0x100(sp)
    csrr    t0, sstatus
    sd      t0,   0x108(sp)
    csrr    t0, scause
    sd      t0,   0x110(sp)
    csrr    t0, stval
    sd      t0,   0x118(sp)
.endm

# Write back sepc and sstatus from the frame at sp (clobbers t0). The
# handler may have changed them; scause / stval are not restored.
.macro RESTORE_CSRS
    ld      t0,   0x100(sp)
    csrw    sepc, t0
    ld      t0,   0x108(sp)
    csrw    sstatus, t0
.endm

# Restore every GPR except x0 and x2 (sp) from the frame at sp.
.macro RESTORE_GPRS
    ld      x1,   0x08(sp)
    ld      x3,   0x18(sp)
    ld      x4,   0x20(sp)
    ld      x5,   0x28(sp)
    ld      x6,   0x30(sp)
    ld      x7,   0x38(sp)
    ld      x8,   0x40(sp)
    ld      x9,   0x48(sp)
    ld      x10,  0x50(sp)
    ld      x11,  0x58(sp)
    ld      x12,  0x60(sp)
    ld      x13,  0x68(sp)
    ld      x14,  0x70(sp)
    ld      x15,  0x78(sp)
    ld      x16,  0x80(sp)
    ld      x17,  0x88(sp)
    ld      x18,  0x90(sp)
    ld      x19,  0x98(sp)
    ld      x20,  0xA0(sp)
    ld      x21,  0xA8(sp)
    ld      x22,  0xB0(sp)
    ld      x23,  0xB8(sp)
    ld      x24,  0xC0(sp)
    ld      x25,  0xC8(sp)
    ld      x26,  0xD0(sp)
    ld      x27,  0xD8(sp)
    ld      x28,  0xE0(sp)
    ld      x29,  0xE8(sp)
    ld      x30,  0xF0(sp)
    ld      x31,  0xF8(sp)
.endm

.section .text
.balign 4
.global _trap_entry

_trap_entry:
    csrrw   sp, sscratch, sp
    bnez    sp, .Lfrom_user
    # From S-mode: undo the swap (sp back, sscratch back to 0).
    csrrw   sp, sscratch, sp

    # ── S-mode trap: frame on the current kernel stack ──────────────
    addi    sp, sp, -FRAME_SIZE
    SAVE_GPRS
    addi    t0, sp, FRAME_SIZE
    sd      t0,   0x10(sp)     # frame.sp = sp at trap time
    SAVE_CSRS

    mv      a0, sp
    call    handle_trap

    RESTORE_CSRS
    RESTORE_GPRS
    addi    sp, sp, FRAME_SIZE
    sret

    # ── U-mode trap: sp is the kernel stack, sscratch holds user sp ─
.Lfrom_user:
    addi    sp, sp, -FRAME_SIZE
    SAVE_GPRS
    csrr    t0, sscratch
    sd      t0,   0x10(sp)     # frame.sp = user sp
    # Back in the kernel: sscratch = 0 so any trap taken while the
    # handler runs is classified as an S-mode trap.
    csrw    sscratch, zero
    SAVE_CSRS
    # Kernel gp/tp and the kernel's FP access (FS) back before any Rust.
    ld      gp, (FRAME_SIZE + UCTX_GP)(sp)
    ld      tp, (FRAME_SIZE + UCTX_TP)(sp)
    ld      t0, (FRAME_SIZE + UCTX_SSTATUS)(sp)
    li      t1, SSTATUS_FS
    and     t0, t0, t1
    csrs    sstatus, t0

    mv      a0, sp
    call    handle_user_trap
    bnez    a0, .Luser_leave

    # Resume the process.
    RESTORE_CSRS
    addi    t0, sp, FRAME_SIZE
    csrw    sscratch, t0       # kernel stack for its next trap
    RESTORE_GPRS
    ld      sp,   0x10(sp)     # user sp, last
    sret

    # The process ended: return from `__umode_enter` with a0 = the exit
    # word, restoring the kernel context it saved above the frame.
.Luser_leave:
    addi    sp, sp, FRAME_SIZE
    ld      t0, UCTX_SSTATUS(sp)
    csrw    sstatus, t0
    ld      gp, UCTX_GP(sp)
    ld      tp, UCTX_TP(sp)
    ld      ra,    0(sp)
    ld      s0,    8(sp)
    ld      s1,   16(sp)
    ld      s2,   24(sp)
    ld      s3,   32(sp)
    ld      s4,   40(sp)
    ld      s5,   48(sp)
    ld      s6,   56(sp)
    ld      s7,   64(sp)
    ld      s8,   72(sp)
    ld      s9,   80(sp)
    ld      s10,  88(sp)
    ld      s11,  96(sp)
    addi    sp, sp, UCTX_SIZE
    ret

# u64 __umode_enter(const UserEntry *e)  -- e = [pc, sp, a0], three words
#
# Saves the kernel's callee-saved registers, gp, tp and sstatus, points
# sscratch at them, and srets to `e->pc` in U-mode with `e->sp` and
# `e->a0`. Every other GPR is zeroed and FS is Off, so no kernel value
# leaks into U-mode. Returns (through `.Luser_leave`) the non-zero exit
# word `handle_user_trap` produced when the process ended; sstatus
# (SIE and FS included), gp and tp are as they were on entry.
.global __umode_enter
__umode_enter:
    addi    sp, sp, -UCTX_SIZE
    sd      ra,    0(sp)
    sd      s0,    8(sp)
    sd      s1,   16(sp)
    sd      s2,   24(sp)
    sd      s3,   32(sp)
    sd      s4,   40(sp)
    sd      s5,   48(sp)
    sd      s6,   56(sp)
    sd      s7,   64(sp)
    sd      s8,   72(sp)
    sd      s9,   80(sp)
    sd      s10,  88(sp)
    sd      s11,  96(sp)
    sd      gp, UCTX_GP(sp)
    sd      tp, UCTX_TP(sp)
    csrr    t0, sstatus
    sd      t0, UCTX_SSTATUS(sp)
    # No interrupt between here and sret: one taken with sscratch != 0
    # but SPP = 1 would be misrouted to the U-mode path.
    csrci   sstatus, SSTATUS_SIE
    csrw    sscratch, sp

    ld      t0,  0(a0)
    csrw    sepc, t0
    # SPP = 0: sret lands in U-mode. SPIE = 0: deterministic SIE after
    # the next trap (S-mode interrupts reach U-mode regardless). FS = Off:
    # the kernel's FP registers stay out of U-mode's reach.
    li      t0, SSTATUS_SPP | SSTATUS_SPIE | SSTATUS_FS
    csrc    sstatus, t0

    ld      t0,  8(a0)
    ld      a0, 16(a0)
    mv      sp, t0
    li      ra, 0
    li      gp, 0
    li      tp, 0
    li      t0, 0
    li      t1, 0
    li      t2, 0
    li      s0, 0
    li      s1, 0
    li      a1, 0
    li      a2, 0
    li      a3, 0
    li      a4, 0
    li      a5, 0
    li      a6, 0
    li      a7, 0
    li      s2, 0
    li      s3, 0
    li      s4, 0
    li      s5, 0
    li      s6, 0
    li      s7, 0
    li      s8, 0
    li      s9, 0
    li      s10, 0
    li      s11, 0
    li      t3, 0
    li      t4, 0
    li      t5, 0
    li      t6, 0
    sret
