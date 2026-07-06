<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Wari — Team Parallel Tasks (while IPC + network are in flight)

> **Reserved lanes (do not touch — active owners):**
> - `kernel/src/sched/**`, IPC/scheduler/context-switch → **Option B TCB
>   scheduler** (in flight).
> - `drivers/net/**`, GMAC1 RGMII bring-up → **network** (the board).
> - `scripts/build.sh`, Makefile build entrypoint → **build refactor**.
>
> Everything below is **parallel-safe** against those. `[P]` = start now,
> no dependency. `[coord]` = ping the lane owner first. `[you]` = architect
> decision. Each task names its files + an acceptance check so it can be
> handed off cold.

---

## AOT lane (native-speed WASM)

- **T1 · WNM tooling** `[P]` — `tools/wnm-dump`: a host CLI that reads a
  `.wnm`, prints header + section table (uses `wari_wnm::load_plan`), and
  a golden-corpus test (hand-built WNMs). *Files:* `tools/wnm-dump/`,
  `wari-wnm` tests. *Accept:* `wnm-dump valid.wnm` prints the sections;
  `cargo test -p wari-wnm` covers a corrupt corpus.
- **T2 · M0 benchmark/oracle** `[P]` — the measure-first harness: time
  representative WASM under `wasmi`, `wasmi` as the differential
  correctness reference. *Files:* `tools/wari-bench/`, reuse
  `tools/qemu-runner`. *Accept:* prints per-workload interp time + a
  pass/fail differential vs a reference trace.
- **T3 · Target ABI for compiled code** `[you]` `[coord]` — spec how AOT
  code addresses linear memory + calls host fns + maps traps (needs the
  backend decision). *Files:* `docs/aot-target-abi.md`.

## Agentic / WASI-NN lane

- **T4 · Accelerator driver skeleton** `[P]` — a Tier-2 `drivers/nn`
  WASM stub that answers the `wari_abi::nn` ops (load/compute/get_output)
  with canned tensors, so the surface is exercisable before real GPU.
  *Files:* `drivers/nn/`. *Accept:* builds to wasm32, signs, loads under
  QEMU; `nn_compute` returns a canned output.
- **T5 · Policy action-table + taint labels** `[P]` — extend `wari-policy`
  with the concrete action-id → `Consequence` table and a taint-label
  type + propagation helper (pure). *Files:* `wari-policy/src/`.
  *Accept:* host tests for the table + label lattice; still `no_std`.
- **T6 · Bounded-attenuation cap primitive (design)** `[you]` — spec
  count/time/target-boxed `mint` (the Supervisor primitive, B4).
  *Files:* `docs/cap-attenuation.md`.

## Testing & hardening

- **T7 · Finish the stale-marker fix** `[P]` — the WIP on
  `phase-1c/net-6e-gtxclk-divider` reconciles `HELLO_EXIT_0` →
  `TENANT_EXIT_0`; land it as its own PR off `main` and green the security
  suite on QEMU. *Files:* `tests/security/**`. *Accept:*
  `cargo test --manifest-path tests/security/Cargo.toml` passes.
- **T8 · Bespoke adversarial blobs** `[P]` — the deferred full adversarial
  tests: a Tier-1 WASM that drives `cap_register` (forged / stale-gen /
  table-full) and the ring (`ring_submit` TOCTOU / OOB), asserting the
  errnos on the serial log. *Files:* `tests/security/**`, a small
  `apps/`-style blob. *Accept:* each attack fails safely, no panic.
- **T9 · Fuzz targets** `[P]` — `cargo-fuzz` over the pure decoders:
  `wari_abi::{reg::validate_handle, ring::decode_sqe, nn}`,
  `wari_wnm::validate_header`, `wari_policy::evaluate`. *Files:*
  `tests/fuzz/`. *Accept:* targets build + run clean for a smoke duration.

## Tooling & docs

- **T10 · `make doctor` + `make ci`** `[coord]` — toolchain/target/QEMU
  sanity + one-command green check, folded into `scripts/build.sh`
  (coordinate with the build owner). *Accept:* `make doctor` reports the
  cargo-resolution + targets; `make ci` runs fmt+clippy+test+QEMU smoke.
- **T11 · Invariants catalog** `[P]` — add `docs/invariants.md` entries
  for the cap-fastpath INV-α/β/γ + the policy/ring properties now that the
  code has landed. *Files:* `docs/invariants.md`. *Accept:* every new
  `unsafe`-adjacent property has a numbered INV.
- **T12 · Kani proof harnesses** `[P]` — `#[cfg(kani)]` proofs for the
  pure predicates: `validate_handle`, `policy::evaluate`,
  `wnm::validate_header` (small, total functions — provable). *Files:*
  `wari-abi`, `wari-policy`, `wari-wnm`. *Accept:* `cargo kani` proves the
  harnesses (or documents the property).

## Network lane (owner-adjacent — for a second pair of hands)

- **T13 · smoltcp TCP socket layer** `[coord]` — Net-6c: the TCP
  bind/listen/accept path for the HTTP demo, on top of the GMAC1 RX once
  it's up. *Files:* `drivers/net/**` (coordinate with the board owner).

---

## Suggested first picks by skill

- **Rust/pure-logic dev:** T5, T9, T12 — self-contained, host-testable.
- **Systems/WASM dev:** T4, T8 — Tier-2/Tier-1 blobs under QEMU.
- **Tooling dev:** T1, T2, T10 — CLIs + harnesses.
- **Architect:** T3, T6 — the decisions that unblock AOT + attenuation.

None of these touch `sched/`, `drivers/net/`, or `scripts/build.sh` —
so they run cleanly alongside the IPC scheduler, the network fix, and the
build refactor already in flight.
