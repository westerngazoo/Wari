// SPDX-License-Identifier: AGPL-3.0-only
//! Per-platform board descriptor — one record per SoC, as data.
//!
//! One concern: hold everything the kernel needs to know that *differs
//! by board* in a single value per platform, so adding a third board
//! is `pub const R2S: BoardDescriptor = …` plus one `cfg` arm in the
//! kernel — not an edit across ~15 scattered `cfg` sites and the
//! constants that only *coincide* between QEMU and the VF2 today.
//! See `docs/rfcs/board-descriptor.md`.
//!
//! ## The descriptor is the single source; the kernel reads it
//!
//! `kernel/src/board.rs` selects one instance behind a single `cfg`,
//! and every per-SoC constant site (`uart_ns16550`, `plic`, `main`,
//! `kvm`, `tier2_net`) reads `board::BOARD.field` instead of its own
//! `cfg`-gated literal. The [`tests`] below pin every field to the
//! value the kernel expects, with the source line cited — so the
//! migration is behavior-preserving (a drift surfaces as a QEMU/VF2
//! integration failure) and this file is the *one* place a value is
//! written. Adding a third board is one more `const` here plus one arm
//! in the kernel selector.
//!
//! ## The one field that is a decision, not a transcription
//!
//! [`BoardDescriptor::dma_coherent`]. The JH7110 GMAC path is
//! coherent — proven on silicon (the ping loss was a missing
//! `volatile`, not coherence). QEMU's VirtIO is coherent. The Ky X1's
//! DMA-master coherence is *unverified* and is a per-SoC property, so
//! a third board sets this flag from a bring-up experiment, and the
//! CMO hooks a later brick adds are no-ops when it is `true`.

use crate::MmioWindow;

/// Everything about a board that differs from another board.
///
/// One `const` instance per supported platform ([`QEMU`], [`VF2`]);
/// the kernel selects one behind a single `cfg`. Pure data — no
/// `unsafe`, no MMIO, host-constructible and host-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardDescriptor {
    /// Stable identifier, for banners and diagnostics.
    pub name: &'static str,
    /// Console UART MMIO base.
    pub uart_base: usize,
    /// Byte distance between consecutive UART registers: `1` for a
    /// true NS16550 (QEMU), `4` for the DesignWare 8250 (JH7110). A
    /// wrong stride kills the console before any diagnostic prints —
    /// the single worst bring-up bug, which is why it is a named field.
    pub uart_stride: usize,
    /// PLIC MMIO base.
    pub plic_base: usize,
    /// The boot hart's S-mode PLIC context. NOT `2*hart+1` in general:
    /// contexts are numbered by the device tree's `interrupts-extended`
    /// order, and JH7110 hart 0 (the S7 monitor core) has no S-mode, so
    /// the U74 the kernel boots on is context 2, not 3.
    pub plic_hart_context: usize,
    /// The hart id the kernel boots on (OpenSBI hands off there).
    pub boot_hart_id: usize,
    /// DRAM load origin — cross-checks the linker script's `ORIGIN`.
    pub dram_origin: usize,
    /// `rdtime` frequency in Hz, for the millisecond clock.
    pub timebase_hz: u64,
    /// PLIC source number for UART0 — the console IRQ. QEMU virt uses
    /// 10; the JH7110 uses 32 (per its interrupt map). A different SoC
    /// has its own; wrong, and Ctrl-R (the only bound console IRQ)
    /// silently stops working.
    pub uart_irq: u32,
    /// The NIC MMIO windows the Tier-2 net driver is licensed to touch
    /// (INV-20). Reuses the existing per-platform window tables.
    pub net_windows: &'static [MmioWindow],
    /// The device MMIO regions the kernel identity-maps RW at boot,
    /// beyond UART and PLIC (which map from their own scalar fields).
    /// A *superset* of [`net_windows`]: the kernel maps a region so the
    /// driver can reach it; `net_windows` is the narrower INV-20 cap-gate
    /// for what it may touch. They coincide on VF2 and differ on QEMU
    /// (mapped `0x8000` ⊋ licensed `0x200`), so this is its own field.
    pub mmio_regions: &'static [MmioWindow],
    /// Is the NIC's DMA master cache-coherent with the CPU on this SoC?
    /// `true` on JH7110 (proven) and QEMU; a third board decides this
    /// by experiment. When `false`, the net driver's (future) CMO hooks
    /// clean/invalidate around DMA; when `true`, they are no-ops.
    pub dma_coherent: bool,
}

/// QEMU `virt` RV64. Values pinned in [`tests`] to the kernel's HEAD.
pub const QEMU: BoardDescriptor = BoardDescriptor {
    name: "qemu-virt",
    uart_base: 0x1000_0000,
    uart_stride: 1,
    plic_base: 0x0c00_0000,
    plic_hart_context: 1,
    boot_hart_id: 0,
    dram_origin: 0x8020_0000,
    timebase_hz: 10_000_000,
    uart_irq: 10,
    net_windows: crate::windows::qemu::NET_WINDOWS,
    mmio_regions: crate::windows::qemu::MMIO_REGIONS,
    dma_coherent: true,
};

/// StarFive VisionFive 2 (JH7110, RV64GC).
pub const VF2: BoardDescriptor = BoardDescriptor {
    name: "starfive-vf2",
    uart_base: 0x1000_0000,
    uart_stride: 4,
    plic_base: 0x0c00_0000,
    plic_hart_context: 2,
    boot_hart_id: 1,
    dram_origin: 0x4020_0000,
    timebase_hz: 4_000_000,
    uart_irq: 32,
    net_windows: crate::windows::vf2::NET_WINDOWS,
    mmio_regions: crate::windows::vf2::MMIO_REGIONS,
    dma_coherent: true,
};

/// Orange Pi R2S — Ky X1 / SpacemiT K1, RV64GCVB, Sv39 (roadmap p3b).
///
/// **Partial and provisional.** The board's own device tree is the
/// final word; this record exists so the R2S build target compiles and
/// so first-light can prove the *sourced* constants on silicon. Fields
/// split into two classes:
///
/// - **Sourced** from mainline Linux `spacemit,k1` DT (pinned in
///   [`tests`]): `uart_base`, `uart_stride`, `plic_base`, `timebase_hz`,
///   `dram_origin`. R2S first-light exercises exactly these (banner +
///   MMU) and nothing else.
/// - **Sentinel / DT-blocked**: `plic_hart_context`, `uart_irq`,
///   `net_windows`, `mmio_regions`, `dma_coherent` — deliberately
///   invalid or empty until read from *this* board's `board.dts`. The
///   kernel halts (`main.rs::r2s_first_light`) before any of them is
///   touched, so a sentinel can never drive silicon wrongly.
///
/// See `docs/r2s-bringup.md`.
pub const R2S: BoardDescriptor = BoardDescriptor {
    name: "orangepi-r2s",
    // ── Sourced from mainline spacemit,k1 DT (provisional) ──
    uart_base: 0xd401_7000,   // serial@d4017000, "spacemit,k1-uart"
    uart_stride: 4,           // reg-shift=<2>, reg-io-width=<4> (DW8250 family)
    plic_base: 0xe000_0000,   // plic@e0000000, "sifive,plic-1.0.0", ndev=159
    dram_origin: 0x0020_0000, // DRAM base 0x0 + 2 MiB (VF2 convention); linker ORIGIN cross-check
    timebase_hz: 24_000_000,  // timebase-frequency=<24000000>
    // ── DT-blocked: read from board.dts before first-light goes past
    //    the banner. See `r2s_dt_blocked_fields_are_sentinels`. ──
    boot_hart_id: 0,          // provisional banner label; first-light prints the LIVE boot hart
    plic_hart_context: usize::MAX, // SENTINEL — K1 pattern 2*hart+1; exact value from board.dts
    uart_irq: u32::MAX,       // SENTINEL — uart0 `interrupts=<N>` from board.dts
    net_windows: crate::windows::r2s::NET_WINDOWS,   // empty until DT names the MAC
    mmio_regions: crate::windows::r2s::MMIO_REGIONS, // empty: UART+PLIC map from scalars
    dma_coherent: false,      // UNKNOWN — conservative default; set by bring-up experiment
};

#[cfg(test)]
mod tests {
    use super::*;

    // Each assertion pins a descriptor field to the value the kernel
    // uses at HEAD, with the source cited. Slice 2 makes the kernel
    // READ these; until then this is the single transcription point,
    // and a drift between here and the kernel would surface as a
    // QEMU/VF2 integration failure the moment slice 2 lands.

    #[test]
    fn qemu_matches_kernel_head() {
        // kernel/src/mmio/uart_ns16550.rs:38,48
        assert_eq!(QEMU.uart_base, 0x1000_0000);
        assert_eq!(QEMU.uart_stride, 1);
        // kernel/src/mmio/plic.rs:64,97
        assert_eq!(QEMU.plic_base, 0x0c00_0000);
        assert_eq!(QEMU.plic_hart_context, 1);
        // kernel/src/main.rs:115 (BOOT_HART_ID, not(vf2))
        assert_eq!(QEMU.boot_hart_id, 0);
        // kernel/linker.ld:20
        assert_eq!(QEMU.dram_origin, 0x8020_0000);
        // kernel/src/runtime/tier2_net.rs:119
        assert_eq!(QEMU.timebase_hz, 10_000_000);
        // kernel/src/mmio/plic.rs UART_IRQ (qemu)
        assert_eq!(QEMU.uart_irq, 10);
        assert!(QEMU.dma_coherent);
    }

    #[test]
    fn vf2_matches_kernel_head() {
        // kernel/src/mmio/uart_ns16550.rs:38,51
        assert_eq!(VF2.uart_base, 0x1000_0000);
        assert_eq!(VF2.uart_stride, 4);
        // kernel/src/mmio/plic.rs:64,100
        assert_eq!(VF2.plic_base, 0x0c00_0000);
        assert_eq!(VF2.plic_hart_context, 2);
        // kernel/src/main.rs:112 (BOOT_HART_ID, vf2)
        assert_eq!(VF2.boot_hart_id, 1);
        // kernel/linker-vf2.ld:19
        assert_eq!(VF2.dram_origin, 0x4020_0000);
        // kernel/src/runtime/tier2_net.rs:117
        assert_eq!(VF2.timebase_hz, 4_000_000);
        // kernel/src/mmio/plic.rs UART_IRQ (vf2)
        assert_eq!(VF2.uart_irq, 32);
        assert!(VF2.dma_coherent);
    }

    #[test]
    fn the_coincidental_constants_are_the_trap_a_third_board_springs() {
        // UART and PLIC bases coincide between QEMU and VF2 today —
        // which is exactly why they were never gated and why a third
        // board would silently inherit them. The descriptor makes them
        // per-board data so the Ky X1 sets its own; this test documents
        // the coincidence so it is a known fact, not a lurking bug.
        assert_eq!(QEMU.uart_base, VF2.uart_base);
        assert_eq!(QEMU.plic_base, VF2.plic_base);
        // The things that already differ, and that a third board will
        // also differ on:
        assert_ne!(QEMU.uart_stride, VF2.uart_stride);
        assert_ne!(QEMU.boot_hart_id, VF2.boot_hart_id);
        assert_ne!(QEMU.dram_origin, VF2.dram_origin);
        assert_ne!(QEMU.timebase_hz, VF2.timebase_hz);
    }

    #[test]
    fn net_windows_are_the_existing_tables() {
        assert_eq!(QEMU.net_windows, crate::windows::qemu::NET_WINDOWS);
        assert_eq!(VF2.net_windows, crate::windows::vf2::NET_WINDOWS);
        assert!(!VF2.net_windows.is_empty());
    }

    #[test]
    fn mmio_regions_are_the_existing_tables() {
        assert_eq!(QEMU.mmio_regions, crate::windows::qemu::MMIO_REGIONS);
        assert_eq!(VF2.mmio_regions, crate::windows::vf2::MMIO_REGIONS);
        // kernel/src/mem/kvm.rs mapped VirtIO 0x10001000..0x10009000.
        assert_eq!(QEMU.mmio_regions.len(), 1);
        assert_eq!(QEMU.mmio_regions[0].base, 0x1000_1000);
        assert_eq!(QEMU.mmio_regions[0].len, 0x8000);
        // kernel/src/mem/kvm.rs mapped these 6 JH7110 device windows,
        // in this order. Pin every (base, len) — not just the count —
        // so a silently enlarged or shifted region (one that still
        // covers net_windows, hence invisible to the coverage test)
        // fails HERE instead of changing the boot-time map set unseen.
        let vf2_expected: [(usize, usize); 6] = [
            (0x1603_0000, 0x2_0000), // GMAC0 + GMAC1
            (0x1023_0000, 0x1_0000), // STGCRG
            (0x1302_0000, 0x1_0000), // SYSCRG
            (0x1303_0000, 0x1000),   // SYS syscon
            (0x1700_0000, 0x1_0000), // AONCRG
            (0x1701_0000, 0x1000),   // AON syscon
        ];
        assert_eq!(VF2.mmio_regions.len(), vf2_expected.len());
        for (r, (base, len)) in VF2.mmio_regions.iter().zip(vf2_expected) {
            assert_eq!((r.base, r.len), (base, len));
        }
    }

    /// The safety invariant slice 3 turns from a coincidence into a
    /// checked fact: every window the net driver is *licensed* to touch
    /// (INV-20 cap-gate) must lie inside a region the kernel actually
    /// *maps*, or a licensed access page-faults. `mmio_regions` is thus
    /// a superset of `net_windows` on every board.
    #[test]
    fn mmio_regions_cover_every_licensed_net_window() {
        fn covered(w: &MmioWindow, regions: &[MmioWindow]) -> bool {
            regions
                .iter()
                .any(|r| w.base >= r.base && w.base + w.len <= r.base + r.len)
        }
        // R2S included: both its tables are empty, so the invariant
        // holds vacuously — and stays checked the moment the DT fills them.
        for b in [QEMU, VF2, R2S] {
            for w in b.net_windows {
                assert!(
                    covered(w, b.mmio_regions),
                    "{}: licensed net window {:#x?} is not inside any mapped region",
                    b.name,
                    w
                );
            }
        }
    }

    #[test]
    fn r2s_sourced_fields_from_spacemit_k1_dt() {
        // Sourced from mainline Linux `spacemit,k1` device tree —
        // provisional until this board's own DT confirms them, pinned so
        // an accidental drift is caught. These are the ONLY fields R2S
        // first-light exercises (banner + MMU). See docs/r2s-bringup.md.
        assert_eq!(R2S.uart_base, 0xd401_7000);
        assert_eq!(R2S.uart_stride, 4);
        assert_eq!(R2S.plic_base, 0xe000_0000);
        assert_eq!(R2S.timebase_hz, 24_000_000);
        assert_eq!(R2S.dram_origin, 0x0020_0000);
        // R2S diverges from both existing boards on every one of these —
        // the whole reason B3 made them per-board data.
        assert_ne!(R2S.uart_base, VF2.uart_base);
        assert_ne!(R2S.plic_base, VF2.plic_base);
        assert_ne!(R2S.timebase_hz, VF2.timebase_hz);
    }

    #[test]
    fn r2s_dt_blocked_fields_are_sentinels() {
        // These are DELIBERATELY not-real: they must be read from the
        // board's device tree. When you fill them, THIS TEST FAILING is
        // the reminder to also let first-light proceed past the banner
        // (kernel/src/main.rs::r2s_first_light). See docs/r2s-bringup.md.
        assert_eq!(R2S.plic_hart_context, usize::MAX);
        assert_eq!(R2S.uart_irq, u32::MAX);
        assert!(R2S.net_windows.is_empty());
        assert!(R2S.mmio_regions.is_empty());
        assert!(!R2S.dma_coherent);
    }
}
