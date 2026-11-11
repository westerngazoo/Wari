// SPDX-License-Identifier: AGPL-3.0-only
//! Network-wait bookkeeping: which tenants are suspended in an accept
//! and on which socket, plus the decision of whether an accept result
//! ends the wait.
//!
//! A Tier-1 tenant that calls `net_socket_accept` before a client has
//! connected used to busy-poll the host fn, keeping the only hart and
//! charging the whole wait to one fuel step. Now the host fn blocks the
//! tenant (`BlockReason::NetWait`) and records it here; the kernel
//! scheduler re-runs the accept for every waiter after each network
//! poll and wakes the ones whose result is final. This is the blocking
//! I/O shape of every microkernel with a single scheduler (seL4's
//! notification wait, Hubris's `recv` with a notification mask): the
//! waiting task holds no CPU and the event source readies it.
//!
//! Policy lives here; mechanism (the static table, the poll, the wake)
//! stays in the kernel's `runtime::net_wait`.

/// What the accept primitive returns while the socket is still
/// listening with no client: the driver's `socket_poll_accept` answers
/// 0, and the kernel's accept-window gate passes it through untouched
/// until the window expires.
pub const ACCEPT_PENDING: i32 = 0;

/// Whether one accept result ends a wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptPoll {
    /// No client yet and the window is still open: keep waiting.
    Pending,
    /// The wait is over; this is the value the host fn returns to the
    /// tenant (1 accepted, `E_TIMEDOUT` window expired, any other
    /// negative errno a driver or cap failure).
    Done(i32),
}

/// Classify one accept result.
///
/// # Contract
/// - [`ACCEPT_PENDING`] → [`AcceptPoll::Pending`]; every other value →
///   [`AcceptPoll::Done`] carrying it unchanged.
/// - Panics: never.
///
/// ```
/// use wari_sched::net_wait::{classify_accept, AcceptPoll, ACCEPT_PENDING};
///
/// assert_eq!(classify_accept(ACCEPT_PENDING), AcceptPoll::Pending);
/// assert_eq!(classify_accept(1), AcceptPoll::Done(1));
/// assert_eq!(classify_accept(-7), AcceptPoll::Done(-7));
/// ```
pub const fn classify_accept(rc: i32) -> AcceptPoll {
    if rc == ACCEPT_PENDING {
        AcceptPoll::Pending
    } else {
        AcceptPoll::Done(rc)
    }
}

/// Why [`NetWaiters::register`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetWaitError {
    /// `proc_id` is not below the table size.
    OutOfRange,
    /// The process already waits; a blocked tenant cannot issue a
    /// second accept, so this is a kernel bug the caller fails closed on.
    AlreadyWaiting,
}

/// The waiter set: slot `i` holds the driver socket handle process `i`
/// waits on, or `None`. Indexed by `proc_id` like the process table,
/// so it can never hold more waiters than there are processes and
/// registration needs no search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetWaiters<const N: usize> {
    handles: [Option<u32>; N],
}

impl<const N: usize> Default for NetWaiters<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> NetWaiters<N> {
    /// The empty set. `const` so the kernel can place it in a static.
    pub const fn new() -> Self {
        Self { handles: [None; N] }
    }

    /// Record that `proc_id` waits for a connection on `handle`.
    ///
    /// # Errors
    /// - [`NetWaitError::OutOfRange`] if `proc_id as usize >= N`.
    /// - [`NetWaitError::AlreadyWaiting`] if `proc_id` already waits;
    ///   the existing entry is left untouched.
    ///
    /// ```
    /// use wari_sched::net_wait::{NetWaiters, NetWaitError};
    ///
    /// let mut w = NetWaiters::<4>::new();
    /// assert_eq!(w.register(2, 0), Ok(()));
    /// assert_eq!(w.register(2, 1), Err(NetWaitError::AlreadyWaiting));
    /// assert_eq!(w.register(4, 0), Err(NetWaitError::OutOfRange));
    /// ```
    pub fn register(&mut self, proc_id: u8, handle: u32) -> Result<(), NetWaitError> {
        let slot = self
            .handles
            .get_mut(proc_id as usize)
            .ok_or(NetWaitError::OutOfRange)?;
        if slot.is_some() {
            return Err(NetWaitError::AlreadyWaiting);
        }
        *slot = Some(handle);
        Ok(())
    }

    /// Remove `proc_id` from the set and return the handle it waited
    /// on. `None` if it was not waiting or is out of range. Idempotent,
    /// so the kernel can call it on every tenant release.
    pub fn take(&mut self, proc_id: u8) -> Option<u32> {
        self.handles.get_mut(proc_id as usize)?.take()
    }

    /// `true` if no process waits.
    pub fn is_empty(&self) -> bool {
        self.handles.iter().all(Option::is_none)
    }

    /// Snapshot of `(proc_id, handle)` for every waiter, lowest
    /// `proc_id` first. A copy, so the kernel can wake waiters (and
    /// mutate the set) while walking it.
    pub fn snapshot(&self) -> [Option<(u8, u32)>; N] {
        let mut out = [None; N];
        for (i, h) in self.handles.iter().enumerate().take(u8::MAX as usize + 1) {
            out[i] = h.map(|h| (i as u8, h));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_only_pending_keeps_waiting() {
        assert_eq!(classify_accept(0), AcceptPoll::Pending);
        for rc in [1, -1, -2, -3, -7, i32::MIN, i32::MAX] {
            assert_eq!(classify_accept(rc), AcceptPoll::Done(rc));
        }
    }

    #[test]
    fn register_take_round_trip() {
        let mut w = NetWaiters::<4>::new();
        assert!(w.is_empty());
        assert_eq!(w.register(1, 3), Ok(()));
        assert_eq!(w.register(3, 0), Ok(()));
        assert!(!w.is_empty());
        assert_eq!(w.snapshot(), [None, Some((1, 3)), None, Some((3, 0))]);
        assert_eq!(w.take(1), Some(3));
        assert_eq!(w.take(1), None, "take is idempotent");
        assert_eq!(w.take(3), Some(0));
        assert!(w.is_empty());
    }

    #[test]
    fn register_rejects_out_of_range_and_double_wait() {
        let mut w = NetWaiters::<2>::new();
        assert_eq!(w.register(2, 0), Err(NetWaitError::OutOfRange));
        assert_eq!(w.register(u8::MAX, 0), Err(NetWaitError::OutOfRange));
        assert_eq!(w.register(0, 5), Ok(()));
        assert_eq!(w.register(0, 6), Err(NetWaitError::AlreadyWaiting));
        assert_eq!(w.take(0), Some(5), "refused register kept the first entry");
        assert!(w.is_empty());
    }

    #[test]
    fn take_out_of_range_is_none() {
        let mut w = NetWaiters::<2>::new();
        assert_eq!(w.take(9), None);
    }
}
