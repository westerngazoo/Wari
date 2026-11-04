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
//! tenant takes, and today that is a busy-wait: the HTTP demo polls
//! `net_socket_accept` in a loop for up to the kernel's accept window
//! (`wari_abi::net::ACCEPT_DEADLINE_MS`, 60 s), and a host fn call is
//! not a yield, so the whole wait is one step. Measured cost of that
//! loop, build 171: about 0.67 M units/s on the VF2 (U74) and about
//! 1.0 M units/s on QEMU TCG on the development Mac. Ten million units
//! faulted the demo after ~10–15 s of waiting on both.
//!
//! 150 M units is the 60 s window at the faster of the two rates with
//! a margin of 2.5. The price is how long a runaway tenant keeps the
//! hart before it is faulted: the `apps/hog` spin burns about 24 M
//! units/s on QEMU, so about six seconds rather than a fraction of
//! one. That is the emergency setting; the fix is to
//! make network waits yield, so a waiting tenant costs no fuel at all
//! and the budget can come back down to a computation bound.
//!
//! Tier-2 drivers are not metered here (they get a larger budget and
//! a watchdog in a later step, per ADR-002).

/// Fuel a Tier-1 instance may burn in one scheduling step before the
/// scheduler faults it. See the module docs for how it is sized.
pub const TIER1_FUEL_PER_STEP: u64 = 150_000_000;
