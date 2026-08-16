// SPDX-License-Identifier: AGPL-3.0-only
//! Fuzz `wari_mcp::obj_get_raw` with attacker-controlled object *and*
//! key.
//!
//! ## Property
//!
//! `obj_get_raw` walks a JSON object span looking for `key`, driving
//! the same depth-bounded `value_len` skipper `parse_request` uses. It
//! is a `pub` field-extraction primitive tool handlers call directly on
//! the raw `params` span, and it is refuse-by-default — `None` for a
//! missing key *and* for malformed JSON. Its contract is therefore the
//! same "never panics for any input". libFuzzer aborts on a panic; any
//! `Some`/`None` is the success path.
//!
//! ## Split encoding
//!
//! Unlike `parse_request`, `obj_get_raw` takes two byte slices, so the
//! fuzzer's single buffer is split so both arguments are attacker-
//! controlled and independent: byte 0 is the key length `k` (clamped to
//! what remains); bytes `1..1+k` are the key; the rest is the object
//! span. Dependency-free (no `arbitrary`), matching the manual-split
//! style of `fuzz_abi_decode`.
//!
//! ## Run
//!
//! ```bash
//! cargo +nightly fuzz run fuzz_mcp_obj_get -- -max_total_time=90
//! ```
//!
//! Stable-CI mirror: exercised transitively by `wari-mcp`'s corpus and
//! `depth_bomb_is_refused_not_a_stack_overflow` unit tests (cargo-fuzz
//! needs nightly — see `README.md`).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&klen, rest)) = data.split_first() else {
        return;
    };
    let klen = core::cmp::min(klen as usize, rest.len());
    let (key, obj) = rest.split_at(klen);
    let _ = wari_mcp::obj_get_raw(obj, key);
});
