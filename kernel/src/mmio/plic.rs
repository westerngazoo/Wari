// SPDX-License-Identifier: AGPL-3.0-only
//! Platform-Level Interrupt Controller (PLIC) driver.
//!
//! Standard RV64 PLIC at `0x0c00_0000`. Same MMIO base on QEMU virt
//! and on the StarFive JH7110 (every RISC-V spec-compliant SoC); the
//! hart-context indices differ, see `HART_CONTEXT` below.
//!
//! ## What this driver does in Phase 1b
//!
//! - **Initialize the PLIC**: set the per-hart threshold to 0 so any
//!   non-zero priority is accepted.
//! - **Enable external interrupts on the hart**: set `sie.SEIE`
//!   (bit 9) so the trap dispatcher actually sees the interrupt.
//! - **Bind an IRQ source to a `Notification` pool index**: when IRQ
//!   N fires, the trap handler calls `dispatch()`, which claims the
//!   IRQ via PLIC, looks up the bound notification, sets its signal
//!   bit, and completes the PLIC claim/complete cycle.
//! - **Provide enable/disable for individual IRQs**: drivers register
//!   their hardware IRQ at boot via `enable_irq(irq, priority)`.
//!
//! ## What this driver does NOT do in Phase 1b
//!
//! - **No dynamic binding via syscall.** Phase 1b binds IRQ →
//!   Notification only at boot (from `cap::boot::init_root_caps` once
//!   the net driver lands; until then the binding table is empty).
//!   A `sys_irq_bind` syscall could land in Phase 1c when there's a
//!   driver that needs to register IRQs at runtime.
//! - **No wait queues.** The driver-side host fn `notification_wait`
//!   is Phase-1b polling: returns `0` if the signal bit is set,
//!   `E_AGAIN` otherwise. A driver that wants to block must poll in
//!   a loop. Phase 2+ adds real wait queues to the scheduler.
//! - **No IRQ priority arbitration beyond simple PLIC priorities.**
//!   Every bound IRQ gets priority 1; threshold is 0. Multiple
//!   simultaneous IRQs are claimed in PLIC's natural order (highest
//!   priority then lowest IRQ number).
//!
//! ## Invariants
//!
//! - **INV-3** (MMIO Address Validity): every PLIC register access
//!   goes through `VolatilePtr` constructed from the fixed PLIC
//!   base. Hardware-spec-fixed.
//! - **INV-23** (IRQ Routing Determinism, NEW): `IRQ_NOTIFICATION_BINDINGS`
//!   is `static mut` bound at boot via `bind_irq_to_notification`; in
//!   Phase 1b no path mutates it after boot. The trap-to-notification
//!   mapping is therefore deterministic and read-only after init.
//!   When dynamic binding (`sys_irq_bind`) lands in Phase 1c, this
//!   invariant is replaced by INV-1 (single-hart) for the binding
//!   write path.

#![allow(dead_code)]
#![allow(clippy::doc_lazy_continuation)]

use core::arch::asm;
use core::ptr::{addr_of, addr_of_mut};

use crate::error::KernelError;
use crate::mmio::volatile::VolatilePtr;

// ─────────────────────────────────────────────────────────────────
// PLIC register layout (RV64 standard)
// ─────────────────────────────────────────────────────────────────

/// PLIC MMIO base address. Standard RV64 (QEMU virt + JH7110 both).
pub const PLIC_BASE: usize = 0x0c00_0000;

/// Maximum number of IRQ sources Phase 1b binds. PLIC supports up to
/// 1023 sources; we cap our binding table at 64 to keep static
/// memory cost low (~128 bytes).
pub const MAX_BOUND_IRQS: usize = 64;

/// S-mode hart context for the boot hart.
///
/// Each hart has two PLIC contexts: M-mode (even index) and S-mode
/// (odd index). Wari runs in S-mode after OpenSBI hands off.
///
/// - **QEMU virt** boots on hart 0 → S-mode context = 1
/// - **JH7110 (VF2)** boots on hart 1 → S-mode context = 3
///   (per the JH7110 TRM; verify on first VF2 net bring-up in
///   Phase 1c)
#[cfg(feature = "qemu")]
const HART_CONTEXT: usize = 1;
#[cfg(feature = "vf2")]
const HART_CONTEXT: usize = 3;

// Address helpers — `const fn` so callers see a constant at compile
// time when irq/context are constant.

const fn priority_addr(irq: u32) -> usize {
    PLIC_BASE + 4 * irq as usize
}

const fn pending_word_addr(irq_word: usize) -> usize {
    PLIC_BASE + 0x1000 + 4 * irq_word
}

const fn enable_word_addr(context: usize, irq_word: usize) -> usize {
    PLIC_BASE + 0x2000 + 0x80 * context + 4 * irq_word
}

const fn threshold_addr(context: usize) -> usize {
    PLIC_BASE + 0x20_0000 + 0x1000 * context
}

const fn claim_addr(context: usize) -> usize {
    PLIC_BASE + 0x20_0004 + 0x1000 * context
}

// ─────────────────────────────────────────────────────────────────
// IRQ → Notification binding table
// ─────────────────────────────────────────────────────────────────

/// Static binding table from IRQ source number to `Notification`
/// pool index. Populated at boot via `bind_irq_to_notification`,
/// read at every external-interrupt trap by `dispatch`.
///
/// `None` at index `i` means "IRQ `i` is not bound to any
/// notification" — the trap handler claims it and completes it
/// without signaling anything (defensive: a stray IRQ does not
/// crash the kernel).
static mut IRQ_NOTIFICATION_BINDINGS: [Option<u16>; MAX_BOUND_IRQS] =
    [const { None }; MAX_BOUND_IRQS];

// ─────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────

/// Initialize the PLIC for the boot hart.
///
/// Sets the S-mode threshold to 0 (accept any priority ≥ 1) and
/// enables external interrupts in `sie.SEIE`. After this, the
/// trap dispatcher will see external interrupts as
/// `SCAUSE_S_EXT = 9`.
///
/// Per-IRQ enables are NOT set here — drivers call `enable_irq`
/// for the specific sources they care about. Phase 1b's `kmain`
/// calls `init` after `trap::install`; PR Net-3 onward calls
/// `enable_irq` when the net driver binds its NIC IRQ.
pub fn init() {
    // Set threshold = 0 (accept any priority ≥ 1).
    // SAFETY: INV-3. PLIC MMIO at the fixed RV64 base.
    let threshold = unsafe { VolatilePtr::<u32>::new(threshold_addr(HART_CONTEXT) as *mut u32) };
    threshold.write(0);

    // Enable external interrupts in S-mode. `sie.SEIE` is bit 9.
    // SAFETY: INV-7. `csrs sie` is an S-mode privileged CSR write.
    unsafe {
        asm!("csrs sie, {0}", in(reg) 1usize << 9);
    }
}

/// Enable IRQ source `irq` at priority `priority` (1..=7).
///
/// Drivers call this at boot for each hardware IRQ they want to
/// receive. Priority 0 disables the IRQ; Phase 1b uses priority 1
/// for everything (no priority arbitration in our use case yet).
///
/// # Errors
///
/// `KernelError::InvalidArgument` if `irq >= MAX_BOUND_IRQS` or
/// `priority > 7`.
pub fn enable_irq(irq: u32, priority: u32) -> Result<(), KernelError> {
    if (irq as usize) >= MAX_BOUND_IRQS {
        return Err(KernelError::InvalidArgument);
    }
    if priority > 7 {
        return Err(KernelError::InvalidArgument);
    }

    // Write priority register.
    // SAFETY: INV-3.
    let prio = unsafe { VolatilePtr::<u32>::new(priority_addr(irq) as *mut u32) };
    prio.write(priority);

    // Set the corresponding bit in the per-context enable bitmap.
    let word = (irq / 32) as usize;
    let bit = irq % 32;
    // SAFETY: INV-3.
    let enable =
        unsafe { VolatilePtr::<u32>::new(enable_word_addr(HART_CONTEXT, word) as *mut u32) };
    let cur = enable.read();
    enable.write(cur | (1u32 << bit));
    Ok(())
}

/// Disable IRQ source `irq` (sets priority to 0; clears enable bit).
///
/// Mirror of `enable_irq`. Phase 1b's drivers don't currently
/// disable IRQs after enabling them; this exists for completeness
/// and for Phase 2+ teardown paths.
pub fn disable_irq(irq: u32) -> Result<(), KernelError> {
    if (irq as usize) >= MAX_BOUND_IRQS {
        return Err(KernelError::InvalidArgument);
    }
    // SAFETY: INV-3.
    let prio = unsafe { VolatilePtr::<u32>::new(priority_addr(irq) as *mut u32) };
    prio.write(0);
    let word = (irq / 32) as usize;
    let bit = irq % 32;
    // SAFETY: INV-3.
    let enable =
        unsafe { VolatilePtr::<u32>::new(enable_word_addr(HART_CONTEXT, word) as *mut u32) };
    let cur = enable.read();
    enable.write(cur & !(1u32 << bit));
    Ok(())
}

/// Bind a hardware IRQ source to a kernel `Notification` pool
/// index.
///
/// When IRQ `irq` fires, the trap handler's PLIC dispatch path
/// looks up `IRQ_NOTIFICATION_BINDINGS[irq]`, signals the
/// referenced notification, and completes the PLIC claim cycle.
///
/// Phase 1b binds at boot only (no syscall); the binding becomes
/// read-only after `init_root_caps` runs.
///
/// # Errors
///
/// `KernelError::InvalidArgument` if `irq >= MAX_BOUND_IRQS`.
pub fn bind_irq_to_notification(irq: u32, notification_pool_index: u16) -> Result<(), KernelError> {
    if (irq as usize) >= MAX_BOUND_IRQS {
        return Err(KernelError::InvalidArgument);
    }
    // SAFETY: INV-1 (single-hart) + INV-8 (post-init access; the
    // binding table is `[const { None }; N]`-initialized so any
    // pre-init access reads `None`, which is harmless).
    let bindings = unsafe { &mut *addr_of_mut!(IRQ_NOTIFICATION_BINDINGS) };
    bindings[irq as usize] = Some(notification_pool_index);
    Ok(())
}

/// Look up the notification bound to `irq`, if any. Used by tests
/// and diagnostic prints; the trap dispatcher uses the same lookup
/// inline for performance.
pub fn notification_for_irq(irq: u32) -> Option<u16> {
    if (irq as usize) >= MAX_BOUND_IRQS {
        return None;
    }
    // SAFETY: INV-1 + INV-8.
    let bindings = unsafe { &*addr_of!(IRQ_NOTIFICATION_BINDINGS) };
    bindings[irq as usize]
}

/// Trap-handler entry. Called from `trap::handle_trap` when
/// `scause` is `SCAUSE_S_EXT` (S-mode external interrupt).
///
/// Performs the PLIC claim → signal-notification → complete cycle:
///
/// 1. Read the claim register (returns the highest-priority pending
///    IRQ, or 0 if none).
/// 2. If non-zero: look up the bound notification (if any) and set
///    its signal bit; then write the IRQ back to the claim register
///    (PLIC complete, re-enables the source).
/// 3. If zero: spurious; just return.
///
/// Multiple IRQs pending at once are handled by the trap handler
/// re-entering this function on the next external interrupt; one
/// claim per dispatch.
pub fn dispatch() {
    // SAFETY: INV-3. Claim register at fixed PLIC base.
    let claim_reg = unsafe { VolatilePtr::<u32>::new(claim_addr(HART_CONTEXT) as *mut u32) };
    let irq = claim_reg.read();
    if irq == 0 {
        // Spurious interrupt; nothing to do.
        return;
    }

    // Look up the bound notification and set its signal bit.
    if let Some(notif_idx) = notification_for_irq(irq) {
        let pools = crate::cap::object_pools();
        if let Some(notif) = pools.notifications.get_mut(notif_idx) {
            // The signal bit position is the IRQ number modulo 32
            // (`signals` is a u32). Multiple IRQs above 31 will
            // collide on the same bit, but Phase 1b's bound IRQs
            // (VirtIO-net = 8) all fit in the low 32.
            let bit = irq % 32;
            notif.signals |= 1u32 << bit;
        }
    }
    // Else: IRQ fired but no notification bound. Defensive
    // behavior: complete the cycle anyway so the PLIC doesn't
    // re-fire indefinitely.

    // Complete: write the IRQ back to the claim register.
    claim_reg.write(irq);
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

// Live PLIC tests would require a QEMU integration harness to
// inject interrupts; deferred to PR Net-4 when there's a real
// IRQ source (VirtIO-net) to test against.
//
// The address-arithmetic helpers below have const-fn signatures and
// can be smoke-tested via const_assert. (Keeping the file no_std
// + binary-only for now per the PR-1 rationale around host tests
// for the kernel crate.)
const _PRIORITY_OFFSET_CHECK: () = {
    assert!(priority_addr(0) == PLIC_BASE);
    assert!(priority_addr(8) == PLIC_BASE + 32);
};

const _ENABLE_LAYOUT_CHECK: () = {
    // QEMU S-mode context (1) enable bitmap word 0.
    assert!(enable_word_addr(1, 0) == PLIC_BASE + 0x2080);
};

const _CLAIM_LAYOUT_CHECK: () = {
    // QEMU S-mode context (1) claim/complete register.
    assert!(claim_addr(1) == PLIC_BASE + 0x20_1004);
};
