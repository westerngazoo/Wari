// SPDX-License-Identifier: AGPL-3.0-only
//! Per-process metadata tracked by the Phase-1b scheduler — the pure
//! state machine, extracted from `kernel/src/sched/process.rs`.
//!
//! A `Process` is the kernel-side handle that connects a `proc_id`
//! (the index into `cap::storage::CSPACES`) to its tier, module, and
//! current state. The wasmi `Instance` + `Store` *for currently
//! executing processes* live elsewhere — Tier-2 drivers in
//! `runtime::tier2_uart` (singleton, INV-14), Tier-1 instances in
//! the kernel's `runtime::tier1_pool`.
//!
//! Phase-1b processes are sequential: the scheduler spawns each
//! Tier-1 instance, runs it to completion (or until `proc_exit` /
//! an IPC block), and then advances to the next. There is no
//! preemption and no fuel timer. Phase 2+ adds those when there are
//! real workloads that need them.

#![allow(clippy::doc_lazy_continuation)]

use wari_cap::{ModuleId, Tier};

/// Why a blocked process is blocked — re-exported from `wari-ipc`,
/// the pure rendezvous core (Lane B / B2). One source of truth: the
/// decision logic in `wari_ipc::resolve` returns these same values,
/// so the scheduler cannot drift from the IPC state machine.
pub use wari_ipc::BlockReason;

/// seL4-style message registers — the slice of TCB context a
/// synchronous IPC transfer carries (design: `docs/ipc-design.md`
/// §3, "short messages in registers, no copy on the fast path").
///
/// A message is a `badge` (identifies the sender's capability to
/// the receiver) plus [`MSG_WORDS`] data words. Larger payloads go
/// through shared linear memory via the cap-fastpath ring (B1);
/// IPC itself stays register-sized.
///
/// Under the Option-B plan this is the first slice of the full TCB
/// register context: the rendezvous transfer needs exactly these
/// words. The complete GPR save/restore area arrives with timer
/// preemption (a later brick) and will embed this struct rather
/// than replace it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsgRegs {
    /// Capability badge presented to the receiver. 0 = unbadged.
    pub badge: u64,
    /// Message payload words (seL4's MRs).
    pub words: [u64; MSG_WORDS],
}

/// Number of data words a message carries (`docs/ipc-design.md`
/// §9.2 — badge + 4 words).
pub const MSG_WORDS: usize = 4;

impl MsgRegs {
    /// The zero message: unbadged, all words 0.
    pub const EMPTY: MsgRegs = MsgRegs {
        badge: 0,
        words: [0; MSG_WORDS],
    };
}

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
    /// Process is blocked on a synchronous-IPC object and must not
    /// be scheduled until a rendezvous (or a revoke of that object)
    /// readies it. Invariant (docs/ipc-design.md §7): `Blocked` is
    /// always paired with the object it waits on — `ep_idx` is the
    /// Endpoint pool index — so revoking that endpoint can find and
    /// wake every waiter with an error instead of leaking a
    /// permanently-blocked process.
    Blocked {
        /// Which wait this is (sender / receiver / caller /
        /// awaiting-reply) — `wari_ipc::BlockReason`.
        reason: BlockReason,
        /// Endpoint pool index the process is queued on.
        ep_idx: u8,
    },
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
    /// Index into the kernel's process table and `CSPACES`.
    pub proc_id: u8,
    /// Privilege tier of the instance (Tier 1 tenant / Tier 2 driver).
    pub tier: Tier,
    /// Which embedded module this process runs.
    pub module_id: ModuleId,
    /// Current lifecycle state.
    pub state: ProcessState,
    /// Message registers — the IPC slice of this process's TCB
    /// context. Written by a rendezvous transfer while the process
    /// is Blocked; read back by the process's own send/recv host fn
    /// when it resumes. `MsgRegs::EMPTY` outside an IPC exchange.
    pub msg_regs: MsgRegs,
    /// Linear-memory offset of the caller's message buffer, recorded
    /// when the process blocks in `recv`/`call` so the scheduler can
    /// flush a delivered `msg_regs` into it just before resuming
    /// (`runtime::tier1_pool::flush_msg_to_linmem`). [`NO_MSG_BUF`]
    /// = nothing to flush.
    pub msg_buf: u32,
}

/// Sentinel for [`Process::msg_buf`]: no flush pending.
pub const NO_MSG_BUF: u32 = u32::MAX;

impl Process {
    /// Construct a Tier-1 tenant in the `Ready` state, with empty
    /// message registers and no flush pending.
    pub const fn new(proc_id: u8, tier: Tier, module_id: ModuleId) -> Self {
        Self {
            proc_id,
            tier,
            module_id,
            state: ProcessState::Ready,
            msg_regs: MsgRegs::EMPTY,
            msg_buf: NO_MSG_BUF,
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
            msg_regs: MsgRegs::EMPTY,
            msg_buf: NO_MSG_BUF,
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

    /// `true` if the process is blocked on an IPC object.
    pub fn is_blocked(&self) -> bool {
        matches!(self.state, ProcessState::Blocked { .. })
    }

    /// Block this process on the endpoint at `ep_idx` for `reason`.
    ///
    /// # Contract
    /// - Precondition: the process is `Running` (only the currently
    ///   executing process can block itself — a scheduler invariant
    ///   the caller upholds; this method does not verify it because
    ///   the Phase-2 wake-on-revoke path also legitimately rewrites
    ///   `Blocked → Blocked` when promoting `CallWait → ReplyWait`).
    /// - Postcondition: `is_blocked()`, and the pairing invariant
    ///   holds — the state names the endpoint it waits on.
    /// - Panics: never.
    pub fn block(&mut self, reason: BlockReason, ep_idx: u8) {
        self.state = ProcessState::Blocked { reason, ep_idx };
    }

    /// Wake a blocked process: `Blocked → Ready`. Returns `false`
    /// (and changes nothing) if the process was not blocked — wake
    /// is idempotent-safe so the endpoint-revoke sweep can call it
    /// unconditionally on every queued TcbRef.
    pub fn wake(&mut self) -> bool {
        if self.is_blocked() {
            self.state = ProcessState::Ready;
            true
        } else {
            false
        }
    }
}

/// Transfer one message: copy the sender's message registers into
/// the receiver's. The rendezvous data plane — deliberately a pure
/// function over two `MsgRegs` (no process-table access) so it is
/// host-testable and, later, Kani-provable alongside
/// `wari_ipc::resolve` (the decision plane).
///
/// # Contract
/// - Postcondition: receiver's regs are byte-identical to the
///   sender's at call time; sender's regs are unchanged (seL4
///   semantics — delivery does not consume the sender's MRs).
/// - Panics: never.
#[inline]
pub fn transfer_msg(sender: &MsgRegs, receiver: &mut MsgRegs) {
    *receiver = *sender;
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

    #[test]
    fn blocked_is_not_runnable_and_pairs_with_endpoint() {
        let mut p = Process::new(2, Tier::One, ModuleId::Tier1Hello);
        p.block(BlockReason::RecvWait, 3);
        assert!(p.is_blocked());
        assert!(!p.is_runnable());
        assert!(!p.is_terminated());
        // The pairing invariant (ipc-design §7): the state itself
        // names the endpoint, so a revoke sweep can find waiters.
        assert_eq!(
            p.state,
            ProcessState::Blocked {
                reason: BlockReason::RecvWait,
                ep_idx: 3
            }
        );
    }

    #[test]
    fn wake_readies_only_blocked() {
        let mut p = Process::new(2, Tier::One, ModuleId::Tier1Hello);
        p.block(BlockReason::CallWait, 0);
        assert!(p.wake());
        assert_eq!(p.state, ProcessState::Ready);
        assert!(p.is_runnable());
        // Idempotent-safe: waking a non-blocked process is a no-op.
        assert!(!p.wake());
        assert_eq!(p.state, ProcessState::Ready);
        // Never wakes a terminated process into Ready.
        p.state = ProcessState::Exited(0);
        assert!(!p.wake());
        assert_eq!(p.state, ProcessState::Exited(0));
    }

    #[test]
    fn call_wait_promotes_to_reply_wait() {
        // The kernel promotes CallWait → ReplyWait once a receiver
        // takes the message (wari_ipc::BlockReason docs). block()
        // permits the Blocked → Blocked rewrite.
        let mut p = Process::new(2, Tier::One, ModuleId::Tier1Hello);
        p.block(BlockReason::CallWait, 1);
        p.block(BlockReason::ReplyWait, 1);
        assert_eq!(
            p.state,
            ProcessState::Blocked {
                reason: BlockReason::ReplyWait,
                ep_idx: 1
            }
        );
    }

    #[test]
    fn transfer_copies_and_preserves_sender() {
        let src = MsgRegs {
            badge: 0xB0,
            words: [1, 2, 3, 4],
        };
        let mut dst = MsgRegs::EMPTY;
        transfer_msg(&src, &mut dst);
        assert_eq!(dst, src);
        // seL4 semantics: delivery does not consume the sender's MRs.
        assert_eq!(src.words, [1, 2, 3, 4]);
    }

    #[test]
    fn fresh_process_has_empty_msg_regs() {
        let p = Process::new(2, Tier::One, ModuleId::Tier1Hello);
        assert_eq!(p.msg_regs, MsgRegs::EMPTY);
    }
}
