// SPDX-License-Identifier: AGPL-3.0-only
//! StarFive VisionFive 2 (JH7110) — platform constants.
//!
//! References:
//!   - JH7110 datasheet — UART0 at 0x1000_0000 (same base as QEMU virt;
//!     happy coincidence).
//!   - DesignWare 8250 register layout — 32-bit-aligned registers, so
//!     register `i` lives at `base + i * 4` (vs. NS16550A's `base + i`).
//!
//! Cherry-picked from goose-os/kernel/src/platform.rs (the
//! `#[cfg(feature = "vf2")]` block) under the "only copy what makes
//! sense" discipline.

#![allow(dead_code)]

/// JH7110 UART0 (DesignWare 8250-compatible).
pub const UART_BASE: usize = 0x1000_0000;

/// Bytes between consecutive UART registers. DesignWare 8250 on JH7110
/// has 4-byte spacing (THR=+0, IER=+4, FCR=+8, LCR=+12, LSR=+20).
pub const UART_REG_STRIDE: usize = 4;

/// MMIO window length validated by `validate::is_uart_mmio_addr`.
/// Six register slots × 4-byte stride = 24 bytes; rounded up to 32 (one
/// cache line) for the validator's exclusive upper bound.
pub const UART_MMIO_LEN: usize = 0x20;

/// JH7110 PLIC base.
pub const PLIC_BASE: usize = 0x0c00_0000;

/// Kernel image is loaded here by U-Boot on JH7110.
/// Must agree with `linker-vf2.ld`'s `MEMORY { RAM ... ORIGIN = ... }`.
pub const KERNEL_LOAD_ADDR: usize = 0x4020_0000;

/// Boot hart ID. JH7110: hart 0 is the SiFive S7 monitor core (M-only,
/// no MMU); harts 1-4 are U74 application cores. Hart 1 is the
/// conventional OpenSBI boot hart and the one `linker-vf2.ld` exports
/// `_boot_hart_id = 1` for boot.S to compare against.
pub const BOOT_HART: usize = 1;
