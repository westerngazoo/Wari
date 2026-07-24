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

/// Accept-deadline tracking slots (Phase-1c Ctrl-R fix B), indexed
/// by the driver's raw smoltcp socket handle. Sized above the
/// driver's `SOCKET_BACKING_LEN` (4) with headroom; a handle at or
/// beyond this bound is simply not deadline-tracked (degrades to
/// the old unbounded behavior rather than misindexing).
const ACCEPT_TRACK_SLOTS: usize = 8;

/// Per-handle timestamp of the FIRST `socket_accept` call, in
/// monotonic ms (`next_tick`). `None` = no accept window open.
/// Set on first accept, cleared on successful accept and on
/// `socket_close`; once `ACCEPT_DEADLINE_MS` elapses, `socket_accept`
/// returns `E_TIMEDOUT` persistently until the socket is closed.
///
/// Why kernel-side: the kernel owns the monotonic clock, and the
/// bound must hold for ANY Tier-1 caller — a misbehaving tenant's
/// blind busy-poll (50M iterations ≈ hours on silicon) must not be
/// able to starve the kmain idle loop and its Ctrl-R reboot check.
static mut ACCEPT_FIRST_MS: [Option<u64>; ACCEPT_TRACK_SLOTS] = [None; ACCEPT_TRACK_SLOTS];

/// One-shot guard for the `[accept] window expired` console line:
/// `socket_accept` returns `E_TIMEDOUT` persistently after expiry,
/// so without this flag the print would fire on every subsequent
/// busy-poll iteration (potentially millions). Set when the expiry
/// line is printed; cleared wherever the window itself is cleared
/// (successful accept, close). Same INV-1 single-hart access rules
/// as `ACCEPT_FIRST_MS`.
static mut ACCEPT_EXPIRED_LOGGED: [bool; ACCEPT_TRACK_SLOTS] = [false; ACCEPT_TRACK_SLOTS];

/// RISC-V `time` CSR frequency. JH7110's CLINT timebase runs at
/// 4 MHz (Linux dts `timebase-frequency = <4000000>`); QEMU virt
/// uses 10 MHz.
#[cfg(feature = "vf2")]
const TIMEBASE_HZ: u64 = 4_000_000;
#[cfg(feature = "qemu")]
const TIMEBASE_HZ: u64 = 10_000_000;

/// Real monotonic milliseconds from the RISC-V `time` CSR.
///
/// Build 138 — replaces the old `TICK_MS += 10` per-idle-iteration
/// counter. That counter advanced smoltcp's virtual clock ~1000x
/// faster than wall time (the idle loop spins ~100k times per real
/// second, so smoltcp saw ~1000 virtual seconds per real second).
/// Consequence on silicon: smoltcp's 60-second neighbor-cache
/// lifetime expired every ~60 real MILLISECONDS, so the ARP entry
/// for the ping peer was almost always stale — an ICMP echo could
/// only be answered in the brief window right after an ARP
/// exchange refreshed the cache. Observed as intermittent,
/// worsening ping timeouts on build 137 despite a fully working
/// RX/TX datapath (MMC counters clean, no FIFO overflow, no MTL
/// missed, no RBU). Every smoltcp timer (ARP, TCP retransmit,
/// delayed-ACK) was similarly compressed — this fix is a
/// prerequisite for the Net-6d TCP demo, not just for ping.
pub fn next_tick() -> u64 {
    let t: u64;
    // SAFETY: `rdtime` reads the S-mode `time` CSR — side-effect
    // free, always readable (no TVM/counter-enable trap: OpenSBI
    // delegates TIME counter access on both platforms). R6: no
    // memory access, no ordering requirement.
    unsafe { core::arch::asm!("rdtime {t}", t = out(reg) t) };
    t / (TIMEBASE_HZ / 1000)
}

/// Single accessor for the `TIER2_NET` singleton. Returns
/// `&mut Option<Tier2NetHandle>` so callers can either take a
/// `.as_mut()` borrow (steady-state poll / socket calls) or
/// pattern-match for the `None` (uninstalled) case.
///
/// Centralizes the `static mut` aliasing so the rest of the
/// module never names `TIER2_NET` directly.
///
/// # Safety
/// INV-1 single-hart + install-then-read ordering (INV-14
/// generalized). Caller must not hold another live borrow.
unsafe fn tier2_net_slot() -> &'static mut Option<Tier2NetHandle> {
    // SAFETY: contract above. addr_of_mut!()/.as_mut() is the
    // 2024-edition-safe way to materialize a `&'static mut` to a
    // `static mut`; the lifetime is sound because the static is
    // valid for the kernel's whole lifetime.
    unsafe { &mut *addr_of_mut!(TIER2_NET) }
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
/// convention + safety as [`socket_create`]. Also closes any open
/// accept-deadline window for the handle so a reused handle starts
/// with a fresh window (see `ACCEPT_FIRST_MS`).
///
/// # Safety
///
/// Same as [`socket_create`].
pub unsafe fn socket_close(handle: u32) -> Result<i32, KernelError> {
    if (handle as usize) < ACCEPT_TRACK_SLOTS {
        // SAFETY: INV-1 (single-hart host-fn path), as in
        // socket_accept.
        unsafe {
            (*addr_of_mut!(ACCEPT_FIRST_MS))[handle as usize] = None;
            (*addr_of_mut!(ACCEPT_EXPIRED_LOGGED))[handle as usize] = false;
        }
    }
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
/// waiting, `E_TIMEDOUT` once the accept window has expired,
/// negative errno on driver error.
///
/// The accept window opens at the FIRST call for `handle` and lasts
/// `wari_abi::net::ACCEPT_DEADLINE_MS` of wall-clock time; after
/// that, every call returns `E_TIMEDOUT` (no poll driven) until the
/// socket is closed. Phase-1c Ctrl-R fix B — see `ACCEPT_FIRST_MS`.
///
/// # Safety
/// Same as [`socket_create`].
pub unsafe fn socket_accept(handle: u32, tick_ms: u64) -> Result<i32, KernelError> {
    // Deadline gate first: an expired window answers without
    // touching the driver (the kmain idle loop owns polling once
    // tenants exit).
    if (handle as usize) < ACCEPT_TRACK_SLOTS {
        // SAFETY: INV-1 (single-hart) — socket_accept/socket_close
        // are host-fn paths on the one hart; no concurrent access.
        // INV-8 install-then-read ordering as for TIER2_NET.
        let first = unsafe { &mut (*addr_of_mut!(ACCEPT_FIRST_MS))[handle as usize] };
        match *first {
            None => {
                *first = Some(tick_ms);
                // Timing trace: stopwatch this line against the
                // matching "expired" line below — the real gap must
                // be ~ACCEPT_DEADLINE_MS (60 s). A gap far off that
                // means the rdtime-derived clock (TIMEBASE_HZ) is
                // wrong on this platform.
                crate::kprintln!("[accept] window open h={} t0={}ms", handle, tick_ms);
            }
            Some(t0) => {
                // Wedge trace (build 15x): the deadline never fired on
                // silicon despite a sane t0. Print both operands,
                // throttled to ~1/8192 polls so the UART survives.
                #[cfg(feature = "debug-kernel")]
                {
                    static mut DL_TICK: u32 = 0;
                    // SAFETY: INV-1 single-hart; diagnostic counter only.
                    let n = unsafe {
                        DL_TICK = DL_TICK.wrapping_add(1);
                        DL_TICK
                    };
                    if n & 0x1FFF == 0 {
                        crate::kdebug!(
                            net,
                            "deadline h{} t0={}ms now={}ms elapsed={}ms budget={}ms",
                            handle,
                            t0,
                            tick_ms,
                            tick_ms.saturating_sub(t0),
                            crate::abi::net::ACCEPT_DEADLINE_MS
                        );
                    }
                }
                if crate::abi::net::deadline_exceeded(
                    t0,
                    tick_ms,
                    crate::abi::net::ACCEPT_DEADLINE_MS,
                ) {
                    // SAFETY: INV-1, same access rules as `first`.
                    let logged =
                        unsafe { &mut (*addr_of_mut!(ACCEPT_EXPIRED_LOGGED))[handle as usize] };
                    if !*logged {
                        *logged = true;
                        crate::kprintln!(
                            "[accept] window expired h={} t={}ms (open {}ms, budget {}ms)",
                            handle,
                            tick_ms,
                            t0,
                            crate::abi::net::ACCEPT_DEADLINE_MS,
                        );
                    }
                    return Ok(crate::cap::syscall::E_TIMEDOUT);
                }
            }
        }
    }
    // SAFETY: INV-1 + INV-8.
    let slot =
        unsafe { addr_of_mut!(TIER2_NET).as_mut() }.expect("TIER2_NET ref always valid (static)");
    let h = slot.as_mut().ok_or(KernelError::DriverError)?;
    // Drive one smoltcp poll first so any pending SYN is processed
    // into the listening socket's state before we inspect it.
    let _ = h
        .poll_fn
        .call(&mut h.store, tick_ms)
        .map_err(|_| KernelError::DriverError)?;
    let rc = h
        .socket_accept_fn
        .call(&mut h.store, handle)
        .map_err(|_| KernelError::DriverError)?;
    if rc == 1 && (handle as usize) < ACCEPT_TRACK_SLOTS {
        // Connection accepted — the window served its purpose.
        // SAFETY: INV-1, as above.
        unsafe {
            (*addr_of_mut!(ACCEPT_FIRST_MS))[handle as usize] = None;
            (*addr_of_mut!(ACCEPT_EXPIRED_LOGGED))[handle as usize] = false;
        }
        crate::kprintln!("[accept] accepted h={}", handle);
    }
    Ok(rc)
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
    let slot =
        unsafe { addr_of_mut!(TIER2_NET).as_mut() }.expect("TIER2_NET ref always valid (static)");
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
