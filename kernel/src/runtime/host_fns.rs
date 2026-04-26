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

/// Per-instance host context threaded through `Store<Tier2HostState>`.
///
/// Keeps the `Caps` granted at load time so every host-fn invocation
/// can re-check (cheap, single-hart, no contention).
///
/// **Tier separation**: this state is Tier-2-only. Tier-1 instances use
/// `runtime::wasi::Tier1HostState`. Two types instead of one shared
/// struct so each tier's linker is parameterised by exactly the cap
/// shape its host fns inspect — preventing a Tier-1 host fn from
/// accidentally reading a Tier-2 cap (and vice versa) at the type level.
pub struct Tier2HostState {
    /// Capabilities granted to this instance at `load_tier2` time.
    pub caps: Caps,
}

/// Register Phase-0 Tier-2 host fns into a fresh linker.
///
/// # Contract
///
/// - Precondition: `linker` is freshly constructed (no prior
///   `wari::*` definitions).
/// - Postcondition: `wari::mmio_write8` is bound. Future Tier-2 host
///   fns (IRQ ack, fuel refill, …) get appended here as they land.
/// - Errors: `KernelError::BadWasm` if wasmi rejects the binding
///   (signature mismatch with itself; should not happen).
pub fn register_host_fns(linker: &mut Linker<Tier2HostState>) -> Result<(), KernelError> {
    linker
        .func_wrap("wari", "mmio_write8", host_mmio_write8)
        .map_err(|_| KernelError::BadWasm)?;
    linker
        .func_wrap("wari", "mmio_read8", host_mmio_read8)
        .map_err(|_| KernelError::BadWasm)?;

    // Phase-1b cap-management host fns. Registered for Tier-2 with
    // `proc_id = PROC_ID_TIER2_UART` baked in. The implementations
    // live in `cap::syscall`; we just bind them to the linker here.
    use crate::cap::{
        cap_copy_impl, cap_delete_impl, cap_lookup_impl, cap_mint_impl,
        cap_revoke_impl, PROC_ID_TIER2_UART,
    };
    linker
        .func_wrap(
            "wari",
            "cap_mint",
            |_: Caller<'_, Tier2HostState>, ps: u32, ts: u32, r: u32, b: u32| -> i32 {
                cap_mint_impl(PROC_ID_TIER2_UART, ps, ts, r, b)
            },
        )
        .map_err(|_| KernelError::BadWasm)?;
    linker
        .func_wrap(
            "wari",
            "cap_copy",
            |_: Caller<'_, Tier2HostState>, src: u32, tgt: u32| -> i32 {
                cap_copy_impl(PROC_ID_TIER2_UART, src, tgt)
            },
        )
        .map_err(|_| KernelError::BadWasm)?;
    linker
        .func_wrap(
            "wari",
            "cap_revoke",
            |_: Caller<'_, Tier2HostState>, slot: u32| -> i32 {
                cap_revoke_impl(PROC_ID_TIER2_UART, slot)
            },
        )
        .map_err(|_| KernelError::BadWasm)?;
    linker
        .func_wrap(
            "wari",
            "cap_delete",
            |_: Caller<'_, Tier2HostState>, slot: u32| -> i32 {
                cap_delete_impl(PROC_ID_TIER2_UART, slot)
            },
        )
        .map_err(|_| KernelError::BadWasm)?;
    linker
        .func_wrap(
            "wari",
            "cap_lookup",
            |mut caller: Caller<'_, Tier2HostState>, slot: u32, out_buf: u32| -> i32 {
                cap_lookup_impl(&mut caller, PROC_ID_TIER2_UART, slot, out_buf)
            },
        )
        .map_err(|_| KernelError::BadWasm)?;

    Ok(())
}

/// `wari::mmio_write8(addr: u32, val: u32) -> i32` — write the low
/// byte of `val` to the MMIO register at `addr`, gated by the
/// caller's UART endpoint capability and the validator's range
/// check.
///
/// **Phase 1b cap-mediated path**: the legacy `host.caps.mmio_uart`
/// boolean has been replaced with a real cap lookup against the
/// Tier-2 UART driver's CSpace. The driver authority to do MMIO is
/// expressed as "holds the receive side of the UART IPC endpoint".
/// See `cap::syscall::check_cap` for the predicate.
fn host_mmio_write8(_caller: Caller<'_, Tier2HostState>, addr: u32, val: u32) -> i32 {
    use crate::cap::{
        check_cap, ObjectKind, CAP_RIGHT_READ, PROC_ID_TIER2_UART,
    };
    // INV-3 + cap gate: Tier-2 driver holds an Endpoint cap with
    // READ rights at slot 0 (the receive side of uart_ipc_ep).
    // Without this cap the driver cannot perform MMIO operations.
    if check_cap(PROC_ID_TIER2_UART, 0, ObjectKind::Endpoint, CAP_RIGHT_READ).is_err() {
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

/// `wari::mmio_read8(addr: u32) -> u32` — read a byte from the MMIO
/// register at `addr` (zero-extended in the `u32` return). Same
/// cap-mediated gating as `mmio_write8`. Needed by the Phase-1a
/// UART driver's LSR-poll loop.
///
/// **Sentinel**: returns `u32::MAX` on permission or range failure.
/// A legitimate UART status read would not produce `0xFFFFFFFF`, but
/// the driver should treat this value as "stop polling" defensively.
/// A richer error encoding lands when the ABI gains result-tuple
/// shapes (Phase 2+).
fn host_mmio_read8(_caller: Caller<'_, Tier2HostState>, addr: u32) -> u32 {
    use crate::cap::{
        check_cap, ObjectKind, CAP_RIGHT_READ, PROC_ID_TIER2_UART,
    };
    // Cap gate (PR 3b): Tier-2 driver holds the UART endpoint cap.
    if check_cap(PROC_ID_TIER2_UART, 0, ObjectKind::Endpoint, CAP_RIGHT_READ).is_err() {
        return u32::MAX;
    }
    if !validate::is_uart_mmio_addr(addr as usize) {
        return u32::MAX;
    }

    // SAFETY: INV-3 (validator-narrowed MMIO address) + capability
    // check above. The 8-bit read of a UART register is non-mutating
    // and well-defined for the entire NS16550/DW8250 register window.
    let byte = unsafe { core::ptr::read_volatile(addr as usize as *const u8) };
    byte as u32
}
