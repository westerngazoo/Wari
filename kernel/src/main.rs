//! Wari kernel — Tier 0 entry point.
//!
//! This file is the kernel crate's root. It declares modules, sets up
//! the `no_std` / `no_main` environment, and provides the panic handler.
//!
//! The actual boot sequence lives in `boot.rs` as a list of named stages
//! with documented pre- and post-conditions (goose-os pattern; see
//! book Part 1, Ch 4 "Inheritance from Goose").
//!
//! Phase 0 PR 1: boot.S lands hart 0 in `kmain`, which prints the
//! banner and halts. Paging, trap vector, wasmi, and everything else
//! lands in later PRs per the approved Phase-0 plan.

#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

use core::panic::PanicInfo;

// Assemble boot.S into the crate. Keeps the build single-step (no
// build.rs, no cc crate). The linker script's `KEEP(*(.text.entry))`
// places the resulting `_start` at the load address.
core::arch::global_asm!(include_str!("boot.S"));

// Module skeleton — populated per the approved Phase-0 plan.
mod abi;
mod boot;
mod cap;
mod error;
mod ipc;
mod kputc;
mod mem;
mod mmio;
mod runtime;
mod sbi;
mod sched;
mod trap;
mod validate;

/// Build identifier string — supplied by the Makefile via the
/// `WARI_BUILD` env var. Falls back to `"dev"` for ad-hoc builds
/// (e.g. `cargo build` without going through make).
const BUILD: &str = match option_env!("WARI_BUILD") {
    Some(s) => s,
    None => "dev",
};

/// Greppable build tag baked into the kernel ELF so `wari status`
/// (and any external tooling) can extract the actual build number
/// the binary was compiled with — independent of `.build_number`,
/// which lives in the working tree and can drift. Format:
/// `WARI-BUILD-TAG-<n>` followed by a NUL byte.
///
/// `#[used]` keeps the link-time GC from stripping it; the unique
/// `WARI-BUILD-TAG-` prefix makes `strings | grep` unambiguous.
#[used]
#[no_mangle]
pub static WARI_BUILD_TAG: [u8; 64] = {
    let mut buf = [0u8; 64];
    let prefix = b"WARI-BUILD-TAG-";
    let suffix = BUILD.as_bytes();
    let mut i = 0;
    while i < prefix.len() {
        buf[i] = prefix[i];
        i += 1;
    }
    let mut j = 0;
    while j < suffix.len() && i < buf.len() - 1 {
        buf[i] = suffix[j];
        i += 1;
        j += 1;
    }
    buf
};

/// Boot hart id, selected at compile time. Mirrors the linker
/// script's `_boot_hart_id` (`0` on QEMU virt, `1` on VF2) — both
/// truths come from the same `--features vf2` build switch, so
/// keeping them in sync is a build-time concern. We don't read the
/// `a0` passed by OpenSBI because some OpenSBI ports (notably
/// VF2's StarFive build) leave `a0` with junk by the time the Rust
/// prologue saves it for kprintln, producing nonsense like
/// `hart 100000` at boot. We don't read the linker symbol itself
/// because PC-relative addressing in the medany code model can't
/// reach an absolute symbol at value 0/1 from the kernel base.
#[cfg(feature = "vf2")]
const BOOT_HART_ID: usize = 1;
#[cfg(not(feature = "vf2"))]
const BOOT_HART_ID: usize = 0;

/// Kernel entry point, called from `boot.S` after OpenSBI hands
/// control to S-mode and the boot stack is set up. Never returns.
///
/// # Safety
///
/// First Rust code to run after OpenSBI. Interrupts disabled, MMU
/// off, only the kernel image mapped. `.bss` has already been zeroed
/// by `boot.S`.
#[no_mangle]
pub extern "C" fn kmain(_hart_id: usize, _dtb_addr: usize) -> ! {
    mmio::uart_ns16550::init();
    boot::stage_banner(BUILD, BOOT_HART_ID);

    if let Err(e) = mem::kvm::init() {
        kprintln!("MMU init failed: {:?}", e);
        loop {
            // SAFETY: INV-7 — wfi is an S-mode instruction in S-mode.
            unsafe { core::arch::asm!("wfi"); }
        }
    }
    trap::install();
    mmio::plic::init();
    kprintln!("mmu OK, traps installed, plic up");

    if let Err(e) = cap::boot::init_root_caps() {
        kprintln!("cap pools init failed: {:?}", e);
        loop {
            // SAFETY: INV-7 — wfi is an S-mode instruction in S-mode.
            unsafe { core::arch::asm!("wfi"); }
        }
    }
    kprintln!("cap pools initialized");

    if let Err(e) = runtime::run_tier2_uart() {
        kprintln!("wari runtime: tier-2 uart load failed: {:?}", e);
        loop {
            // SAFETY: INV-7 — wfi is an S-mode instruction in S-mode.
            unsafe { core::arch::asm!("wfi"); }
        }
    }
    kprintln!("tier-2 uart driver loaded");

    if let Err(e) = runtime::run_tier2_net() {
        kprintln!("wari runtime: tier-2 net load failed: {:?}", e);
        loop {
            // SAFETY: INV-7 — wfi is an S-mode instruction in S-mode.
            unsafe { core::arch::asm!("wfi"); }
        }
    }
    kprintln!("tier-2 net driver loaded");

    // Register the Tier-2 driver as a "library" process and the
    // two Tier-1 hello instances as Ready tenants, then hand off
    // to the scheduler. The scheduler runs each Tier-1 in proc_id
    // order; cap isolation between them is enforced by the
    // per-instance CSpaces populated in `cap::boot::init_root_caps`.
    if let Err(e) = sched::register_library(
        cap::PROC_ID_TIER2_UART,
        cap::Tier::Two,
        cap::ModuleId::Tier2Uart,
    ) {
        kprintln!("wari sched: tier-2 uart register failed: {:?}", e);
        loop {
            // SAFETY: INV-7 — wfi is S-mode.
            unsafe { core::arch::asm!("wfi"); }
        }
    }
    if let Err(e) = sched::register_library(
        cap::PROC_ID_TIER2_NET,
        cap::Tier::Two,
        cap::ModuleId::Tier2Net,
    ) {
        kprintln!("wari sched: tier-2 net register failed: {:?}", e);
        loop {
            // SAFETY: INV-7 — wfi is S-mode.
            unsafe { core::arch::asm!("wfi"); }
        }
    }
    if let Err(e) = sched::register_tenant(
        cap::PROC_ID_TIER1_HELLO,
        cap::Tier::One,
        cap::ModuleId::Tier1Hello,
    ) {
        kprintln!("wari sched: tier-1 A register failed: {:?}", e);
        loop {
            // SAFETY: INV-7 — wfi is S-mode.
            unsafe { core::arch::asm!("wfi"); }
        }
    }
    if let Err(e) = sched::register_tenant(
        cap::PROC_ID_TIER1_HELLO_B,
        cap::Tier::One,
        cap::ModuleId::Tier1Hello,
    ) {
        kprintln!("wari sched: tier-1 B register failed: {:?}", e);
        loop {
            // SAFETY: INV-7 — wfi is S-mode.
            unsafe { core::arch::asm!("wfi"); }
        }
    }
    if let Err(e) = sched::run() {
        kprintln!("wari sched: run failed: {:?}", e);
        loop {
            // SAFETY: INV-7 — wfi is S-mode.
            unsafe { core::arch::asm!("wfi"); }
        }
    }
    kprintln!("[sched] all tenants exited, idling (Ctrl-R = reboot)");

    // Idle loop. Two responsibilities:
    //   1. Drive smoltcp's Interface::poll if the net driver is
    //      installed (Phase-1b idle-loop polling per PR Net-5b).
    //   2. Watch UART RX for Ctrl-R (0x12) → SBI cold reboot. This
    //      restores goose-os's reset-key affordance. Busy-polled
    //      because UART RX isn't yet routed through the PLIC; a
    //      future PR can wire IRQ-driven `wfi` to drop the busy
    //      cost.
    let net_up = runtime::tier2_net::is_installed();
    // UART-RX trace (debug-kernel builds): heartbeat every 10 s of
    // monotonic time. Proves (a) the idle loop is alive, (b) the
    // rdtime-derived clock runs at wall speed (stopwatch the line
    // spacing), and (c) how LSR reads under 8-bit vs 32-bit access —
    // hold any key ≥ 10 s and compare bit 0 (DR) in both lanes. See
    // uart_ns16550::debug_lsr_snapshot for why the widths may differ
    // on the JH7110 DW8250.
    const HEARTBEAT_MS: u64 = 10_000;
    let mut next_beat: u64 = 0;
    loop {
        if net_up {
            // Shared monotonic tick — also consumed by the
            // Phase-1c HTTP-demo host fns (`net_socket_accept`,
            // `net_socket_send_canned`). Pulling the counter into
            // a shared helper keeps smoltcp's perceived clock
            // monotonic across both kmain idle ticks and inline
            // host-fn driven polls.
            let _ = unsafe { runtime::tier2_net::poll(runtime::tier2_net::next_tick()) };
        }
        let now = runtime::tier2_net::next_tick();
        if now >= next_beat {
            next_beat = now + HEARTBEAT_MS;
            let (_l8, _l32) = mmio::uart_ns16550::debug_lsr_snapshot();
            kdebug!(uart, "[idle] t={}ms lsr8={:#04x} lsr32={:#010x}", now, _l8, _l32);
        }
        if let Some(b) = mmio::uart_ns16550::try_read_byte() {
            if b == 0x12 {
                kprintln!("\r\n[reboot] Ctrl-R received, restarting via SBI...");
                sbi::system_reset();
            }
            // Echo every non-Ctrl-R byte so the operator can see
            // whether keypresses reach the kernel at all (the
            // terminal → adapter → DW8250 → try_read_byte path).
            kdebug!(uart, "rx byte={:#04x} t={}ms", b, now);
        }
    }
}

/// Kernel panic handler.
///
/// Per CLAUDE R5, panics in the kernel are last-resort assertions
/// only. When one fires, we disable interrupts and halt — the system
/// is in an undefined state and attempting recovery is worse than
/// stopping.
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        // SAFETY: INV-7 — wfi is an S-mode instruction in S-mode.
        unsafe { core::arch::asm!("wfi"); }
    }
}
