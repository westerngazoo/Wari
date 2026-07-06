// SPDX-License-Identifier: AGPL-3.0-only
//! Wari — synchronous IPC rendezvous core (Lane B / B2, Option B).
//!
//! The **pure decision logic** for seL4-style synchronous IPC: given an
//! operation and whether a compatible peer is currently waiting on the
//! Endpoint, decide whether the caller rendezvouses now or enqueues and
//! blocks — and, for a `call`, that it always ends up awaiting a reply.
//!
//! This is the host-testable heart of the base IPC the Option-B
//! preemptive TCB scheduler is built on (see `docs/ipc-design.md`). The
//! kernel wires the impure mechanism around it: the Endpoint queues, the
//! register/message transfer, the `ProcessState::Blocked(reason)`
//! transition, and the context switch. Keeping the *decision* pure means
//! it can be exhaustively tested (and later proved) without the
//! scheduler. No `unsafe`, no allocation.

#![cfg_attr(not(test), no_std)]

/// Why a thread is blocked, waiting on an Endpoint (or its reply object).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    /// Enqueued as a sender: waiting for a receiver to take the message.
    SendWait,
    /// Enqueued as a receiver: waiting for a sender to deliver one.
    RecvWait,
    /// Message delivered; now waiting for the reply (`call` after it was
    /// received).
    ReplyWait,
    /// Enqueued as a caller: waiting first to be received, then replied
    /// to (`call` with no receiver ready). The kernel promotes this to
    /// `ReplyWait` once a receiver takes the message.
    CallWait,
}

/// What the *caller* of the op does after the pure decision, when a peer
/// was waiting and the message transferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerNext {
    /// The op is complete; the caller keeps running (`send`/`recv` that
    /// found a peer, and `reply`).
    Continue,
    /// The caller blocks for the reply (`call` that delivered to a waiting
    /// receiver).
    Block(BlockReason),
}

/// The outcome the kernel acts on for one IPC op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A compatible peer was waiting: transfer the message, ready the
    /// peer, and the caller does `caller`.
    Rendezvous {
        /// What the caller does after the transfer.
        caller: CallerNext,
    },
    /// No peer waiting: the caller enqueues on the Endpoint and blocks
    /// with this reason.
    Enqueue {
        /// The state the caller blocks in.
        block: BlockReason,
    },
    /// The op is not valid in this state — e.g. a `reply` with no caller
    /// awaiting one. Fail closed (the kernel returns an error).
    Invalid,
}

/// The synchronous IPC operations, over an Endpoint capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Deliver a message; block only if no receiver is ready.
    Send,
    /// Receive a message; block only if no sender is ready.
    Recv,
    /// Send + block for a reply (mints a one-shot reply to the receiver).
    Call,
    /// Reply to a caller currently in `ReplyWait`.
    Reply,
}

/// Resolve one IPC op given whether a **compatible peer is currently
/// waiting** on the Endpoint (a receiver for `Send`/`Call`; a sender for
/// `Recv`; a reply-waiting caller for `Reply`).
///
/// Pure: the kernel does the queue push/pop, the register transfer, and
/// the context switch around this decision.
///
/// - `Send`: deliver + `Continue`; else enqueue `SendWait`.
/// - `Recv`: take + `Continue`; else enqueue `RecvWait`.
/// - `Call`: deliver + `Block(ReplyWait)`; else enqueue `CallWait` (always awaits a reply).
/// - `Reply`: deliver + `Continue` if a caller waits; else `Invalid`.
///
/// ```
/// use wari_ipc::{resolve, Op, Outcome, CallerNext, BlockReason};
/// // A send with a receiver ready hands off and continues.
/// assert_eq!(
///     resolve(Op::Send, true),
///     Outcome::Rendezvous { caller: CallerNext::Continue }
/// );
/// // A call with no receiver enqueues, awaiting receive-then-reply.
/// assert_eq!(
///     resolve(Op::Call, false),
///     Outcome::Enqueue { block: BlockReason::CallWait }
/// );
/// // A reply with nobody waiting is invalid.
/// assert_eq!(resolve(Op::Reply, false), Outcome::Invalid);
/// ```
#[inline]
pub const fn resolve(op: Op, peer_waiting: bool) -> Outcome {
    match op {
        Op::Send => {
            if peer_waiting {
                Outcome::Rendezvous {
                    caller: CallerNext::Continue,
                }
            } else {
                Outcome::Enqueue {
                    block: BlockReason::SendWait,
                }
            }
        }
        Op::Recv => {
            if peer_waiting {
                Outcome::Rendezvous {
                    caller: CallerNext::Continue,
                }
            } else {
                Outcome::Enqueue {
                    block: BlockReason::RecvWait,
                }
            }
        }
        Op::Call => {
            if peer_waiting {
                Outcome::Rendezvous {
                    caller: CallerNext::Block(BlockReason::ReplyWait),
                }
            } else {
                Outcome::Enqueue {
                    block: BlockReason::CallWait,
                }
            }
        }
        Op::Reply => {
            if peer_waiting {
                Outcome::Rendezvous {
                    caller: CallerNext::Continue,
                }
            } else {
                Outcome::Invalid
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONT: Outcome = Outcome::Rendezvous {
        caller: CallerNext::Continue,
    };

    #[test]
    fn send_delivers_or_blocks() {
        assert_eq!(resolve(Op::Send, true), CONT);
        assert_eq!(
            resolve(Op::Send, false),
            Outcome::Enqueue {
                block: BlockReason::SendWait
            }
        );
    }

    #[test]
    fn recv_takes_or_blocks() {
        assert_eq!(resolve(Op::Recv, true), CONT);
        assert_eq!(
            resolve(Op::Recv, false),
            Outcome::Enqueue {
                block: BlockReason::RecvWait
            }
        );
    }

    #[test]
    fn call_always_ends_awaiting_reply() {
        // Delivered to a waiting receiver → block for the reply.
        assert_eq!(
            resolve(Op::Call, true),
            Outcome::Rendezvous {
                caller: CallerNext::Block(BlockReason::ReplyWait)
            }
        );
        // No receiver → enqueue as a caller (promoted to ReplyWait later).
        assert_eq!(
            resolve(Op::Call, false),
            Outcome::Enqueue {
                block: BlockReason::CallWait
            }
        );
    }

    #[test]
    fn reply_needs_a_waiting_caller() {
        assert_eq!(resolve(Op::Reply, true), CONT);
        assert_eq!(resolve(Op::Reply, false), Outcome::Invalid);
    }

    #[test]
    fn only_call_blocks_the_caller_on_rendezvous() {
        // Send/Recv/Reply continue on rendezvous; only Call blocks.
        for op in [Op::Send, Op::Recv, Op::Reply] {
            assert_eq!(resolve(op, true), CONT, "{op:?} should continue");
        }
        assert!(matches!(
            resolve(Op::Call, true),
            Outcome::Rendezvous {
                caller: CallerNext::Block(_)
            }
        ));
    }

    #[test]
    fn const_evaluable() {
        const O: Outcome = resolve(Op::Send, true);
        assert_eq!(O, CONT);
    }
}
