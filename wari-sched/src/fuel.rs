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
//! default). The Phase-2 demos (hello, planner/executor, guard) use
//! well under 100 000 units per step on QEMU; ten million is two
//! orders of magnitude of headroom and still bounds a runaway tenant
//! to a fraction of a second on the VF2. Tier-2 drivers are not
//! metered here (they get a larger budget and a watchdog in a later
//! step, per ADR-002).

/// Fuel a Tier-1 instance may burn in one scheduling step before the
/// scheduler faults it.
pub const TIER1_FUEL_PER_STEP: u64 = 10_000_000;
