#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# verify-build.sh — sanity-check build/wari.bin against the
# expected platform's ELF entry point. Called by deploy.bat after
# the WSL build step.
#
# Usage:
#   verify-build.sh <expected-entry-hex>
#   e.g. verify-build.sh 0x40200000
#
# Exits 0 on match, 1 on any mismatch / missing artifact.

set -e

EXPECTED="${1:?expected entry point hex required, e.g. 0x40200000}"

if [ ! -s build/wari.bin ]; then
    echo "verify-build: build/wari.bin missing or empty"
    exit 1
fi

BUILD=$(cat .build_number 2>/dev/null || echo "?")
TAG=$(strings build/wari.bin | grep -m1 'WARI-BUILD-TAG-' || echo "(none)")
ENTRY=$(python3 -c '
import struct, sys
with open("target/riscv64gc-unknown-none-elf/release/wari", "rb") as f:
    d = f.read(0x100)
print(hex(struct.unpack("<Q", d[0x18:0x20])[0]))
')

echo "  .build_number:          $BUILD"
echo "  embedded tag:           $TAG"
echo "  kernel ELF entry:       $ENTRY"
echo "  expected entry:         $EXPECTED"

if [ "$ENTRY" != "$EXPECTED" ]; then
    echo ""
    echo "VERIFY FAILED: entry point $ENTRY does not match expected $EXPECTED"
    echo "               wrong make rule produced this binary"
    exit 1
fi
