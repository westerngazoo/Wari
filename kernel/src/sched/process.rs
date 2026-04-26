// SPDX-License-Identifier: AGPL-3.0-only
//! Per-process metadata tracked by the Phase-1b scheduler.
//!
//! A `Process` is the kernel-side handle that connects a `proc_id`
//! (the index into `cap::storage::CSPACES`) to its tier, module, and
//! current state. The wasmi `Instance` + `Store` *for currently
//! executing processes* live elsewhere — Tier-2 drivers in
//! `runtime::tier2_uart` (singleton, INV-14), Tier-1 instances in
//! the scheduler's stack frame while they run.
//!
//! Phase-1b processes are sequential: the scheduler spawns each
//! Tier-1 instance, runs it to completion (or until `proc_exit`),
//! and then advances to the next. There is no preemption, no
//! blocking, no fuel timer. Phase 2+ adds those when there are real
//! workloads that need them.

#![allow(dead_code)]
#![allow(clippy::doc_lazy_continuation)]

use crate::cap::{ModuleId, Tier};

/// Lifecycle state of a process tracked by the scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// Slot is unused.
    Free,
    /// Process is loaded but has not yet been scheduled to run.
    Ready,
    /// Process is the currently-running tenant on the hart.
    Running,
    /// Process is loaded as a "library" (Phase-1b Tier-2 drivers
    /// are loaded once, never scheduled — they're called into via
    /// host fns). Conceptually `Ready` but with a different
    /// lifecycle: the scheduler does not pick it for `run`.
    Library,
    /// Process exited cleanly with the given code.
    Exited(i32),
    /// Process trapped (kernel-side fault, BadWasm at runtime, etc.)
    /// and is being torn down. Phase 1b doesn't yet revoke caps on
    /// fault; Phase 2+ adds that via `cap::syscall::cap_revoke`.
    Faulted,
}

/// Per-process metadata. `proc_id` is the index into `CSPACES`;
/// the cap state for this process lives there, not here.
///
/// Phase 1b keeps this struct small (the wasmi `Instance` + `Store`
/// are NOT held here — see module docstring). Phase 2+ may add
/// fields for runqueue links, fuel tracking, and last-IPC-target.
#[derive(Debug, Clone, Copy)]
pub struct Process {
    pub proc_id: u8,
    pub tier: Tier,
    pub module_id: ModuleId,
    pub state: ProcessState,
}

impl Process {
    pub const fn new(proc_id: u8, tier: Tier, module_id: ModuleId) -> Self {
        Self {
            proc_id,
            tier,
            module_id,
            state: ProcessState::Ready,
        }
    }

    /// Construct a "library" process — loaded but never scheduled.
    /// Used for Phase-1b Tier-2 drivers (UART) that are reached via
    /// host fns from Tier-1 callers, never via direct execution.
    pub const fn new_library(proc_id: u8, tier: Tier, module_id: ModuleId) -> Self {
        Self {
            proc_id,
            tier,
            module_id,
            state: ProcessState::Library,
        }
    }

    /// `true` if the scheduler should pick this process to run.
    pub fn is_runnable(&self) -> bool {
        matches!(self.state, ProcessState::Ready | ProcessState::Running)
    }

    /// `true` if the process has terminated (cleanly or otherwise).
    pub fn is_terminated(&self) -> bool {
        matches!(self.state, ProcessState::Exited(_) | ProcessState::Faulted)
    }
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_process_is_ready() {
        let p = Process::new(2, Tier::One, ModuleId::Tier1Hello);
        assert_eq!(p.state, ProcessState::Ready);
        assert!(p.is_runnable());
        assert!(!p.is_terminated());
    }

    #[test]
    fn library_process_not_runnable() {
        let p = Process::new_library(1, Tier::Two, ModuleId::Tier2Uart);
        assert_eq!(p.state, ProcessState::Library);
        assert!(!p.is_runnable());
        assert!(!p.is_terminated());
    }

    #[test]
    fn exited_process_terminated() {
        let mut p = Process::new(2, Tier::One, ModuleId::Tier1Hello);
        p.state = ProcessState::Exited(0);
        assert!(p.is_terminated());
        assert!(!p.is_runnable());
    }

    #[test]
    fn faulted_process_terminated() {
        let mut p = Process::new(2, Tier::One, ModuleId::Tier1Hello);
        p.state = ProcessState::Faulted;
        assert!(p.is_terminated());
        assert!(!p.is_runnable());
    }
}
