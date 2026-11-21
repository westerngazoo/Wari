// SPDX-License-Identifier: AGPL-3.0-only
//! Boot-time U-mode self-test (ADR-004 step 1): run a ten-instruction
//! stub in U-mode twice — once well-behaved, once hostile — and prove
//! the kernel comes back from both.
//!
//! There are no U-mode processes yet. This module exists so the U→S
//! trap path, `sret` to U-mode and "a U-mode fault ends the process,
//! not the kernel" are exercised on every boot, on QEMU and on the
//! VF2, before step 2 builds address spaces on top of them.
//!
//! Prior art: the stub-then-return shape is xv6-riscv's first user
//! process (`initcode.S`, entered by `usertrapret`), cut down to a
//! self-test; the trap path itself follows Linux (see `trap.S`).

use core::arch::global_asm;

use crate::kprintln;
use crate::mem::kvm;
use wari_sched::utrap::{cause_name, UserExit, CAUSE_LOAD_PAGE_FAULT};

// The stub. Position-independent and memory-free except for the one
// hostile load, so it can be copied to any page and run there.
//
//   a0 on entry: 0 = behave; otherwise an address to read.
//   ecall 0 (echo) with 41 must return 42; then ecall 1 (exit) with 0.
//   A wrong echo exits with 1. The hostile read faults before exit.
global_asm!(
    r#"
    .section .rodata.umode_stub, "a"
    .balign 4
    .global umode_stub_start
    .global umode_stub_end
umode_stub_start:
    mv      s0, a0
    li      a7, 0
    li      a0, 41
    ecall
    li      t0, 42
    bne     a0, t0, 1f
    beqz    s0, 2f
    ld      t1, 0(s0)
2:  li      a7, 1
    li      a0, 0
    ecall
1:  li      a7, 1
    li      a0, 1
    ecall
umode_stub_end:
"#
);

extern "C" {
    static umode_stub_start: u8;
    static umode_stub_end: u8;
    static _text_start: u8;
    fn __umode_enter(entry: *const UserEntry) -> u64;
}

/// What `__umode_enter` reads: must match `trap.S`.
#[repr(C)]
struct UserEntry {
    pc: usize,
    sp: usize,
    a0: usize,
}

/// Run the two self-tests and log one line each, then
/// `[umode] kernel continues`. Never halts: a failed self-test is
/// logged as such and boot goes on (the integration and security tests
/// assert the expected lines).
///
/// Preconditions: `mem::kvm::init` and `trap::install` have run, and no
/// interrupt source is enabled yet (called before `plic::init`), so
/// the stub runs undisturbed.
pub fn self_test() {
    match run_stub(0) {
        Ok(Some(UserExit::Exited(0))) => kprintln!("[umode] stub: ecall 0 returned, exit 0"),
        other => kprintln!("[umode] stub: FAILED, got {:?}", other),
    }

    // SAFETY: INV-4. Reading a linker symbol's address; no deref here.
    // The stub dereferences it from U-mode, which is the point.
    let kernel_va = unsafe { &_text_start as *const u8 as usize };
    match run_stub(kernel_va) {
        Ok(Some(UserExit::Killed(CAUSE_LOAD_PAGE_FAULT))) => kprintln!(
            "[umode] hostile stub killed: {} reading kernel VA {:#x}",
            cause_name(CAUSE_LOAD_PAGE_FAULT),
            kernel_va
        ),
        other => kprintln!("[umode] hostile stub: FAILED, got {:?}", other),
    }

    kprintln!("[umode] kernel continues");
}

/// Copy the stub into fresh U=1 pages, enter it with `a0 = arg`, and
/// report how it ended. `Ok(None)` means the exit word was malformed.
fn run_stub(arg: usize) -> Result<Option<UserExit>, crate::error::KernelError> {
    // SAFETY: INV-4. Linker-visible symbols bounding the stub's bytes in
    // `.rodata`, which is mapped and immutable.
    let code = unsafe {
        let start = &umode_stub_start as *const u8;
        let len = (&umode_stub_end as *const u8 as usize) - start as usize;
        core::slice::from_raw_parts(start, len)
    };
    let pages = kvm::map_user_stub(code)?;
    let entry = UserEntry {
        pc: pages.code,
        sp: pages.stack + wari_mem::page_alloc::PAGE_SIZE,
        a0: arg,
    };
    // SAFETY: INV-2, INV-7. `__umode_enter` srets to `entry.pc` in
    // U-mode on pages mapped U=1 just above; the only way back is a
    // trap, which `_trap_entry` routes (sscratch != 0) to
    // `handle_user_trap`, and the only way out of that is resume or the
    // leave path, which restores the callee-saved registers and sstatus
    // saved here and returns the exit word. The kernel stack below the
    // saved context holds the trap frame; nothing else uses it meanwhile.
    let word = unsafe { __umode_enter(&entry) };
    kvm::unmap_user_stub(pages)?;
    Ok(UserExit::decode(word))
}
