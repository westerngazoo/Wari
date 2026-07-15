// SPDX-License-Identifier: AGPL-3.0-only
//! Pure scheduling policy — decisions over a snapshot of process
//! states, extracted from `kernel/src/sched/mod.rs`.
//!
//! Policy lives here; mechanism stays in the kernel. Each function
//! takes an iterator over the process table's states (`None` = empty
//! slot, index = `proc_id`) and decides — it never mutates, so it is
//! host-testable and, later, a Kani target alongside the state
//! machine in [`crate::process`].

use crate::process::ProcessState;

/// Find the lowest `proc_id` whose state is [`ProcessState::Ready`].
///
/// `states` is a snapshot of the process table in `proc_id` order
/// (`None` = empty slot).
///
/// # Contract
/// - Returns the first `Ready` index — run-to-completion in
///   registration order is the Phase-1b policy (see
///   `kernel/src/sched/mod.rs` module docs).
/// - Returns `None` when no `Ready` entry exists.
/// - Only the first 256 slots are considered: `proc_id` is `u8`
///   (`cap::MAX_PROCS` is 64 today, so the bound is theoretical, but
///   it makes the `as u8` conversion provably lossless).
/// - Panics: never.
///
/// ```
/// use wari_sched::{pick_next_tenant, ProcessState};
///
/// let table = [None, Some(ProcessState::Library), Some(ProcessState::Ready)];
/// assert_eq!(pick_next_tenant(table), Some(2));
///
/// // No Ready tenant → no pick.
/// assert_eq!(pick_next_tenant([None, Some(ProcessState::Faulted)]), None);
/// ```
pub fn pick_next_tenant<I>(states: I) -> Option<u8>
where
    I: IntoIterator<Item = Option<ProcessState>>,
{
    states
        .into_iter()
        .take(u8::MAX as usize + 1)
        .enumerate()
        .find(|(_, s)| matches!(s, Some(ProcessState::Ready)))
        .map(|(i, _)| i as u8)
}

/// Count entries in [`ProcessState::Blocked`] — the scheduler's
/// all-blocked (IPC deadlock) detector.
///
/// # Contract
/// - Counts exactly the `Blocked { .. }` entries; empty slots and
///   every other state contribute 0.
/// - Panics: never.
///
/// ```
/// use wari_sched::{count_blocked, BlockReason, ProcessState};
///
/// let table = [
///     None,
///     Some(ProcessState::Blocked { reason: BlockReason::RecvWait, ep_idx: 0 }),
///     Some(ProcessState::Ready),
/// ];
/// assert_eq!(count_blocked(table), 1);
/// assert_eq!(count_blocked([None, None]), 0);
/// ```
pub fn count_blocked<I>(states: I) -> usize
where
    I: IntoIterator<Item = Option<ProcessState>>,
{
    states
        .into_iter()
        .filter(|s| matches!(s, Some(ProcessState::Blocked { .. })))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::BlockReason;

    fn blocked() -> Option<ProcessState> {
        Some(ProcessState::Blocked {
            reason: BlockReason::SendWait,
            ep_idx: 0,
        })
    }

    #[test]
    fn picks_lowest_ready_skipping_other_states() {
        let table = [
            None,
            Some(ProcessState::Library),
            Some(ProcessState::Running),
            blocked(),
            Some(ProcessState::Exited(0)),
            Some(ProcessState::Ready),
            Some(ProcessState::Ready),
        ];
        // First Ready wins; the later Ready at index 6 is not picked.
        assert_eq!(pick_next_tenant(table), Some(5));
    }

    #[test]
    fn empty_and_ready_less_tables_yield_none() {
        assert_eq!(pick_next_tenant([]), None);
        assert_eq!(pick_next_tenant([None, None, None]), None);
        assert_eq!(
            pick_next_tenant([blocked(), Some(ProcessState::Faulted)]),
            None
        );
    }

    #[test]
    fn slots_beyond_u8_range_are_never_picked() {
        // Ready sits at index 256 — outside the u8 proc_id space.
        // The policy must not truncate 256 → 0; it ignores the slot.
        let mut table = vec![None; 257];
        table[256] = Some(ProcessState::Ready);
        assert_eq!(pick_next_tenant(table), None);
    }

    #[test]
    fn counts_only_blocked() {
        let table = [
            None,
            blocked(),
            Some(ProcessState::Ready),
            blocked(),
            Some(ProcessState::Running),
        ];
        assert_eq!(count_blocked(table), 2);
    }
}
