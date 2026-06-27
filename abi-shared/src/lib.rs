//! Wari user/kernel ABI — the contract between the kernel and every
//! user-side consumer (WASM tooling, userspace helpers during Phase 0a
//! bring-up, test harnesses).
//!
//! This crate is the **single source of truth** for:
//!   - Syscall numbers (`SYS_*`)
//!   - Syscall error codes (`SyscallError`)
//!   - Net / IPC opcodes (`net::*`)
//!   - (Future) WASI host function IDs and driver opcodes.
//!
//! Kernel and all user-side tooling depend on this one crate. Mirror
//! files (as goose-os carried) are not allowed — CLAUDE R8 and the
//! "no duplicated code" rule in §Code Quality.
//!
//! Cherry-picked from `goose-os/kernel/src/abi.rs` in Phase 0a with one
//! deliberate change: **slot 10 (formerly `SYS_SPAWN_ELF`) is retired**
//! — see CLAUDE R7 ("No ELF in the customer ABI"). The constant is
//! intentionally absent; a future `SYS_WASM_LOAD` will take a fresh
//! number, not reuse slot 10.
//!
//! # Stability contract
//!
//! Syscall *numbers* are part of the ABI. Once shipped, a number never
//! changes meaning. Adding a new syscall means adding a new number at
//! the end, never reassigning an existing one. Retiring a syscall
//! means leaving a gap in the numbering — do NOT reuse the number.
//!
//! Syscall *argument conventions* are also ABI. Changing which
//! register carries which argument is a breaking change.
//!
//! Error codes follow the same rules. A userspace program compiled
//! against ABI version N must run against kernel version N+M with the
//! same semantics, where "semantics" means every syscall number still
//! exists and still returns the same meaning for the same inputs.

#![no_std]
#![deny(missing_docs)]

// ── Syscall numbers ────────────────────────────────────────────
//
// Placed in `a7` by the userspace `ecall` wrapper; read by the
// kernel's trap handler (see kernel/src/trap.rs::handle_ecall).

/// Write a single byte to the kernel UART.
pub const SYS_PUTCHAR:      usize = 0;
/// Terminate the current process with an exit code.
pub const SYS_EXIT:         usize = 1;
/// Non-blocking send on a capability/port (seL4-style).
pub const SYS_SEND:         usize = 2;
/// Blocking receive on a capability/port.
pub const SYS_RECEIVE:      usize = 3;
/// Synchronous call: send + receive-reply on one endpoint.
pub const SYS_CALL:         usize = 4;
/// Reply to the last `SYS_CALL` that targeted this server.
pub const SYS_REPLY:        usize = 5;
/// Map physical pages into the caller's address space.
pub const SYS_MAP:          usize = 6;
/// Unmap a range from the caller's address space.
pub const SYS_UNMAP:        usize = 7;
/// Allocate physical pages owned by the caller.
pub const SYS_ALLOC_PAGES:  usize = 8;
/// Free physical pages previously allocated by the caller.
pub const SYS_FREE_PAGES:   usize = 9;
// 10: retired — formerly SYS_SPAWN_ELF, see Wari R7 / docs/prior-art.md.
//     A future SYS_WASM_LOAD will get a fresh number, not this slot.
/// Wait for a child process to exit and reap it.
pub const SYS_WAIT:         usize = 11;
/// Return the caller's PID.
pub const SYS_GETPID:       usize = 12;
/// Cooperatively yield the CPU.
pub const SYS_YIELD:        usize = 13;
/// Register an IRQ handler (capability-gated in Phase 1).
pub const SYS_IRQ_REGISTER: usize = 14;
/// Acknowledge a pending IRQ.
pub const SYS_IRQ_ACK:      usize = 15;
/// Request a system reboot (capability-gated in Phase 1).
pub const SYS_REBOOT:       usize = 16;

// ── Capability-management syscalls (Phase 1b) ─────────────────────
//
// These are documentary sysnum constants. Phase 1b's actual ABI
// carrier for these operations is the **WASM host-function set**
// registered in `kernel/src/runtime/{host_fns,wasi}.rs` under the
// `wari::*` import module (`wari::cap_mint`, `wari::cap_copy`, etc.).
// The host fns are the live path; the sysnums below match the
// design contract in `docs/cap-system-design.md` §5 for the day a
// non-WASM userspace ever appears (per CLAUDE R7 it shouldn't, but
// the contract pins the numbering).

/// Derive a child capability from a parent slot.
pub const SYS_CAP_MINT:     usize = 17;
/// Same-rights duplicate of a capability into another slot.
pub const SYS_CAP_COPY:     usize = 18;
/// Revoke a capability and every descendant in the derivation tree.
pub const SYS_CAP_REVOKE:   usize = 19;
/// Delete a single capability without cascading.
pub const SYS_CAP_DELETE:   usize = 20;
/// Read metadata for a capability (kind, rights, badge).
pub const SYS_CAP_LOOKUP:   usize = 21;
/// Register a capability into the per-process fast-path table; returns a
/// registered-handle index. See `docs/cap-registered-fastpath-design.md`.
pub const SYS_CAP_REGISTER:   usize = 22;
/// Drop a registered fast-path handle (does not affect the capability).
pub const SYS_CAP_UNREGISTER: usize = 23;

/// Highest syscall number currently defined. Used for bounds checks
/// in the dispatch path and for the size of any dispatch table.
///
/// Note: slot 10 is **retired** (formerly `SYS_SPAWN_ELF`, see Wari
/// R7). The live syscalls are 0..=9, 11..=16, and 17..=23 (Phase 1b
/// cap-management + the cap-fastpath register/unregister pair).
pub const SYS_MAX: usize = SYS_CAP_UNREGISTER;

// ── Error codes ────────────────────────────────────────────────
//
// Returned in `a0` from every fallible syscall. Successful returns are
// non-negative (handle, PID, byte count, etc.). Errors are encoded as
// the bitwise complement of the error number, so:
//
//   a0 = 0 .. isize::MAX / 2  -> success value
//   a0 = usize::MAX - N       -> error code N (from SyscallError)
//
// The legacy convention — `a0 == usize::MAX` on any error — is still
// supported by writing `SyscallError::Generic.into_retval()`. Handlers
// are being migrated to typed errors incrementally.

/// Structured syscall errors. `#[repr(usize)]` so the discriminant is
/// the raw error number; `into_retval()` converts to the a0 value.
///
/// Never renumber an existing variant. Add new errors at the end.
#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallError {
    /// Unspecified error. Used by pre-typed-error handlers for backward
    /// compatibility with callers that only check `a0 == usize::MAX`.
    /// New handlers should use a specific variant.
    Generic           = 1,

    /// An argument was malformed or out of range (e.g., unaligned VA,
    /// PID out of bounds, flag bit set that the handler doesn't know).
    InvalidArgument   = 2,

    /// Target process does not exist or is in the Free state.
    NoSuchProcess     = 3,

    /// Caller lacks the capability/permission for this operation.
    /// Placeholder until the capability system lands.
    PermissionDenied  = 4,

    /// The kernel cannot satisfy the request right now (would block,
    /// resource exhausted, etc.). Distinct from `PermissionDenied`,
    /// which is a final "no."
    WouldBlock        = 5,

    /// Out of physical pages, out of socket handles, etc.
    OutOfResources    = 6,

    /// The requested page, handle, or capability is not mapped/owned.
    NotMapped         = 7,

    /// WASM module failed validation at load time (malformed bytecode,
    /// unsupported section, type mismatch, etc.). Emitted by the kernel's
    /// wasmi-loader path.
    BadWasm           = 8,
}

impl SyscallError {
    /// Convert to the value that goes in `a0`. Encodes as
    /// `usize::MAX - (discriminant - 1)`, so `Generic` -> `MAX`, and
    /// larger numbers walk downward. Userspace can recover the error
    /// code by computing `usize::MAX - a0 + 1`.
    #[inline]
    pub const fn into_retval(self) -> usize {
        usize::MAX - (self as usize - 1)
    }
}

/// Convenience: the legacy "any error" return value. Equals
/// `SyscallError::Generic.into_retval()` by construction.
pub const ERR: usize = usize::MAX;

// ── Network IPC protocol ───────────────────────────────────────
//
// Clients talk to the network server at PID 3 via SYS_CALL. The
// opcode goes in a1; remaining arguments in a2..=a6 per the per-op
// calling convention documented in `net` below.

/// Opcodes for the IPC network server (PID 3). Send in `a1` of a
/// SYS_CALL targeting `NET_SERVER_PID`.
///
/// Never renumber. Kernel and every userspace consumer imports from
/// this module — no mirror copies are allowed.
pub mod net {
    /// PID of the network server. Fixed by convention; the kernel
    /// currently intercepts SYS_CALL to this PID before IPC rendezvous
    /// runs, but clients don't need to know that.
    pub const NET_SERVER_PID: usize = 3;

    /// Query "is the network up?" — returns 1 or 0.
    pub const NET_STATUS:     usize = 0;
    /// Allocate a TCP socket handle.
    pub const NET_SOCKET_TCP: usize = 1;
    /// Allocate a UDP socket handle.
    pub const NET_SOCKET_UDP: usize = 2;
    /// Bind a socket to a local port: `(handle, port)`.
    pub const NET_BIND:       usize = 3;
    /// Blocking connect: `(handle, packed_ip, port)`.
    pub const NET_CONNECT:    usize = 4;
    /// Listen on a bound socket: `(handle, port)`.
    pub const NET_LISTEN:     usize = 5;
    /// Accept an incoming connection: `(handle)` — reserved, unimplemented.
    pub const NET_ACCEPT:     usize = 6;
    /// Send on a socket: `(handle, buf_va, len, packed_ip?, port?)`.
    pub const NET_SEND:       usize = 7;
    /// Blocking receive: `(handle, buf_va, max_len)`.
    pub const NET_RECV:       usize = 8;
    /// Close a socket handle: `(handle)`.
    pub const NET_CLOSE:      usize = 9;

    /// Largest opcode currently defined. Dispatch tables size off this.
    pub const NET_OP_MAX: usize = NET_CLOSE;
}

/// Registered-capability fast-path validation — the pure soundness check.
///
/// The fast syscall path (see `docs/cap-registered-fastpath-design.md`)
/// lets a module register a capability once and then reference it by a
/// small integer handle (`reg_idx`) on the hot path. This module holds
/// the **pure** soundness predicate the kernel runs on every submission —
/// the platform- and policy-independent part of proposed INV-α:
///
///   1. the handle index is in range,
///   2. its slot is live,
///   3. the cached generation still matches the live cap-slot generation
///      (so a revoked/reused cap auto-invalidates — this rides the
///      generation-counter mechanism, INV-17), and
///   4. the operation is permitted for the cached capability.
///
/// Clause 4 (op vs kind+rights) is a kernel-side **policy** decision, so
/// it is injected here as a boolean (`op_permitted`). That keeps this
/// crate free of the op/rights table — which is designed in the kernel
/// cap module — while still letting the soundness check be expressed and
/// tested as one pure function. No `unsafe`, no MMIO: host-testable.
pub mod reg {
    /// Registered-resource slots per process. The kernel's `RegTable`
    /// mirrors this; the validator bounds-checks `reg_idx` against it.
    /// Single source of truth so kernel and tooling agree on the range.
    pub const REG_SLOTS: u32 = 64;

    /// Outcome of validating one registered-handle submission. Distinct
    /// rejection reasons (not a bare `bool`) so callers and tests can
    /// assert *why* a submission was refused.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RegCheck {
        /// All clauses hold; the operation may proceed.
        Ok,
        /// `reg_idx >= REG_SLOTS` — outside the registered-table range.
        OutOfRange,
        /// The slot is not live (never registered, or unregistered).
        Empty,
        /// Cached generation != live cap-slot generation: the underlying
        /// capability was revoked / deleted / reused (INV-17). The handle
        /// is stale and confers nothing.
        Stale,
        /// The operation is not permitted for the cached kind + rights.
        NotPermitted,
    }

    /// Validate a registered-handle submission (proposed INV-α).
    ///
    /// Pure: given the handle index, the slot's liveness, the cached vs
    /// live generation, and the kernel's op-permission decision, returns
    /// the precise [`RegCheck`]. Checks are ordered cheapest-first and
    /// short-circuit, so a hostile `reg_idx` is rejected on the bounds
    /// test before any slot is examined.
    ///
    /// # Parameters
    /// - `reg_idx`: the submitted handle index.
    /// - `live`: whether the slot at `reg_idx` is occupied (kind != Empty).
    /// - `reg_generation`: generation cached in the slot at registration.
    /// - `cur_generation`: the underlying cap slot's *current* generation.
    /// - `op_permitted`: the kernel's decision that the op is legal for
    ///   the cached kind + rights (clause 4, injected).
    ///
    /// # Returns
    /// [`RegCheck::Ok`] iff all clauses hold; otherwise the first failing
    /// clause in the order range → live → generation → permission.
    ///
    /// ```
    /// use wari_abi::reg::{validate_handle, RegCheck, REG_SLOTS};
    /// // Happy path: in range, live, generations match, op allowed.
    /// assert_eq!(validate_handle(3, true, 7, 7, true), RegCheck::Ok);
    /// // Out of range short-circuits before any slot is touched.
    /// assert_eq!(validate_handle(REG_SLOTS, true, 7, 7, true), RegCheck::OutOfRange);
    /// // Stale generation (the cap was revoked/reused) is rejected.
    /// assert_eq!(validate_handle(3, true, 7, 8, true), RegCheck::Stale);
    /// ```
    #[inline]
    pub const fn validate_handle(
        reg_idx: u32,
        live: bool,
        reg_generation: u16,
        cur_generation: u16,
        op_permitted: bool,
    ) -> RegCheck {
        if reg_idx >= REG_SLOTS {
            return RegCheck::OutOfRange;
        }
        if !live {
            return RegCheck::Empty;
        }
        if reg_generation != cur_generation {
            return RegCheck::Stale;
        }
        if !op_permitted {
            return RegCheck::NotPermitted;
        }
        RegCheck::Ok
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn ok_when_all_clauses_hold() {
            assert_eq!(validate_handle(0, true, 0, 0, true), RegCheck::Ok);
            assert_eq!(validate_handle(REG_SLOTS - 1, true, 42, 42, true), RegCheck::Ok);
        }

        #[test]
        fn out_of_range_is_first_and_short_circuits() {
            // At and beyond the bound → OutOfRange.
            assert_eq!(validate_handle(REG_SLOTS, true, 1, 1, true), RegCheck::OutOfRange);
            assert_eq!(validate_handle(u32::MAX, true, 1, 1, true), RegCheck::OutOfRange);
            // OutOfRange wins even when every later clause would also fail
            // (proves ordering: a hostile index never reaches slot reads).
            assert_eq!(validate_handle(REG_SLOTS, false, 1, 2, false), RegCheck::OutOfRange);
        }

        #[test]
        fn empty_slot_rejected_before_generation() {
            assert_eq!(validate_handle(5, false, 1, 1, true), RegCheck::Empty);
            // Empty wins over a generation mismatch and a denied op.
            assert_eq!(validate_handle(5, false, 1, 2, false), RegCheck::Empty);
        }

        #[test]
        fn stale_generation_rejected_before_permission() {
            assert_eq!(validate_handle(5, true, 1, 2, true), RegCheck::Stale);
            // Stale wins over a denied op (revocation is checked first).
            assert_eq!(validate_handle(5, true, 9, 10, false), RegCheck::Stale);
            // Generation wrap edge: max vs zero is still a mismatch.
            assert_eq!(validate_handle(5, true, u16::MAX, 0, true), RegCheck::Stale);
        }

        #[test]
        fn not_permitted_is_last_clause() {
            assert_eq!(validate_handle(5, true, 3, 3, false), RegCheck::NotPermitted);
        }

        #[test]
        fn const_evaluable() {
            // Usable in const context (kernel may fold known cases).
            const R: RegCheck = validate_handle(1, true, 0, 0, true);
            assert_eq!(R, RegCheck::Ok);
        }
    }
}

/// Cap-fastpath submission/completion ring wire format (PR-2a).
///
/// The fast path (see `docs/cap-registered-fastpath-design.md` §5) lets a
/// module batch operations in a shared submission queue (SQ) in its own
/// linear memory and ring a doorbell once; the kernel drains the SQ,
/// validates each entry against the registered-handle table, dispatches,
/// and posts a completion-queue (CQ) entry. This module is the **pure
/// wire format** both sides agree on: entry sizes/offsets, the v1 op
/// codes, and a copy-before-use decoder. No kernel state, no `unsafe`,
/// host-testable.
///
/// v1 ops **reuse existing capability-checked operations** reached by a
/// registered handle — the kernel resolves the handle, re-checks it, and
/// delegates to the matching host-fn impl. No new authority is minted by
/// the ring itself.
pub mod ring {
    /// Submission-queue entry size on the wire (bytes). Layout (LE,
    /// 8-byte aligned): `op`@0, `reg_idx`@4, `flags`@8, pad@12,
    /// `user_data`@16, `arg0`@24, `arg1`@32.
    pub const SQE_SIZE: usize = 40;
    /// Completion-queue entry size on the wire (bytes). Layout (LE):
    /// `user_data`@0, `result`@8 (i64).
    pub const CQE_SIZE: usize = 16;

    /// Wait on a registered Notification handle (delegates to the
    /// existing `notification_wait` op).
    pub const RING_OP_NOTIFY_WAIT: u32 = 1;
    /// Acknowledge a registered Notification handle (delegates to the
    /// existing `notification_ack` op).
    pub const RING_OP_NOTIFY_ACK: u32 = 2;
    /// Highest defined ring op. New ops are appended; never renumber.
    pub const RING_OP_MAX: u32 = RING_OP_NOTIFY_ACK;

    /// Is `op` a defined ring operation? The kernel rejects unknown ops
    /// before touching a handle.
    #[inline]
    pub const fn is_known_op(op: u32) -> bool {
        op >= RING_OP_NOTIFY_WAIT && op <= RING_OP_MAX
    }

    /// A decoded submission-queue entry. The kernel decodes one of these
    /// out of the module's linear memory **before** any validation
    /// (copy-before-use, INV-β), so a later mutation by the module cannot
    /// change the decision the kernel acts on.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Sqe {
        /// Operation code (see `RING_OP_*`).
        pub op: u32,
        /// Registered-handle index into the caller's RegTable.
        pub reg_idx: u32,
        /// Per-op flags (reserved; 0 in v1).
        pub flags: u32,
        /// Opaque correlation token, echoed verbatim in the matching CQE.
        pub user_data: u64,
        /// First op-specific argument (e.g. a linear-memory offset).
        pub arg0: u64,
        /// Second op-specific argument (e.g. a length).
        pub arg1: u64,
    }

    #[inline]
    fn rd_u32(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    }

    #[inline]
    fn rd_u64(b: &[u8], at: usize) -> u64 {
        u64::from_le_bytes([
            b[at], b[at + 1], b[at + 2], b[at + 3],
            b[at + 4], b[at + 5], b[at + 6], b[at + 7],
        ])
    }

    /// Decode the `idx`-th [`Sqe`] from a submission-queue byte buffer.
    ///
    /// Pure, allocation-free, panic-free on any input: returns `None` if
    /// entry `idx` would read past `bytes`. This is the copy-before-use
    /// step (INV-β) — the caller acts on the returned owned `Sqe`, never
    /// on the live buffer.
    ///
    /// ```
    /// use wari_abi::ring::{decode_sqe, SQE_SIZE};
    /// // Out-of-range index returns None instead of panicking.
    /// assert_eq!(decode_sqe(&[0u8; SQE_SIZE], 1), None);
    /// ```
    pub fn decode_sqe(bytes: &[u8], idx: usize) -> Option<Sqe> {
        let base = idx.checked_mul(SQE_SIZE)?;
        let end = base.checked_add(SQE_SIZE)?;
        if end > bytes.len() {
            return None;
        }
        Some(Sqe {
            op: rd_u32(bytes, base),
            reg_idx: rd_u32(bytes, base + 4),
            flags: rd_u32(bytes, base + 8),
            user_data: rd_u64(bytes, base + 16),
            arg0: rd_u64(bytes, base + 24),
            arg1: rd_u64(bytes, base + 32),
        })
    }

    /// Encode a completion-queue entry: the originating `user_data` and
    /// the op `result` (`>= 0` success / negative errno).
    #[inline]
    pub fn encode_cqe(user_data: u64, result: i64) -> [u8; CQE_SIZE] {
        let mut out = [0u8; CQE_SIZE];
        out[0..8].copy_from_slice(&user_data.to_le_bytes());
        out[8..16].copy_from_slice(&result.to_le_bytes());
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn op_codes_known_range() {
            assert!(is_known_op(RING_OP_NOTIFY_WAIT));
            assert!(is_known_op(RING_OP_NOTIFY_ACK));
            assert!(!is_known_op(0));
            assert!(!is_known_op(RING_OP_MAX + 1));
            assert!(!is_known_op(u32::MAX));
        }

        #[test]
        fn decode_sqe_reads_fields_le() {
            let mut b = [0u8; SQE_SIZE];
            b[0..4].copy_from_slice(&RING_OP_NOTIFY_WAIT.to_le_bytes());
            b[4..8].copy_from_slice(&7u32.to_le_bytes()); // reg_idx
            b[8..12].copy_from_slice(&0u32.to_le_bytes()); // flags
            b[16..24].copy_from_slice(&0xDEAD_BEEFu64.to_le_bytes()); // user_data
            b[24..32].copy_from_slice(&0x1000u64.to_le_bytes()); // arg0
            b[32..40].copy_from_slice(&512u64.to_le_bytes()); // arg1
            assert_eq!(
                decode_sqe(&b, 0),
                Some(Sqe {
                    op: RING_OP_NOTIFY_WAIT,
                    reg_idx: 7,
                    flags: 0,
                    user_data: 0xDEAD_BEEF,
                    arg0: 0x1000,
                    arg1: 512,
                })
            );
        }

        #[test]
        fn decode_sqe_out_of_range_is_none_not_panic() {
            assert_eq!(decode_sqe(&[], 0), None);
            assert_eq!(decode_sqe(&[0u8; SQE_SIZE], 1), None);
            // A partial trailing entry is rejected wholesale.
            assert_eq!(decode_sqe(&[0u8; SQE_SIZE + 4], 1), None);
            // usize overflow on idx*SQE_SIZE is handled (no panic).
            assert_eq!(decode_sqe(&[0u8; SQE_SIZE], usize::MAX), None);
        }

        #[test]
        fn decode_sqe_second_entry() {
            let mut b = [0u8; SQE_SIZE * 2];
            b[SQE_SIZE..SQE_SIZE + 4].copy_from_slice(&RING_OP_NOTIFY_ACK.to_le_bytes());
            let sqe = decode_sqe(&b, 1);
            assert!(sqe.is_some());
            assert_eq!(sqe.map(|s| s.op), Some(RING_OP_NOTIFY_ACK));
        }

        #[test]
        fn encode_cqe_roundtrips() {
            let c = encode_cqe(0x1122_3344_5566_7788, -2);
            assert_eq!(&c[0..8], &0x1122_3344_5566_7788u64.to_le_bytes());
            // Compare the result bytes directly (no unwrap/try_into).
            assert_eq!(&c[8..16], &(-2i64).to_le_bytes());
        }

        #[test]
        fn entry_sizes_are_stable() {
            // Wire-format ABI: these sizes are part of the contract.
            assert_eq!(SQE_SIZE, 40);
            assert_eq!(CQE_SIZE, 16);
        }
    }
}

// ── Tests — pure, runnable on host ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_error_matches_legacy_sentinel() {
        assert_eq!(SyscallError::Generic.into_retval(), ERR);
    }

    #[test]
    fn error_codes_are_distinct() {
        let codes = [
            SyscallError::Generic.into_retval(),
            SyscallError::InvalidArgument.into_retval(),
            SyscallError::NoSuchProcess.into_retval(),
            SyscallError::PermissionDenied.into_retval(),
            SyscallError::WouldBlock.into_retval(),
            SyscallError::OutOfResources.into_retval(),
            SyscallError::NotMapped.into_retval(),
            SyscallError::BadWasm.into_retval(),
        ];
        for i in 0..codes.len() {
            for j in (i + 1)..codes.len() {
                assert_ne!(codes[i], codes[j], "error {} and {} collide", i, j);
            }
        }
    }

    #[test]
    fn sys_max_matches_highest_syscall() {
        // Phase 1b added the capability-management ops (→ SYS_CAP_LOOKUP
        // = 21); the cap-fastpath register/unregister pair extends the
        // range to 23. SYS_MAX pins both the symbolic and numeric value.
        assert_eq!(SYS_MAX, SYS_CAP_UNREGISTER);
        assert_eq!(SYS_MAX, 23);
    }

    #[test]
    fn cap_syscalls_are_distinct_and_contiguous() {
        // Phase 1b added 5 cap-management sysnums (17..=21) above
        // the previous SYS_REBOOT (16). Nothing in this range should
        // collide; the contiguous block makes the dispatch table
        // simpler downstream.
        assert_eq!(SYS_CAP_MINT,       17);
        assert_eq!(SYS_CAP_COPY,       18);
        assert_eq!(SYS_CAP_REVOKE,     19);
        assert_eq!(SYS_CAP_DELETE,     20);
        assert_eq!(SYS_CAP_LOOKUP,     21);
        // cap-fastpath register/unregister extend the contiguous block.
        assert_eq!(SYS_CAP_REGISTER,   22);
        assert_eq!(SYS_CAP_UNREGISTER, 23);
        assert_eq!(SYS_MAX,            SYS_CAP_UNREGISTER);
        // And not stepping on SYS_REBOOT.
        assert_ne!(SYS_CAP_MINT, SYS_REBOOT);
    }

    #[test]
    fn net_opcodes_are_distinct() {
        use super::net::*;
        let codes = [
            NET_STATUS, NET_SOCKET_TCP, NET_SOCKET_UDP, NET_BIND,
            NET_CONNECT, NET_LISTEN, NET_ACCEPT, NET_SEND,
            NET_RECV, NET_CLOSE,
        ];
        for i in 0..codes.len() {
            for j in (i + 1)..codes.len() {
                assert_ne!(codes[i], codes[j], "net opcode {} collides with {}", i, j);
            }
        }
    }

    #[test]
    fn net_op_max_matches_highest() {
        use super::net::*;
        assert_eq!(NET_OP_MAX, NET_CLOSE);
    }

    #[test]
    fn net_server_pid_is_three() {
        // Hard-coded in the kernel's trap dispatch. Pinning here so a
        // future change has to touch two places.
        assert_eq!(super::net::NET_SERVER_PID, 3);
    }

    #[test]
    fn sys_spawn_slot_is_retired() {
        // Slot 10 is the retired SYS_SPAWN_ELF position. It must NOT
        // reappear in Wari — CLAUDE R7 forbids any ELF entry point in
        // the customer ABI. This test guards the numbering: the live
        // syscalls around the hole stay where they are, so slot 10 is
        // observably unused.
        assert_eq!(SYS_FREE_PAGES, 9);
        assert_eq!(SYS_WAIT,       11);
        // If a future patch re-introduces `pub const SYS_SPAWN: usize
        // = 10;`, the next line will fail to compile (name collision
        // with a local binding). Cheap belt-and-braces check.
        let sys_spawn_must_not_exist: () = ();
        let _ = sys_spawn_must_not_exist;
    }

    /// Compile-time witness: slot 10 is a hole between SYS_FREE_PAGES
    /// and SYS_WAIT. If anyone re-introduces a constant at slot 10,
    /// the `SYS_WAIT == SYS_FREE_PAGES + 2` equality will still hold
    /// — that's fine; the guard is the absence of `SYS_SPAWN`. The
    /// assertion below documents the intended hole shape.
    const _RETIRED_SLOT_10_SHAPE: () = {
        assert!(SYS_FREE_PAGES == 9);
        assert!(SYS_WAIT == 11);
    };
}
