// SPDX-License-Identifier: AGPL-3.0-only
//! Wari — scheduler pure logic (host-testable core).
//!
//! Lane B-1 of the Option-B extraction program
//! (`docs/kernel-host-testing-design.md` §4/§9): the process-metadata
//! state machine ([`process`]) and the pick-next scheduling policy
//! ([`policy`]), extracted from `kernel/src/sched/` so they compile
//! and test on the host. The kernel keeps the imperative shell — the
//! `PROCESSES` static (INV-1/INV-8), the `tier1_pool`
//! resumable-execution machinery, and the run loop — plus a
//! re-export shim (`kernel/src/sched/process.rs`) so kernel call
//! sites are unchanged. Same pattern as `wari-mem`.
//!
//! Policy lives here; mechanism stays in the kernel. The functions in
//! [`policy`] take a snapshot of process states and *decide*; they
//! never touch the process table.

#![cfg_attr(not(test), no_std)]

pub mod policy;
pub mod process;

pub use policy::{count_blocked, pick_next_tenant};
pub use process::{
    transfer_msg, BlockReason, MsgRegs, Process, ProcessState, MSG_WORDS, NO_MSG_BUF,
};
