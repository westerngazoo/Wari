// SPDX-License-Identifier: AGPL-3.0-only
//! Host functions exposed to Tier-2 WASM modules.
//!
//! Phase 0 exports exactly one host fn: `wari::mmio_write8`. It is
//! double-gated:
//!   1. Capability check (`HostState.caps.mmio_uart`) — refuses unless
//!      the calling instance was loaded with `CAP_MMIO_UART`.
//!   2. Range check (`validate::is_uart_mmio_addr`) — refuses any
//!      address outside the NS16550 register window.
//!
//! Both checks must pass before the raw `write_volatile`. The second
//! check narrows INV-3 (MMIO address validity) to the precise window
//! the UART driver is licensed to touch.
//!
//! ## WASM-side ABI
//!
//! ```text
//! (import "wari" "mmio_write8" (func (param i32 i32) (result i32)))
//! ```
//!
//! Returns 0 on success, a negative errno on failure:
//!   - `E_PERM`  (-1) — capability denied.
//!   - `E_INVAL` (-2) — address outside the licensed window.

#![allow(dead_code)]

use wasmi::{Caller, Linker};

use crate::cap::Caps;
use crate::error::KernelError;
use crate::validate;

/// Errno returned to WASM when the caller lacks the required cap.
pub const E_PERM: i32 = -1;
/// Errno returned to WASM when the address fails the range check.
pub const E_INVAL: i32 = -2;

/// Per-instance host context threaded through `Store<HostState>`.
///
/// Keeps the `Caps` granted at load time so every host-fn invocation
/// can re-check (cheap, single-hart, no contention).
pub struct HostState {
    /// Capabilities granted to this instance at `load_tier2` time.
    pub caps: Caps,
}

/// Register Phase-0 host fns into a fresh linker.
///
/// # Contract
///
/// - Precondition: `linker` is freshly constructed (no prior
///   `wari::*` definitions).
/// - Postcondition: `wari::mmio_write8` is bound. Future Tier-2 host
///   fns (IRQ ack, fuel refill, …) get appended here as they land.
/// - Errors: `KernelError::BadWasm` if wasmi rejects the binding
///   (signature mismatch with itself; should not happen).
pub fn register_host_fns(linker: &mut Linker<HostState>) -> Result<(), KernelError> {
    linker
        .func_wrap("wari", "mmio_write8", host_mmio_write8)
        .map_err(|_| KernelError::BadWasm)?;
    Ok(())
}

/// `wari::mmio_write8(addr: u32, val: u32) -> i32` — write the low
/// byte of `val` to the MMIO register at `addr`, gated by the caller's
/// `mmio_uart` capability and the validator's range check.
fn host_mmio_write8(caller: Caller<'_, HostState>, addr: u32, val: u32) -> i32 {
    let host = caller.data();

    if !host.caps.mmio_uart {
        return E_PERM;
    }
    if !validate::is_uart_mmio_addr(addr as usize) {
        return E_INVAL;
    }

    // SAFETY: INV-3 (MMIO address validity, narrowed by the validator
    // above to the NS16550 register window). The capability check
    // ensures only an instance loaded with `CAP_MMIO_UART` reaches
    // this point. The pointer is valid for an 8-bit volatile write
    // because QEMU's NS16550 model accepts byte writes throughout the
    // register window.
    unsafe {
        core::ptr::write_volatile(addr as usize as *mut u8, val as u8);
    }
    0
}
