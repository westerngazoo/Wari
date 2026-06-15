//! Pure argument validators for the syscall boundary.
//!
//! No `unsafe`, no MMIO, no statics — host-testable. The `validate`
//! module is the standing answer to "did userspace give us coherent
//! arguments?" It never decides policy (that's the capability system);
//! it only decides shape.
//!
//! Cherry-picked from `goose-os/kernel/src/security.rs`, renamed because
//! (a) it's validation, not enforcement; (b) we want to reserve the
//! name "security" for the capability module that lands in Phase 1.

#![allow(dead_code)]

/// 4 KB page — RISC-V Sv39 leaf.
pub const PAGE_SIZE: usize = 4096;

/// Maximum number of processes. Single source of truth; referenced by
/// the process table, IPC validators, and capability table.
pub const MAX_PROCS: usize = 64;

/// Maximum number of PLIC IRQs the kernel will track.
pub const MAX_IRQS: usize = 64;

/// QEMU `virt` NS16550 register window — base address.
pub const UART_MMIO_BASE: usize = 0x1000_0000;

/// QEMU `virt` NS16550 register window — length in bytes.
pub const UART_MMIO_LEN: usize = 0x8;

// ── Net MMIO windows (Phase 1b PR Net-3) ─────────────────────────
//
// Per `docs/net-driver-design.md` §4: Phase-1b targets QEMU
// VirtIO-net only (VF2 GMAC is Phase 1c). The validator's accepted
// range cfg-gates per platform.

/// QEMU virt VirtIO-net MMIO base. The Phase-1b demo uses the 4th
/// VirtIO MMIO slot at `0x10008000`. The 0x200-byte window covers
/// both the VirtIO MMIO transport register set (offsets 0x000..0x100,
/// per VirtIO 1.2 §4.2.2) and the device-specific config region
/// (offsets 0x100..0x200, where VirtIO-net's MAC + status + MTU
/// live per VirtIO 1.2 §5.1.4). PR Net-4b widens this from 0x100 to
/// 0x200 so the driver can read the MAC.
#[cfg(feature = "qemu")]
pub const NET_MMIO_BASE: usize = 0x1000_8000;
#[cfg(feature = "qemu")]
pub const NET_MMIO_LEN:  usize = 0x200;

/// JH7110 GMAC eth1 register window. Phase-1c-10: switched from
/// GMAC0 (eth0, 0x16030000) to GMAC1 (eth1, 0x16040000) because
/// eth1 is the port connected to the OpenWrt LAN (192.168.50.x).
/// GMAC0 is on the internet-router side and is not Wari's interface.
#[cfg(feature = "vf2")]
pub const NET_MMIO_BASE: usize = 0x1604_0000;
#[cfg(feature = "vf2")]
pub const NET_MMIO_LEN:  usize = 0x1_0000;

/// User-mappable VA range. Below `USER_VA_START` is MMIO; at or above
/// `USER_VA_END` is kernel space. Phase-0 scaffold — revisit when the
/// capability system gates mappings per-module.
pub const USER_VA_START: usize = 0x5000_0000;
pub const USER_VA_END:   usize = 0x8000_0000;

/// Is `addr` page-aligned?
#[inline]
#[allow(clippy::manual_is_multiple_of)] // is_multiple_of not const-stable on pinned 1.95.0
pub const fn is_page_aligned(addr: usize) -> bool {
    addr % PAGE_SIZE == 0
}

/// Is `va` in the user-mappable VA range?
#[inline]
pub const fn is_user_va(va: usize) -> bool {
    va >= USER_VA_START && va < USER_VA_END
}

/// Is `target` a valid IPC target from `current`?
///
/// Rules:
///   - target != 0  (PID 0 is the kernel; no direct IPC to it)
///   - target < MAX_PROCS
///   - target != current  (no self-IPC; would deadlock on sync rendezvous)
#[inline]
pub const fn is_valid_ipc_target(target: usize, current: usize) -> bool {
    target > 0 && target < MAX_PROCS && target != current
}

/// Is `irq` a valid PLIC IRQ number?
#[inline]
pub const fn is_valid_irq(irq: usize) -> bool {
    irq < MAX_IRQS
}

/// Is `addr` inside the NS16550 register window?
///
/// Phase 0 grants `CAP_MMIO_UART` exclusively to the Tier-2 UART
/// driver, so this is the only MMIO surface that capability covers.
/// The validator narrows INV-3 (MMIO address validity) to this exact
/// range; any address outside it must be refused at the host-fn
/// boundary regardless of capability.
#[inline]
pub const fn is_uart_mmio_addr(addr: usize) -> bool {
    addr >= UART_MMIO_BASE && addr < UART_MMIO_BASE + UART_MMIO_LEN
}

/// Is `addr` inside the NIC register window for the active platform?
///
/// Sister to `is_uart_mmio_addr`. Phase 1b grants the `Net` cap
/// exclusively to the Tier-2 net driver; this validator narrows
/// INV-3 (MMIO address validity) and the new INV-20 (NIC MMIO
/// Window Validity) to the exact register set the driver is
/// licensed to touch.
///
/// QEMU range: `[0x10008000, 0x10008100)` — VirtIO-net MMIO.
/// VF2 ranges (Phase-1c-3b): GMAC0 + the three JH7110 clock+reset
/// generators the driver needs to bring the GMAC out of idle.
#[inline]
pub const fn is_net_mmio_addr(addr: usize) -> bool {
    if addr >= NET_MMIO_BASE && addr < NET_MMIO_BASE + NET_MMIO_LEN {
        return true;
    }
    #[cfg(feature = "vf2")]
    {
        // SYSCRG — AHB0/NOC_BUS_STG_AXI parent gates (+0x024/+0x180)
        // and GMAC1 clock gates (+0x184..+0x19C). GMAC1 lives entirely
        // in the SYSCRG clock domain (no AON CRG involvement).
        if addr >= 0x1302_0000 && addr < 0x1303_0000 {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_alignment_boundaries() {
        assert!(is_page_aligned(0));
        assert!(is_page_aligned(PAGE_SIZE));
        assert!(!is_page_aligned(1));
        assert!(!is_page_aligned(PAGE_SIZE - 1));
    }

    #[test]
    fn user_va_exclusive_endpoints() {
        assert!(is_user_va(USER_VA_START));
        assert!(!is_user_va(USER_VA_END));      // exclusive upper bound
        assert!(!is_user_va(USER_VA_START - 1));
    }

    #[test]
    fn ipc_target_rules() {
        assert!(!is_valid_ipc_target(0, 1));          // no kernel target
        assert!(!is_valid_ipc_target(2, 2));          // no self
        assert!(!is_valid_ipc_target(MAX_PROCS, 1));  // out of bounds
        assert!(is_valid_ipc_target(2, 1));           // ok
    }

    #[test]
    fn uart_mmio_window_boundaries() {
        // Inside (inclusive lower bound).
        assert!(is_uart_mmio_addr(0x1000_0000));
        // Inside (last byte of the 8-byte window).
        assert!(is_uart_mmio_addr(0x1000_0007));
        // Just past — exclusive upper bound.
        assert!(!is_uart_mmio_addr(0x1000_0008));
        // Just below — outside the window.
        assert!(!is_uart_mmio_addr(0x0FFF_FFFF));
    }
}
