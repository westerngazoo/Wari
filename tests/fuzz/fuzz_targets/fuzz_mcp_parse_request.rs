// SPDX-License-Identifier: AGPL-3.0-only
//! Fuzz `wari_mcp::parse_request` against arbitrary byte streams.
//!
//! ## Property
//!
//! Every input either parses to a typed `RpcRequest` or returns a
//! typed `McpError`. **Zero panics.** `parse_request` decodes the
//! JSON-RPC envelope of an HTTP body an unauthenticated peer POSTs to
//! the MCP endpoint; its doc contract states `# Panics: never`, and
//! its internal skipper is depth-bounded by `MAX_DEPTH` so nesting is
//! a reviewable constant rather than an attacker-driven stack. A panic
//! here is a denial-of-service primitive on the AI-capability surface,
//! so we hold the line at fuzz time — libFuzzer aborts the worker on a
//! panic; a typed `Ok`/`Err` is the success path.
//!
//! ## Run
//!
//! ```bash
//! cargo +nightly fuzz run fuzz_mcp_parse_request -- -max_total_time=90
//! ```
//!
//! Stable-CI mirror: `wari-mcp`'s `corpus_smoke_parse_request_never_panics`
//! unit test (cargo-fuzz needs nightly — see `README.md`).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = wari_mcp::parse_request(data);
});
