// SPDX-License-Identifier: AGPL-3.0-only
//! QEMU `virt` — platform constants.
//!
//! Authoritative memory map: <https://github.com/qemu/qemu/blob/master/hw/riscv/virt.c>.

#![allow(dead_code)]

/// NS16550A-compatible UART (8-bit register stride).
pub const UART_BASE: usize = 0x1000_0000;

/// Bytes between consecutive UART registers. NS16550A on QEMU `virt`
/// has 1-byte spacing (THR=+0, IER=+1, FCR=+2, LCR=+3, LSR=+5).
pub const UART_REG_STRIDE: usize = 1;

/// MMIO window length validated by `validate::is_uart_mmio_addr`.
/// Six register slots × 1-byte stride = 8 bytes (rounded for clarity).
pub const UART_MMIO_LEN: usize = 0x8;

/// PLIC base (Phase-1+ when external IRQs land).
pub const PLIC_BASE: usize = 0x0c00_0000;

/// Kernel image is loaded here by `-kernel` / OpenSBI hand-off.
/// Must agree with `linker.ld`'s `MEMORY { RAM ... ORIGIN = ... }`.
pub const KERNEL_LOAD_ADDR: usize = 0x8020_0000;

/// Boot hart ID. QEMU `virt` brings hart 0 up first.
pub const BOOT_HART: usize = 0;
