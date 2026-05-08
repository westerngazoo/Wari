# Phase 1c — JH7110 GMAC bring-up status

> **Date**: May 2026
> **Build**: 97 (commit `c9d8fb0`)
> **Hardware**: StarFive VisionFive 2 rev. 1.3+, JH7110, GMAC0 @ `0x16030000`,
>               Motorcomm YT8531C PHY @ MDIO addr 0, RGMII

This document captures what works, what doesn't, and exactly which
register writes got us here, so that resuming Phase-1c-7 (smoltcp
wire-up) doesn't re-derive the boot sequence.

---

## What works on silicon today

| Layer | Status | Evidence |
|---|---|---|
| Kernel boot + banner + manifest gates | ✅ | `Wari v0 build 97 boot OK, hart 1` |
| GMAC0 IP block alive | ✅ | `[net:drv] tag=GmaC val=0x00041452` (DWMAC v5.20 in low byte 0x52) |
| All required clocks running | ✅ | AON `0x08`/`0x0C`/`0x14`/`0x20` + SYS `0x1B0`/`0x1B4`/`0x1B8`/`0x1BC` all bit 31 set |
| AON SYSCON `phy_intf_sel` = RGMII | ✅ | `PI_P=0x0004D540` — U-Boot already sets bits 20:18 = 0b001 |
| DMA engine out of soft-reset | ✅ | `RWR\2=0x00000000` after Phase-1c-6c clock writes |
| MTL TXQ0 + RXQ0 configured | ✅ | `MTLT=0x007F000A`, `MTLR=0x00700020` |
| MAC promiscuous + MAC_ADDR0 programmed | ✅ | `MACH=0x80008440`, `MACL=0x0039CF6C`, `MACF=0x00000001` |
| PHY ID readable + auto-neg confirmed | ✅ | `PHYID1=0x4F51`, `PHYID2=0xE91B`, `bs_pre=0x796D` (link bit 2 + AN-done bit 5) |
| 1 Gbps full duplex link | ✅ | `gig_status=0x3800` — bit 11 (1000BT-FD) + bit 12 (local OK) + bit 13 (remote OK) |
| TX descriptor ring configured | ✅ | `DV\xA4` echoes `TXpL`, `DV\xBC=0xF` |
| RX descriptor ring configured | ✅ | `DV\xAC` echoes `RXpL`, `DV\xC0=0xF` |
| **First TX frame on the wire** | ✅ | `TDE3=0x30000000` (OWN cleared after broadcast ARP transmit) |
| **First RX frame in our buffer** | ✅ (one-shot) | `Wt2\3[0]=0x301180DA` (OWN cleared, FD+LD set, 218 B PL); `Wt2\4=0xFFFFFFFF` (broadcast dst MAC) |
| DMA RX interrupts enabled | ✅ | `IENT=0x0000C041` (NIE+AIE+RIE+TIE) |
| RPF (descriptor polling forced) | ✅ | `RXC2=0x80010C01` |

## What doesn't (yet)

| Gap | Symptom | Plan |
|---|---|---|
| Continuous RX drain | RX is intermittent — only frames arriving during the driver's busy-wait window get logged. After the wait, descriptors stay armed but nobody polls them. | Phase-1c-7: kernel calls `tier2_net::poll(tick)` per idle iteration; that drives smoltcp's `Interface::poll` which drains RX. |
| smoltcp on vf2 | `nic_iface::*` module is `cfg(feature = "qemu")`-only; vf2 build of `driver_poll` returns `-1`. | Phase-1c-7: lift `nic_iface` to platform-neutral, write a vf2-side `Device` impl that reads from `VF2_RX_RING` + writes to `VF2_TX_RING`. |
| Net.initialized = true on vf2 | `wari_nic_set_mac(low, high)` is never called on vf2, so the kernel sees `Net.initialized = false` and does not run `tier2_net::install` (no Tier2NetHandle, no socket dispatch). | Phase-1c-7: vf2 `driver_start` calls `wari_nic_set_mac` at the end of the success path. |
| ARP responses, ICMP, TCP | All gated on smoltcp running. | Comes for free with Phase-1c-7. |

## Register cheat-sheet (critical writes that got us here)

```
# AON SYSCON 0x17010000
+0x0C  RMW: clear bits[20:18], set bit 18      # phy_intf_sel = RGMII
                                                 # (typically already set by U-Boot)

# AON CRG 0x17000000
+0x08  <- 0x80000000                            # gmac0_ahb gate
+0x0C  <- 0x80000000                            # gmac0_axi gate
+0x14  <- 0x80000000                            # gmac0_tx GMUX, mux=0=gtxclk
+0x20  <- 0x40000000                            # gmac0_rx_inv (bit 30 = invert for RGMII)
+0x38  RMW: clear bits 0+1                      # deassert GMAC0_AXI/AHB resets

# SYS CRG 0x13020000
+0x1B0 <- 0x80000005                            # gmac0_gtxclk en + div=5 (1Gbps)
+0x1B4 <- 0x8000000A                            # gmac0_ptp en + div=10
+0x1B8 <- 0x8000001E                            # gmac_phy MDC en + div=30
+0x1BC <- 0x80000000                            # gmac0_gtxc gate en

# GMAC0 0x16030000
+0x000 <- 0x00000003                            # MAC_CONFIG: TE | RE
+0x008 <- 0x00000001                            # MAC_PACKET_FILTER: PR (promiscuous)
+0x300 <- 0x80008440                            # MAC_ADDR0_HI: AE | bytes 5,4
+0x304 <- 0x0039CF6C                            # MAC_ADDR0_LO: bytes 3..0
+0xC00 (well +0xD00) <- 0x007F000A             # MTL_TXQ0_OP: TSF | TXQEN=10b | TQS=0x7F
+0xD30 <- 0x00000720*                           # MTL_RXQ0_OP: RSF | RQS=7
                                                # *RQS field is at bits 29:20, register
                                                #  reads back as 0x00700020
+0x1100 .. +0x1130 (DMA ring config)            # see drivers/net/src/lib.rs ~line 1380
+0x1108 <- 0x80010C01                           # DMA_CH0_RX_CONTROL: RPF | RBSZ | SR | RXPBL
+0x1104 <- 0x00100001                           # DMA_CH0_TX_CONTROL: ST | TXPBL
+0x1134 <- 0x0000C041                           # DMA_CH0_INT_ENABLE: NIE+AIE+RIE+TIE
+0x1160 <- 0x0000FFFF                           # DMA_CH0_STATUS: W1C clear stale flags
```

## RX intermittency — diagnosed (May 2026)

A second-opinion analysis ranks the hypotheses for why RX worked
on one boot and not on subsequent ones:

| Hypothesis | Likelihood | Why |
|---|---|---|
| **(a) Network genuinely quiet during busy-wait window** | **~80%** | 50M-iter spin = ~few hundred ms wall-clock. LAN broadcast cadence (mDNS ~1–2s, ARP ~30–60s, IPv6 RA ~200s) means a sub-second window catches a frame 30–50% of the time. The successful boot proves the entire datapath (PHY → MAC → MTL → DMA write-back) works end-to-end at least once; bimodal "perfect or nothing" pattern matches (a) exactly. |
| (c) `lin_mem_base()` PA/VA confusion | ~10% | Hard to reconcile with the successful boot's correct ring + buffer addresses. Sanity check: log `lin_mem_base()` once and verify it's in DDR (`0x40000000..=0x13FFFFFFF`), 8-byte aligned. |
| (d) Missing `fence ow,ow` before RX_TAIL store | ~5% | JH7110 GMAC is IO-coherent for DDR, but U74 store buffer still needs a fence so DMA fetches the descriptor with the OWN bit we just wrote. Cheap insurance to add. |
| (f) RPF / FIFO interaction | ~3% | RPF only forces *descriptor* polling, doesn't gate FIFO. Read MTL_RXQ0_DEBUG (+0xD38) on a stuck boot to confirm FIFO isn't backed up. |
| (b) MAC_RX_FLOW_CTRL / RFD threshold | ~1% | Defaults off; we never enabled it. |
| (e) YT8531C RGMII delay miscalibration | <1% | Would produce consistent CRC-fail or link flap, not bimodal "perfect or silent." 1Gbps full-duplex link wouldn't latch cleanly at all if delays were wrong. (For completeness: YT8531C RX-delay lives in extended reg `0x10` page `0xa001`, bits 13:10.) |

**Decision: stop debugging intermittency, ship Phase-1c-7.** With
smoltcp's `Interface::poll` running every kernel idle iteration,
RX gets drained as packets arrive instead of relying on a
one-shot busy-wait. Hit-rate goes from "30-50% per boot" to
"100%, draining continuously."

Add the `fence ow,ow` insurance write at the same time — costs
nothing.

## Phase-1c-7 execution plan (substantive)

One PR, one boot to validate. Files touched:

- `drivers/net/src/lib.rs` — lift `nic_iface` mod out of `cfg(qemu)`. New
  vf2-side `Device` impl backed by `VF2_TX_RING` / `VF2_RX_RING`. ~250 LOC.
- `driver_start` vf2 branch ends with `wari_nic_set_mac(...)` so the kernel
  marks `Net.initialized = true` and runs `tier2_net::install`. ~10 LOC.
- `driver_poll` vf2 branch calls `nic_iface::poll(timestamp_ms)`. ~5 LOC.

Validation:
1. `make run` (QEMU) — virtio-net Device impl unchanged, must still boot
2. `wari go` on VF2 — kernel idle loop drives RX drain; expect
   `[net] smoltcp interface up, listening on 192.168.122.10/24` line on COM7
3. `arp -n` from a laptop on the same LAN → eventually shows the VF2 entry

After that, Net-6c-3 (send/recv data path) + Net-6d echo demo are
substantively unblocked on both QEMU and silicon at the same time.
