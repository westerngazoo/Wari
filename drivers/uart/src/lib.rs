// SPDX-License-Identifier: AGPL-3.0-only
//! Tier-2 UART driver — Phase-1a, platform-aware.
//!
//! Built as a separately-signed `.wasm` per platform: one for QEMU
//! `virt` (NS16550, 8-bit register stride), one for StarFive
//! VisionFive 2 (JH7110, DesignWare 8250, 32-bit-aligned register
//! stride). Activated via cargo feature: `--features qemu` or
//! `--features vf2`.
//!
//! ## Why per-platform blobs (Why/How depth)
//!
//! Picked: two signed blobs, one per platform, with the MMIO base
//! address and the register stride hardcoded as `const`s at WASM
//! build time. Each blob has exactly one MMIO surface visible at
//! audit time.
//!
//! Considered: one blob with stride passed as a host-fn argument or
//! read from a host-fn at startup — rejected because it widens the
//! driver's MMIO surface (in principle the driver could write any
//! address the host marshals through, blunting the static-audit
//! story for Tier-2 modules).
//!
//! Cost accepted: the signing pipeline runs twice (`make
//! sign-uart-driver` handles both); two signed blobs in `build/drivers/`.
//!
//! ## Lockstep maintenance
//!
//! The platform constants below duplicate the kernel's
//! `kernel/src/platform/{qemu_virt,vf2}.rs` exports. The duplication
//! is structural — a `wasm32-unknown-unknown` cdylib cannot depend on
//! the kernel crate. If hardware moves, update both sides in the same
//! PR. The kernel-side validator (`validate::is_uart_mmio_addr`)
//! enforces that the driver only ever writes to the agreed UART
//! window regardless.

#![no_std]
#![no_main]

#[cfg(not(any(feature = "qemu", feature = "vf2")))]
compile_error!("wari-driver-uart requires --features qemu or --features vf2.");

#[cfg(all(feature = "qemu", feature = "vf2"))]
compile_error!("wari-driver-uart accepts only one of --features qemu / vf2.");

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // Tier-2 panics are bugs in signed code. wasmi's trap will fire
    // before this in practice; the loop is the structural fallback.
    loop {}
}

// ── Platform constants (lockstep with kernel/src/platform/*.rs) ──

#[cfg(feature = "qemu")]
mod plat {
    /// NS16550 base on QEMU `virt`.
    pub const UART_BASE: u32 = 0x1000_0000;
    /// 8-bit registers — 1 byte per logical register index.
    pub const UART_REG_STRIDE: u32 = 1;
}

#[cfg(feature = "vf2")]
mod plat {
    /// JH7110 UART0 base.
    pub const UART_BASE: u32 = 0x1000_0000;
    /// DesignWare 8250 — 32-bit-aligned registers (4 bytes per index).
    pub const UART_REG_STRIDE: u32 = 4;
}

const UART_THR_REG: u32 = 0; // Transmit Holding Register
const UART_LSR_REG: u32 = 5; // Line Status Register
const UART_LSR_THRE: u8 = 0x20; // bit 5 — Transmit Holding Register Empty

// ── Host-function imports ────────────────────────────────────────

#[link(wasm_import_module = "wari")]
extern "C" {
    /// Host fn — write `val as u8` to MMIO `addr`. Capability-gated
    /// (`CAP_MMIO_UART`) and range-validated by the kernel.
    /// Returns 0 on success, negative errno on failure.
    #[link_name = "mmio_write8"]
    fn wari_mmio_write8(addr: u32, val: u32) -> i32;

    /// Host fn — read a byte from MMIO `addr`. Same gating as
    /// `mmio_write8`. Returns the byte (zero-extended in `u32`) on
    /// success, or `u32::MAX` as a no-permission / out-of-range
    /// sentinel — see `kernel/src/runtime/host_fns.rs::host_mmio_read8`.
    #[link_name = "mmio_read8"]
    fn wari_mmio_read8(addr: u32) -> u32;
}

// ── Address helpers ──────────────────────────────────────────────

#[inline]
fn lsr_addr() -> u32 {
    plat::UART_BASE + UART_LSR_REG * plat::UART_REG_STRIDE
}

#[inline]
fn thr_addr() -> u32 {
    plat::UART_BASE + UART_THR_REG * plat::UART_REG_STRIDE
}

// ── Per-byte UART path ───────────────────────────────────────────

/// Spin-poll LSR.THRE, then write one byte. Returns 0 on success or
/// the negative errno from a failing host call.
fn put_byte(b: u8) -> i32 {
    loop {
        // SAFETY: extern fn into wasmi host import — host validates
        // the address falls in the UART MMIO window.
        let lsr = unsafe { wari_mmio_read8(lsr_addr()) } as u8;
        if lsr & UART_LSR_THRE != 0 {
            break;
        }
    }
    // SAFETY: same as above.
    unsafe { wari_mmio_write8(thr_addr(), b as u32) }
}

// ── Exports ──────────────────────────────────────────────────────

/// Push `len` bytes from linear memory at `buf_ptr` to the UART.
///
/// Returns `len` on success, or the negative errno from the first
/// failing host call.
#[no_mangle]
pub extern "C" fn write(buf_ptr: u32, len: u32) -> i32 {
    let mem = buf_ptr as usize as *const u8;
    for i in 0..(len as usize) {
        // SAFETY: caller passes a pointer into our linear memory;
        // wasmi bounds-checks every load. OOB traps the instance
        // rather than corrupting host state.
        let byte = unsafe { mem.add(i).read() };
        let r = put_byte(byte);
        if r != 0 {
            return r;
        }
    }
    len as i32
}

#[no_mangle]
pub extern "C" fn _start() {}
