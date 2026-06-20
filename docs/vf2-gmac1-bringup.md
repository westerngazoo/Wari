<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Wari — JH7110 GMAC1 Bring-up (VisionFive 2, eth1)

> **Status:** Phase-1c silicon bring-up, in progress (June 2026).
> GMAC1 is out of reset and alive on real silicon; the RGMII/PHY
> link layer is the remaining work. This document is the authoritative
> record of how the JH7110 GMAC1 is brought up from a cold SD boot and
> what is left to do. It exists because the knowledge was paid for in
> a multi-day debugging session and must not be re-derived.

This is a companion to [`net-driver-design.md`](net-driver-design.md)
(the overall Tier-2 net architecture) and [`vf2-bringup.md`](vf2-bringup.md)
(board setup). It is GMAC1-specific: the registers, the ordering, and
the evidence.

---

## 1 · Why GMAC1 and not GMAC0

The VF2 (JH7110) exposes two Gigabit MACs:

| Port | MMIO base | EEPROM MAC | Clock/reset domain |
|------|-----------|------------|--------------------|
| `eth0` (GMAC0) | `0x16030000` | `6c:cf:39:00:40:84` | **AON** CRG (`0x17000000`) + STGCRG |
| `eth1` (GMAC1) | `0x16040000` | `6c:cf:39:00:40:85` | **SYS** CRG (`0x13020000`) only |

The decisive fact discovered during bring-up: **the VF2's OpenWrt LAN
cable is on `eth1` (GMAC1)**, not `eth0`. This was found by reading the
OpenWrt router's ARP table — the VF2's reachable interface answered from
GMAC1's MAC (`…:85`). Earlier Phase-1c work targeted GMAC0; switching to
GMAC1 is what `phase-1c/net-6d-http-demo` does.

The two MACs are **not** symmetric. They live in different clock and
reset domains:

- **GMAC0** clocks/reset come from the **AON CRG** (`AONRST_GMAC0_AXI=0`,
  `AONRST_GMAC0_AHB=1`) plus STGCRG bus clocks.
- **GMAC1** clocks/reset come **entirely from the SYS CRG** (`SYSRST_*`,
  SYSCRG gates). No AON, no STGCRG involvement.

Porting the driver GMAC0→GMAC1 is therefore **not** a base-address
change. It is a clock/reset-domain change. Every AON/STGCRG write in the
GMAC0 path must be replaced by the SYSCRG equivalent, and the kernel's
Sv39 identity map must map GMAC1 + SYSCRG (not GMAC0 + AON + STG).

---

## 2 · The bug that cost us days: GMAC1 left in reset

A cold **SD boot** does not network-probe either MAC. The driver had
inherited an assumption from the GMAC0 path:

> *"Hardware reset for GMAC1 is in SYSCRG reset registers; U-Boot
> deasserts it during PXE probe so we rely on that."*

That is false for an SD boot. U-Boot enumerates `eth1: ethernet@16040000`
but never deasserts its reset, because it never does a network boot. The
block stayed in reset, so **every** GMAC1 register read returned `0`:

- MAC version (`0x110`) = `0`
- MDIO reads (PHY ID, BMCR, BMSR) = `0`
- DMA channel registers (`0x1108` RX control, `0x1134` IE, `0x1160`
  status) = `0`

Each of those zeros looked like a separate problem (wrong PHY address,
dead MDIO clock, DMA never started). They were all the **same** root
cause: the IP block was held in reset, so the whole MMIO aperture read
back zeros.

**Proof the block is fine once released:** from Debian (which brings
GMAC1 up correctly), `/dev/mem` read of `0x16040110` returns `0x00004152`
(DWMAC v5.20). The hardware works; we simply never released it.

---

## 3 · Register map (authoritative)

All offsets verified against the mainline Linux JH7110 drivers and the
VF2 EEPROM/clock dumps. Sources cited in §8.

### 3.1 SYSCRG (`0x13020000`)

Clock registers are one 32-bit word per clock id, at `id × 4`. The
`jh71x0` clock register format:

| Bits | Field | Meaning |
|------|-------|---------|
| 31 | `JH71X0_CLK_ENABLE` | gate enable (1 = on) |
| 30 | `JH71X0_CLK_INVERT` | polarity invert |
| 29:24 | mux | parent select |
| 23:0 | divider | integer divide |

GMAC1-relevant clock gate offsets:

| Offset | Clock | Notes |
|--------|-------|-------|
| `+0x024` | `AHB0` | bus parent; serves GMAC1 AHB |
| `+0x180` | `NOC_BUS_STG_AXI` | bus parent; serves GMAC1 AXI |
| `+0x184` | `gmac1_gtxclk` | RGMII GTX 125 MHz; divider matters (see §6) |
| `+0x188` | `gmac1_gtxc` | GTX clock to PHY |
| `+0x190` | `gmac1_ahb` | CSR/register access clock |
| `+0x194` | `gmac1_axi` | DMA AXI clock |
| `+0x198` | `gmac1_rgmii_rx` | RGMII RX path gate |
| `+0x19C` | `gmac1_ptp` | PTP reference |

Software-reset registers (the JH7110 `jh7110_sys_info` reset block):

| Property | Value |
|----------|-------|
| assert base | `0x2F8` |
| status base | `0x308` |
| resets per register | 32 |

Reset index → register/bit: `assert_reg = 0x2F8 + (id/32)*4`,
`bit = id % 32`; matching status register at `0x308 + (id/32)*4`.

| Reset | Index | Assert reg | Bit | Status reg |
|-------|-------|------------|-----|------------|
| `SYSRST_GMAC1_AXI` | 66 | `0x300` | 2 | `0x310` |
| `SYSRST_GMAC1_AHB` | 67 | `0x300` | 3 | `0x310` |

So GMAC1's reset is **`0x13020300`, bits 2–3 (mask `0x0C`)**. Clear a
bit to deassert; the matching status bit in `0x13020310` then reads `0`
(`jh71x0` reset semantics: deasserted → status 0).

> ⚠️ Note the collision-by-coincidence: GMAC's *own* `MAC_ADDRESS0_HIGH`
> is also at offset `0x300`, but relative to the **GMAC base**
> (`0x16040300`), not SYSCRG. Different bases; do not confuse them.

### 3.2 GMAC1 MAC/DMA (`0x16040000`)

| Offset | Register | Notes |
|--------|----------|-------|
| `0x000` | `MAC_CONFIGURATION` | bit0 `RE` (rx en), bit1 `TE` (tx en) |
| `0x008` | `MAC_PACKET_FILTER` | bit0 `PR` = promiscuous |
| `0x110` | `MAC_VERSION` | `0x4152` when alive (DWMAC 5.20) |
| `0x200` | `MAC_MDIO_ADDRESS` | GB/GOC/CR/RDA/PA fields (see §3.3) |
| `0x204` | `MAC_MDIO_DATA` | low 16 bits = PHY data |
| `0x300` | `MAC_ADDRESS0_HIGH` | AE(31) + bytes 5:4 |
| `0x304` | `MAC_ADDRESS0_LOW` | bytes 3:0 |
| `0x1100` | `DMA_CH0_CONTROL` | |
| `0x1104` | `DMA_CH0_TX_CONTROL` | bit0 `ST` start tx |
| `0x1108` | `DMA_CH0_RX_CONTROL` | bit0 `SR` start rx + `RBSZ` |
| `0x1134` | `DMA_CH0_INTERRUPT_ENABLE` | |
| `0x1160` | `DMA_CH0_STATUS` | |
| `DMA_BUS_MODE` | — | bit0 `SWR` software reset |

For GMAC1 MAC `6c:cf:39:00:40:85`:
- `MAC_ADDRESS0_LOW`  = `0x0039CF6C` (b3..b0 = `00:39:cf:6c` LE)
- `MAC_ADDRESS0_HIGH` = `0x80008540` (AE + b5=`0x85`, b4=`0x40`)

### 3.3 MDIO

DWMAC4 `MAC_MDIO_ADDRESS` (`0x200`):

| Bits | Field | Value used |
|------|-------|------------|
| 0 | `GB` | busy/start |
| 3:2 | `GOC` | `11` = read C22, `01`/`00` = write |
| 11:8 | `CR` | CSR clock range; `4` = CSR/26 |
| 20:16 | `RDA` | register address |
| 25:21 | `PA` | PHY address |

**PHY address is 0.** Confirmed from Debian: `YT8531 … stmmac-1:00`
(and an alias at `:01`). The YT8531C answers at MDIO address 0 on
GMAC1's bus. `PHYID1` (reg 2) ≈ `0x4F51`.

> If `CR=4` (CSR/26) ever produces an out-of-spec MDC for GMAC1's CSR
> clock (which may differ from GMAC0's), MDIO reads return `0` even with
> the block out of reset. Revisit `CR` if MDIO stays dead after §4.

### 3.4 PHY RGMII delay (YT8531C)

Motorcomm extended-register protocol: write reg `0x1E` (page select) =
ext-reg address, then read/write reg `0x1F` (page data).

`YT8521_RGMII_CONFIG1_REG = 0xA003`:

| Bits | Field | VF2 value |
|------|-------|-----------|
| 14 | `TX_CLK_SEL_INVERTED` | 1 (tx-clk-1000-inverted) |
| 13:10 | `RX_DELAY` | `0xA` (1500 ps @ 150 ps/step) |
| 7:4 | `FE_TX_DELAY` (100M) | 0 |
| 3:0 | `GE_TX_DELAY` (1G) | `0xA` (1500 ps) |

Final value `0x680A`. Matches mainline DT
`jh7110-starfive-visionfive-2-v1.3b.dts`:
`rx-internal-delay-ps=1500`, `tx-internal-delay-ps=1500`,
`tx-clk-1000-inverted`.

---

## 4 · Required bring-up sequence (ordered)

Order matters. Each step depends on the previous. The historical bug was
doing register access (version read, PHY probe) **before** clocks +
reset, so everything read zeros.

1. **Enable SYSCRG clocks** — read-modify-write `| ENABLE_BIT(31)` on the
   bus parents (`0x024`, `0x180`) and the six GMAC1 gates
   (`0x184`..`0x19C`). RMW preserves U-Boot's divider/mux bits.
2. **Deassert GMAC1 reset** — clear bits 2,3 of `0x13020300`; poll
   `0x13020310` until those bits read `0` (bounded spin).
3. **Re-read `MAC_VERSION` (`0x110`)** — must now be `0x4152`. This is
   the single go/no-go gate; if still `0`, steps 1–2 did not take.
4. **DMA software reset** — set/clear `SWR` in `DMA_BUS_MODE`, poll to 0.
   Cannot complete while the block is in hardware reset, hence step 2
   first.
5. **Program MAC address** — `MAC_ADDRESS0_LOW`/`HIGH`; enable
   promiscuous (`MAC_PACKET_FILTER` bit0) for bring-up; set `RE`+`TE` in
   `MAC_CONFIGURATION` (→ reads `0x2003`).
6. **PHY** — MDIO read ID (expect `0x4F51`), write RGMII delay `0x680A`
   to ext-reg `0xA003`, wait for autoneg, read `BMSR` link bit.
7. **DMA rings** — program TX/RX descriptor base + tail pointers, RBSZ,
   then set `SR`/`ST` start bits (→ `DMA_CH0_RX_CONTROL` reads
   `0x80010c01`).

In the current driver, steps 1–2 are a **power-on block at the very top
of the vf2 init path** (added in build 138), before any register touch.
A later SYSCRG block re-asserts the gates while driving the DMA SWR
clear; that re-assertion is idempotent.

---

## 5 · What is proven (evidence ledger)

| Claim | Evidence | Build |
|-------|----------|-------|
| eth1 = GMAC1 = `0x16040000`, MAC `…:85` | U-Boot `Net: … eth1: ethernet@16040000`; EEPROM dump | — |
| OpenWrt LAN is on eth1 | OpenWrt ARP table shows VF2 at `…:85` | — |
| Kernel must map GMAC1 + SYSCRG | page fault at `stval=0x16040110` until `kvm.rs` mapped GMAC1 | 128 |
| Block was held in reset | all GMAC1 regs read `0`; Debian `/dev/mem 0x16040110 = 0x4152` | ≤136 |
| Reset is SYSRST 66/67 → `0x300` bits 2,3 | mainline `starfive,jh7110-crg.h` + `jh7110_sys_info` | — |
| Reset-deassert brings block alive | `MACc=0x2003` (RE+TE), `dRXc=0x80010c01` (SR) — both were `0` | 138/140 |
| PHY is at MDIO addr 0 | Debian `YT8531 … stmmac-1:00` | — |

---

## 6 · Remaining work (the last mile)

The block is alive but **no frames flow** (`StRf` frames-found stays `0`
across 850k+ poll cycles; `StDc` drop-count `0` ⇒ nothing reaches the
DMA ring at all). This is the PHY/RGMII link layer — the exact issue the
project notes flagged as "YT8531C RGMII delay calibration."

Open items, in priority order:

1. **Confirm MDIO works post-reset.** Read `PHYID1` — must be `0x4F51`.
   If still `0`, the block is alive but MDIO isn't (revisit `CR`
   divider, §3.3). Without MDIO we cannot program RGMII delays.
2. **PHY link state.** Read `BMSR` bit 2. Physical: is the eth1 port
   LINK LED lit? Dark ⇒ no carrier (PHY/cable/autoneg); lit ⇒ link up
   and the problem is RX timing.
3. **`gmac1_gtxclk` divider.** Debian idle shows `0x13020184 = 0`
   (gated). Active needs a divider (was `0x0c` = ÷12 → register
   `0x8000000c`). The current driver only OR-s the enable bit, which
   yields divider `0` if U-Boot left it `0`. If RGMII TX is dead, set
   the divider explicitly: `0x8000000c`.
4. **RGMII RX delay.** Once MDIO works, verify `0x680A` actually lands
   in ext-reg `0xA003` (read-back). A wrong RX delay = frames arrive but
   fail CRC and never reach the ring (`StRf=0`, `StDc=0`).
5. **TX path.** `StTx` is `0`; verify the descriptor write + tail
   doorbell once RX proves the ring plumbing.

A reasonable test ladder once link is up: ARP from OpenWrt (one L2 hop,
inspect its ARP table) → ICMP → the canned-HTTP TCP demo on port 7000.

---

## 7 · Diagnostic tags added this session

Decode big-endian ASCII (see [`diagnostic-tags.md`](diagnostic-tags.md)).
All are `[net:drv] tag=… val=…` lines from `wari_drv_log_u32`.

| Tag | ASCII | Meaning |
|-----|-------|---------|
| `0x52737470` | `Rstp` | SYSCRG reset-assert `0x300` before deassert |
| `0x52737473` | `Rsts` | reset status `0x310` after (bits 2,3 → 0 = released) |
| `0x52737477` | `Rstw` | poll iterations to release |
| `0x476d6143` | `GmaC` | `MAC_VERSION` after power-on (`0x4152` = alive) |
| `0x50596931` | `PYi1` | PHYID1 (`~0x4F51`) |
| `0x50596932` | `PYi2` | PHYID2 |
| `0x50596c6b` | `PYlk` | `BMSR` (bit2 = link up) |
| `0x4d414376` | `MACv` | MAC version (snapshot copy) |
| `0x4d414363` | `MACc` | `MAC_CONFIGURATION` (`0x2003` = RE+TE) |
| `0x64525863` | `dRXc` | `DMA_CH0_RX_CONTROL` (`0x80010c01` = SR started) |
| `0x64535453` | `dSTS` | `DMA_CH0_STATUS` |

The periodic counter dump (`StRc`/`StRf`/`StCc`/`StDc`/`StRa`/`StTx`)
now fires **only when frames-found or tx-sent changes**, not every 65 536
calls — an all-zero RX path goes silent after one baseline burst so the
end-of-init snapshot stays readable on a scrolling console.

---

## 8 · References consulted

- Mainline Linux `include/dt-bindings/reset/starfive,jh7110-crg.h` —
  `JH7110_SYSRST_GMAC1_AXI=66`, `_AHB=67`; `JH7110_AONRST_GMAC0_*`.
- Mainline `drivers/reset/starfive/reset-starfive-jh7110.c` —
  `jh7110_sys_info { assert_offset=0x2F8, status_offset=0x308 }`.
- Mainline `drivers/clk/starfive/clk-starfive-jh7110-sys.c` and
  `clk-starfive-jh7110.h` — SYSCRG clock register layout.
- `stmmac` (Synopsys DWMAC) register semantics — **read as a sketch, do
  not copy** (GPL vs our AGPL posture; see net-driver-design.md §11).
- Motorcomm YT8531C datasheet + mainline `motorcomm.c`
  (`ytphy_write_ext`) for the RGMII delay ext-register protocol.
- VF2 `jh7110-starfive-visionfive-2-v1.3b.dts` — `rx/tx-internal-delay-ps`.

---

## Appendix · Tooling note: `wari go` persistence

`vf2-bringup.md` installs the deploy helper by **copying**
`scripts/wari-upgrade.sh` to `/root/wari-upgrade.sh` and sourcing that
copy from `.bashrc`. Consequence: a `git reset --hard` in the repo
updates the repo file but **not** the sourced copy, so script fixes do
not survive a new login. The in-session self-update (`WARI_SCRIPT_CHANGED`
re-source) only patches the *current* shell.

Two fixes (pick one in a follow-up):

1. Source the repo file directly: `.bashrc` →
   `source /root/wari/scripts/wari-upgrade.sh`. Then `git reset --hard`
   alone keeps the helper current.
2. Have the self-update step `cp` the new script over
   `/root/wari-upgrade.sh` whenever `WARI_SCRIPT_CHANGED=1`.

Also fixed this session: `wari go` now follows the **current** branch
(`git rev-parse --abbrev-ref HEAD`) with `git fetch` + `git reset --hard`
— no `main` hard-coding, no `git pull` (which deadlocks on the divergent
history every deploy creates by committing `build/wari.bin`).
