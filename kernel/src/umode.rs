// SPDX-License-Identifier: AGPL-3.0-only
//! Boot-time U-mode self-test (ADR-004 steps 1, 2 and 3a): run small
//! stubs in U-mode, each in its own address space, and prove the kernel
//! comes back from every one.
//!
//! There are no U-mode processes yet. This module exists so the U→S
//! trap path, `sret` to U-mode, "a U-mode fault ends the process, not
//! the kernel" and "a process cannot reach another's memory" are
//! exercised on every boot, on QEMU and on the VF2, before step 3 puts
//! wasmi in U-mode on top of them. Step 3a adds the IPC buffer: a
//! stub writes to the console through it, and the kernel refuses a
//! length beyond it, another descriptor and an unknown syscall.
//!
//! Prior art: the stub-then-return shape is xv6-riscv's first user
//! process (`initcode.S`, entered by `usertrapret`), cut down to a
//! self-test; the trap path itself follows Linux (see `trap.S`); the
//! address spaces follow seL4 (see `mem::uspace`).

use core::arch::global_asm;

use crate::error::KernelError;
use crate::kprintln;
use crate::mem::uspace::{self, AddressSpace};
use wari_abi::umode::{FD_STDOUT, IPC_BUFFER_LEN, IPC_BUFFER_VA};
use wari_abi::{SYS_ECHO, SYS_EXIT, SYS_MAX, SYS_WRITE};
use wari_mem::page_alloc::{self, PAGE_SIZE};
use wari_mem::page_table::{KERNEL_RW, USER_RW, USER_RX};
use wari_mem::uspace::{USER_WINDOW_BASE, USER_WINDOW_END};
use wari_sched::utrap::{
    cause_name, UserExit, CAUSE_LOAD_PAGE_FAULT, EINVAL_RET, ENOSYS_RET, ENOTMAPPED_RET,
};

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
//   SYS_ECHO with 41 must return 42; then SYS_EXIT with 0.
//   A wrong echo exits with 1. The hostile read faults before exit.
global_asm!(
    r#"
    .section .rodata.umode_stub, "a"
    .balign 4
    .global umode_stub_start
    .global umode_stub_end
umode_stub_start:
    mv      s0, a0
    li      a7, {echo}
    li      a0, 41
    ecall
    li      t0, 42
    bne     a0, t0, 1f
    beqz    s0, 2f
    ld      t1, 0(s0)
2:  li      a7, {exit}
    li      a0, 0
    ecall
1:  li      a7, {exit}
    li      a0, 1
    ecall
umode_stub_end:
"#,
    echo = const SYS_ECHO,
    exit = const SYS_EXIT,
);

// The IPC-buffer stub (step 3a). Copies its message into the IPC
// buffer and writes it with SYS_WRITE, which must return its length;
// then checks three refusals, each an error value and not a kill:
//   SYS_WRITE longer than the buffer   -> EINVAL  (else exit 2)
//   SYS_WRITE to a descriptor not 1    -> EINVAL  (else exit 3)
//   an unknown syscall number          -> ENOSYS  (else exit 4)
// A short write exits 1. All checks passed: exit 0.
// Position-independent: the message is reached pc-relative, and
// `norelax` keeps the linker from turning that `lla` into a
// gp-relative access, which would break once the bytes are copied.
global_asm!(
    r#"
    .section .rodata.umode_write_stub, "a"
    .option push
    .option norelax
    .balign 4
    .global umode_write_stub_start
    .global umode_write_stub_end
umode_write_stub_start:
    j       1f
2:  .ascii  "[umode] hello from U-mode, through the IPC buffer\r\n"
3:  .balign 4
1:  lla     t0, 2b
    li      t1, {ipc}
    li      t2, 3b - 2b
    li      t3, 0
4:  add     t4, t0, t3
    lbu     t5, 0(t4)
    add     t4, t1, t3
    sb      t5, 0(t4)
    addi    t3, t3, 1
    bltu    t3, t2, 4b
    li      a7, {write}
    li      a0, {stdout}
    mv      a1, t2
    ecall
    li      s1, 1
    bne     a0, t2, 9f
    li      t6, {einval}
    li      a7, {write}
    li      a0, {stdout}
    li      a1, {too_long}
    ecall
    li      s1, 2
    bne     a0, t6, 9f
    li      a7, {write}
    li      a0, {stdout} + 1
    li      a1, 1
    ecall
    li      s1, 3
    bne     a0, t6, 9f
    li      a7, {unknown}
    ecall
    li      t6, {enosys}
    li      s1, 4
    bne     a0, t6, 9f
    li      s1, 0
9:  li      a7, {exit}
    mv      a0, s1
    ecall
umode_write_stub_end:
    .option pop
"#,
    ipc = const IPC_BUFFER_VA,
    write = const SYS_WRITE,
    stdout = const FD_STDOUT,
    too_long = const IPC_BUFFER_LEN + 1,
    einval = const EINVAL_RET as i64,
    unknown = const SYS_MAX + 1,
    enosys = const ENOSYS_RET as i64,
    exit = const SYS_EXIT,
);

// The no-IPC-buffer stub: SYS_WRITE of one byte must come back as
// NotMapped (exit 0); anything else exits 1. It touches no memory, so
// it runs in a space without an IPC buffer.
global_asm!(
    r#"
    .section .rodata.umode_nomap_stub, "a"
    .balign 4
    .global umode_nomap_stub_start
    .global umode_nomap_stub_end
umode_nomap_stub_start:
    li      a7, {write}
    li      a0, {stdout}
    li      a1, 1
    ecall
    li      t6, {notmapped}
    li      s1, 0
    beq     a0, t6, 1f
    li      s1, 1
1:  li      a7, {exit}
    mv      a0, s1
    ecall
umode_nomap_stub_end:
"#,
    write = const SYS_WRITE,
    stdout = const FD_STDOUT,
    notmapped = const ENOTMAPPED_RET as i64,
    exit = const SYS_EXIT,
);

extern "C" {
    static umode_stub_start: u8;
    static umode_stub_end: u8;
    static umode_write_stub_start: u8;
    static umode_write_stub_end: u8;
    static umode_nomap_stub_start: u8;
    static umode_nomap_stub_end: u8;
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
        Ok(Some(UserExit::Exited(0))) => kprintln!("[umode] stub: echo ecall returned, exit 0"),
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

    match ipc_buffer_write() {
        Ok(Some(UserExit::Exited(0))) => kprintln!(
            "[umode] ipc-buffer write: SYS_WRITE returned its length; oversize length, other descriptor and unknown syscall refused"
        ),
        other => kprintln!("[umode] ipc-buffer write: FAILED, got {:?}", other),
    }

    match ipc_buffer_missing() {
        Ok(()) => kprintln!(
            "[umode] ipc-buffer missing: SYS_WRITE with no IPC buffer, or code at its address, returned NotMapped"
        ),
        Err(why) => kprintln!("[umode] ipc-buffer missing: FAILED, got {}", why),
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

/// Run the IPC-buffer stub in a fresh space; it prints its own line
/// through `SYS_WRITE` and exits 0 only if every check held.
fn ipc_buffer_write() -> Result<Option<UserExit>, KernelError> {
    // SAFETY: INV-4. Linker symbols bounding the stub in `.rodata`.
    let code = unsafe { linker_bytes(&umode_write_stub_start, &umode_write_stub_end) };
    let space = load_process(code)?;
    Ok(run(&space, 0))
}

/// INV-31's other half: a process whose IPC-buffer VA is not mapped,
/// or holds code instead of a data page, gets `NotMapped` from
/// `SYS_WRITE` — the kernel reads nothing — and is not killed.
fn ipc_buffer_missing() -> Result<(), &'static str> {
    // SAFETY: INV-4. Linker symbols bounding the stub in `.rodata`.
    let code = unsafe { linker_bytes(&umode_nomap_stub_start, &umode_nomap_stub_end) };
    let none = load_code(code).map_err(|_| "space without IPC buffer not created")?;
    if run(&none, 0) != Some(UserExit::Exited(0)) {
        return Err("no IPC buffer: SYS_WRITE did not return NotMapped");
    }
    let mut code_there = load_code(code).map_err(|_| "second space not created")?;
    code_there
        .map_zeroed(IPC_BUFFER_VA, USER_RX)
        .map_err(|_| "code page at the IPC-buffer VA not mapped")?;
    if run(&code_there, 0) != Some(UserExit::Exited(0)) {
        return Err("code at the IPC-buffer VA: SYS_WRITE did not return NotMapped");
    }
    Ok(())
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

/// A fresh space running the self-test stub.
fn new_process() -> Result<AddressSpace, KernelError> {
    // SAFETY: INV-4. Linker-visible symbols bounding the stub's bytes in
    // `.rodata`, which is mapped and immutable.
    let code = unsafe { linker_bytes(&umode_stub_start, &umode_stub_end) };
    load_process(code)
}

/// The bytes between two linker symbols, `start` first.
///
/// # Safety
///
/// INV-4: `start` and `end` must bound one mapped, immutable region
/// with `start <= end` (a code stub in `.rodata`).
pub(crate) unsafe fn linker_bytes(start: &'static u8, end: &'static u8) -> &'static [u8] {
    let start = start as *const u8;
    let len = (end as *const u8 as usize) - start as usize;
    // SAFETY: the caller's contract (INV-4).
    unsafe { core::slice::from_raw_parts(start, len) }
}

/// A fresh space holding a copy of `code` at `CODE_VA`, an empty
/// stack page at `STACK_VA` and an empty IPC buffer at
/// `IPC_BUFFER_VA`. Shared with the step-2 measurement (`umode_bench`).
///
/// Errors: `InvalidArgument` if `code` does not fit in one page;
/// whatever `AddressSpace::create`/`map_zeroed` return.
pub(crate) fn load_process(code: &[u8]) -> Result<AddressSpace, KernelError> {
    let mut space = load_code(code)?;
    space.map_zeroed(IPC_BUFFER_VA, USER_RW)?;
    Ok(space)
}

/// `load_process` without the IPC buffer: code at `CODE_VA` and a stack
/// page, nothing else. Errors as `load_process`.
fn load_code(code: &[u8]) -> Result<AddressSpace, KernelError> {
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
pub(crate) fn run(space: &AddressSpace, arg: usize) -> Option<UserExit> {
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
