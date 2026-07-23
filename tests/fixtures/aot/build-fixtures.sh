#!/usr/bin/env bash
set -e

cd "$(dirname "$0")"

for f in *.wat; do
    echo "Compiling $f to ${f%.wat}.wasm"
    wat2wasm "$f" -o "${f%.wat}.wasm"
done
