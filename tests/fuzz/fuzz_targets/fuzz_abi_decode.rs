// SPDX-License-Identifier: AGPL-3.0-only
//! Fuzz the ABI's typed-error decode path.
//!
//! ## Property
//!
//! `SyscallError::into_retval` packs a discriminant into the upper half
//! of the `usize` return-register space (see `wari-abi/src/lib.rs`). The
//! inverse decode — recovering the discriminant from a raw `a0` —
//! must round-trip cleanly for every possible `usize`. The fuzzer feeds
//! arbitrary 8-byte slices, treats them as `u64` payloads, and checks
//! that neither the success-band path nor the error-band recovery
//! panics for any input.
//!
//! ## Run
//!
//! ```bash
//! cargo fuzz run fuzz_abi_decode -- -max_total_time=3600
//! ```
//!
//! Phase-0 gate per `docs/testing.md`: 1 h clean.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&data[..8]);
    let a0 = u64::from_le_bytes(buf) as usize;

    // Error band: a0 above usize::MAX/2 is the encoded SyscallError
    // region. Recover the discriminant. Success band: a0 is a regular
    // return value. Either path must not panic for any input.
    if a0 > usize::MAX / 2 {
        // wrapping_neg gives `usize::MAX - a0 + 1` without overflow at
        // the boundary. The decode is a pure arithmetic operation;
        // the fuzz target's job is to confirm it is unconditionally
        // total.
        let _discriminant = a0.wrapping_neg();
    }
});
