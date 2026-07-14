#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# build.sh — the one true Wari build pipeline.
#
# WHY THIS EXISTS
# ---------------
# Three desync incidents traced to the same root cause — the build had
# separable steps and a human sequenced them:
#   - builds 107..114: driver wasm build silently failed (inline asm on
#     wasm32); cargo reused the stale build-106 artifact while the kernel
#     banner said 114. A week of PHY debugging ran against dead code.
#   - builds 122..124: parallel-dev deploys bumped the kernel while the
#     signed driver wasm stayed at 121; kernel-side socket_accept
#     resolution referenced exports the embedded driver didn't have.
#   - builds 130..134: hand-run 8-step pipeline on a box without make;
#     every invocation risked a skipped sign or a mismatched WARI_BUILD.
#
# This script is the single entrypoint. It ALWAYS runs the host
# unit-test gate (pure-logic crates), then builds the full closure of
# everything the kernel include_bytes!'s (Tier-1 programs, UART
# driver both platforms, net driver both platforms, signatures),
# then the kernel, then verifies every embedded build tag matches —
# and only then advances .build_number. There is no partial mode.
#
# USAGE
# -----
#   scripts/build.sh <profile> [--programs a,b,c] [--no-bump]
#
# PROFILES
#   release   VF2 hardware, production features        (vf2 gmac1)
#   debug     release + kernel debug-kernel logging    (vf2 gmac1 | kernel +debug-kernel)
#   trace     release + net-diag register snapshots    (vf2 gmac1 net-diag)
#   qemu      QEMU virt, VirtIO-net                    (qemu)
#
# OPTIONS
#   --programs LIST  comma-separated Tier-1 apps to build+stage from
#                    apps/<name> (default: hello). Convention:
#                    apps/<name> -> target/wasm32-unknown-unknown/release/
#                    wari_<name>.wasm -> build/apps/<name>.wasm
#   --no-bump        rebuild at the CURRENT .build_number (no increment).
#                    For rebuilding identical sources after a clean.
#
# OUTPUTS
#   Canonical (what the kernel embeds / wari-upgrade flashes — committed):
#     build/wari.bin, build/drivers/*.signed.wasm, build/apps/*.wasm,
#     .build_number
#   Per-branch/profile archive (NOT committed; local disambiguation):
#     build/out/<branch-slug>/<profile>/
#         wari.bin, net-vf2.signed.wasm, net-qemu.signed.wasm,
#         build-info.txt   (number, profile, branch, sha, date, features)
#
# INVARIANTS ENFORCED
#   I1: driver wasm + kernel are built with the SAME WARI_BUILD env in
#       the same invocation — no cross-invocation drift possible.
#   I2: both net-driver platform variants are rebuilt every time, so
#       the four-way tag verify below is always meaningful.
#   I3: .build_number only advances after the verify stage passes.
#   I4: any step failing aborts the whole build loudly (set -e + trap).

set -euo pipefail

# ── locate repo root ────────────────────────────────────────────
cd "$(dirname "$0")/.."
REPO_ROOT="$(pwd)"

STEP="init"
trap 'echo ""; echo "!! BUILD FAILED at step: $STEP (build number NOT advanced)"; exit 1' ERR

# ── args ────────────────────────────────────────────────────────
PROFILE="${1:-}"
shift || true
PROGRAMS="hello"
BUMP=1
PUBLISH=0
while [ $# -gt 0 ]; do
    case "$1" in
        --programs) PROGRAMS="$2"; shift 2 ;;
        --no-bump)  BUMP=0; shift ;;
        --publish)  PUBLISH=1; shift ;;
        *) echo "unknown option: $1"; exit 2 ;;
    esac
done

case "$PROFILE" in
    release) DRV_FEATURES="vf2 gmac1";          KRN_FEATURES="vf2";              PLATFORM="vf2"  ;;
    debug)   DRV_FEATURES="vf2 gmac1";          KRN_FEATURES="vf2,debug-kernel"; PLATFORM="vf2"  ;;
    trace)   DRV_FEATURES="vf2 gmac1 net-diag"; KRN_FEATURES="vf2";              PLATFORM="vf2"  ;;
    qemu)    DRV_FEATURES="qemu";               KRN_FEATURES="qemu";             PLATFORM="qemu" ;;
    *) echo "usage: scripts/build.sh <release|debug|trace|qemu> [--programs a,b] [--no-bump]"; exit 2 ;;
esac

# ── build number (read now, written only after verify) ──────────
STEP="build-number"
CUR_BUILD="$(cat .build_number 2>/dev/null || echo 0)"
if [ "$BUMP" = "1" ]; then
    NEXT_BUILD=$(( CUR_BUILD + 1 ))
else
    NEXT_BUILD=$CUR_BUILD
fi

BRANCH="$(git branch --show-current 2>/dev/null || true)"
[ -n "$BRANCH" ] || BRANCH="detached-$(git rev-parse --short HEAD)"
BRANCH_SLUG="$(echo "$BRANCH" | tr '/' '-')"
SHA="$(git rev-parse --short HEAD)"
DIRTY="$(git status --porcelain 2>/dev/null | wc -l | tr -d ' ')"

echo "=== Wari build $NEXT_BUILD  profile=$PROFILE  platform=$PLATFORM"
echo "    branch=$BRANCH sha=$SHA dirty-files=$DIRTY programs=$PROGRAMS"
echo "    driver features: $DRV_FEATURES"
echo "    kernel features: $KRN_FEATURES"

# ── tools ───────────────────────────────────────────────────────
STEP="find-objcopy"
OBJCOPY="$(find "$HOME/.rustup" -name 'llvm-objcopy*' -type f 2>/dev/null | head -1)"
[ -n "$OBJCOPY" ] || { echo "llvm-objcopy not found (rustup component add llvm-tools)"; exit 1; }

# ── 1. host unit tests (pure-logic gate) ────────────────────────
STEP="host-unit-tests"
echo "--- [1/7] Host unit tests (pure-logic crates)"
# Same crate list as Makefile::HOST_CRATES — keep the two in sync.
# Explicit -p list, NOT --workspace: the kernel and the wasm32
# crates cannot build under the host test harness (E0152 + RISC-V
# asm — see docs/kernel-host-testing-design.md §2). Duplicated here
# rather than shelling out to make because this script exists
# precisely for boxes without make (see header).
cargo test --quiet \
    -p wari-abi -p wari-driver-iface -p wari-mem -p wari-wnm \
    -p wari-policy -p wari-ipc -p wari-wasi

# ── 2. Tier-1 programs ──────────────────────────────────────────
STEP="tier1-programs"
mkdir -p build/apps
IFS=',' read -ra PROG_ARR <<< "$PROGRAMS"
for p in "${PROG_ARR[@]}"; do
    echo "--- [2/7] Tier-1 program: $p"
    [ -d "apps/$p" ] || { echo "apps/$p does not exist"; exit 1; }
    ( cd "apps/$p" && cargo build --release )
    cp "target/wasm32-unknown-unknown/release/wari_${p}.wasm" "build/apps/${p}.wasm"
done

# ── 3. UART driver, both platforms + sign ───────────────────────
STEP="uart-driver"
mkdir -p build/drivers
echo "--- [3/7] UART driver (qemu + vf2) + sign"
( cd drivers/uart && cargo build --release --features qemu --no-default-features )
cp target/wasm32-unknown-unknown/release/wari_driver_uart.wasm build/drivers/uart-qemu.wasm
( cd drivers/uart && cargo build --release --features vf2 --no-default-features )
cp target/wasm32-unknown-unknown/release/wari_driver_uart.wasm build/drivers/uart-vf2.wasm
cargo run --quiet --manifest-path scripts/Cargo.toml --bin sign-module -- \
    build/drivers/uart-qemu.wasm build/drivers/uart-qemu.signed.wasm
cargo run --quiet --manifest-path scripts/Cargo.toml --bin sign-module -- \
    build/drivers/uart-vf2.wasm build/drivers/uart-vf2.signed.wasm

# ── 4. Net driver, both platforms + sign (I2) ───────────────────
STEP="net-driver"
echo "--- [4/7] Net driver vf2 [$DRV_FEATURES] + qemu + sign"
( cd drivers/net && WARI_BUILD=$NEXT_BUILD \
    cargo build --release --features "$DRV_FEATURES" --no-default-features \
    --target wasm32-unknown-unknown )
cp target/wasm32-unknown-unknown/release/wari_driver_net.wasm build/drivers/net-vf2.wasm
( cd drivers/net && WARI_BUILD=$NEXT_BUILD \
    cargo build --release --features qemu --no-default-features \
    --target wasm32-unknown-unknown )
cp target/wasm32-unknown-unknown/release/wari_driver_net.wasm build/drivers/net-qemu.wasm
# For the qemu profile the roles swap: the "vf2" staging slot must
# still exist for the four-way verify, but both were built above, so
# both staging files are fresh either way.
cargo run --quiet --manifest-path scripts/Cargo.toml --bin sign-module -- \
    build/drivers/net-vf2.wasm build/drivers/net-vf2.signed.wasm
cargo run --quiet --manifest-path scripts/Cargo.toml --bin sign-module -- \
    build/drivers/net-qemu.wasm build/drivers/net-qemu.signed.wasm

# ── 5. kernel ───────────────────────────────────────────────────
STEP="kernel"
echo "--- [5/7] Kernel [$KRN_FEATURES]"
( cd kernel && WARI_BUILD=$NEXT_BUILD \
    cargo build --release --features "$KRN_FEATURES" --no-default-features )
"$OBJCOPY" -O binary target/riscv64gc-unknown-none-elf/release/wari build/wari.bin

# ── 6. verify (I3 gate) ─────────────────────────────────────────
STEP="verify"
echo "--- [6/7] Verify"
KBIN="$(strings build/wari.bin | grep '^WARI-BUILD-TAG-' | head -1 | sed 's/WARI-BUILD-TAG-//')"
DVF2="$(strings build/drivers/net-vf2.signed.wasm | grep '^WARI-DRV-BUILD-TAG-' | head -1 | sed 's/WARI-DRV-BUILD-TAG-//')"
DQEM="$(strings build/drivers/net-qemu.signed.wasm | grep '^WARI-DRV-BUILD-TAG-' | head -1 | sed 's/WARI-DRV-BUILD-TAG-//')"
echo "    expected=$NEXT_BUILD  kernel=$KBIN  drv-vf2=$DVF2  drv-qemu=$DQEM"
if [ "$KBIN" != "$NEXT_BUILD" ] || [ "$DVF2" != "$NEXT_BUILD" ] || [ "$DQEM" != "$NEXT_BUILD" ]; then
    echo "!! TAG MISMATCH — artifacts incoherent, .build_number NOT advanced"
    exit 1
fi
for f in build/drivers/uart-vf2.signed.wasm build/drivers/uart-qemu.signed.wasm; do
    [ -s "$f" ] || { echo "!! $f missing or empty"; exit 1; }
done
for p in "${PROG_ARR[@]}"; do
    [ -s "build/apps/${p}.wasm" ] || { echo "!! build/apps/${p}.wasm missing or empty"; exit 1; }
done

# Only now advance the number (I3).
echo "$NEXT_BUILD" > .build_number

# ── 7. per-branch/profile archive ───────────────────────────────
STEP="archive"
OUT="build/out/$BRANCH_SLUG/$PROFILE"
mkdir -p "$OUT"
cp build/wari.bin "$OUT/wari.bin"
cp build/drivers/net-vf2.signed.wasm "$OUT/net-vf2.signed.wasm"
cp build/drivers/net-qemu.signed.wasm "$OUT/net-qemu.signed.wasm"
{
    echo "build:    $NEXT_BUILD"
    echo "profile:  $PROFILE"
    echo "platform: $PLATFORM"
    echo "branch:   $BRANCH"
    echo "sha:      $SHA"
    echo "dirty:    $DIRTY uncommitted files at build time"
    echo "date:     $(date -u +%FT%TZ)"
    echo "driver-features: $DRV_FEATURES"
    echo "kernel-features: $KRN_FEATURES"
    echo "programs: $PROGRAMS"
} > "$OUT/build-info.txt"

# ── 7b. release pointer + optional publish ─────────────────────
#
# build/wari.bin is NOT tracked by git anymore (binary artifacts in
# git made every parallel branch conflict on an unmergeable file).
# Instead the repo tracks build/wari.release — a one-line text
# pointer naming the GitHub Release tag that carries this build's
# binaries. The device-side `wari go` downloads the binary named by
# the pointer and verifies its embedded WARI-BUILD-TAG as before.
STEP="release-pointer"
REL_TAG="build-${NEXT_BUILD}-${BRANCH_SLUG}"
echo "$REL_TAG" > build/wari.release
if [ "$PUBLISH" = "1" ]; then
    STEP="publish"
    echo "--- [7b] Publishing release $REL_TAG"
    {
        echo "Build $NEXT_BUILD ($PROFILE) — branch $BRANCH @ $SHA"
        echo ""
        echo "sha256:"
        shasum -a 256 build/wari.bin build/drivers/net-vf2.signed.wasm build/drivers/net-qemu.signed.wasm 2>/dev/null \
          || sha256sum build/wari.bin build/drivers/net-vf2.signed.wasm build/drivers/net-qemu.signed.wasm
    } > /tmp/wari-release-notes.txt
    gh release create "$REL_TAG" \
        build/wari.bin \
        build/drivers/net-vf2.signed.wasm \
        build/drivers/net-qemu.signed.wasm \
        --title "Build $NEXT_BUILD ($PROFILE, $BRANCH_SLUG)" \
        --notes-file /tmp/wari-release-notes.txt
    echo "    published: $REL_TAG"
else
    echo "    release pointer: $REL_TAG  (NOT published — run with --publish"
    echo "    or: gh release create $REL_TAG build/wari.bin build/drivers/net-*.signed.wasm)"
fi

echo "--- [7/7] Done"
echo ""
echo "=== BUILD $NEXT_BUILD OK ($PROFILE) ==="
echo "    canonical: build/wari.bin (embedded tag WARI-BUILD-TAG-$NEXT_BUILD)"
echo "    archive:   $OUT/"
echo ""
echo "    To deploy: build with --publish, then git add build/wari.release .build_number && git commit && git push"
echo "    On VF2:    wari upgrade && wari go -y      (flashes main)"
echo "               wari go-branch $BRANCH          (flashes this branch)"
