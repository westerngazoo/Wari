# SPDX-License-Identifier: AGPL-3.0-only
#
# Wari trap vector — S-mode only (Phase 0).
#
# Cherry-picked and adapted from goose-os/kernel/src/trap.S at
# 69d9908b6956315684c567fb95cec542062a61a5 under the "only copy what
# makes sense" discipline. Removed: U-mode entry path, sscratch swap
# convention, IPC fast-path, scheduler-tick branch, syscall jump table.
#
# Phase 0 has no userspace — every trap originates in S-mode. The
# kernel runs with interrupts disabled outside the trap vector, so
# nested traps don't occur. That collapses the goose-os two-mode
# dance into a straight save / call / restore.
#
# TrapFrame layout — must match `trap.rs`:
#   x0..x31  at offsets 0x00..0xF8     (x0 stored as 0)
#   sepc     at offset  0x100
#   sstatus  at offset  0x108
#   scause   at offset  0x110
#   stval    at offset  0x118
# Total: 288 bytes.

.section .text
.balign 4
.global _trap_entry

_trap_entry:
    # Allocate the trap frame on the current (kernel) stack.
    addi    sp, sp, -288

    # Store x0 as 0 for index symmetry. (x0 is hardwired to zero, so
    # this is just a placeholder so the Rust struct field offsets
    # line up with xN * 8.)
    sd      x0,   0x00(sp)

    sd      x1,   0x08(sp)     # ra
    # x2 is sp — we'll save the *original* sp below using t0.
    sd      x3,   0x18(sp)     # gp
    sd      x4,   0x20(sp)     # tp
    # Save t0 (x5) early because we need a temp for sp + CSR reads.
    sd      x5,   0x28(sp)     # t0
    sd      x6,   0x30(sp)     # t1
    sd      x7,   0x38(sp)     # t2
    sd      x8,   0x40(sp)     # s0
    sd      x9,   0x48(sp)     # s1
    sd      x10,  0x50(sp)     # a0
    sd      x11,  0x58(sp)     # a1
    sd      x12,  0x60(sp)     # a2
    sd      x13,  0x68(sp)     # a3
    sd      x14,  0x70(sp)     # a4
    sd      x15,  0x78(sp)     # a5
    sd      x16,  0x80(sp)     # a6
    sd      x17,  0x88(sp)     # a7
    sd      x18,  0x90(sp)     # s2
    sd      x19,  0x98(sp)     # s3
    sd      x20,  0xA0(sp)     # s4
    sd      x21,  0xA8(sp)     # s5
    sd      x22,  0xB0(sp)     # s6
    sd      x23,  0xB8(sp)     # s7
    sd      x24,  0xC0(sp)     # s8
    sd      x25,  0xC8(sp)     # s9
    sd      x26,  0xD0(sp)     # s10
    sd      x27,  0xD8(sp)     # s11
    sd      x28,  0xE0(sp)     # t3
    sd      x29,  0xE8(sp)     # t4
    sd      x30,  0xF0(sp)     # t5
    sd      x31,  0xF8(sp)     # t6

    # Recover original sp = current sp + 288 and store it.
    addi    t0, sp, 288
    sd      t0,   0x10(sp)     # frame.sp

    # Save CSRs into the frame.
    csrr    t0, sepc
    sd      t0,   0x100(sp)
    csrr    t0, sstatus
    sd      t0,   0x108(sp)
    csrr    t0, scause
    sd      t0,   0x110(sp)
    csrr    t0, stval
    sd      t0,   0x118(sp)

    # Call Rust dispatcher with a0 = &mut TrapFrame.
    mv      a0, sp
    call    handle_trap

    # Restore CSRs that the handler may have updated (sepc / sstatus
    # are the ones the spec lets us write; scause / stval are read-only
    # in practice for sret semantics).
    ld      t0,   0x100(sp)
    csrw    sepc, t0
    ld      t0,   0x108(sp)
    csrw    sstatus, t0

    # Restore GPRs.
    ld      x1,   0x08(sp)     # ra
    # skip x2 (sp) — recovered below by frame deallocation
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

    # Deallocate the trap frame and return.
    addi    sp, sp, 288
    sret
