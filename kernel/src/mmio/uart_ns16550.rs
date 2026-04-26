// SPDX-License-Identifier: AGPL-3.0-only
//! NS16550 / DW8250 UART — kernel-private putc for early boot + panic.
//!
//! This is **not** the customer UART driver. The Tier-2 WASM UART
//! driver (`drivers/uart/`) handles the production data path with
//! capability gating. This file is the raw-MMIO printk that lets the
//! kernel log during boot (before the runtime exists) and during a
//! panic (when the runtime may be gone).
//!
//! Targets two register-stride variants of the same NS16550 register
//! map:
//!   - QEMU virt — emulated NS16550A, 1-byte stride.
//!   - StarFive VF2 (JH7110) — DesignWare 8250, 4-byte stride.
//!
//! Both expose UART0 at `0x1000_0000` (QEMU model + JH7110 SoC layout
//! happen to coincide). The platform difference is the byte distance
//! between consecutive logical registers, not the base.
//!
//! Reference: Texas Instruments TL16C550 datasheet (SLLS177I), §6
//! "Register summary"; Synopsys DesignWare DW_apb_uart spec for the
//! VF2 stride and FCR/MCR semantics.
//!
//! Register indices (multiplied by stride for actual byte offset):
//!   0  THR/RBR  transmit / receive holding register
//!   1  IER      interrupt enable
//!   2  FCR/IIR  FIFO control / interrupt id
//!   3  LCR      line control
//!   4  MCR      modem control
//!   5  LSR      line status
//!
//! Phase 0 scope: poll-write bytes, no interrupts, no reads. The
//! Tier-2 driver handles RX, IRQs, flow control, etc.

use super::volatile::VolatilePtr;

/// UART0 base address. Same on QEMU virt and JH7110 — kept as a single
/// constant; if a future platform diverges, gate this with `cfg` too.
const UART_BASE: usize = 0x1000_0000;

/// Register stride (byte distance between consecutive logical registers).
/// QEMU's NS16550A uses 1-byte stride; the JH7110 DW8250 uses 4-byte
/// stride. Picked at compile time by the kernel's platform feature.
#[cfg(not(feature = "vf2"))]
const UART_REG_STRIDE: usize = 1;
#[cfg(feature = "vf2")]
const UART_REG_STRIDE: usize = 4;

const THR_REG: usize = 0;
const IER_REG: usize = 1;
const FCR_REG: usize = 2;
const LCR_REG: usize = 3;
const MCR_REG: usize = 4;
const LSR_REG: usize = 5;

/// Bit 5 of LSR: transmit holding register empty (ready to accept).
const LSR_THRE: u8 = 1 << 5;

/// LCR — 8 data bits, 1 stop bit, no parity.
const LCR_8N1: u8 = 0x03;
/// FCR — enable + clear both FIFOs, 1-byte RX trigger.
const FCR_FIFO_RESET: u8 = 0x07;
/// MCR — DTR + RTS + OUT2. OUT2 gates IRQ output to the PLIC; DTR/RTS
/// are required for RX on real hardware (the JH7110 holds the line
/// otherwise). Set even though Phase 0 only transmits — matches the
/// goose-os sequence proven across ~100 builds.
const MCR_DTR_RTS_OUT2: u8 = 0x0B;
/// IER — RX-data-available interrupt. We don't use it yet (Phase 1b),
/// but setting it here mirrors the goose-os init that the JH7110
/// silently requires for stable RX in U-Boot's later interactive shell
/// after a Wari boot lock-in is removed.
const IER_RX_AVAIL: u8 = 0x01;

/// Compute the MMIO address of a logical UART register.
#[inline(always)]
fn reg_addr(index: usize) -> usize {
    UART_BASE + index * UART_REG_STRIDE
}

/// Construct a `VolatilePtr<u8>` to a logical register.
///
/// # Safety
/// INV-3 — the resulting pointer targets a fixed UART register on
/// the active platform. Caller must restrict `index` to the documented
/// register set (THR/IER/FCR/LCR/MCR/LSR).
#[inline(always)]
unsafe fn reg(index: usize) -> VolatilePtr<u8> {
    // SAFETY: caller-asserted register index; UART_BASE + stride *
    // index is a hardware register address (INV-3).
    unsafe { VolatilePtr::new(reg_addr(index) as *mut u8) }
}

/// Initialize the UART for 8N1, FIFO-enabled, OUT2-gated operation.
///
/// QEMU's NS16550A model accepts these writes as no-ops (it transmits
/// regardless). The JH7110 DW8250 *requires* the FCR/MCR setup — the
/// pre-PR-10 no-op `init()` left the device idle on real silicon and
/// produced silent boots. Sequence cherry-picked from
/// goose-os/kernel/src/uart.rs::init at HEAD.
///
/// # Contract
/// - Precondition: kernel in S-mode; UART MMIO range identity-mapped
///   RW (kvm.rs handles this via `UART_MMIO_BASE`).
/// - Postcondition: subsequent `putc` calls deliver bytes.
/// - Panics: never.
pub fn init() {
    // SAFETY: INV-3. Each `reg(i)` returns a typed wrapper for a
    // fixed NS16550/DW8250 register (THR..LSR); writes are hardware
    // register operations, not arbitrary memory access.
    unsafe {
        // Disable all interrupts during setup.
        reg(IER_REG).write(0x00);
        // 8N1.
        reg(LCR_REG).write(LCR_8N1);
        // FIFOs: enable + clear, 1-byte RX trigger.
        reg(FCR_REG).write(FCR_FIFO_RESET);
        // Modem control: DTR + RTS + OUT2.
        reg(MCR_REG).write(MCR_DTR_RTS_OUT2);
        // Re-enable RX-available interrupt to match the goose-os
        // proven sequence. TX stays poll-driven (ETBEI clear).
        reg(IER_REG).write(IER_RX_AVAIL);
    }
}

/// Write one byte to the UART, blocking until the THR is free.
///
/// # Contract
///
/// - Precondition: `init()` has run (Phase 0: called once from boot).
/// - Postcondition: the byte has been handed to the UART's THR. QEMU
///   flushes to stdout synchronously; real hardware has a shift
///   register latency we do not wait on.
/// - Panics: never.
pub fn putc(byte: u8) {
    // SAFETY: INV-3. THR / LSR are fixed NS16550 / DW8250 registers.
    let lsr = unsafe { reg(LSR_REG) };
    // SAFETY: INV-3 (same).
    let thr = unsafe { reg(THR_REG) };

    // Poll LSR until THRE is set. On QEMU this is effectively
    // always set; on the JH7110 the shift register actually drains.
    while lsr.read() & LSR_THRE == 0 {}
    thr.write(byte);
}
