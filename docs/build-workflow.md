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
  authenticated `gh`, and the commit already pushed to GitHub: the tag
  is pinned to it). With `--publish`, N is **global** — one past the
  highest build published from any branch. Without it only the
  release pointer is written and N is the local count.

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
bash scripts/wari-build.sh <branch> [release|debug|trace]   # default: main release

# On the VF2 (Debian):
wari go --build <branch>.<N>          # -y skips the reboot prompt
wari upgrade --build <branch>.<N>     # flash only; reboot later with `wari reboot`
```

`wari-build.sh` fast-forwards the branch from Gitea, pushes it and `main` to
the GitHub mirror (fast-forward only; a divergence stops the script),
runs `build.sh <profile> --publish`, puts
`.build_number` and `build/wari.release` back (nothing to commit) and
prints the `wari go --build` line.

Nothing else mirrors Gitea to GitHub. A Gitea push-mirror is not used
because it pushes `--mirror`, which would prune the branches and the
`build-*` release tags that exist only on GitHub.

A build is named `<branch>.<N>` (e.g. `main.163`,
`phase-2/net-quiet-release-console.158`): it maps to the release tag
`build-<N>-<branch-slug>`, the slug being the branch with `/` turned
into `-`. N alone is globally unique; the branch is a cross-check
against a typo in N, and it disambiguates the numbers published before
the global numbering, which collide across branches.

`--build` needs no git on the board side: it finds that release tag on
GitHub (if the tag is missing it says which branch N belongs to),
downloads its `wari.bin`, checks it
against the sha256 in the release notes and checks that the embedded
`WARI-BUILD-TAG` is N, then copies it to `/boot/kernel.bin` with an
md5 check and an atomic rename; `go` then asks before rebooting.

`wari go` / `go-branch` (pull the branch, read `build/wari.release`,
download, flash) still work for branches whose pointer is committed,
but need that pointer commit on the branch; prefer `--build`.

The Makefile's `kernel-vf2`, `deploy` and `flash-sd` targets and
`deploy.bat` predate this flow (they commit and push the binary and
carry fixed lab addresses); don't use them for deployable builds.

## Build-number semantics

A **published** build's number is globally unique: `build.sh
--publish` takes one past the highest `build-<N>-*` release on GitHub,
whatever branch it came from. That is what lets the board flash by
number. Releases before this rule reused numbers per branch (three
branches each published a "build 155"); they stay addressable by
their full tag. Unpublished local builds keep counting from
`.build_number` and never reach the board.

The full identity of a binary is still the triple (branch, sha,
embedded tag): it is in the release name and notes, in
`build-info.txt`, and recoverable from a flashed binary via
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
