#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# dump-gmac1-regs.sh — golden-reference register dump for the GMAC1
# RX-path debug (Phase-1c, builds 124-134).
#
# WHY THIS EXISTS
# ---------------
# Wari's GMAC1 driver shows: link up 1G/FD, MAC_CONFIG/FILTER correct,
# PHY at MDIO addr 1 responding, SYS SYSCON phy_intf_sel = RGMII —
# and yet zero frames ever reach the MAC counters, MTL, or DMA ring.
# Linux (5.15.0-starfive) on the SAME board, SAME cable, SAME port
# receives every frame.
#
# Instead of guessing one register per build, run this script on
# Debian while end1 is up and passing traffic. It dumps every
# register cluster the RX path depends on, in `addr: value` format.
# Diff the output against Wari's boot-trace dump (docs/diagnostic-
# tags.md maps Wari's tags to these addresses). The first register
# where working-Linux differs from broken-Wari is the bug.
#
# It also dumps both candidate MMC counter blocks (0x700 and 0x900
# areas) plus `ethtool -S`, so the long-standing "which offset is
# MMC_RX_FRAMECOUNT_GB on this IP?" dispute gets settled by value-
# matching a live counter.
#
# USAGE (on VF2 Debian, as root, with end1 up and pinging something):
#   bash scripts/dump-gmac1-regs.sh | tee /tmp/gmac1-golden.txt
#
# Generate some RX traffic while it runs (e.g. ping the VF2 from the
# laptop) so the frame counters are visibly non-zero and climbing:
# run it twice a few seconds apart and compare counter deltas.

set -u

GMAC1=0x16040000
SYSCRG=0x13020000
SYSSYSCON=0x13030000

# ── one 32-bit MMIO read ─────────────────────────────────────────
# Prefer devmem/busybox devmem; fall back to dd+od on /dev/mem.
have_devmem=""
if command -v devmem >/dev/null 2>&1; then
    have_devmem="devmem"
elif command -v busybox >/dev/null 2>&1 && busybox devmem 0x16040110 >/dev/null 2>&1; then
    have_devmem="busybox devmem"
fi

rd32() { # rd32 <addr-hex>
    local addr=$1
    if [ -n "$have_devmem" ]; then
        $have_devmem "$addr" 2>/dev/null
    else
        # dd: bs=4, skip counted in 4-byte blocks. od -tx4 prints the
        # word respecting host endianness (RISC-V LE -> correct value).
        local blocks=$(( addr / 4 ))
        dd if=/dev/mem bs=4 skip=$blocks count=1 2>/dev/null \
          | od -An -tx4 | tr -d ' \n' | sed 's/^/0x/'
        echo
    fi
}

dump_range() { # dump_range <label> <base-hex> <first-off> <last-off>
    local label=$1 base=$2 first=$3 last=$4
    echo "== $label =="
    local off=$first
    while [ $off -le $last ]; do
        local addr=$(( base + off ))
        printf "0x%08x: %s\n" "$addr" "$(rd32 $addr)"
        off=$(( off + 4 ))
    done
}

dump_one() { # dump_one <base> <off> <name>
    local addr=$(( $1 + $2 ))
    printf "0x%08x: %-10s  # %s\n" "$addr" "$(rd32 $addr)" "$3"
}

echo "### GMAC1 golden-reference dump — $(uname -r) — $(date -u +%FT%TZ)"
echo "### Interface state:"
ip -s link show end1 2>/dev/null | sed 's/^/    /'
echo

echo "== MAC core (0x16040000) =="
dump_one $GMAC1 0x000  "MAC_CONFIGURATION"
dump_one $GMAC1 0x004  "MAC_EXT_CONFIGURATION"
dump_one $GMAC1 0x008  "MAC_PACKET_FILTER"
dump_one $GMAC1 0x090  "MAC_RX_FLOW_CTRL"
dump_one $GMAC1 0x0A0  "MAC_RXQ_CTRL0        <-- the build-133/134 question"
dump_one $GMAC1 0x0A4  "MAC_RXQ_CTRL1"
dump_one $GMAC1 0x0A8  "MAC_RXQ_CTRL2"
dump_one $GMAC1 0x0B0  "MAC_INTERRUPT_STATUS"
dump_one $GMAC1 0x0F8  "MAC_PHYIF_CONTROL_STATUS"
dump_one $GMAC1 0x110  "MAC_VERSION"
dump_one $GMAC1 0x114  "MAC_DEBUG"
dump_one $GMAC1 0x300  "MAC_ADDRESS0_HIGH"
dump_one $GMAC1 0x304  "MAC_ADDRESS0_LOW"
echo

# Both candidate MMC blocks — settles the 0x700-vs-0x900 offset
# dispute by value-matching against ethtool -S below.
dump_range "MMC block A (0x700-0x7FC)" $GMAC1 0x700 0x7FC
echo
dump_range "MMC block B (0x900-0x97C)" $GMAC1 0x900 0x97C
echo

echo "== MTL RXQ0 =="
dump_one $GMAC1 0xC00  "MTL_OPERATION_MODE"
dump_one $GMAC1 0xC30  "MTL_RXQ_DMA_MAP0     <-- queue->DMA-channel routing"
dump_one $GMAC1 0xD00  "MTL_TXQ0_OPERATION_MODE"
dump_one $GMAC1 0xD30  "MTL_RXQ0_OPERATION_MODE"
dump_one $GMAC1 0xD34  "MTL_RXQ0_MISSED_PKT_OVF_CNT"
dump_one $GMAC1 0xD38  "MTL_RXQ0_DEBUG"
dump_one $GMAC1 0xD3C  "MTL_RXQ0_CONTROL"
echo

echo "== DMA (0x1000/0x1100) =="
dump_one $GMAC1 0x1000 "DMA_MODE"
dump_one $GMAC1 0x1004 "DMA_SYSBUS_MODE      <-- AXI burst/coherency setup"
dump_one $GMAC1 0x1100 "DMA_CH0_CONTROL"
dump_one $GMAC1 0x1104 "DMA_CH0_TX_CONTROL"
dump_one $GMAC1 0x1108 "DMA_CH0_RX_CONTROL"
dump_one $GMAC1 0x1114 "DMA_CH0_RXDESC_LIST_HI"
dump_one $GMAC1 0x111C "DMA_CH0_RXDESC_LIST_LO"
dump_one $GMAC1 0x1128 "DMA_CH0_RXDESC_TAIL"
dump_one $GMAC1 0x1130 "DMA_CH0_RXDESC_RING_LEN"
dump_one $GMAC1 0x1134 "DMA_CH0_INTERRUPT_ENABLE"
dump_one $GMAC1 0x114C "DMA_CH0_CUR_RXDESC"
dump_one $GMAC1 0x1154 "DMA_CH0_CUR_RXBUF"
dump_one $GMAC1 0x1160 "DMA_CH0_STATUS"
echo

echo "== SYSCRG GMAC1 cluster (0x13020000) =="
dump_one $SYSCRG 0x184 "gmac1_ahb gate"
dump_one $SYSCRG 0x188 "gmac1_axi gate"
dump_one $SYSCRG 0x18C "gmac_src (shared root div)"
dump_one $SYSCRG 0x190 "gmac1_gtxclk div"
dump_one $SYSCRG 0x194 "gmac1_rmii_rtx (if present)"
dump_one $SYSCRG 0x198 "gmac1_ptp"
dump_one $SYSCRG 0x19C "gmac1_rx MUX          <-- Wari reads 0 here; Linux value is decisive"
dump_one $SYSCRG 0x1A0 "gmac1_rx_inv"
dump_one $SYSCRG 0x1A4 "gmac1_tx GMUX"
dump_one $SYSCRG 0x1A8 "gmac1_tx_inv (if present)"
dump_one $SYSCRG 0x1AC "gmac1_gtxc gate"
dump_one $SYSCRG 0x1B8 "gmac_phy MDC (shared)"
dump_one $SYSCRG 0x300 "SYS reset assert word 2 (gmac1 axi/ahb = bits 2,3)"
dump_one $SYSCRG 0x310 "SYS reset status word 2"
echo

echo "== SYS SYSCON =="
dump_one $SYSSYSCON 0x8C "syscon +0x8C (neighbor)"
dump_one $SYSSYSCON 0x90 "phy_intf_sel GMAC1 (bits 4:2, 0b001 = RGMII)"
dump_one $SYSSYSCON 0x94 "syscon +0x94 (neighbor)"
echo

echo "== ethtool -S end1 (Linux's own view of the counters) =="
ethtool -S end1 2>/dev/null | sed 's/^/    /' || echo "    (ethtool unavailable)"
echo
echo "### done. Run twice ~5s apart during active ping to see which"
echo "### MMC addresses are climbing — those are the real RX counters."
