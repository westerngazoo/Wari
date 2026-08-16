<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Wari — Fuzz Harness

`cargo-fuzz` targets for the attacker-facing pure crates. Each target
asserts one property: **the parser never panics on any input.** A
panic in one of these crates is not contained by the MMU — the MCP
server parses untrusted wire bytes in-process, so a host-Rust panic is
a denial-of-service primitive on the transport, not a sandboxed fault.

See `CLAUDE.md` §"Fuzz harness" and `../README.md` for where this layer
sits in the four-layer test strategy.

## Requires nightly

`cargo-fuzz` wraps `libFuzzer`, which needs the nightly-only
`-Z sanitizer=address` instrumentation. The workspace pins **stable**
`1.95.0` (`rust-toolchain.toml`, R8), so these targets **do not build
on the default toolchain** — that is expected, not a breakage. Install
the tooling once:

```bash
rustup toolchain install nightly
cargo install cargo-fuzz
```

Every `cargo fuzz` command below is written `cargo +nightly fuzz …` so
it selects nightly explicitly regardless of the pinned default.

## Targets

| Target                    | Crate       | Entry point                  | Stable-CI mirror (runs on pinned 1.95)                     |
|---------------------------|-------------|------------------------------|------------------------------------------------------------|
| `fuzz_wasm_validator`     | `wasmi`     | `Module::new`                | —                                                          |
| `fuzz_abi_decode`         | `wari-abi`  | `SyscallError` decode        | `wari-abi` unit tests                                      |
| `fuzz_http_parse`         | `wari-http` | `parse`                      | `wari-http::tests::corpus_smoke_parse_never_panics`        |
| `fuzz_mcp_parse_request`  | `wari-mcp`  | `parse_request`              | `wari-mcp::tests::corpus_smoke_parse_request_never_panics` |
| `fuzz_mcp_obj_get`        | `wari-mcp`  | `obj_get_raw` (split input)  | `wari-mcp::tests` corpus + `depth_bomb_*`                  |

The **stable-CI mirror** column matters: cargo-fuzz cannot run in the
per-PR gate (wrong toolchain, and fuzzing is too slow for CI anyway —
see `CLAUDE.md`, fuzz runs on milestones and weekly). So each parser
also carries a dependency-free "corpus smoke" unit test inside its own
`#[cfg(test)] mod tests` — a hardcoded array of nasty inputs
(truncations, depth bombs, oversized lengths, invalid UTF-8, smuggling
shapes, all-one-byte, empty) looped through the parser. That test is
the durable regression net on stable; the fuzz targets here are the
deep search when a nightly box is available.

## Run

From this directory (`tests/fuzz/`). Phase-2 smoke budget is ~90 s per
target; raise `-max_total_time` for milestone or pre-release runs.

```bash
cargo +nightly fuzz run fuzz_http_parse        -- -max_total_time=90
cargo +nightly fuzz run fuzz_mcp_parse_request -- -max_total_time=90
cargo +nightly fuzz run fuzz_mcp_obj_get       -- -max_total_time=90
```

Longer, milestone-grade runs (per `docs/testing.md`, 1 h clean):

```bash
cargo +nightly fuzz run fuzz_wasm_validator -- -max_total_time=3600
cargo +nightly fuzz run fuzz_abi_decode     -- -max_total_time=3600
```

List every registered target:

```bash
cargo +nightly fuzz list
```

## On a crash

`cargo-fuzz` writes the reproducing input to
`artifacts/<target>/crash-<hash>`. Reproduce and minimize it:

```bash
cargo +nightly fuzz run   fuzz_http_parse artifacts/fuzz_http_parse/crash-<hash>
cargo +nightly fuzz tmin  fuzz_http_parse artifacts/fuzz_http_parse/crash-<hash>
```

Then add the minimized bytes as a new entry in the crate's
`corpus_smoke_*` array so the regression is pinned on stable, and
**report the crashing input** — do not paper over a parser bug in the
fuzz target. `corpus/` and `artifacts/` are git-ignored.
