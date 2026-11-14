// SPDX-License-Identifier: AGPL-3.0-only
//! Network waits that yield the hart: the kernel side of
//! `wari_sched::net_wait`.
//!
//! `net_socket_accept` with no client yet blocks the calling tenant
//! instead of returning "still listening" for it to spin on:
//!
//! 1. The host fn tries once ([`accept`]). A final result (accepted,
//!    window expired, error) returns straight away, as before.
//! 2. Otherwise it records the tenant in [`NET_WAITERS`], marks it
//!    `Blocked { reason: NetWait }` and yields with
//!    `Err(host(IpcBlock))` — the `tier1_pool` yield protocol, the same
//!    one IPC and `event_read` use.
//! 3. The scheduler calls [`service`] before every pick: it re-runs
//!    the accept for each waiter (each attempt drives one smoltcp poll)
//!    and wakes the ones whose result is final, leaving that result as
//!    the host fn's return value. When nothing is runnable but waiters
//!    exist, it keeps servicing instead of declaring them deadlocked
//!    (`sched::run`).
//!
//! A wait always ends: the accept window (`wari_abi::net::
//! ACCEPT_DEADLINE_MS`, enforced in `tier2_net::socket_accept`) turns
//! every pending accept into `E_TIMEDOUT` after 60 s.
//!
//! The waiter keeps the driver handle it resolved through its Socket
//! cap when it blocked. A blocked tenant cannot drop its own cap, and
//! the driver socket stays open while a cap names it (INV-26), so the
//! handle stays valid until the wait ends. If the tenant dies while
//! waiting, `tier1_pool::release` forgets the waiter before the
//! scheduler closes its sockets.

use core::ptr::addr_of_mut;

use wari_sched::net_wait::{classify_accept, AcceptPoll, NetWaiters};
use wasmi::Error;

use crate::cap::syscall::{net_socket_accept_on, net_socket_accept_resolve, E_INVAL};
use crate::cap::MAX_PROCS;
use crate::kprintln;
use crate::runtime::tier1_pool::{self, IpcBlock};
use crate::sched::process::BlockReason;

/// `ep_idx` recorded for a net waiter. Not a real endpoint index: net
/// waits are queued here, never on an Endpoint, so the endpoint-revoke
/// sweep must never match one. Same reserved value and reasoning as
/// `event_read`'s `EVENT_WAIT_SENTINEL` (`runtime::wasi`); both can
/// share it because neither reason is ever looked up by endpoint.
const NET_WAIT_SENTINEL: u8 = u8::MAX;

/// Tenants suspended in an accept, by `proc_id`.
static mut NET_WAITERS: NetWaiters<MAX_PROCS> = NetWaiters::new();

/// Accessor for [`NET_WAITERS`].
///
/// # Safety contract
/// INV-1 (single-hart): only the host-fn path and the scheduler touch
/// it, both on the one hart, never concurrently. INV-8: statically
/// initialized. Callers must not hold the returned reference across
/// another call — same discipline as `sched::processes()`.
fn waiters() -> &'static mut NetWaiters<MAX_PROCS> {
    // SAFETY: INV-1 + INV-8 as documented above.
    unsafe { &mut *addr_of_mut!(NET_WAITERS) }
}

/// Body of the `wari::net_socket_accept(slot) -> i32` host fn.
///
/// # Returns
/// - `Ok(1)` a connection is ready for `net_socket_send_canned`.
/// - `Ok(E_TIMEDOUT)` the accept window expired.
/// - `Ok(errno)` cap or driver failure (`E_INVAL`, `E_PERM`, `E_NOMEM`).
/// - `Err(host(IpcBlock))` no client yet: the tenant is now blocked and
///   resumes later with one of the values above, never with
///   `ACCEPT_PENDING`.
pub fn accept(pid: u8, slot: u32) -> Result<i32, Error> {
    let handle = match net_socket_accept_resolve(pid, slot) {
        Ok(h) => h,
        Err(e) => return Ok(e),
    };
    match classify_accept(net_socket_accept_on(handle)) {
        AcceptPoll::Done(rc) => Ok(rc),
        AcceptPoll::Pending => block(pid, handle),
    }
}

/// Record `pid` as waiting on `handle`, mark it blocked and produce the
/// yield. A refused registration (the tenant already waits, impossible
/// for a running tenant) answers `E_INVAL` instead of suspending it
/// without a waker.
fn block(pid: u8, handle: u32) -> Result<i32, Error> {
    if waiters().register(pid, handle).is_err() {
        return Ok(E_INVAL);
    }
    let table = crate::sched::processes();
    match table[pid as usize].as_mut() {
        Some(p) => {
            p.block(BlockReason::NetWait, NET_WAIT_SENTINEL);
            Err(Error::host(IpcBlock))
        }
        None => {
            let _ = waiters().take(pid);
            Ok(E_INVAL)
        }
    }
}

/// `true` if any tenant waits on the network.
pub fn has_waiters() -> bool {
    !waiters().is_empty()
}

/// Re-run the accept for every waiter and wake each one whose result
/// is final, with that result as its host fn's return value. Returns
/// how many were woken. Called from scheduler context only (no wasmi
/// frame of any Tier-1 instance is live).
pub fn service() -> usize {
    let mut woken = 0;
    for (pid, handle) in waiters().snapshot().into_iter().flatten() {
        let AcceptPoll::Done(rc) = classify_accept(net_socket_accept_on(handle)) else {
            continue;
        };
        let _ = waiters().take(pid);
        if tier1_pool::set_resume_value(pid, rc).is_err() || crate::sched::wake(pid).is_err() {
            // The waiter's slot or process is gone without `forget`
            // having run: a teardown path missed it. Dropping the
            // entry is the whole repair; say so on the console.
            kprintln!("[net-wait] stale waiter proc_id={} dropped", pid);
            continue;
        }
        woken += 1;
    }
    woken
}

/// Drop `pid`'s wait, if any. Called whenever its Tier-1 slot is
/// released, so a tenant that dies never leaves a waiter behind.
pub fn forget(pid: u8) {
    let _ = waiters().take(pid);
}
