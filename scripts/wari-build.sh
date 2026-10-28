#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# wari-build.sh — Mac-side deploy build (the "push side").
#
# Mirrors a branch to GitHub, builds the VF2 kernel for it, and
# publishes wari.bin + signed driver blobs as the GitHub Release
# build-<N>-<branch>. N is globally unique (build.sh --publish), so
# the board needs nothing but the number.
#
#   Usage:  bash scripts/wari-build.sh [branch] [profile]
#           branch  default: main
#           profile default: release (or debug / trace)
#
#   Then on the VF2 (its Debian console — the "pull side"):
#       wari flash <N> -y
#
# Why GitHub: the VF2 downloads releases from the public GitHub repo,
# while day-to-day pushes go to Gitea (origin). The mirror push keeps
# the source of every published binary reachable next to it.
set -euo pipefail

BRANCH="${1:-main}"
PROFILE="${2:-release}"

# Homebrew's cargo lacks the riscv64gc / wasm32 targets; the rustup one
# has them. Without this, build.sh dies on "target may not be installed".
export PATH="$HOME/.cargo/bin:$PATH"

cd "$(git rev-parse --show-toplevel)"

# This script switches branches — refuse to run over uncommitted work.
if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "ERROR: uncommitted changes in the working tree." >&2
    echo "       Commit or stash them first (this script runs 'git checkout')." >&2
    git status --short >&2
    exit 1
fi

echo "=== wari-build: branch '$BRANCH' profile '$PROFILE' ==="
git fetch origin "$BRANCH"
git checkout "$BRANCH"
git merge --ff-only "origin/$BRANCH"

# The release tag is pinned to this commit, so GitHub must have it
# before the publish. A non-fast-forward here means the mirror
# diverged from Gitea: stop and look, never force it.
git push github "$BRANCH"

# build.sh is not chmod +x, so it is invoked through bash on purpose.
bash scripts/build.sh "$PROFILE" --publish
N="$(cat .build_number)"

# The release is the record of the build. .build_number and
# build/wari.release only feed the legacy `wari go` / `go-branch`
# flow; committing them is what used to force pointer commits onto
# main, so they are put back instead.
git checkout -- .build_number build/wari.release

echo ""
echo "=== done: build $N published — on the VF2 run: ==="
echo "        wari flash $N -y"
