// SPDX-License-Identifier: AGPL-3.0-only
//! Boot-initialized singleton for the Tier-2 net driver instance.
//!
//! Mirrors `tier2_uart.rs`. The Tier-2 net driver's `_start` runs the
//! VirtIO init sequence and configures the smoltcp `Interface`; after
//! `_start` returns, the kernel needs to keep calling the driver's
//! exported `poll` function periodically so smoltcp can drain incoming
//! packets and deliver outgoing ones. PR Net-5b establishes this
//! singleton + the kernel idle loop in `kmain` that drives it.
//!
//! ## Why a singleton (Why/How depth)
//!
//! Same shape as `tier2_uart::Tier2UartHandle`: a `static mut
//! Option<Tier2NetHandle>` written once during boot via `install`,
//! read via `&mut TIER2_NET` in `poll` calls. Single-hart kernel
//! (INV-1) means no synchronization; INV-8 establishes
//! post-init access; INV-14's "single boot install" pattern carries
//! through to the net handle (call it INV-14 generalized).
//!
//! ## What's stored
//!
//! - `instance` + `store` — wasmi's per-instance state. Held so
//!   subsequent `poll` calls can re-enter the driver's WASM linear
//!   memory.
//! - `poll_fn` — the typed-func handle for the driver's
//!   `poll(timestamp_ms: u64) -> i32` export. Resolved once during
//!   `run_tier2_net` so each idle-loop iteration doesn't repeat the
//!   export lookup.
//!
//! Future PRs (Net-6 socket host fns) extend this struct with
//! additional resolved exports — `socket_create`, `socket_send`,
//! `socket_recv`, etc.

#![allow(dead_code)]
#![allow(clippy::doc_lazy_continuation)]

use core::ptr::addr_of_mut;

use wasmi::{Instance, Store, TypedFunc};

use crate::error::KernelError;
use crate::runtime::host_fns::Tier2HostState;

/// Live handle to the Tier-2 net driver instance.
///
/// Carries the per-instance `Store` (so `poll_fn.call` can mutate
/// driver-side state across calls) and the resolved typed-func
/// handle for `poll`.
pub struct Tier2NetHandle {
    pub instance: Instance,
    pub store: Store<Tier2HostState>,
    /// `wari::poll(timestamp_ms: u64) -> i32` — driver export that
    /// advances smoltcp's Interface::poll cycle. Returns 1 if any
    /// state changed (packets drained or queued), 0 otherwise.
    /// Phase-1b QEMU only; vf2 stub returns -1.
    pub poll_fn: TypedFunc<u64, i32>,
}

/// Boot-initialized singleton. Set once by `install` from
/// `runtime::run_tier2_net`; read by every `poll` call from the
/// kmain idle loop.
static mut TIER2_NET: Option<Tier2NetHandle> = None;

/// Install the net driver handle. Called once from
/// `runtime::run_tier2_net` after the driver's `_start` has
/// completed (which means VirtIO init succeeded AND the smoltcp
/// `Interface` is configured).
///
/// # Safety
///
/// Caller guarantees:
/// - This is the only call to `install` in the lifetime of the
///   kernel boot. Subsequent calls would silently overwrite the
///   prior handle, leaking its `Store`.
/// - Called before any `poll` call. INV-8 + INV-14 generalized.
pub unsafe fn install(handle: Tier2NetHandle) {
    // SAFETY: caller's contract above; INV-1 (single-hart) +
    // INV-8 (boot-time post-init) — same justification as
    // tier2_uart::install.
    unsafe {
        *addr_of_mut!(TIER2_NET) = Some(handle);
    }
}

/// Advance the smoltcp `Interface::poll` cycle by calling into the
/// driver. Returns the driver's i32 return value (1 = state
/// changed, 0 = idle, negative = error) or
/// `KernelError::DriverError` if the call traps.
///
/// Phase-1b kmain idle loop calls this in a `wfi`-paced loop.
/// `timestamp_ms` is a logical monotonic counter (NOT wall-clock);
/// smoltcp uses it for retransmit interval decisions, which still
/// work correctly as long as the counter advances monotonically.
///
/// # Safety
///
/// Caller guarantees `install` has run. Multiple concurrent calls
/// would alias the `&mut TIER2_NET`; INV-1 (single-hart) prevents
/// this in Phase 1b. Phase 2+ SMP migration revisits this.
pub unsafe fn poll(timestamp_ms: u64) -> Result<i32, KernelError> {
    // SAFETY: INV-1 + INV-8 + INV-14 generalized. Single-hart
    // single accessor; install ran during boot.
    let slot = unsafe { addr_of_mut!(TIER2_NET).as_mut() }
        .expect("TIER2_NET ref always valid (static)");
    let h = slot.as_mut().ok_or(KernelError::DriverError)?;
    h.poll_fn
        .call(&mut h.store, timestamp_ms)
        .map_err(|_| KernelError::DriverError)
}

/// `true` if `install` has been called. Used by the kmain idle
/// loop to decide whether to enter the polling loop.
pub fn is_installed() -> bool {
    // SAFETY: INV-1; only reads.
    let slot = unsafe { addr_of_mut!(TIER2_NET).as_ref() }
        .expect("TIER2_NET ref always valid (static)");
    slot.is_some()
}
