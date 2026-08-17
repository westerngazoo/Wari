// SPDX-License-Identifier: AGPL-3.0-only
//! Pure event-record format and ring arithmetic for the kernel audit
//! stream.
//!
//! One concern: the **shape** of the audit stream — what an event
//! record is, how the fixed-capacity ring assigns slots, and how a
//! reader detects loss. The kernel's `runtime::events` owns the
//! actual static ring and the emit sites; guard agents (Tier-1/2
//! WASM) will decode records with this same crate. One source of
//! truth for the layout on both sides of the trust boundary —
//! INV-24's discipline applied to events instead of wire frames.
//!
//! ## Why lossy-oldest with a monotonic sequence
//!
//! The ring **overwrites the oldest** record when full. The
//! alternatives are refusing new events (an attacker who fills the
//! ring silences the audit of their next move — the worst possible
//! failure mode for a security stream) or blocking the emitter
//! (unacceptable in kernel paths). Loss is instead made *visible*:
//! sequence numbers are monotonic, so a reader that fell behind can
//! compute exactly how many records it lost and report the gap
//! rather than silently missing it. Prior art: Linux perf ring
//! buffers and lockless tracing buffers make the same trade for the
//! same reason.
//!
//! ## Trust posture
//!
//! Records are kernel-authored; readers are less-trusted WASM. The
//! record carries no pointers and no variable-length data — two
//! scalar payload fields interpreted per [`EventKind`]. Anything
//! richer (names, paths) belongs in a follow-up record kind that
//! indexes a separate bounded table, never inline.

#![no_std]

/// Records the ring holds. Power of two so slot arithmetic is a
/// mask, and small enough (128 × 16 B = 2 KiB) that the guard's
/// question is "did I fall behind", not "is the kernel spending RAM
/// on me".
pub const RING_CAPACITY: usize = 128;

/// What happened. `#[repr(u16)]`, append-only, never renumbered —
/// a guard agent compiled against an older list must still decode
/// newer streams (unknown kinds pass through as [`EventKind::raw`]
/// values it can log-and-skip).
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// A module envelope was staged into the registry.
    /// `a` = registry slot, `b` = envelope length in bytes.
    ModuleStaged = 1,
    /// A staged envelope passed signature verification and was
    /// registered. `a` = registry slot, `b` = assigned proc_id.
    SpawnVerified = 2,
    /// A staged envelope FAILED verification and was refused.
    /// `a` = registry slot, `b` = 0. The record every guard agent
    /// exists to see.
    SpawnRejected = 3,
    /// A Tier-1 tenant exited. `a` = proc_id, `b` = exit code
    /// (`as u32` — negative codes wrap, decode with `as i32`).
    TenantExited = 4,
    /// A Tier-1 tenant faulted (trap, invalid WASM behavior).
    /// `a` = proc_id, `b` = 0.
    TenantFaulted = 5,
    /// The Supervisor attenuated a spawn's requested authority: the
    /// module asked for more than its ceiling allowed, and the excess
    /// was withheld. `a` = proc_id, `b` = the DENIED `GrantSpec` bits.
    /// A security-relevant record every guard exists to see — the
    /// system telling you a module reached for authority it did not
    /// get.
    GrantAttenuated = 6,
    /// A daemon-flagged module terminated and the registry restarted
    /// it. `a` = the NEW proc_id, `b` = restarts remaining in its
    /// budget. A resident guard watching this sees its peers heal.
    DaemonRestarted = 7,
    /// A daemon exhausted its restart budget and was NOT restarted —
    /// it crash-looped past the cap. `a` = last proc_id, `b` = 0. The
    /// record that says "a service is down and staying down".
    DaemonGaveUp = 8,
    // Append-only. New kinds get the next discriminant.
}

impl EventKind {
    /// Decode a raw discriminant. `None` for kinds this build does
    /// not know — the caller logs-and-skips rather than refusing the
    /// stream (a newer kernel must not brick an older guard).
    pub fn from_raw(v: u16) -> Option<Self> {
        match v {
            1 => Some(EventKind::ModuleStaged),
            2 => Some(EventKind::SpawnVerified),
            3 => Some(EventKind::SpawnRejected),
            4 => Some(EventKind::TenantExited),
            5 => Some(EventKind::TenantFaulted),
            6 => Some(EventKind::GrantAttenuated),
            7 => Some(EventKind::DaemonRestarted),
            8 => Some(EventKind::DaemonGaveUp),
            _ => None,
        }
    }
}

/// One audit record. 16 bytes, `repr(C)`, no padding surprises —
/// the layout is asserted below because WASM readers will see these
/// bytes across the boundary.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    /// Monotonic sequence number, never reused, starts at 0.
    pub seq: u64,
    /// Raw [`EventKind`] discriminant (`from_raw` to decode).
    pub kind: u16,
    /// First payload scalar; meaning per kind.
    pub a: u16,
    /// Second payload scalar; meaning per kind.
    pub b: u32,
}

const _: () = assert!(core::mem::size_of::<Event>() == 16);
const _: () = assert!(RING_CAPACITY.is_power_of_two());

/// Writer-side ring state: the next sequence number to assign.
/// The kernel holds one of these next to its static record array.
#[derive(Debug, Default, Clone, Copy)]
pub struct RingState {
    /// Next sequence to assign == count of events ever recorded.
    pub next_seq: u64,
}

impl RingState {
    /// Claim the slot for the next record.
    ///
    /// # Contract
    /// - Returns `(slot_index, seq)`; the caller stores an [`Event`]
    ///   with exactly that `seq` at exactly that index.
    /// - Monotonic: each call returns `seq` one greater than the
    ///   last. Wrapping u64 is not reachable in bounded uptime
    ///   (2⁶⁴ events at one per nanosecond is five centuries).
    /// - Panics: never.
    ///
    /// ```
    /// use wari_events::{RingState, RING_CAPACITY};
    /// let mut s = RingState::default();
    /// assert_eq!(s.claim(), (0, 0));
    /// assert_eq!(s.claim(), (1, 1));
    /// // After a full lap the slot repeats but the seq never does:
    /// let mut s = RingState { next_seq: RING_CAPACITY as u64 };
    /// assert_eq!(s.claim(), (0, RING_CAPACITY as u64));
    /// ```
    pub fn claim(&mut self) -> (usize, u64) {
        let seq = self.next_seq;
        self.next_seq += 1;
        ((seq as usize) & (RING_CAPACITY - 1), seq)
    }
}

/// What a reader at `reader_seq` should do next, given the writer's
/// `next_seq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadPlan {
    /// Nothing new; wait or poll again.
    UpToDate,
    /// Read the record with sequence `seq` at ring index `slot`,
    /// then advance the cursor to `seq + 1`.
    Read {
        /// Ring index holding the record.
        slot: usize,
        /// Its sequence number (equals the reader's cursor).
        seq: u64,
    },
    /// The reader fell behind and `lost` records were overwritten.
    /// Resume at `resume_seq` (the oldest still-available record)
    /// after reporting the gap — loss is surfaced, never silent.
    Lagged {
        /// How many records were irrecoverably overwritten.
        lost: u64,
        /// The cursor to resume from.
        resume_seq: u64,
    },
}

/// Compute the next read action for a cursor.
///
/// # Contract
/// - `reader_seq` is the next sequence the reader wants;
///   `next_seq` is the writer's [`RingState::next_seq`].
/// - Total: every `(reader_seq <= next_seq + anything)` input maps
///   to a plan; a cursor *ahead* of the writer (corrupt reader
///   state) is treated as `UpToDate` rather than trusted.
/// - Panics: never.
///
/// ```
/// use wari_events::{read_plan, ReadPlan, RING_CAPACITY};
/// // Fresh reader, empty ring:
/// assert_eq!(read_plan(0, 0), ReadPlan::UpToDate);
/// // One event available:
/// assert_eq!(read_plan(0, 1), ReadPlan::Read { slot: 0, seq: 0 });
/// // Reader slept through 300 events on a 128-ring: the oldest
/// // surviving record is seq 300-128=172, and 172 were lost.
/// assert_eq!(
///     read_plan(0, 300),
///     ReadPlan::Lagged { lost: 172, resume_seq: 172 }
/// );
/// ```
pub fn read_plan(reader_seq: u64, next_seq: u64) -> ReadPlan {
    if reader_seq >= next_seq {
        return ReadPlan::UpToDate;
    }
    let oldest = next_seq.saturating_sub(RING_CAPACITY as u64);
    if reader_seq < oldest {
        return ReadPlan::Lagged {
            lost: oldest - reader_seq,
            resume_seq: oldest,
        };
    }
    ReadPlan::Read {
        slot: (reader_seq as usize) & (RING_CAPACITY - 1),
        seq: reader_seq,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_wrap_seqs_do_not() {
        let mut s = RingState::default();
        let mut last_seq = None;
        for i in 0..(RING_CAPACITY * 3) {
            let (slot, seq) = s.claim();
            assert_eq!(slot, i & (RING_CAPACITY - 1));
            assert_eq!(seq, i as u64);
            if let Some(prev) = last_seq {
                assert_eq!(seq, prev + 1);
            }
            last_seq = Some(seq);
        }
    }

    #[test]
    fn reader_walks_a_partially_filled_ring_without_loss() {
        let mut w = RingState::default();
        for _ in 0..5 {
            w.claim();
        }
        let mut cursor = 0;
        let mut seen = 0;
        while let ReadPlan::Read { slot, seq } = read_plan(cursor, w.next_seq) {
            assert_eq!(slot, seq as usize & (RING_CAPACITY - 1));
            cursor = seq + 1;
            seen += 1;
        }
        assert_eq!(seen, 5);
        assert_eq!(read_plan(cursor, w.next_seq), ReadPlan::UpToDate);
    }

    #[test]
    fn loss_is_exact_and_resumable() {
        // Writer laps the reader twice and a bit.
        let written = (RING_CAPACITY * 2 + 40) as u64;
        match read_plan(0, written) {
            ReadPlan::Lagged { lost, resume_seq } => {
                assert_eq!(lost, written - RING_CAPACITY as u64);
                assert_eq!(resume_seq, written - RING_CAPACITY as u64);
                // Resuming from there reads cleanly.
                assert!(matches!(
                    read_plan(resume_seq, written),
                    ReadPlan::Read { .. }
                ));
            }
            other => panic!("expected Lagged, got {:?}", other),
        }
    }

    #[test]
    fn corrupt_ahead_cursor_is_not_trusted() {
        assert_eq!(read_plan(10, 3), ReadPlan::UpToDate);
        assert_eq!(read_plan(u64::MAX, 3), ReadPlan::UpToDate);
    }

    #[test]
    fn kind_round_trip_and_forward_compat() {
        for k in [
            EventKind::ModuleStaged,
            EventKind::SpawnVerified,
            EventKind::SpawnRejected,
            EventKind::TenantExited,
            EventKind::TenantFaulted,
        ] {
            assert_eq!(EventKind::from_raw(k as u16), Some(k));
        }
        // Unknown kinds decode to None (log-and-skip), not a refusal.
        assert_eq!(EventKind::from_raw(999), None);
    }
}
