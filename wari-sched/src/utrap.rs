// SPDX-License-Identifier: AGPL-3.0-only
//! What the kernel does with a trap taken from U-mode — the pure
//! decision, with no CSR access (ADR-004 step 1).
//!
//! The kernel's assembly entry saves the U-mode registers and hands
//! `scause`, `a7` and `a0` to [`decide`]; the impure glue in
//! `kernel/src/trap.rs` applies the verdict. A trap from U-mode never
//! halts the kernel: it resumes the process, or ends it.
//!
//! The two syscall numbers here are **provisional** and exist only for
//! the step-1 stub. Step 3 (`wari-rt`) defines the real `ecall` ABI in
//! `abi-shared`, which stays the one source of truth for numbers a
//! module can depend on.
//!
//! Prior art: the scause codes and the "ecall leaves `sepc` on the
//! `ecall` itself" rule are from the RISC-V Privileged Spec §4.1.8–9;
//! the "fault ends only the process" policy is the classic Unix/seL4
//! one (a user fault is delivered to, or kills, the faulting thread).

/// `scause` bit 63: set for interrupts, clear for exceptions.
pub const SCAUSE_INTERRUPT_BIT: u64 = 1 << 63;

/// `scause` exception code for an `ecall` executed in U-mode.
pub const CAUSE_ECALL_FROM_U: u64 = 8;

/// `scause` exception codes the self-test and the logs name.
pub const CAUSE_INSTRUCTION_PAGE_FAULT: u64 = 12;
/// Load page fault (a U-mode read of a page without U=1 lands here).
pub const CAUSE_LOAD_PAGE_FAULT: u64 = 13;
/// Store/AMO page fault.
pub const CAUSE_STORE_PAGE_FAULT: u64 = 15;
/// Illegal instruction (e.g. a privileged CSR access from U-mode).
pub const CAUSE_ILLEGAL_INSTRUCTION: u64 = 2;

/// Provisional syscall 0: returns `a0 + 1` (wrapping). Proves the
/// round trip U → S → U with an argument and a result.
pub const SYS_ECHO: u64 = 0;
/// Provisional syscall 1: ends the process with exit code `a0` (low
/// 32 bits).
pub const SYS_EXIT: u64 = 1;
/// Value returned in `a0` for an unknown syscall number (-1).
pub const ENOSYS_RET: u64 = u64::MAX;

/// Length of an `ecall` instruction. `sepc` points at the `ecall`
/// itself, so a syscall that returns must advance it by this much.
pub const ECALL_LEN: usize = 4;

/// The verdict for one trap from U-mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserTrap {
    /// An interrupt arrived while U-mode ran: service it, then resume.
    Interrupt {
        /// `scause` without the interrupt bit.
        code: u64,
    },
    /// A syscall that returns: write `a0`, step `sepc` past the
    /// `ecall`, resume.
    Return {
        /// The value the process sees in `a0`.
        a0: u64,
    },
    /// The process asked to end.
    Exit {
        /// Its exit code.
        code: u32,
    },
    /// Any other exception: the process is ended, the kernel carries on.
    Kill {
        /// The `scause` exception code.
        cause: u64,
    },
}

/// Decide what to do with a trap taken from U-mode.
///
/// Preconditions: the trap came from U-mode (`sstatus.SPP == 0` at
/// trap time) — the caller's assembly guarantees it.
/// Postconditions: total; every `scause` maps to a verdict, and no
/// verdict halts the kernel.
pub fn decide(scause: u64, a7: u64, a0: u64) -> UserTrap {
    if scause & SCAUSE_INTERRUPT_BIT != 0 {
        return UserTrap::Interrupt {
            code: scause & !SCAUSE_INTERRUPT_BIT,
        };
    }
    if scause != CAUSE_ECALL_FROM_U {
        return UserTrap::Kill { cause: scause };
    }
    match a7 {
        SYS_ECHO => UserTrap::Return {
            a0: a0.wrapping_add(1),
        },
        // Truncation is the contract: the exit code is the low 32 bits.
        SYS_EXIT => UserTrap::Exit { code: a0 as u32 },
        _ => UserTrap::Return { a0: ENOSYS_RET },
    }
}

/// How a process ended, as reported back to the code that entered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserExit {
    /// It called [`SYS_EXIT`].
    Exited(u32),
    /// It took an exception; the payload is the `scause` code.
    Killed(u64),
}

const TAG_SHIFT: u32 = 62;
const TAG_EXITED: u64 = 1;
const TAG_KILLED: u64 = 2;
const PAYLOAD_MASK: u64 = (1 << TAG_SHIFT) - 1;

impl UserExit {
    /// Encode as the single word the assembly leave path carries in
    /// `a0`. Never 0: 0 means "resume the process".
    ///
    /// A `Killed` cause is an exception code, which never has the top
    /// two bits set, so the mask loses nothing.
    pub fn encode(self) -> u64 {
        match self {
            UserExit::Exited(code) => (TAG_EXITED << TAG_SHIFT) | code as u64,
            UserExit::Killed(cause) => (TAG_KILLED << TAG_SHIFT) | (cause & PAYLOAD_MASK),
        }
    }

    /// Decode a word produced by [`UserExit::encode`]. Returns `None`
    /// for 0 (resume) and for any word `encode` cannot produce.
    pub fn decode(word: u64) -> Option<UserExit> {
        let payload = word & PAYLOAD_MASK;
        match word >> TAG_SHIFT {
            TAG_EXITED if payload <= u32::MAX as u64 => Some(UserExit::Exited(payload as u32)),
            TAG_KILLED => Some(UserExit::Killed(payload)),
            _ => None,
        }
    }
}

/// Human name of an exception code, for the kernel log.
pub fn cause_name(cause: u64) -> &'static str {
    match cause {
        CAUSE_ILLEGAL_INSTRUCTION => "illegal instruction",
        CAUSE_INSTRUCTION_PAGE_FAULT => "instruction page fault",
        CAUSE_LOAD_PAGE_FAULT => "load page fault",
        CAUSE_STORE_PAGE_FAULT => "store page fault",
        _ => "exception",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_returns_argument_plus_one() {
        assert_eq!(
            decide(CAUSE_ECALL_FROM_U, SYS_ECHO, 41),
            UserTrap::Return { a0: 42 }
        );
        assert_eq!(
            decide(CAUSE_ECALL_FROM_U, SYS_ECHO, u64::MAX),
            UserTrap::Return { a0: 0 }
        );
    }

    #[test]
    fn exit_keeps_low_32_bits() {
        assert_eq!(
            decide(CAUSE_ECALL_FROM_U, SYS_EXIT, 7),
            UserTrap::Exit { code: 7 }
        );
        assert_eq!(
            decide(CAUSE_ECALL_FROM_U, SYS_EXIT, 0x1_0000_0003),
            UserTrap::Exit { code: 3 }
        );
    }

    #[test]
    fn unknown_syscall_returns_enosys_instead_of_killing() {
        assert_eq!(
            decide(CAUSE_ECALL_FROM_U, 99, 0),
            UserTrap::Return { a0: ENOSYS_RET }
        );
    }

    #[test]
    fn a_user_fault_kills_the_process_never_the_kernel() {
        for cause in [
            CAUSE_LOAD_PAGE_FAULT,
            CAUSE_STORE_PAGE_FAULT,
            CAUSE_INSTRUCTION_PAGE_FAULT,
            CAUSE_ILLEGAL_INSTRUCTION,
            9, // ecall from S: cannot reach this path, still not a syscall
            0,
            63,
        ] {
            assert_eq!(decide(cause, SYS_ECHO, 0), UserTrap::Kill { cause });
        }
    }

    #[test]
    fn interrupts_are_serviced_not_fatal() {
        assert_eq!(
            decide(SCAUSE_INTERRUPT_BIT | 5, 0, 0),
            UserTrap::Interrupt { code: 5 }
        );
        assert_eq!(
            decide(SCAUSE_INTERRUPT_BIT | 9, SYS_EXIT, 0),
            UserTrap::Interrupt { code: 9 }
        );
    }

    #[test]
    fn exit_words_round_trip_and_are_never_zero() {
        for e in [
            UserExit::Exited(0),
            UserExit::Exited(u32::MAX),
            UserExit::Killed(0),
            UserExit::Killed(CAUSE_LOAD_PAGE_FAULT),
        ] {
            let w = e.encode();
            assert_ne!(w, 0, "{e:?} would read as resume");
            assert_eq!(UserExit::decode(w), Some(e));
        }
    }

    #[test]
    fn decode_rejects_words_encode_cannot_produce() {
        assert_eq!(UserExit::decode(0), None);
        assert_eq!(UserExit::decode(3 << TAG_SHIFT), None);
        assert_eq!(
            UserExit::decode((TAG_EXITED << TAG_SHIFT) | (1 << 32)),
            None
        );
    }

    #[test]
    fn cause_names() {
        assert_eq!(cause_name(CAUSE_LOAD_PAGE_FAULT), "load page fault");
        assert_eq!(cause_name(1), "exception");
    }
}
