// SPDX-License-Identifier: AGPL-3.0-only
//! Fuzz `wari_http::parse` against arbitrary byte streams.
//!
//! ## Property
//!
//! Every input either parses (`Parse::Complete` / `Parse::Partial`) or
//! returns a typed `HttpError`. **Zero panics.** `parse` runs inside
//! the Tier-2 MCP server on wire bytes an unauthenticated peer fully
//! controls, and its doc contract states `# Panics: never`. A panic
//! here is a denial-of-service primitive on the transport, so we hold
//! the line at fuzz time — libFuzzer detects the panic and aborts the
//! worker; a typed `Ok`/`Err` is the success path.
//!
//! `parse` borrows the input and retains nothing, so one call is the
//! whole test.
//!
//! ## Run
//!
//! ```bash
//! cargo +nightly fuzz run fuzz_http_parse -- -max_total_time=90
//! ```
//!
//! Stable-CI mirror: `wari-http`'s `corpus_smoke_parse_never_panics`
//! unit test (cargo-fuzz needs nightly — see `README.md`).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = wari_http::parse(data);
});
