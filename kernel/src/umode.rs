// SPDX-License-Identifier: AGPL-3.0-only
//! Boot-time U-mode self-test (ADR-004 steps 1 and 2): run a
//! ten-instruction stub in U-mode, each run in its own address space,
//! and prove the kernel comes back from every one.
//!
//! There are no U-mode processes yet. This module exists so the U→S
//! trap path, `sret` to U-mode, "a U-mode fault ends the process, not
//! the kernel" and "a process cannot reach another's memory" are
//! exercised on every boot, on QEMU and on the VF2, before step 3 puts
//! wasmi in U-mode on top of them.
//!
//! Prior art: the stub-then-return shape is xv6-riscv's first user
//! process (`initcode.S`, entered by `usertrapret`), cut down to a
//! self-test; the trap path itself follows Linux (see `trap.S`); the
//! address spaces follow seL4 (see `mem::uspace`).

use core::arch::global_asm;

use crate::error::KernelError;
use crate::kprintln;
use crate::mem::uspace::{self, AddressSpace};
use wari_mem::page_alloc::{self, PAGE_SIZE};
use wari_mem::page_table::{KERNEL_RW, USER_RW, USER_RX};
use wari_mem::uspace::{USER_WINDOW_BASE, USER_WINDOW_END};
use wari_sched::utrap::{cause_name, UserExit, CAUSE_LOAD_PAGE_FAULT};

/// Where every test process has its code page.
const CODE_VA: usize = USER_WINDOW_BASE;
/// Where every test process has its stack page (grows down from the
/// page's end).
const STACK_VA: usize = USER_WINDOW_BASE + 0x10_0000;
/// Where process B keeps the page process A tries to read.
const DATA_VA: usize = USER_WINDOW_BASE + 0x20_0000;
/// What B's page holds; checked after A's attempt.
const DATA_PATTERN: u64 = 0x5741_5249_5f42_0001; // "WARI_B" + 1

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

/// Run the self-tests and log one line each, then
/// `[umode] kernel continues`. Never halts: a failed self-test is
/// logged as such and boot goes on (the integration and security tests
/// assert the expected lines).
///
/// Preconditions: `mem::kvm::init` and `trap::install` have run, and no
/// interrupt source is enabled yet (called before `plic::init`), so
/// the stubs run undisturbed.
pub fn self_test() {
    // SAFETY: INV-1, INV-8. Single-hart boot; the allocator was
    // installed by `kvm::init`. Read-only use of the count.
    let free_before = unsafe { page_alloc::get() }.free_count();

    match run_alone(0) {
        Ok(Some(UserExit::Exited(0))) => kprintln!("[umode] stub: ecall 0 returned, exit 0"),
        other => kprintln!("[umode] stub: FAILED, got {:?}", other),
    }

    // SAFETY: INV-4. Reading a linker symbol's address; no deref here.
    // The stub dereferences it from U-mode, which is the point.
    let kernel_va = unsafe { &_text_start as *const u8 as usize };
    match run_alone(kernel_va) {
        Ok(Some(UserExit::Killed(CAUSE_LOAD_PAGE_FAULT))) => kprintln!(
            "[umode] hostile stub killed: {} reading kernel VA {:#x}",
            cause_name(CAUSE_LOAD_PAGE_FAULT),
            kernel_va
        ),
        other => kprintln!("[umode] hostile stub: FAILED, got {:?}", other),
    }

    match cross_space_read() {
        Ok(()) => kprintln!(
            "[umode] cross-space read killed: {} reading {:#x}, mapped only in the other space; its page intact",
            cause_name(CAUSE_LOAD_PAGE_FAULT),
            DATA_VA
        ),
        Err(why) => kprintln!("[umode] cross-space read: FAILED, got {}", why),
    }

    match refusals() {
        Ok(()) => kprintln!(
            "[umode] address-space refusals: remap, out-of-window and kernel-flag mappings rejected"
        ),
        Err(why) => kprintln!("[umode] address-space refusals: FAILED, got {}", why),
    }

    // SAFETY: as above.
    let free_after = unsafe { page_alloc::get() }.free_count();
    if free_after == free_before {
        kprintln!("[umode] address spaces destroyed, every page returned");
    } else {
        kprintln!(
            "[umode] address spaces: FAILED, got {} pages free, expected {}",
            free_after,
            free_before
        );
    }

    kprintln!("[umode] kernel continues");
}

/// Run the stub in a fresh space of its own with `a0 = arg`; the space
/// is destroyed when it goes out of scope.
fn run_alone(arg: usize) -> Result<Option<UserExit>, KernelError> {
    let space = new_process()?;
    Ok(run(&space, arg))
}

/// The mappings INV-28 forbids are refused: a second mapping of a VA
/// (which would orphan the first page), a VA outside the user window,
/// and kernel (non-user, GLOBAL) flags.
fn refusals() -> Result<(), &'static str> {
    let mut space = new_process().map_err(|_| "space not created")?;
    let refused = |r: Result<usize, KernelError>| r == Err(KernelError::InvalidArgument);
    if !refused(space.map_zeroed(CODE_VA, USER_RW)) {
        return Err("remap of a mapped VA accepted");
    }
    if !refused(space.map_zeroed(USER_WINDOW_END, USER_RW)) {
        return Err("VA outside the user window accepted");
    }
    if !refused(space.map_zeroed(DATA_VA, KERNEL_RW)) {
        return Err("kernel flags accepted");
    }
    Ok(())
}

/// ADR-004 step 2 gate: process A reads a VA that holds process B's
/// page. B reading it is the control (the VA is valid, in B); A must
/// die of a load page fault, and B's page must be untouched.
/// Both spaces are destroyed when they go out of scope, on every path.
fn cross_space_read() -> Result<(), &'static str> {
    let mut b = new_process().map_err(|_| "B not created")?;
    let a = new_process().map_err(|_| "A not created")?;
    let data = b
        .map_zeroed(DATA_VA, USER_RW)
        .map_err(|_| "B's data page not mapped")?;
    // SAFETY: INV-5. `data` is a fresh allocator page in the
    // identity-mapped, kernel-writable heap; the kernel writes it
    // through that U=0 identity alias, not through B's U=1 mapping.
    unsafe { core::ptr::write_volatile(data as *mut u64, DATA_PATTERN) };

    if run(&b, DATA_VA) != Some(UserExit::Exited(0)) {
        return Err("control: B could not read its own page");
    }
    if run(&a, DATA_VA) != Some(UserExit::Killed(CAUSE_LOAD_PAGE_FAULT)) {
        return Err("A was not killed by a load page fault");
    }
    // SAFETY: INV-5, as above; B is not running.
    if unsafe { core::ptr::read_volatile(data as *const u64) } != DATA_PATTERN {
        return Err("B's page changed");
    }
    Ok(())
}

/// A fresh space holding a copy of the stub at `CODE_VA` and an empty
/// stack page at `STACK_VA`.
fn new_process() -> Result<AddressSpace, KernelError> {
    // SAFETY: INV-4. Linker-visible symbols bounding the stub's bytes in
    // `.rodata`, which is mapped and immutable.
    let code = unsafe {
        let start = &umode_stub_start as *const u8;
        let len = (&umode_stub_end as *const u8 as usize) - start as usize;
        core::slice::from_raw_parts(start, len)
    };
    if code.len() > PAGE_SIZE {
        return Err(KernelError::InvalidArgument);
    }
    // On an error below, the partly built space is dropped, freeing it.
    let mut space = AddressSpace::create()?;
    let code_pa = space.map_zeroed(CODE_VA, USER_RX)?;
    // SAFETY: INV-5. `code_pa` is a fresh allocator page in the
    // identity-mapped heap, written through its U=0 identity alias;
    // `code.len() <= PAGE_SIZE` and a fresh page cannot overlap the stub
    // in `.rodata`.
    unsafe { core::ptr::copy_nonoverlapping(code.as_ptr(), code_pa as *mut u8, code.len()) };
    space.map_zeroed(STACK_VA, USER_RW)?;
    Ok(space)
}

/// Enter the stub in `space` with `a0 = arg` and report how it ended.
/// `None` means the exit word was malformed. The hart is back on the
/// kernel root when this returns.
fn run(space: &AddressSpace, arg: usize) -> Option<UserExit> {
    let entry = UserEntry {
        pc: CODE_VA,
        sp: STACK_VA + PAGE_SIZE,
        a0: arg,
    };
    space.activate();
    // SAFETY: INV-2, INV-7. `fence.i` (R6) makes the stub bytes copied
    // through the identity alias visible to instruction fetch at
    // `CODE_VA`. `__umode_enter` srets to `entry.pc` in U-mode in the
    // space just activated, which maps the kernel exactly as the kernel
    // root does; the only way back is a trap, which `_trap_entry`
    // routes (sscratch != 0) to `handle_user_trap`, and the only way
    // out of that is resume or the leave path, which restores the
    // callee-saved registers and sstatus saved here and returns the
    // exit word. The kernel stack below the saved context holds the
    // trap frame; nothing else uses it meanwhile.
    let word = unsafe {
        core::arch::asm!("fence.i");
        __umode_enter(&entry)
    };
    uspace::activate_kernel();
    UserExit::decode(word)
}
