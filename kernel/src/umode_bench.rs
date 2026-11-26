// SPDX-License-Identifier: AGPL-3.0-only
//! ADR-004 step-2 measurement gate (feature `umode-bench`): measure on
//! the board, at boot, the numbers step 3 depends on, and print them as
//! `[bench]` lines.
//!
//! 1. ASIDLEN — write ones to `satp.ASID` and read it back.
//! 2. `ecall` round trip from U-mode with a full trap frame.
//! 3. Tenant switch — `csrw satp` plus `sfence.vma`. A lower bound:
//!    the loop's working set is tiny, so it does not include refilling
//!    the TLB for a real tenant (wasmi's working set), which step 4
//!    measures under load.
//! 4. Timer: Sstc is reported by OpenSBI's own M-mode probe in its boot
//!    banner (`Boot HART ISA Extensions`), so the kernel does not probe
//!    it; it measures the fallback, the SBI `set_timer` call.
//!
//! Every count comes from `rdtime` and is averaged over many iterations
//! (the VF2 timer ticks at 4 MHz, 250 ns per tick). The arithmetic is
//! in `wari_sched::bench`.
//!
//! Diagnostic build only: `scripts/build.sh bench` (VF2) or
//! `qemu-bench`. Never part of the production profiles.

use core::arch::global_asm;

use crate::kprintln;
use crate::mem::uspace::{self, AddressSpace};
use crate::umode;
use wari_sched::bench::{
    asid_bits, avg_ns, switch_within_budget, SATP_ASID_MASK, SATP_ASID_SHIFT, SWITCH_BUDGET_NS,
};
use wari_sched::utrap::UserExit;

/// Iterations of the `ecall` and tenant-switch loops.
const ITERS: u64 = 10_000;
/// Iterations of the SBI `set_timer` loop (each is an M-mode round trip).
const TIMER_ITERS: u64 = 1_000;

/// SBI TIME extension, `sbi_set_timer` (SBI spec v2.0, chapter 6).
const SBI_EXT_TIME: usize = 0x5449_4D45;
const SBI_TIME_SET_TIMER: usize = 0;

// The ecall loop: `a0` iterations of `ecall 0` (echo), then `ecall 1`
// (exit 0). Position-independent and memory-free.
global_asm!(
    r#"
    .section .rodata.umode_bench_stub, "a"
    .balign 4
    .global umode_bench_stub_start
    .global umode_bench_stub_end
umode_bench_stub_start:
    mv      s0, a0
1:  li      a7, 0
    li      a0, 0
    ecall
    addi    s0, s0, -1
    bnez    s0, 1b
    li      a7, 1
    li      a0, 0
    ecall
umode_bench_stub_end:
"#
);

extern "C" {
    static umode_bench_stub_start: u8;
    static umode_bench_stub_end: u8;
}

/// Run the four measurements and print one `[bench]` line each, then
/// the gate verdict. Never halts; a measurement that cannot run says so.
///
/// Preconditions: as `umode::self_test` (MMU and traps up, no interrupt
/// source enabled), and called right after it.
pub fn run() {
    let hz = crate::board::BOARD.timebase_hz;
    kprintln!("[bench] timebase {} Hz", hz);

    let readback = probe_asid();
    kprintln!(
        "[bench] asidlen {} (satp.ASID read back {:#x})",
        asid_bits(readback),
        (readback & SATP_ASID_MASK) >> SATP_ASID_SHIFT
    );

    match ecall_ticks() {
        Some(ticks) => print_avg("ecall round trip", ticks, hz, ITERS),
        None => kprintln!("[bench] ecall round trip: FAILED, stub did not exit 0"),
    }

    let switch_ns = match switch_ticks() {
        Some(ticks) => {
            print_avg("tenant switch (satp + sfence.vma)", ticks, hz, 2 * ITERS);
            avg_ns(ticks, hz, 2 * ITERS)
        }
        None => {
            kprintln!("[bench] tenant switch: FAILED, address spaces not created");
            None
        }
    };

    match set_timer_ticks() {
        Ok(ticks) => print_avg("sbi_set_timer", ticks, hz, TIMER_ITERS),
        Err(error) => kprintln!("[bench] sbi_set_timer: FAILED, SBI error {}", error),
    }
    kprintln!("[bench] sstc: see OpenSBI's `Boot HART ISA Extensions` line above");

    match switch_ns {
        Some(ns) if switch_within_budget(ns) => {
            kprintln!(
                "[bench] gate: tenant switch {} ns <= {} ns: PASS",
                ns,
                SWITCH_BUDGET_NS
            )
        }
        Some(ns) => kprintln!(
            "[bench] gate: tenant switch {} ns > {} ns: REVIEW THE DESIGN",
            ns,
            SWITCH_BUDGET_NS
        ),
        None => kprintln!("[bench] gate: no switch measurement"),
    }
}

fn print_avg(what: &str, ticks: u64, hz: u64, iters: u64) {
    match avg_ns(ticks, hz, iters) {
        Some(ns) => kprintln!(
            "[bench] {}: {} ns avg over {} ({} ticks)",
            what,
            ns,
            iters,
            ticks
        ),
        None => kprintln!("[bench] {}: FAILED, no rate", what),
    }
}

fn rdtime() -> u64 {
    let t: u64;
    // SAFETY: INV-7. `rdtime` reads the S-mode-readable `time` CSR; no
    // side effects.
    unsafe { core::arch::asm!("rdtime {0}", out(reg) t) };
    t
}

/// Write all ones to `satp.ASID` (same root, same mode), read `satp`
/// back, and restore it. Returns the value read back.
fn probe_asid() -> u64 {
    let readback: u64;
    // SAFETY: INV-7. S-mode CSR accesses. The root and mode do not
    // change, and every kernel PTE is GLOBAL, so kernel fetches and
    // accesses translate the same under any ASID; nothing in the user
    // window is touched while the probe ASID is live.
    //
    // Memory ordering (R6): the closing `sfence.vma zero, zero` drops
    // any translation cached under the probe ASID once `satp` is back.
    unsafe {
        core::arch::asm!(
            "csrr {orig}, satp",
            "or   {tmp}, {orig}, {mask}",
            "csrw satp, {tmp}",
            "csrr {back}, satp",
            "csrw satp, {orig}",
            "sfence.vma zero, zero",
            orig = out(reg) _,
            tmp = out(reg) _,
            back = out(reg) readback,
            mask = in(reg) SATP_ASID_MASK,
        );
    }
    readback
}

/// Ticks for `ITERS` echo `ecall`s from U-mode (plus one entry and one
/// exit, amortized). `None` if the stub did not exit 0.
fn ecall_ticks() -> Option<u64> {
    // SAFETY: INV-4. Linker symbols bounding the stub in `.rodata`.
    let code = unsafe { umode::linker_bytes(&umode_bench_stub_start, &umode_bench_stub_end) };
    let space = umode::load_process(code).ok()?;
    let start = rdtime();
    let exit = umode::run(&space, ITERS as usize);
    let ticks = rdtime().wrapping_sub(start);
    (exit == Some(UserExit::Exited(0))).then_some(ticks)
}

/// Ticks for `2 * ITERS` switches between two address spaces.
fn switch_ticks() -> Option<u64> {
    let a = AddressSpace::create().ok()?;
    let b = AddressSpace::create().ok()?;
    let start = rdtime();
    for _ in 0..ITERS {
        a.activate();
        b.activate();
    }
    let ticks = rdtime().wrapping_sub(start);
    uspace::activate_kernel();
    Some(ticks)
}

/// Ticks for `TIMER_ITERS` SBI `set_timer` calls, each arming the timer
/// at `u64::MAX` (never fires; `sie.STIE` is off anyway).
///
/// Errors: the SBI error code of the first call that does not return
/// `SBI_SUCCESS` (0). A failing call returns early from M-mode, so its
/// time would look like a fast, valid measurement.
fn set_timer_ticks() -> Result<u64, i64> {
    let start = rdtime();
    for _ in 0..TIMER_ITERS {
        let error = sbi_set_timer(u64::MAX);
        if error != 0 {
            return Err(error);
        }
    }
    Ok(rdtime().wrapping_sub(start))
}

/// SBI `set_timer(stime)`; returns the SBI error code (0 = success).
fn sbi_set_timer(stime: u64) -> i64 {
    let error: i64;
    // SAFETY: INV-7. An SBI call from S-mode; per the SBI spec it
    // preserves every register but a0 and a1. `set_timer(u64::MAX)` only
    // programs the timer compare, which nothing in the kernel uses, and
    // clears any pending supervisor timer interrupt.
    unsafe {
        core::arch::asm!(
            "ecall",
            inlateout("a0") stime => error,
            lateout("a1") _,
            in("a6") SBI_TIME_SET_TIMER,
            in("a7") SBI_EXT_TIME,
        );
    }
    error
}
