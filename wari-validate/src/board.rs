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
//! ## Slice 1 (this file): the descriptor exists as data
//!
//! This is purely additive. The kernel does NOT read from these yet —
//! it still uses its own per-site constants, and the [`tests`] below
//! pin every field to exactly the value the kernel uses at HEAD, with
//! the source line cited. Slice 2 migrates the kernel to read
//! `board::BOARD.field`; because the values are pinned here, that
//! migration is behavior-preserving and the QEMU/VF2 integration tests
//! prove it. Nothing about VF2 or QEMU behavior changes in this slice.
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
    /// The NIC MMIO windows the Tier-2 net driver is licensed to touch
    /// (INV-20). Reuses the existing per-platform window tables.
    pub net_windows: &'static [MmioWindow],
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
    net_windows: crate::windows::qemu::NET_WINDOWS,
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
    net_windows: crate::windows::vf2::NET_WINDOWS,
    dma_coherent: true,
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
}
