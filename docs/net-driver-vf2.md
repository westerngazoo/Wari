# The VF2 network driver — how it works, and how we debugged it

> Supersedes `phase-1c-status.md` as the reference for the silicon
> network path. Current as of build 138 (2026-07-07), the build that
> made ping stable on real hardware.
>
> Hardware: StarFive VisionFive 2 (`starfive,visionfive-v2`, the base
> v1.0/1.2 board — NOT the `-v1.3b` mainline variant), JH7110 SoC,
> GMAC1 at `0x16040000`, Motorcomm YT8531C PHY at **MDIO address 1**,
> RGMII, isolated test net `192.168.50.0/24` (Wari = `.10`).

---

## 1. Architecture — the path of one ping

```
 laptop (192.168.50.4)
   │  ARP who-has .10 / ICMP echo
   ▼
 OpenWrt switch (isolated, no WAN)
   ▼
 VF2 eth1 RJ45 ──► YT8531C PHY (MDIO addr 1, RGMII, delays in ext regs)
   ▼ RGMII (RX clock from SYSCRG gmac1_rx mux)
 GMAC1 MAC @ 0x16040000 (DWMAC v5.20)     ← MMC counters count here
   ▼ MTL RXQ0 FIFO
 DMA channel 0 ──► writes frame into VF2_RX_BUFS[i], clears OWN in
   │               VF2_RX_RING.descs[i][3]   (16 descriptors, 1536 B each)
   ▼
 ── the software boundary ──
 kernel idle loop (kmain, native RISC-V)
   └─ tier2_net::poll(next_tick())          ← next_tick = rdtime-based ms
        └─ wasmi call into the signed Tier-2 driver wasm
             └─ driver_poll → nic_iface::poll
                  └─ smoltcp Interface::poll(Instant, Device, Sockets)
                       ├─ Device::receive() walks the ring for OWN=0,
                       │   yields (RxToken, TxToken)
                       ├─ RxToken::consume → frame bytes to smoltcp,
                       │   then vf2_rx_rearm(i): re-arm descriptor
                       │   (OWN|IOC|BUF1V) + kick RX_TAIL doorbell
                       └─ smoltcp replies (ARP/ICMP) via
                           TxToken::consume → TDES3 OWN|FD|LD|len,
                           kick TX_TAIL doorbell → DMA → MAC → wire
```

Key properties:

- **The driver is WASM** (`drivers/net`, `wasm32-unknown-unknown`,
  signed, embedded in the kernel via `include_bytes!`). All MMIO goes
  through cap-gated host fns (`wari::net_mmio_read32/write32`);
  the validator (`kernel/src/validate.rs::is_net_mmio_addr`) allows
  `[0x16030000,0x16050000)` + SYSCRG + SYS SYSCON + AON windows.
  **No inline asm in driver code, ever** — it doesn't compile to
  wasm32 and cargo will silently reuse the last good artifact
  (that's the builds-107-114 incident).
- **Polling, not interrupts.** The kernel idle loop drives
  `Interface::poll` continuously; wasmi's interpreter overhead makes
  this ~100k polls/sec on the U74.
- **The `gmac1` cargo feature** switches every platform constant:
  `NIC_BASE` 0x16030000→0x16040000, `PHY_ADDR` 0→1, clock/reset/
  syscon blocks AON→SYS, MAC `…:84`→`…:85`, PHY delay profile.

## 2. Bring-up sequence (driver_start, gmac1 path)

Order matters. This is what build 138 does, with the golden values:

| # | Step | Register(s) | Value |
|---|---|---|---|
| 1 | SYS CRG gates | `0x13020184` (ahb), `0x188` (axi) | `0x80000000` |
| 2 | Deassert GMAC1 resets (RMW only!) | `0x13020300` bits 2,3 → 0 | verify at `0x310` |
| 3 | phy_intf_sel = RGMII (early) | `0x13030090` bits 4:2 | `0b001` |
| 4 | Read MAC_VERSION | `0x16040110` | expect `0x…52` (v5.20) |
| 5 | PHY ID via MDIO addr **1** | std regs 2,3 | `0x4F51` / `0xE91B` |
| 6 | **PHY init (BSP 3-step RMW)** | ext `0xA001` clear bit 8; ext `0xA010` mask `0xF030` → `0xC030`; ext `0xA003` delay nibbles → `0x0850` | per StarFive motorcomm.c `ytphy_of_config` |
| 7 | Re-AN if PHY config changed | std reg 0 = `0x1200` | delays latch at link-up |
| 8 | **Clock cluster (golden)** | `0x190`=`0xC`, `0x194`=`0x1`, `0x198`=`0x8000000A`, `0x19C`=**`0x20`**, `0x1A0`=`0x40000000`, `0x1A4`=**`0x81000000`**, `0x1A8`=`0x40000000`, `0x1AC`=`0x80000020`, `0x1B8`=`0x8000001E` | copied verbatim from working Linux |
| 9 | DMA soft-reset clear, rings, MTL, MAC addr (`0x80008540`/`0x0039CF6C`), packet filter PR=1, MAC_CONFIG `0x2003` | see source | |
| 10 | `nic_iface::init(mac)` + `wari_nic_set_mac` | | kernel installs Tier2NetHandle, idle loop starts polling |

The single most fragile line: **`0x1302019C = 0x00000020`** (gmac1_rx).
Writing bit 31 there does nothing (reads back 0) — the value `0x20`
is what working Linux runs and what finally clocked the RX domain.

## 3. The clock — smoltcp needs real time

`kernel/src/runtime/tier2_net.rs::next_tick()` reads the RISC-V
`time` CSR (`rdtime`) and converts to milliseconds (JH7110 timebase
4 MHz, QEMU 10 MHz). smoltcp uses this for its neighbor (ARP) cache
lifetime, TCP retransmit, and delayed-ACK timers.

**Never replace this with a loop counter.** Builds ≤137 advanced a
fake clock 10 "ms" per idle iteration → ~1000 virtual seconds per
real second → the 60-s ARP cache expired every 60 real ms → smoltcp
re-ARPed before almost every reply (log evidence: 111 ARP frames
transmitted vs 82 ICMP replies) → intermittent worsening ping loss
on an otherwise perfect datapath.

## 4. Diagnostics

Two layers, both documented tag-by-tag in `diagnostic-tags.md`:

- **Always-on milestones**: boot-time register writes log pre/post
  values (`G1RG/G1RA/G1Rs/G1Rt`, `pIeP/pIeN`, `PaDr`, `PHY\x`,
  `RC1R/RC1p`, `CC0r`, `PDSr`, `Sy1..` cluster dump, `MACH/MACL/MACF`,
  `RXQ0`).
- **`net-diag` cargo feature** (trace profile): every ~32k
  `receive()` calls, a 17-register snapshot spanning MAC config /
  MMC RX counters / MTL / DMA (`NdgS`, `NM_*`, `Nm*`, `NT_*`,
  `ND_*`), plus a one-shot deep dump on the first frame (`NdgF`),
  plus cumulative event counters every ~65k calls
  (`StRc/StRf/StCc/StDc/StRa/StTx`).

Reading a snapshot answers "which layer drops frames" in one screen:

| Pattern | Layer at fault |
|---|---|
| `NM_P` link bit low | PHY / cable |
| link up, `NmGB` not counting | PHY↔MAC (RGMII timing, clocks) |
| `NmGB` counts, `NmCr` tracks it | RGMII delay wrong (CRC) |
| `NmGB` counts clean, `NT_M` > 0 | MTL dropping |
| `ND_S` bit 7 (RBU) | ring exhausted / re-arm too slow |
| all clean but `StRf` = 0 | descriptor handoff / RX_NEXT bug |
| all clean, `StRf`=`StCc`, replies missing | **look above the driver: smoltcp config/clock** |

Note: the MMC counters on this die are **reset-on-read** — snapshot
values are per-interval rates, not cumulative.

Golden-reference companion: `scripts/dump-gmac1-regs.sh` runs on the
VF2's Debian (same silicon, working driver) and dumps every register
the RX path depends on via `/dev/mem`. Diff its output against
Wari's boot trace to find any config delta.

## 5. The troubleshooting war — builds 124→138

Fourteen builds to first stable ping. Worth recording because the
*shape* of the failure is a general lesson.

### The three masked faults

RX was silently zero because **three independent faults were live at
once** — and every single-variable fix was "disproven" on silicon
because the other two still zeroed RX:

| Fault | What it was | Fixed |
|---|---|---|
| **A** | PHY writes went to MDIO address 0; GMAC1's PHY is at **1** (BSP DT: `&gmac1 { ethernet-phy@1 }`) | 130 |
| **B** | SYSCRG clock cluster wrong — five registers, worst being `gmac1_rx` (`0x19C`) left at 0 = **no RX clock at all** | 136 |
| **C** | PHY RGMII delays left at U-Boot residue (`0xA003=0x00F1`, rx-delay 0) instead of the BSP config (`0x0850` + `0xA001` bit-8 clear + `0xA010` drive) | 137 |

The trap in sequence: build 127 wrote the right PHY values — to the
wrong address, through a dead clock ("PHY writes don't help").
Build 130 fixed the address — clock still dead ("0x0850 doesn't
help"). Build 131 therefore **removed** the PHY writes as useless.
Build 136 fixed the clocks — PHY now wrong again ("clocks didn't
help either"). Only 137, with A+B+C all fixed, worked.

**Lesson: when multiple faults coexist, one-variable-at-a-time
elimination generates false negatives, and you will revert correct
fixes.** The way out was the golden-reference diff: stop theorizing,
dump the entire register state of the working system (Debian, same
board), and make the broken system byte-identical.

### The fourth bug: the clock (§3)

Found only after RX worked, because the diag counters could finally
prove the datapath clean while ping still stuttered. TX frame-length
census (111×42 B ARP vs 82×74 B ICMP) was the fingerprint.

### Red herrings we chased (and what disproved each)

- **`MAC_RXQ_CTRL0` "RXQ0 enable"** (builds 133/134): the golden dump
  shows working Linux also has it at 0 — no-op on this synthesis.
- **RGMII delay *value* sweeps** (`0x680A`/`0x0800`/`0x0850`) before
  the address+clock fixes — values were never the sole problem.
- **Descriptor recycling / caching / RBU** — plausible early, ruled
  out by counters (`StCc == StRf`, no RBU, ring rotating).

### Build-system incidents that compounded the hunt

- 107–114: driver wasm build broke silently (inline `asm!` on
  wasm32); cargo reused the stale artifact for 8 builds.
- 122–124: kernel and driver bumped independently by parallel deploys.
- Fix: `scripts/build.sh` (single entrypoint, full closure, four-way
  tag verify — see `build-workflow.md`) plus the `kernel/build.rs`
  stale-driver guard. The guard caught a real stale-blob attempt
  during build 138 development.

## 6. Known remaining deltas vs Linux (documented, non-blocking)

The golden diff still shows differences that the counters prove are
NOT currently dropping frames, kept here for the day symptoms change:

| Register | Linux | Wari | Note |
|---|---|---|---|
| `MAC_CONFIGURATION` | `0x08072203` (IPC, JE/JD, DCRS…) | `0x2003` | checksum-offload + jumbo bits; revisit for TCP perf |
| `MAC_PACKET_FILTER` | `0x404` (hash filter) | `0x1` (promiscuous) | PR is more permissive; fine for now |
| `MTL_RXQ0_OP` | `0x00700000` (threshold mode — DT has `snps,force_thresh_dma_mode`) | `0x00700020` (RSF) | watch if large-frame issues appear |
| `DMA_SYSBUS_MODE` | `0x030308F1` (burst config) | reset default | throughput, not correctness |
| `DMA_CH0_RX_CONTROL` | RXPBL=16 | RXPBL=1, +RPF | throughput |
| RX ring | 512 descriptors | 16 | fine at ping rates; grow for TCP |

## 7. Operating it

```bash
# Dev machine — build (see build-workflow.md for profiles):
scripts/build.sh trace          # or release / debug / qemu
git add build/wari.bin .build_number && git commit && git push

# VF2 (Debian side):
sudo ip route del default via 192.168.50.1 dev end1   # OpenWrt has no WAN
wari upgrade && wari go -y      # flash main
wari go-branch <branch>         # or flash a testing branch

# Laptop:
arp -d * && ping -t 192.168.50.10

# Reading the trace (PuTTY logging → C:\projects\putty.log):
grep -a "tag=0x4e6d4742" putty.log     # NmGB — frames at MAC
grep -a "tag=0x53745478" putty.log     # StTx — smoltcp replies
```

Topology: VF2 `end0` → home router (internet, for `wari upgrade`);
VF2 `end1` → isolated OpenWrt (test net). `.bashrc` on the VF2
sources `~/wari/scripts/wari-upgrade.sh` from the repo — never a
frozen copy.
