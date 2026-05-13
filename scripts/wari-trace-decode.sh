#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# wari-trace-decode — pretty-print a COM7 paste with diagnostic tags.
#
# Usage:
#   pbpaste                   | scripts/wari-trace-decode.sh
#   cat com7-capture.txt      | scripts/wari-trace-decode.sh
#   scripts/wari-trace-decode.sh < capture.txt
#
# Decodes the `[net:drv] tag=0xXXXX val=0xYYYY` lines documented in
# docs/diagnostic-tags.md. Lines that aren't recognized passthrough
# unchanged so you can pipe whole traces.

decode_tag() {
    case "$1" in
        # Event tags (drivers/net/src/lib.rs)
        0x72584672) printf "rXFr  frame in ring        " ;;
        0x72584365) printf "rXCe  consume entered      " ;;
        0x72584472) printf "rXDr  drop fired           " ;;
        0x7258434e) printf "rXCn  descriptor rearmed   " ;;
        0x7258546c) printf "rXTl  tail doorbell        " ;;
        0x64507262) printf "dPrb  receive probe        " ;;
        0x74585472) printf "tXTx  TX frame sent        " ;;

        # Counter stats (build 118+)
        0x53745263) printf "StRc  STAT receive_calls   " ;;
        0x53745266) printf "StRf  STAT frames_found    " ;;
        0x53744363) printf "StCc  STAT consume_calls   " ;;
        0x53744463) printf "StDc  STAT drop_calls      " ;;
        0x53745261) printf "StRa  STAT rearm_calls     " ;;
        0x53745478) printf "StTx  STAT tx_sent         " ;;

        *) return 1 ;;
    esac
    return 0
}

# Lowercase tag for matching; preserve original val display.
while IFS= read -r line; do
    if [[ "$line" =~ \[net:drv\][[:space:]]+tag=(0x[0-9a-fA-F]{8})[[:space:]]+val=(0x[0-9a-fA-F]+) ]]; then
        tag="${BASH_REMATCH[1],,}"   # lowercase
        val="${BASH_REMATCH[2]}"
        if decoded=$(decode_tag "$tag"); then
            echo "[$decoded] val=$val"
        else
            echo "$line"
        fi
    else
        echo "$line"
    fi
done
