// SPDX-License-Identifier: AGPL-3.0-only
//! Tier-2 UART driver — Phase 0.
//!
//! Imports the host fn `wari::mmio_write8` and exports a `write` entry
//! the kernel will invoke from PR 6's Tier-1 hello path. Phase 0 ships
//! the driver loaded + signature-verified; the kernel does not yet call
//! into `write` (that wiring lands when Tier-1 hello arrives in PR 6).

#![no_std]
#![no_main]

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // Tier-2 panics are bugs in signed code. wasmi's trap will fire
    // before this in practice; the loop is the structural fallback.
    loop {}
}

// Map the import to the WASM module/field the kernel registers via
// wasmi's `Linker::func_wrap("wari", "mmio_write8", ...)`. Without
// `#[link(wasm_import_module = ...)]` the default module name is
// "env", which would not match.
#[link(wasm_import_module = "wari")]
extern "C" {
    /// Host fn — write `val as u8` to MMIO `addr`. Gated by the
    /// kernel's `CAP_MMIO_UART` + `validate::is_uart_mmio_addr`.
    /// Returns 0 on success, negative errno on failure.
    #[link_name = "mmio_write8"]
    fn wari_mmio_write8(addr: u32, val: u32) -> i32;
}

/// NS16550 transmit holding register on QEMU `virt`.
const UART_THR: u32 = 0x1000_0000;

/// Push `len` bytes from linear memory at `buf_ptr` to the UART.
///
/// Returns `len` on success, or the negative errno from the first
/// failing host call.
///
/// # Linear-memory addressing convention
///
/// `buf_ptr` is an offset into the driver's own WASM linear memory,
/// passed in by the kernel. WASM bounds-checks every load via wasmi —
/// an out-of-bounds offset traps before this function returns.
///
/// TODO(PR 6): once Tier-1 hello calls `write`, the kernel-side host
/// path will marshal the buffer through linear memory; this function's
/// signature is the contract for that wiring.
#[no_mangle]
pub extern "C" fn write(buf_ptr: u32, len: u32) -> i32 {
    let mem = buf_ptr as usize as *const u8;
    for i in 0..(len as usize) {
        // SAFETY: the kernel-side host fn passes a pointer into our
        // own linear memory. WASM linear-memory loads are bounds-
        // checked by wasmi; an OOB read traps the instance rather
        // than corrupting host state.
        let byte = unsafe { mem.add(i).read() };
        // SAFETY: extern fn into wasmi host import. The host fn does
        // its own capability + range check; the worst this can do is
        // return a negative errno.
        let r = unsafe { wari_mmio_write8(UART_THR, byte as u32) };
        if r != 0 {
            return r;
        }
    }
    len as i32
}

/// Empty start fn — required by some toolchains for `cdylib`.
#[no_mangle]
pub extern "C" fn _start() {}
