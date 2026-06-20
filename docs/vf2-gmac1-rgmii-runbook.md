<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Runbook — GMAC1 RGMII / PHY link bring-up (at the board)

> **Purpose:** a mechanical, decision-tree checklist for the hands-on
> VF2 session that takes GMAC1 from *alive-but-silent* to *frames
> flowing*. Reference details live in
> [`vf2-gmac1-bringup.md`](vf2-gmac1-bringup.md); this is the do-this-then-that.
>
> **Entry state (already done, June 2026):** GMAC1 is out of reset and
> alive — `GmaC=0x4152`, `MACc=0x2003`, `dRXc=0x80010c01`. The wall now
> is `StRf=0`: no frames arrive. This runbook isolates and fixes that.

## 0 · Before you start

- Mac on the **OpenWrt LAN side** (en5), *never* en0/WiFi.
- VF2 `eth1` cabled to the OpenWrt switch.
- Serial console on COM7 open.
- Two branches in play:
  - `phase-1c/net-6d-http-demo` — baseline (block alive, no gtxclk fix).
  - `phase-1c/net-6e-gtxclk-divider` — **staged** explicit gtxclk ÷12.

Flash a branch with: `wari go-branch <branch>` (follows it thereafter
with plain `wari go`).

## 1 · Read the boot snapshot

After flashing, the console settles (the `St**` flood is silenced). Note
these tags from the end-of-init burst:

| Tag | Healthy value | If wrong |
|-----|---------------|----------|
| `GmaC` | `0x4152` | block not alive → go to bringup spec §4 (clocks/reset) |
| `PYi1` | `~0x4F51` | `0`/`0xffff` → MDIO dead → **§2** |
| `PYlk` | bit 2 set | bit 2 clear → no link → **§3** |
| `MACc` | `0x2003` | RE/TE not set → MAC config path |
| `dRXc` | `0x80010c01` | SR not set → DMA ring start path |

Then ping from OpenWrt (`ping 192.168.50.10`) and watch `StRf`
(`0x53745266`). **`StRf` moving = RX works** → jump to **§4**.

## 2 · MDIO dead (`PYi1` = 0 despite `GmaC` = 0x4152)

Block is alive but the MDIO sub-block isn't talking. Likely the MDC
clock divider (`CR`) is wrong for GMAC1's CSR clock.

1. In `mdio_read_phy` / `mdio_write_phy`, `CR = 4` (CSR/26). Try `CR = 5`
   (CSR/102) — a slower, safer MDC — if GMAC1's CSR clock is faster than
   GMAC0's.
2. Re-flash, re-check `PYi1`. Expect `0x4F51`.
3. Only once `PYi1` is good do RGMII-delay writes (§3) mean anything.

## 3 · No link (`PYlk` bit 2 clear) — the RGMII calibration

This is the documented hard part. Work it in this order; stop when
`StRf` starts moving.

### 3a · Physical
- eth1 port **LINK LED**: dark = no carrier. Check cable, swap port on
  the OpenWrt switch, confirm the switch port is up.

### 3b · GTX clock (most likely culprit)
- Flash **`phase-1c/net-6e-gtxclk-divider`**. Check the `Gtxl` tag
  (`0x4774786c`) reads back **`0x8000000c`** (enable + ÷12 = 125 MHz).
- If `Gtxl` ≠ `0x8000000c`, the write didn't stick (clock locked or
  wrong offset) — re-read live from Debian: `0x13020184`.
- Re-test link + `StRf`.

### 3c · RGMII delays land in the PHY
- The driver writes `0x680A` to YT8531C ext-reg `0xA003`. Confirm with a
  read-back (the `RC1p` tag, `0x52433170`) = `0x680A`.
- If the delays are off, frames arrive but fail CRC → `StRf=0`, `StDc=0`.
  The 4-bit RX/TX delay fields are ÷150 ps/step; the VF2 DT value is
  1500 ps = `0xA`. If 1500 ps still fails, sweep RX delay `0x8`…`0xC`
  one step per flash and watch `StRf`.

### 3d · Cross-check against Debian
- Debian drives eth1 fine. From Debian, dump the live PHY ext-reg and
  the gtxclk register; match the driver's writes to those values. (Use
  the `/dev/mem` Python reader from the bringup session; `devmem` isn't
  installed.)

## 4 · RX works (`StRf` moving) — climb the stack

1. **ARP** — from OpenWrt `ping 192.168.50.10`; check `ip neigh` for
   `6c:cf:39:00:40:85`. Confirms RX + TX at L2.
2. **ICMP** — ping succeeds end-to-end. Confirms smoltcp IP + the
   TX path (`StTx` moving).
3. **HTTP demo** — from the Mac (en5): `curl http://192.168.50.10:7000/`.
   Expect the canned reply. This is the Phase-1c exit gate.

If ARP replies but ICMP doesn't: smoltcp IP config (the static
`192.168.50.10/24`) or the TX descriptor path — check `StTx`.

## 5 · Landing the fix

Once a branch makes `curl` work:
1. If it was `net-6e-gtxclk-divider`, fast-forward / cherry-pick the
   gtxclk commit into `phase-1c/net-6d-http-demo` (or merge), so the
   demo branch carries the working config.
2. Update [`vf2-gmac1-bringup.md`](vf2-gmac1-bringup.md) §5 evidence
   ledger (mark gtxclk/RGMII rows proven) and §6 (strike the resolved
   items).
3. Open the PR per the CLAUDE.md template; the security section is
   "None — Tier-2 driver MMIO already cap-gated; no new host fns."

## Quick reference — tags to watch

`GmaC`=alive · `PYi1`=PHY id · `PYlk`=link · `Gtxl`=gtxclk readback ·
`RC1p`=RGMII delay readback · `StRf`=frames in · `StTx`=frames out ·
`StDc`=DMA drops. Decode: big-endian ASCII, see
[`diagnostic-tags.md`](diagnostic-tags.md).
