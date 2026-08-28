// SPDX-License-Identifier: AGPL-3.0-only
//! Wari — pure argument validators for the syscall boundary.
//!
//! Lane B-2 of the extraction program
//! (`docs/kernel-host-testing-design.md` §4/§9), moved from
//! `kernel/src/validate.rs`. No `unsafe`, no MMIO, no statics —
//! host-testable. The validators are the standing answer to "did
//! userspace give us coherent arguments?" They never decide policy
//! (that's the capability system); they only decide shape.
//!
//! Cherry-picked originally from `goose-os/kernel/src/security.rs`,
//! renamed because (a) it's validation, not enforcement; (b) the
//! name "security" is reserved for the capability layer.
//!
//! ## Platform MMIO windows as data
//!
//! The old kernel module `#[cfg(feature)]`-selected one platform's
//! NIC register window at compile time, which meant a host test
//! build could see at most one platform's window per run (and a
//! no-feature build, none). Extraction turns the windows into
//! **data**: [`MmioWindow`] tables for *both* platforms live in
//! [`windows`], the pure predicate [`addr_in_windows`] checks an
//! address against any table, and the *kernel* shim
//! (`kernel/src/validate.rs`) keeps the `#[cfg(feature)]` selection
//! where the platform features live. Both tables are host-tested
//! here; a window-table-parameterized validator is also directly
//! provable over arbitrary tables (Phase-4b Kani).

#![cfg_attr(not(test), no_std)]

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

/// User-mappable VA range. Below `USER_VA_START` is MMIO; at or above
/// `USER_VA_END` is kernel space. Phase-0 scaffold — revisit when the
/// capability system gates mappings per-module.
pub const USER_VA_START: usize = 0x5000_0000;

/// Exclusive upper bound of the user-mappable VA range.
pub const USER_VA_END: usize = 0x8000_0000;

/// One contiguous MMIO register window: `[base, base + len)`.
///
/// A window is pure data — the platform tables in [`windows`] are
/// built from these, and [`addr_in_windows`] is the only predicate
/// over them. Keeping windows as data (rather than cfg-gated
/// constants) is what lets both platforms' tables be host-tested and,
/// later, Kani-quantified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmioWindow {
    /// First byte address inside the window (inclusive).
    pub base: usize,
    /// Window length in bytes; `base + len` is the exclusive end.
    pub len: usize,
}

impl MmioWindow {
    /// Is `addr` inside `[base, base + len)`?
    ///
    /// ```
    /// use wari_validate::MmioWindow;
    /// let w = MmioWindow { base: 0x1000, len: 0x10 };
    /// assert!(w.contains(0x1000));   // inclusive lower bound
    /// assert!(w.contains(0x100F));   // last byte
    /// assert!(!w.contains(0x1010));  // exclusive upper bound
    /// ```
    #[inline]
    pub const fn contains(&self, addr: usize) -> bool {
        addr >= self.base && addr < self.base + self.len
    }

    /// Does this window permit a `width`-byte access at `addr`?
    ///
    /// Stronger than [`contains`], which answers only for the FIRST
    /// byte. A caller that validates one byte and then performs a
    /// 4-byte access can touch up to 3 bytes past the window: an
    /// address 1-3 below the end passes `contains` and spills. This
    /// also requires natural alignment, because the SAFETY comments on
    /// the MMIO host functions asserted the driver presents aligned
    /// offsets with nothing checking it — an assumption about caller
    /// trust written into a soundness proof.
    ///
    /// See INV-20 in `docs/invariants.md`.
    ///
    /// ```
    /// use wari_validate::MmioWindow;
    /// let w = MmioWindow { base: 0x1000, len: 0x10 };
    /// assert!(w.allows(0x100C, 4));   // last aligned word
    /// assert!(!w.allows(0x100E, 4));  // would spill past the end
    /// assert!(!w.allows(0x1002, 4));  // misaligned
    /// ```
    ///
    /// # Contract
    /// - Returns `true` iff `addr` is `width`-aligned and the whole
    ///   range `[addr, addr + width)` lies inside the window.
    /// - Overflow-safe: an `addr` near `usize::MAX` returns `false`
    ///   rather than wrapping.
    /// - Panics: never.
    #[inline]
    pub const fn allows(&self, addr: usize, width: usize) -> bool {
        if width == 0 || addr % width != 0 {
            return false;
        }
        let Some(end) = addr.checked_add(width) else {
            return false;
        };
        addr >= self.base && end <= self.base + self.len
    }
}

/// Is `addr` inside any of `windows`?
///
/// The kernel calls this with its platform-selected NIC table (see
/// `kernel/src/validate.rs::is_net_mmio_addr`); tests and proofs can
/// call it with any table.
///
/// # Contract
/// - Returns `true` iff some window in `windows` contains `addr`.
/// - An empty table refuses every address (refuse-by-default).
/// - Panics: never (window construction is `const`-checked).
///
/// ```
/// use wari_validate::{addr_in_windows, MmioWindow};
/// const TABLE: &[MmioWindow] = &[MmioWindow { base: 0x2000, len: 0x100 }];
/// assert!(addr_in_windows(0x2080, TABLE));
/// assert!(!addr_in_windows(0x1FFF, TABLE));
/// assert!(!addr_in_windows(0x2080, &[])); // empty table refuses all
/// ```
#[inline]
pub const fn addr_in_windows(addr: usize, windows: &[MmioWindow]) -> bool {
    let mut i = 0;
    while i < windows.len() {
        if windows[i].contains(addr) {
            return true;
        }
        i += 1;
    }
    false
}

/// Does any window in `windows` permit a `width`-byte access at `addr`?
///
/// Width- and alignment-checked counterpart to [`addr_in_windows`].
/// Prefer this at every real access site; the byte-wise version
/// remains for range queries that are not themselves accesses.
///
/// ```
/// use wari_validate::{access_in_windows, MmioWindow};
/// const TABLE: &[MmioWindow] = &[MmioWindow { base: 0x2000, len: 0x100 }];
/// assert!(access_in_windows(0x20FC, 4, TABLE));   // last aligned word
/// assert!(!access_in_windows(0x20FE, 4, TABLE));  // spills past the end
/// assert!(!access_in_windows(0x2001, 4, TABLE));  // misaligned
/// assert!(!access_in_windows(0x2000, 4, &[]));    // empty refuses all
/// ```
#[inline]
pub const fn access_in_windows(addr: usize, width: usize, windows: &[MmioWindow]) -> bool {
    let mut i = 0;
    while i < windows.len() {
        if windows[i].allows(addr, width) {
            return true;
        }
        i += 1;
    }
    false
}

/// Per-platform board descriptors (audit blocker B3) — every constant
/// that differs by SoC, as one record per platform. See `board`.
pub mod board;

/// Per-platform NIC MMIO window tables — pure data, both platforms,
/// host-tested. The kernel's `#[cfg(feature)]` shim picks one; the
/// INV-3 / INV-20 narrowing (exact register set the Tier-2 net driver
/// is licensed to touch) is encoded here.
pub mod windows {
    use super::MmioWindow;

    /// QEMU `virt` platform windows.
    pub mod qemu {
        use super::MmioWindow;

        /// QEMU virt VirtIO-net MMIO. The Phase-1b demo uses the 4th
        /// VirtIO MMIO slot at `0x10008000`. The 0x200-byte window
        /// covers both the VirtIO MMIO transport register set
        /// (offsets 0x000..0x100, per VirtIO 1.2 §4.2.2) and the
        /// device-specific config region (offsets 0x100..0x200, where
        /// VirtIO-net's MAC + status + MTU live per VirtIO 1.2
        /// §5.1.4). PR Net-4b widened this from 0x100 to 0x200 so the
        /// driver can read the MAC.
        pub const NET_WINDOWS: &[MmioWindow] = &[MmioWindow {
            base: 0x1000_8000,
            len: 0x200,
        }];

        /// Device MMIO regions the kernel identity-maps RW at boot (B3).
        ///
        /// The full VirtIO MMIO transport range on QEMU virt —
        /// `0x10001000..0x10009000`, 8 transport slots × 0x1000. The
        /// Phase-1b net driver uses the 4th slot at `0x10008000`; the
        /// kernel maps all eight, and the driver's cap-gate
        /// ([`NET_WINDOWS`]) still narrows what it may *touch* to
        /// `0x10008000/0x200`. This is exactly why `MMIO_REGIONS` is a
        /// distinct, wider table than `NET_WINDOWS` on QEMU: the kernel
        /// maps a region so the driver can reach it; the cap-gate is the
        /// separate INV-20 decision about what it is licensed to touch.
        pub const MMIO_REGIONS: &[MmioWindow] = &[MmioWindow {
            base: 0x1000_1000,
            len: 0x8000,
        }];
    }

    /// Orange Pi R2S (Ky X1 / SpacemiT K1) platform windows.
    ///
    /// Both tables are **empty on purpose** and stay that way until the
    /// board's own device tree is read. The R2S NIC complex (2×2.5GbE +
    /// 2×GbE) is not the JH7110 GMAC, so no window can be transcribed
    /// from another board — `addr_in_windows` over an empty table
    /// refuses every address (refuse-by-default), which is exactly the
    /// right posture for a NIC we cannot yet name. See
    /// `docs/r2s-bringup.md` §4 and `board::R2S`.
    pub mod r2s {
        use super::MmioWindow;

        /// NIC MMIO windows the Tier-2 driver may touch (INV-20). Empty
        /// until `board.dts` names the MAC IP; R2S first-light is
        /// net-less, so nothing consults this yet.
        pub const NET_WINDOWS: &[MmioWindow] = &[];

        /// Device regions the kernel identity-maps at boot. Empty for
        /// first-light: UART + PLIC are mapped from the descriptor's
        /// scalar bases, and no other device is touched before the
        /// DT-blocked halt.
        pub const MMIO_REGIONS: &[MmioWindow] = &[];
    }

    /// StarFive VisionFive 2 (JH7110) platform windows.
    pub mod vf2 {
        use super::MmioWindow;

        /// JH7110 GMAC register window plus the clock/reset/syscon
        /// windows the net driver needs to bring the MAC out of idle
        /// (Phase-1c-3b, -6L, -11).
        pub const NET_WINDOWS: &[MmioWindow] = &[
            // GMAC. The Wari net driver picks GMAC0 (0x16030000) or
            // GMAC1 (0x16040000) via the `gmac1` cfg-feature. Both
            // ranges are 64 KiB and sit adjacent — covering them with
            // a single 128 KiB window is the simplest valid cap-gate
            // while keeping the dual-NIC option open (Phase-1c-11).
            // Inside the driver, `plat::NIC_BASE` picks which half is
            // touched.
            MmioWindow {
                base: 0x1603_0000,
                len: 0x2_0000,
            },
            // STGCRG — owns the STG-domain GMAC0 reset bit + bus
            // clocks.
            MmioWindow {
                base: 0x1023_0000,
                len: 0x1_0000,
            },
            // SYSCRG — owns NOC_BUS_STG_AXI which the GMAC0 AXI port
            // depends on, plus several GMAC0_* and GMAC1_* clock
            // gates.
            MmioWindow {
                base: 0x1302_0000,
                len: 0x1_0000,
            },
            // SYS SYSCON — phy-interface-select for GMAC1
            // (Phase-1c-11). Single register at +0x90; widen to a
            // 4 KiB page anyway so the kernel side doesn't need to
            // know exact offsets.
            MmioWindow {
                base: 0x1303_0000,
                len: 0x1000,
            },
            // AONCRG — read-only diagnostic for AON-domain state.
            MmioWindow {
                base: 0x1700_0000,
                len: 0x1_0000,
            },
            // AON SYSCON — phy-interface-select for GMAC0
            // (Phase-1c-6L).
            MmioWindow {
                base: 0x1701_0000,
                len: 0x1000,
            },
        ];

        /// Device MMIO regions the kernel identity-maps RW at boot (B3).
        ///
        /// The JH7110 GMAC register window plus the clock/reset/syscon
        /// windows the net driver must reach to bring the MAC out of
        /// idle. On VF2 this set is byte-identical to [`NET_WINDOWS`] —
        /// every window the driver is licensed to touch is a window the
        /// kernel maps — but the two are kept as separate fields because
        /// the relationship is *subset*, not identity (it is proper
        /// superset on QEMU), and each is its own per-board decision. The
        /// `vf2_mmio_regions_cover_net_windows` host test guards that the
        /// mapped set always covers the licensed set, so a licensed
        /// access can never page-fault on an unmapped region.
        pub const MMIO_REGIONS: &[MmioWindow] = &[
            // GMAC0 (0x16030000) + GMAC1 (0x16040000), 128 KiB.
            MmioWindow {
                base: 0x1603_0000,
                len: 0x2_0000,
            },
            // STGCRG — STG-domain GMAC0 reset bit + bus clocks.
            MmioWindow {
                base: 0x1023_0000,
                len: 0x1_0000,
            },
            // SYSCRG — NOC_BUS_STG_AXI (GMAC0 AXI parent) + GMAC0/1
            // clock gates and the GMAC1 reset register.
            MmioWindow {
                base: 0x1302_0000,
                len: 0x1_0000,
            },
            // SYS syscon — GMAC1 phy-interface-mode select (+0x90).
            MmioWindow {
                base: 0x1303_0000,
                len: 0x1000,
            },
            // AONCRG — AON-domain clocks + GMAC0 rx gate (diagnostics).
            MmioWindow {
                base: 0x1700_0000,
                len: 0x1_0000,
            },
            // AON syscon — GMAC0 phy-interface-mode select (+0x0C).
            MmioWindow {
                base: 0x1701_0000,
                len: 0x1000,
            },
        ];
    }
}

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
        assert!(!is_user_va(USER_VA_END)); // exclusive upper bound
        assert!(!is_user_va(USER_VA_START - 1));
    }

    #[test]
    fn ipc_target_rules() {
        assert!(!is_valid_ipc_target(0, 1)); // no kernel target
        assert!(!is_valid_ipc_target(2, 2)); // no self
        assert!(!is_valid_ipc_target(MAX_PROCS, 1)); // out of bounds
        assert!(is_valid_ipc_target(2, 1)); // ok
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

    #[test]
    fn empty_table_refuses_everything() {
        assert!(!addr_in_windows(0, &[]));
        assert!(!addr_in_windows(usize::MAX, &[]));
    }

    #[test]
    fn qemu_net_window_boundaries() {
        let t = windows::qemu::NET_WINDOWS;
        // Transport register set start (inclusive).
        assert!(addr_in_windows(0x1000_8000, t));
        // Device-config region (MAC lives here, Net-4b).
        assert!(addr_in_windows(0x1000_8100, t));
        // Last byte of the 0x200 window.
        assert!(addr_in_windows(0x1000_81FF, t));
        // Exclusive end.
        assert!(!addr_in_windows(0x1000_8200, t));
        // Just below the window.
        assert!(!addr_in_windows(0x1000_7FFF, t));
        // The UART window must NOT be covered by the NIC table.
        assert!(!addr_in_windows(UART_MMIO_BASE, t));
    }

    #[test]
    fn vf2_net_window_boundaries() {
        let t = windows::vf2::NET_WINDOWS;
        // GMAC0 base and GMAC1 half of the doubled window.
        assert!(addr_in_windows(0x1603_0000, t));
        assert!(addr_in_windows(0x1604_0000, t));
        // Last byte of the 128 KiB GMAC window; exclusive end.
        assert!(addr_in_windows(0x1604_FFFF, t));
        assert!(!addr_in_windows(0x1605_0000, t));
        // STGCRG, SYSCRG, SYS SYSCON, AONCRG, AON SYSCON — one probe
        // inside each, plus each exclusive end.
        assert!(addr_in_windows(0x1023_0000, t));
        assert!(!addr_in_windows(0x1024_0000, t));
        assert!(addr_in_windows(0x1302_FFFF, t));
        // SYSCRG's end abuts SYS SYSCON's base — 0x1303_0000 is
        // inside the table via the *next* window, so probe the SYS
        // SYSCON end instead.
        assert!(addr_in_windows(0x1303_0090, t)); // phy-if-select reg
        assert!(!addr_in_windows(0x1303_1000, t));
        assert!(addr_in_windows(0x1700_8000, t));
        assert!(addr_in_windows(0x1701_0000, t));
        assert!(!addr_in_windows(0x1701_1000, t));
        // Gaps between windows are refused.
        assert!(!addr_in_windows(0x1400_0000, t));
        // The QEMU VirtIO slot is NOT licensed on VF2.
        assert!(!addr_in_windows(0x1000_8000, t));
    }
}
