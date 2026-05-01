// SPDX-License-Identifier: AGPL-3.0-only
//! Supervisor Binary Interface (SBI) calls.
//!
//! Wari runs in S-mode after OpenSBI hands off; SBI is the
//! S-mode→M-mode call interface defined by the RISC-V SBI spec
//! (https://github.com/riscv-non-isa/riscv-sbi-doc).
//!
//! Phase 1b uses exactly one SBI extension: **System Reset (SRST)**,
//! to implement Ctrl-R reboot from the kernel idle loop. Adding
//! more SBI calls (timers, IPI, console) is a Phase 2+ concern;
//! Phase 0/1 has no scheduler that needs them.
//!
//! ## Spec
//!
//! - SBI v1.0 §10 "System Reset Extension (EID #0x53525354 'SRST')"
//! - Function 0 = `sbi_system_reset(reset_type, reset_reason)`
//!   - `reset_type` = 0 (shutdown), 1 (cold reboot), 2 (warm reboot)
//!   - `reset_reason` = 0 (no reason), 1 (system failure), …
//!
//! ## Calling convention (RISC-V SBI)
//!
//! - Extension ID in `a7`
//! - Function ID in `a6`
//! - Args in `a0..=a5`
//! - On return: `a0` = SBI error code, `a1` = result value
//! - System reset does not return on success; if it returns, `a0`
//!   carries the error.

#![allow(dead_code)]

/// SBI extension ID for System Reset Extension. ASCII "SRST" in
/// little-endian: 'S'=0x53 'R'=0x52 'S'=0x53 'T'=0x54.
const SBI_EXT_SRST: usize = 0x5352_5354;

/// Function ID 0 within SRST: `sbi_system_reset`.
const SBI_FN_SYSTEM_RESET: usize = 0;

/// Reset type: cold reboot (firmware re-runs).
const RESET_TYPE_COLD_REBOOT: usize = 1;

/// Reset type: shutdown (firmware halts).
#[allow(dead_code)]
const RESET_TYPE_SHUTDOWN: usize = 0;

/// Reset reason: no reason provided (the user asked).
const RESET_REASON_NONE: usize = 0;

/// Trigger an SBI cold reboot. Does not return on success; if SBI
/// rejects the call (older firmware without SRST), falls back to a
/// `wfi` loop so the kernel halts cleanly instead of returning to
/// an indeterminate caller.
///
/// # Safety contract
///
/// - **INV-7**: `ecall` is a privileged S-mode instruction; we are
///   in S-mode.
/// - The SRST extension is supported by OpenSBI ≥ 0.7 (QEMU virt's
///   bundled OpenSBI is 1.3, VF2's StarFive-patched OpenSBI is
///   `VF2_515_v3.1.5_IMG1.19` ≈ OpenSBI 1.0+). Both have SRST.
pub fn system_reset() -> ! {
    // SAFETY: INV-7. `ecall` is permitted in S-mode and is the
    // standard RISC-V SBI call instruction. The clobber set
    // matches the SBI calling convention (a0/a1 are return; a7
    // is extension; a6 is function id).
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") SBI_EXT_SRST,
            in("a6") SBI_FN_SYSTEM_RESET,
            in("a0") RESET_TYPE_COLD_REBOOT,
            in("a1") RESET_REASON_NONE,
            // a0/a1 are clobbered on return (SBI puts error/result
            // there). We don't use the return values because the
            // call is not supposed to return on success.
            lateout("a0") _,
            lateout("a1") _,
        );
    }
    // If SBI returned (extension not implemented or refused), park.
    // SAFETY: INV-7. wfi is permitted in S-mode.
    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}
