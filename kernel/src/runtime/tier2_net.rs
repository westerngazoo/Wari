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
    /// `wari::socket_create(proto: u32) -> i32` — driver allocates
    /// a smoltcp socket, returns its raw handle on success or a
    /// negative errno (PR Net-6a).
    pub socket_create_fn: TypedFunc<u32, i32>,
    /// `wari::socket_close(handle: u32) -> i32` — tears down a
    /// previously-allocated smoltcp socket.
    pub socket_close_fn: TypedFunc<u32, i32>,
    /// `wari::socket_bind(handle, ip_be, port) -> i32` (Net-6c)
    pub socket_bind_fn: TypedFunc<(u32, u32, u32), i32>,
    /// `wari::socket_listen(handle, backlog) -> i32` (Net-6c)
    pub socket_listen_fn: TypedFunc<(u32, u32), i32>,
    /// `wari::socket_accept(handle) -> i32` (Phase-1c HTTP demo).
    /// Returns 1 if connected, 0 if still listening, negative on
    /// error. Kernel drives `poll_fn` before this call so any
    /// pending SYN is processed first.
    pub socket_accept_fn: TypedFunc<u32, i32>,
    /// `wari::socket_send_canned(handle) -> i32` (Phase-1c HTTP demo).
    /// Queues a hardcoded HTTP/1.0 200 OK reply on the socket and
    /// flags it for FIN. Kernel drives `poll_fn` afterwards to push
    /// the segment to the wire. Returns bytes queued or negative.
    pub socket_send_canned_fn: TypedFunc<u32, i32>,
}

/// Boot-initialized singleton. Set once by `install` from
/// `runtime::run_tier2_net`; read by every `poll` call from the
/// kmain idle loop.
static mut TIER2_NET: Option<Tier2NetHandle> = None;

/// Kernel-wide monotonic tick (ms). Advanced by [`next_tick`] on
/// every smoltcp-driving call: kmain's idle-loop `poll`, plus the
/// new accept / send_canned host fns. Phase-1c QEMU demo timestamp
/// — not wall-clock, just monotonic-enough for smoltcp retransmit
/// decisions on a short-lived connection.
static mut TICK_MS: u64 = 0;

/// Advance and return the kernel-wide monotonic tick (10 ms steps).
/// Single-hart so no atomic needed (INV-1).
///
/// # Safety
/// INV-1 (single-hart). Caller must not hold a live borrow of
/// `TICK_MS` (none exists outside this function; it is a u64 read +
/// write under INV-1, which is sound).
pub fn next_tick() -> u64 {
    // SAFETY: INV-1 — single-hart, no other writer; u64 store is
    // single instruction on RV64.
    unsafe {
        let t = *addr_of_mut!(TICK_MS);
        *addr_of_mut!(TICK_MS) = t.saturating_add(10);
        t
    }
}

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
    // SAFETY: helper docstring + per-fn SAFETY block above.
    let slot = unsafe { tier2_net_slot() };
    let h = slot.as_mut().ok_or(KernelError::DriverError)?;
    h.poll_fn
        .call(&mut h.store, timestamp_ms)
        .map_err(|_| KernelError::DriverError)
}

/// Allocate a smoltcp socket of `proto` via the driver. Returns
/// the driver's i32 (positive raw socket handle on success, or
/// negative errno) or `KernelError::DriverError` on trap. Called
/// from the Tier-1-facing `wari::net_socket_create` host fn after
/// the kernel cap-checks the caller's Net cap.
///
/// # Safety
///
/// `install` must have run; INV-1 single-hart for the
/// `&mut TIER2_NET` accessor.
pub unsafe fn socket_create(proto: u32) -> Result<i32, KernelError> {
    // SAFETY: INV-1 + INV-8.
    // SAFETY: helper docstring + per-fn SAFETY block above.
    let slot = unsafe { tier2_net_slot() };
    let h = slot.as_mut().ok_or(KernelError::DriverError)?;
    h.socket_create_fn
        .call(&mut h.store, proto)
        .map_err(|_| KernelError::DriverError)
}

/// Tear down a smoltcp socket via the driver. Same return
/// convention + safety as [`socket_create`].
///
/// # Safety
///
/// Same as [`socket_create`].
pub unsafe fn socket_close(handle: u32) -> Result<i32, KernelError> {
    // SAFETY: INV-1 + INV-8.
    // SAFETY: helper docstring + per-fn SAFETY block above.
    let slot = unsafe { tier2_net_slot() };
    let h = slot.as_mut().ok_or(KernelError::DriverError)?;
    h.socket_close_fn
        .call(&mut h.store, handle)
        .map_err(|_| KernelError::DriverError)
}

/// PR Net-6c — bind a TCP socket to a local port via the driver.
///
/// # Safety
/// Same as [`socket_create`].
pub unsafe fn socket_bind(handle: u32, ip_be: u32, port: u32) -> Result<i32, KernelError> {
    // SAFETY: INV-1 + INV-8.
    // SAFETY: helper docstring + per-fn SAFETY block above.
    let slot = unsafe { tier2_net_slot() };
    let h = slot.as_mut().ok_or(KernelError::DriverError)?;
    h.socket_bind_fn
        .call(&mut h.store, (handle, ip_be, port))
        .map_err(|_| KernelError::DriverError)
}

/// PR Net-6c — mark a TCP socket as listening on its bound port.
///
/// # Safety
/// Same as [`socket_create`].
pub unsafe fn socket_listen(handle: u32, backlog: u32) -> Result<i32, KernelError> {
    // SAFETY: INV-1 + INV-8.
    // SAFETY: helper docstring + per-fn SAFETY block above.
    let slot = unsafe { tier2_net_slot() };
    let h = slot.as_mut().ok_or(KernelError::DriverError)?;
    h.socket_listen_fn
        .call(&mut h.store, (handle, backlog))
        .map_err(|_| KernelError::DriverError)
}

/// Phase-1c HTTP demo — drive one smoltcp poll, then check whether
/// `handle` has transitioned from listening to connected. Returns 1
/// if a connection is ready for `socket_send_canned`, 0 if still
/// waiting, negative on driver error.
///
/// # Safety
/// Same as [`socket_create`].
pub unsafe fn socket_accept(handle: u32, tick_ms: u64) -> Result<i32, KernelError> {
    // SAFETY: INV-1 + INV-8.
    let slot = unsafe { addr_of_mut!(TIER2_NET).as_mut() }
        .expect("TIER2_NET ref always valid (static)");
    let h = slot.as_mut().ok_or(KernelError::DriverError)?;
    // Drive one smoltcp poll first so any pending SYN is processed
    // into the listening socket's state before we inspect it.
    let _ = h
        .poll_fn
        .call(&mut h.store, tick_ms)
        .map_err(|_| KernelError::DriverError)?;
    h.socket_accept_fn
        .call(&mut h.store, handle)
        .map_err(|_| KernelError::DriverError)
}

/// Phase-1c HTTP demo — queue the canned HTTP/1.0 200 OK reply on a
/// connected socket, then drive one smoltcp poll cycle so the reply
/// (plus the FIN smoltcp now owes after the canned-send's
/// `socket.close()`) leaves the device on this same kernel hop.
/// Returns bytes queued or negative on driver error.
///
/// Why drive poll after, not before: without it, the queued reply
/// waits for the next kmain idle-loop tick. The Tier-1 busy-poll
/// would already have called `net_socket_close` to release the
/// smoltcp slot before the segment ever went out, and we would RST
/// the client. Inlining the post-send poll guarantees the reply
/// arrives before the slot is freed.
///
/// # Safety
/// Same as [`socket_create`].
pub unsafe fn socket_send_canned(handle: u32, tick_ms: u64) -> Result<i32, KernelError> {
    // SAFETY: INV-1 + INV-8.
    let slot = unsafe { addr_of_mut!(TIER2_NET).as_mut() }
        .expect("TIER2_NET ref always valid (static)");
    let h = slot.as_mut().ok_or(KernelError::DriverError)?;
    let queued = h
        .socket_send_canned_fn
        .call(&mut h.store, handle)
        .map_err(|_| KernelError::DriverError)?;
    let _ = h
        .poll_fn
        .call(&mut h.store, tick_ms)
        .map_err(|_| KernelError::DriverError)?;
    Ok(queued)
}

/// `true` if `install` has been called. Used by the kmain idle
/// loop to decide whether to enter the polling loop.
pub fn is_installed() -> bool {
    // SAFETY: INV-1 single-hart read; `tier2_net_slot` is
    // infallible (see its docstring). The `&mut` reborrow to a
    // read is sound because nothing else holds a live borrow
    // under INV-1.
    let slot = unsafe { tier2_net_slot() };
    slot.is_some()
}
