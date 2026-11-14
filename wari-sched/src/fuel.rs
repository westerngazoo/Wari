// SPDX-License-Identifier: AGPL-3.0-only
//! Fuel policy for Tier-1 instances (ADR-002 step 4).
//!
//! One concern: how much wasmi fuel a Tier-1 instance may burn in a
//! single scheduling step. The kernel (`runtime::loader::load_tier1`,
//! `runtime::tier1_pool`) is the mechanism: it enables metering on the
//! Tier-1 engine, sets this budget before `_start` and again before
//! every resume, and faults the tenant when wasmi reports
//! `TrapCode::OutOfFuel`.
//!
//! ## Why a per-step budget, and why exhaustion faults
//!
//! A step is the run between two yields (`_start` to the first IPC
//! block, or one resume to the next). Refilling at every resume means
//! a tenant that cooperates — blocks on IPC, exits — is never charged
//! for its lifetime, only for how long it keeps the hart without
//! letting go. A tenant that never yields hits the budget and is
//! faulted; the scheduler regains the hart and the other tenants, the
//! network poll and Ctrl-R keep working.
//!
//! Faulting rather than preempting is a constraint of wasmi 0.32.3:
//! out-of-fuel is a trap that unwinds the whole invocation, not a
//! resumable yield (resumption after out-of-fuel arrived upstream in
//! wasmi 0.45, 2025-05). The budget and the `TrapCode::OutOfFuel`
//! classification stay the same when that lands; only "fault" becomes
//! "requeue".
//!
//! ## Sizing
//!
//! wasmi charges roughly one unit per instruction (`FuelCosts`
//! default). The budget has to cover the longest step a legitimate
//! tenant takes. Waiting is not computation: a tenant that waits for a
//! client in `net_socket_accept` is blocked (`BlockReason::NetWait`,
//! `crate::net_wait`) and resumed with a fresh budget, so a wait costs
//! no fuel however long it lasts. What is left in a step is real work:
//! no tenant in the boot trace burns more than a few hundred units in
//! one step (283 at most, QEMU, with network waits blocking). A resume
//! can log 0 used: wasmi charges a block's fuel when the block is
//! entered, so straight-line code after a blocking call was paid for
//! before the yield. Any loop still pays at its header on every turn.
//!
//! 10 M units is that bound with several orders of magnitude to spare.
//! It also bounds how long a runaway tenant keeps the hart before it is
//! faulted: the `apps/hog` spin burns about 24 M units/s on QEMU, so
//! well under half a second.
//!
//! History: builds 171–172 ran 150 M while the accept wait was still a
//! busy-poll charged to one step (about 0.67 M units/s on the VF2 for
//! up to 60 s; 10 M faulted the demo after ~15 s). Making the wait
//! block removed that cost and the budget came back down.
//!
//! Tier-2 drivers are not metered here (they get a larger budget and
//! a watchdog in a later step, per ADR-002).

/// Fuel a Tier-1 instance may burn in one scheduling step before the
/// scheduler faults it. See the module docs for how it is sized.
pub const TIER1_FUEL_PER_STEP: u64 = 10_000_000;
