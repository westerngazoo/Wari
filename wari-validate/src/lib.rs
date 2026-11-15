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

/// A register a driver may write, but only with bits inside
/// `allowed`, and with the bits in `required_mask` equal to
/// `required`. Pure data, like [`MmioWindow`].
///
/// `required_mask` exists for registers that pack a field the kernel
/// owns beside bits the driver owns: the DWMAC4 receive control
/// register holds both start/stop (the driver's) and the receive
/// buffer size (the kernel's). The driver keeps writing the whole
/// register, and a write that would change the kernel's field is
/// refused rather than silently corrected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteMask {
    /// Address of the 32-bit register.
    pub addr: usize,
    /// Bits the driver may set; any other bit refuses the write.
    pub allowed: u32,
    /// Bits whose value is fixed; `0` for none.
    pub required_mask: u32,
    /// The value those bits must have. Has no bits outside
    /// `required_mask`.
    pub required: u32,
}

/// May a driver write `val` to the 32-bit register at `addr`, given
/// the value guards in `masks`? A register with no guard accepts any
/// value (its *address* is licensed elsewhere); a guarded register
/// accepts only values within its mask whose fixed bits have their
/// fixed value.
///
/// ```
/// use wari_validate::{write_value_allowed, WriteMask};
/// const G: &[WriteMask] = &[
///     WriteMask { addr: 0x20, allowed: 0b0011, required_mask: 0, required: 0 },
///     WriteMask { addr: 0x28, allowed: 0b1111, required_mask: 0b1100, required: 0b0100 },
/// ];
/// assert!(write_value_allowed(0x20, 0b0001, G));   // inside the mask
/// assert!(write_value_allowed(0x20, 0, G));        // nothing set
/// assert!(!write_value_allowed(0x20, 0b0100, G));  // a bit outside it
/// assert!(write_value_allowed(0x24, 0xFFFF_FFFF, G)); // not guarded
/// assert!(write_value_allowed(0x28, 0b0101, G));   // fixed field as required
/// assert!(!write_value_allowed(0x28, 0b1001, G));  // fixed field changed
/// assert!(!write_value_allowed(0x28, 0b0001, G));  // fixed field cleared
/// ```
#[inline]
pub const fn write_value_allowed(addr: usize, val: u32, masks: &[WriteMask]) -> bool {
    let mut i = 0;
    while i < masks.len() {
        if masks[i].addr == addr {
            let m = masks[i];
            return val & !m.allowed == 0 && val & m.required_mask == m.required;
        }
        i += 1;
    }
    true
}

/// May a driver write the `width`-byte register at `addr` now, given
/// whether the kernel has attached the NIC's DMA rings?
///
/// Two board tables decide it. `needs_attach` registers are refused
/// until the rings are attached: they start the engine, which before
/// the kernel has programmed the descriptor lists would walk whatever
/// the list registers hold after reset (address 0). `frozen` registers
/// are refused once the rings are attached: they reset the engine or
/// the MAC, which clears the list registers the kernel programmed.
///
/// ```
/// use wari_validate::{attach_state_allows, MmioWindow};
/// const START: &[MmioWindow] = &[MmioWindow { base: 0x1104, len: 8 }];
/// const RESET: &[MmioWindow] = &[MmioWindow { base: 0x1000, len: 4 }];
/// assert!(!attach_state_allows(0x1104, 4, false, START, RESET)); // start before attach
/// assert!(attach_state_allows(0x1104, 4, true, START, RESET));
/// assert!(attach_state_allows(0x1000, 4, false, START, RESET));  // reset during bring-up
/// assert!(!attach_state_allows(0x1000, 4, true, START, RESET));  // reset after attach
/// assert!(attach_state_allows(0x1160, 4, true, START, RESET));   // in neither table
/// ```
#[inline]
pub const fn attach_state_allows(
    addr: usize,
    width: usize,
    attached: bool,
    needs_attach: &[MmioWindow],
    frozen: &[MmioWindow],
) -> bool {
    if attached {
        !access_in_windows(addr, width, frozen)
    } else {
        !access_in_windows(addr, width, needs_attach)
    }
}

/// Per-platform board descriptors (audit blocker B3) — every constant
/// that differs by SoC, as one record per platform. See `board`.
pub mod board;
pub mod dma;

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

        /// Registers that decide what the device's DMA touches, which
        /// the driver may not write through `net_mmio_write32`: the
        /// kernel programs them itself (ADR-002 step 1, INV-25).
        /// VirtIO 1.2 §4.2.2:
        ///
        /// - `QueueSel` (0x030), `QueueNum` (0x038), `QueueReady`
        ///   (0x044) and the legacy slots between them. Addresses
        ///   alone are not enough: a driver that could raise
        ///   `QueueNum` past the kernel's 16-entry table would have
        ///   the device walk kernel memory as descriptors and size
        ///   the available and used rings past what was validated;
        ///   one that could select a queue could re-ready it.
        /// - `QueueNotify` (0x050): the driver notifies through
        ///   `nic_queue_notify`, which orders the doorbell after the
        ///   descriptor stores.
        /// - `QueueDescLow/High` (0x080), `QueueDriverLow/High`
        ///   (0x090), `QueueDeviceLow/High` (0x0a0): the addresses.
        ///
        /// All are written by `nic_attach_queue` / `nic_queue_notify`.
        /// The driver keeps `Status`, the feature registers (value-
        /// checked, see [`WRITE_MASKS`]), the interrupt registers and
        /// the device config space.
        pub const DMA_ADDR_REGS: &[MmioWindow] = &[
            MmioWindow {
                base: 0x1000_8030,
                len: 0x18,
            },
            MmioWindow {
                base: 0x1000_8050,
                len: 0x4,
            },
            MmioWindow {
                base: 0x1000_8080,
                len: 0x8,
            },
            MmioWindow {
                base: 0x1000_8090,
                len: 0x8,
            },
            MmioWindow {
                base: 0x1000_80a0,
                len: 0x8,
            },
            // `QueueReset` (0x0c0). Needs a feature the driver cannot
            // negotiate; listed so that every queue register is the
            // kernel's, without exception.
            MmioWindow {
                base: 0x1000_80c0,
                len: 0x4,
            },
        ];

        /// Registers the driver may write only with a restricted
        /// value. One entry: `DriverFeatures` (0x020).
        ///
        /// Feature negotiation changes the shape of the rings the
        /// kernel validated. `VIRTIO_RING_F_EVENT_IDX` (bit 29) adds
        /// a field the device writes after the used ring;
        /// `VIRTIO_RING_F_INDIRECT_DESC` (bit 28) lets a descriptor
        /// point at a driver-supplied descriptor table;
        /// `VIRTIO_F_RING_PACKED` (bit 34) replaces the layout
        /// altogether. The driver may therefore accept exactly what
        /// it uses today: `VIRTIO_NET_F_MAC` (bit 5, low word) and
        /// `VIRTIO_F_VERSION_1` (bit 32 = bit 0 of the high word).
        /// The register is written once per word behind
        /// `DriverFeaturesSel`; the mask is the union of both words'
        /// allowed bits, and neither bit is a ring feature in the
        /// other word (bit 0: `VIRTIO_NET_F_CSUM`; bit 37:
        /// `VIRTIO_F_SR_IOV`), so the check needs no selector state.
        ///
        /// Second entry: `DriverFeaturesSel` (0x024) may only select
        /// word 0 or word 1. The reasoning above covers those two
        /// words; a transport that offered further feature words
        /// would otherwise let the same mask through for bits it was
        /// never checked against.
        pub const WRITE_MASKS: &[crate::WriteMask] = &[
            crate::WriteMask {
                addr: 0x1000_8020,
                allowed: (1 << 5) | 1,
                required_mask: 0,
                required: 0,
            },
            crate::WriteMask {
                addr: 0x1000_8024,
                allowed: 1,
                required_mask: 0,
                required: 0,
            },
        ];

        /// No attach-ordered registers: the VirtIO queue registers
        /// are all the kernel's ([`DMA_ADDR_REGS`]), so there is
        /// nothing for the driver to start early or reset late.
        pub const NEEDS_ATTACH: &[MmioWindow] = &[];
        /// See [`NEEDS_ATTACH`].
        pub const FROZEN_AFTER_ATTACH: &[MmioWindow] = &[];
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

        /// DMA address registers the driver may not write directly.
        /// Empty until the R2S NIC is named.
        pub const DMA_ADDR_REGS: &[MmioWindow] = &[];

        /// Value-restricted registers. None yet on this platform.
        pub const WRITE_MASKS: &[crate::WriteMask] = &[];
        /// Attach-ordered registers. None yet on this platform.
        pub const NEEDS_ATTACH: &[MmioWindow] = &[];
        /// Attach-ordered registers. None yet on this platform.
        pub const FROZEN_AFTER_ATTACH: &[MmioWindow] = &[];
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

        /// The GMAC the kernel drives: GMAC1 (`eth1`), the one every
        /// `scripts/build.sh` variant builds the driver for (`gmac1`).
        pub const GMAC_BASE: usize = 0x1604_0000;
        /// The other GMAC. Its DMA block is refused outright: the
        /// kernel programs no rings for it, so nothing it could be
        /// started on is validated.
        const GMAC0_BASE: usize = 0x1603_0000;

        use crate::dma::dwmac_reg::{
            CH0_CONTROL as DMA_CH0_CONTROL, CH1_FIRST as DMA_CH1_FIRST, CH7_END as DMA_CH7_END,
            CONTROL_START, DMA_MODE, RX_CONTROL as DMA_CH0_RX_CONTROL, RX_CONTROL_RBSZ_MASK,
            RX_RING_LEN, TX_CONTROL as DMA_CH0_TX_CONTROL, TX_LIST_HI as DMA_CH0_LIST_FIRST,
        };
        /// One past the block of list, tail and length registers
        /// (`TX_LIST_HI` 0x1110 .. `RX_RING_LEN` 0x1130). Channels
        /// 1..7 (`DMA_CH1_FIRST`..`DMA_CH7_END`): `MAC_HW_FEATURE2` on
        /// this SoC reports one channel each way, so these are not
        /// expected to exist; refusing them costs nothing if they do
        /// not and closes an unvalidated engine if they do.
        const DMA_CH0_LIST_END: usize = RX_RING_LEN + 4;

        /// `OSF` (TX bit 4); burst length (`TXPBL`/`RXPBL`, bits
        /// 21:16); `RPF` (RX bit 31). Start is bit 0 of both.
        const TX_CONTROL_OSF: u32 = 1 << 4;
        const CONTROL_PBL: u32 = 0x3F << 16;
        const RX_CONTROL_RPF: u32 = 1 << 31;

        /// Registers that decide what the GMAC's DMA touches, which
        /// the driver may not write: the kernel programs them itself
        /// (ADR-002 step 1, INV-25).
        ///
        /// - `DMA_CH0_CONTROL` (0x1100): descriptor skip length (DSL,
        ///   bits 20:18) spaces descriptors apart, so the engine would
        ///   read past the kernel's table; split header (SPH, bit 24)
        ///   follows buffer 2. Written to zero at attach.
        /// - 0x1110..0x1134: list addresses, tail pointers, ring
        ///   lengths. Written by `nic_attach_ring`, `nic_ring_kick`
        ///   and `nic_ring_rewind`.
        /// - Channels 1..7, and GMAC0's whole DMA block (see
        ///   [`GMAC0_BASE`]).
        pub const DMA_ADDR_REGS: &[MmioWindow] = &[
            MmioWindow {
                base: GMAC0_BASE + DMA_MODE,
                len: 0x1000,
            },
            MmioWindow {
                base: GMAC_BASE + DMA_CH0_CONTROL,
                len: 4,
            },
            MmioWindow {
                base: GMAC_BASE + DMA_CH0_LIST_FIRST,
                len: DMA_CH0_LIST_END - DMA_CH0_LIST_FIRST,
            },
            MmioWindow {
                base: GMAC_BASE + DMA_CH1_FIRST,
                len: DMA_CH7_END - DMA_CH1_FIRST,
            },
        ];

        /// Value-restricted registers: the two channel control
        /// registers. The driver starts and stops the engine and
        /// sets its burst length; it may not enable anything that
        /// changes the descriptor format (TX: TSO, enhanced
        /// descriptors), and in the receive control register the
        /// buffer size must stay the kernel's
        /// ([`crate::dma::DWMAC_RBSZ`]): a larger one would have the
        /// engine write past every buffer the kernel validated.
        pub const WRITE_MASKS: &[crate::WriteMask] = &[
            crate::WriteMask {
                addr: GMAC_BASE + DMA_CH0_TX_CONTROL,
                allowed: CONTROL_START | TX_CONTROL_OSF | CONTROL_PBL,
                required_mask: 0,
                required: 0,
            },
            crate::WriteMask {
                addr: GMAC_BASE + DMA_CH0_RX_CONTROL,
                allowed: CONTROL_START | RX_CONTROL_RBSZ_MASK | CONTROL_PBL | RX_CONTROL_RPF,
                required_mask: RX_CONTROL_RBSZ_MASK,
                required: crate::dma::DWMAC_RBSZ << 1,
            },
        ];

        /// Refused until the kernel has attached both rings: the two
        /// channel control registers, whose start bits would set the
        /// engine walking whatever the list registers hold after
        /// reset.
        pub const NEEDS_ATTACH: &[MmioWindow] = &[MmioWindow {
            base: GMAC_BASE + DMA_CH0_TX_CONTROL,
            len: 8,
        }];

        /// Refused once the kernel has attached the rings: everything
        /// that can reset the DMA engine and so clear the list
        /// registers the kernel programmed — `DMA_MODE` (software
        /// reset) and `DMA_SYSBUS_MODE`, and the clock, reset and
        /// syscon windows the driver needs only during bring-up.
        pub const FROZEN_AFTER_ATTACH: &[MmioWindow] = &[
            MmioWindow {
                base: GMAC_BASE + DMA_MODE,
                len: 8,
            },
            MmioWindow {
                base: 0x1023_0000,
                len: 0x1_0000,
            },
            MmioWindow {
                base: 0x1302_0000,
                len: 0x1_0000,
            },
            MmioWindow {
                base: 0x1303_0000,
                len: 0x1000,
            },
            MmioWindow {
                base: 0x1700_0000,
                len: 0x1_0000,
            },
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
