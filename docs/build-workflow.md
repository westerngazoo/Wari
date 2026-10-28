# Build workflow — `scripts/build.sh`

> **One entrypoint. Full closure. Self-verifying.**
> `bash scripts/build.sh <profile>` is the only supported way to
> produce Wari artifacts. The Makefile targets remain for legacy
> Linux flows but the script is canonical — it runs identically on
> Git Bash (Windows) and Linux, which the Makefile does not.

## Why it exists

Three desync incidents, one root cause — the pipeline had separable
steps and a human sequenced them:

| Builds | What went wrong |
|---|---|
| 107–114 | Driver wasm build silently failed (inline asm on wasm32); cargo reused the stale build-106 artifact while the kernel banner said 114. A week of PHY debugging ran against dead code. |
| 122–124 | Parallel-dev deploys bumped the kernel while the signed driver wasm stayed at 121. Kernel resolved `socket_accept` exports the embedded driver didn't have. |
| 130–134 | Hand-run 8-step pipeline (no `make` on the Windows box); every invocation risked a skipped sign or mismatched `WARI_BUILD`. |

## Usage

```bash
scripts/build.sh <profile> [--programs a,b,c] [--no-bump] [--publish]
```

| Profile | Target | Driver features | Kernel features | Use for |
|---|---|---|---|---|
| `release` | VF2 | `vf2 gmac1` | `vf2` | Production silicon builds |
| `debug` | VF2 | `vf2 gmac1` | `vf2,debug-kernel` | Kernel-side `kdebug!` logging |
| `trace` | VF2 | `vf2 gmac1 net-diag` | `vf2` | RX-path register snapshots (current Phase-1c debugging) |
| `qemu` | QEMU virt | `qemu` | `qemu` | Local development, `make run` |

- `--programs hello,foo` — build + stage Tier-1 apps from `apps/<name>`
  (default `hello`). Convention: `apps/<name>` →
  `target/wasm32-unknown-unknown/release/wari_<name>.wasm` →
  `build/apps/<name>.wasm`.
- `--no-bump` — rebuild at the current `.build_number` instead of
  incrementing (for clean-rebuilds of identical source).
- `--publish` — create the GitHub Release `build-<N>-<branch-slug>`
  carrying `wari.bin` and both signed net-driver wasms (needs an
  authenticated `gh`). Without it only the release pointer is written.

## What one invocation always does

1. Host unit-test gate (the pure-logic crates, same as `make test-unit`)
2. Tier-1 programs (all `--programs`)
3. UART driver, **both** platforms, signed
4. Net driver, **both** platforms, with `WARI_BUILD` env, signed
5. Kernel (profile platform), same `WARI_BUILD`, objcopy → `build/wari.bin`
6. **Verify**: `WARI-BUILD-TAG` in the kernel binary and
   `WARI-DRV-BUILD-TAG` in both signed wasms must equal the new build
   number; UART blobs and program wasms must exist non-empty
7. Only then: `.build_number` advances; artifacts archived to
   `build/out/<branch-slug>/<profile>/` with a `build-info.txt`
   (number, profile, branch, sha, dirty-count, date, features);
   `build/wari.release` is rewritten to `build-<N>-<branch-slug>` and,
   with `--publish`, the release is created

There is no partial mode. A failure at any step aborts loudly and the
build number does not advance.

## Invariants

- **I1** — driver wasm and kernel are built with the same `WARI_BUILD`
  env inside one invocation. Cross-invocation drift is impossible.
- **I2** — both net-driver platform variants rebuild every time, so
  the four-way tag verify is always meaningful.
- **I3** — `.build_number` only advances after verify passes.
- **I4** — any step failing aborts the whole build (`set -euo
  pipefail` + ERR trap naming the failed step).

Defense in depth: `kernel/build.rs` independently greps the embedded
signed wasm's tag and refuses to compile on mismatch (the
stale-driver guard from build 116). The script's verify and the
compile-time guard cover each other.

## Outputs

```
build/wari.bin                      ← canonical kernel image (not committed; shipped as a release asset)
build/wari.release                  ← committed: one-line pointer to the release tag
build/drivers/*.signed.wasm         ← staging for kernel include_bytes! (not committed)
build/apps/*.wasm                   ← staging for kernel include_bytes! (not committed)
.build_number                       ← committed
build/out/<branch>/<profile>/       ← local archive, never committed
    wari.bin
    net-vf2.signed.wasm
    net-qemu.signed.wasm
    build-info.txt
```

The archive directory answers "which binary was this?" when juggling
branches: every build is filed under the branch and profile that
produced it, with its provenance in `build-info.txt`.

Binaries are not tracked by git: a binary on every branch made every
parallel branch conflict on an unmergeable file. The repo tracks
`build/wari.release` instead, and the binary travels as a release
asset.

## Deploying

```bash
scripts/build.sh release --publish    # or debug / trace
git add build/wari.release .build_number
git commit -m "Build N [<profile>]: <what changed>"
git push origin <branch>

# On the VF2 (Debian):
wari go                               # flashes main (repo must be on main)
wari go-branch <branch>               # flashes a testing branch
```

`wari go` pulls the branch, reads `build/wari.release`, downloads that
release's `wari.bin` when the local copy's embedded tag differs,
checks the embedded tag against `.build_number`, copies it to
`/boot/kernel.bin` with an md5 check and an atomic rename, then asks
before rebooting (`-y` skips the prompt). `wari upgrade` does the same
without rebooting. A missing release fails the download with
"Was this build published?".

The Makefile's `kernel-vf2`, `deploy` and `flash-sd` targets and
`deploy.bat` predate this flow (they commit and push the binary and
carry fixed lab addresses); don't use them for deployable builds.

## Build-number semantics

The number is monotonic **per branch lineage**, not globally unique —
parallel branches can both mint "build 135". Identity of any binary is
the triple (branch, sha, embedded tag), all recorded in
`build-info.txt` and recoverable from a flashed binary via
`strings kernel.bin | grep WARI-BUILD-TAG`.

## Rules

- Never run `cd kernel && cargo build` directly for deployable
  artifacts. That is exactly how builds 122–124 shipped a kernel with
  a stale embedded driver. The compile-time guard will usually catch
  you; don't rely on it.
- Never hand-edit `.build_number`.
- Adding a Tier-1 program: create `apps/<name>`, add it to
  `--programs`, and add the kernel-side `include_bytes!` blob module.
- Adding a profile: one line in the `case` table in `build.sh` —
  keep features declarative there, never inline in cargo calls
  elsewhere.
