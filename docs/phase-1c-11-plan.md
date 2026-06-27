# Phase 1c-11 — GMAC1 (eth1) port (planned)

> **Status**: not yet applied to source. Ready to land as build 125 on
> the next coding session. Authored 2026-05-15 after the operator's
> isolated-OpenWrt cabling scheme made GMAC1 the test-net port.

## Motivation

Operator's topology:
- VF2 **eth0** (`end0`, GMAC0 `6c:cf:39:00:40:84`) → home router → internet
  (needed so Debian can `wari upgrade` from origin/main).
- VF2 **eth1** (`end1`, GMAC1 `6c:cf:39:00:40:85`) → OpenWrt isolated
  switch → laptop USB-Ethernet adapter.
- Operator does not want to swap cables between sessions.

Wari currently hardcodes GMAC0 (`drivers/net/src/lib.rs:103`,
`plat::NIC_BASE = 0x16030000`). To make ping-from-laptop reach Wari
without recabling, the driver must be ported to GMAC1
(`0x16040000`).

## Source-of-truth delta (vs current GMAC0 init)

### Clock/gate registers — all in SYS CRG `0x13020000`

| GMAC0 (AON CRG `0x17000000`) | GMAC1 (SYS CRG `0x13020000`) | Purpose |
|---|---|---|
| `+0x08` ahb gate (id 2) | **`+0x184`** gmac1_ahb (idx 97) | bit31 enable |
| `+0x0C` axi gate (id 3) | **`+0x188`** gmac1_axi (idx 98) | bit31 enable |
| `+0x14` tx GMUX | **`+0x1A4`** gmac1_tx GMUX (idx 105) | bit31 + bits[25:24]=0 |
| `+0x1C` rx MUX | **`+0x19C`** gmac1_rx MUX (idx 103) | bit31 + bits[25:24]=0 |
| `+0x20` rx_inv | **`+0x1A0`** gmac1_rx_inv (idx 104) | bit30 = invert |

### SYS CRG dividers (GMAC1 has its own copies — NOT the +0x1B0/+0x1B4/+0x1BC block used by GMAC0)

| GMAC0 | GMAC1 | Function |
|---|---|---|
| `+0x1B0` gtxclk (en+div=5) | **`+0x190`** gmac1_gtxclk (idx 100, **plain DIV — no en bit**) | bits[7:0]=div, set to `5` for PLL0/5 = 200 MHz |
| `+0x1B4` ptp (en+div=10) | **`+0x198`** gmac1_ptp (idx 102, GDIV) | bit31 enable + bits[7:0]=div |
| `+0x1B8` MDC (en+div=30) | **shared — keep writing `+0x1B8`** | one MDC root for both MACs |
| `+0x1BC` gtxc gate | **`+0x1AC`** gmac1_gtxc gate (idx 107) | bit31 enable |

There's also a shared root `+0x18C gmac_src` (idx 99, plain DIV from
pll0_out, max div=7) feeding both PTP paths. **Read first** — only
reprogram if zero.

### Reset registers — SYS CRG, RMW only

- `JH7110_SYSRST_GMAC1_AXI = 66` → assert reg **`0x300`**, **bit 2**
- `JH7110_SYSRST_GMAC1_AHB = 67` → assert reg **`0x300`**, **bit 3**
- Status mirror: **`0x310`**, same bit positions.

**Deassert = clear bits 2+3 of `0x13020000 + 0x300`. RMW only.** A
blind write to this register resets every device whose enum is in
[64..96] — DMA, security, USB, PCIe peripherals.

No AON reset for GMAC1, no STG CRG (`0x10230000`) involvement.

### SYS SYSCON `phy_intf_sel`

`jh7110.dtsi`: GMAC1 = `<&sys_syscon 0x90 0x2>`.

- SYS SYSCON base: **`0x13030000`** (compatible `"starfive,jh7110-sys-syscon"`)
- Target register: **`0x13030000 + 0x90 = 0x13030090`**
- Field: **bits[4:2]** (3-bit), mask `0x1C`
- Value for RGMII: **`1`** (`stmmac_get_phy_intf_sel()` in `dwmac-starfive.c`)
- RMW: `(reg & ~0x1C) | (1<<2)` → bits[4:2] = `0b001`

### PHY side (YT8531 on GMAC1's MDIO bus)

- **MDIO address**: **0**, on GMAC1's own MDIO bus at `GMAC1_BASE + 0x200/0x204`.
- **YT8531 delays differ from GMAC0**. VF2 v1.3b dts:
  - `&phy1`: `rx-internal-delay-ps = <300>`, `tx-internal-delay-ps = <0>`,
    `motorcomm,tx-clk-adj-enabled`, `motorcomm,tx-clk-100-inverted`.
  - **No `motorcomm,tx-clk-1000-inverted`**.
- YT8531 delay encoding: 300 ps → `0x2`, 0 ps → `0x0`, 1500 ps → `0xA`.
- Mainline writes ext-reg `0xA003` = `(0x2<<10) | (0x0<<0)` = **`0x0800`**.
- GMAC0's value `0x680A` is **wrong for GMAC1** — using it would
  reproduce the same "1% RX" symptom on this port.

### MAC register layout

**Identical** to GMAC0 at the new base. MAC_CONFIG=0x000, MAC_PACKET_FILTER=0x008,
MAC_ADDR0_HI/LO=0x300/0x304, MTL_TXQ0_OP=0xD00, MTL_RXQ0_OP=0xD30,
DMA_CH0_*=0x1100..0x1160 — all unchanged at GMAC1 base `0x16040000`.

### MAC1 address

- `MAC_ADDR0_LO (0x304)` = **`0x0039CF6C`** (unchanged — bytes 0..3 identical to MAC0)
- `MAC_ADDR0_HI (0x300)` = **`0x80008540`** (vs GMAC0's `0x80008440`)

### Pin mux / pad config

**None needed.** No pinctrl entries in any DT for `&gmac1`. SoC-routed,
U-Boot leaves usable.

## Concrete patch list (`drivers/net/src/lib.rs`)

Gate the GMAC1 path behind `cfg(feature = "gmac1")` so the GMAC0
target keeps working. Default for the `vf2` feature stays GMAC0 —
opt into `gmac1` for the operator's test rig.

1. **`drivers/net/Cargo.toml`** — add `gmac1 = []` to `[features]`.

2. **`lib.rs:103`** — under `cfg(feature = "vf2")`, switch
   `plat::NIC_BASE` based on `cfg(feature = "gmac1")`:
   - `0x16030000` (default GMAC0)
   - `0x16040000` (with `gmac1`)

3. **`lib.rs:1400`** — same gate on `GMAC_BASE` mirror.

4. **`lib.rs:2007-2036`** — delete the AON CRG gate/reset block under
   `cfg(feature = "gmac1")`. Replace with SYS CRG GMAC1 bring-up:
   - `SYSCRG+0x184 = 0x80000000` (gmac1_ahb gate)
   - `SYSCRG+0x188 = 0x80000000` (gmac1_axi gate)
   - RMW `SYSCRG+0x300`: clear bits 2 and 3 (deassert gmac1_axi/ahb reset)
   - Verify-poll `SYSCRG+0x310` bits 2,3 cleared

5. **`lib.rs:2354-2363`** — under `cfg(feature = "gmac1")`, replace
   AON SYSCON phy_intf write with: RMW `*(u32*)0x13030090`,
   mask `0x1C`, set value `(1<<2)=0x04`.

6. **`lib.rs:2384-2403`** — replace AON CRG datapath +
   SYS CRG GMAC0 dividers with GMAC1 equivalents:
   - `SYSCRG+0x190 = 0x5` (gmac1_gtxclk, plain DIV, **NO bit31**)
   - `SYSCRG+0x198 = 0x8000000A` (gmac1_ptp en+div=10)
   - **Keep `SYSCRG+0x1B8 = 0x8000001E`** (shared MDC root)
   - `SYSCRG+0x1AC = 0x80000000` (gmac1_gtxc gate)
   - `SYSCRG+0x1A4 = 0x80000000` (gmac1_tx GMUX, mux=0=gtxclk)
   - `SYSCRG+0x19C = 0x80000000` (gmac1_rx MUX, mux=0=rgmii_rxin)
   - `SYSCRG+0x1A0 = 0x40000000` (gmac1_rx_inv bit30)

7. **`lib.rs:2088`** (`YT8531_RC1R_VF2_VALUE`) — under
   `cfg(feature = "gmac1")`, change to **`0x0800`** (rx=300ps, tx=0ps,
   no tx-clk-1000-inv per VF2 v1.3b `&phy1`).

8. **`lib.rs:2743`** (`mac_hi`) — under `cfg(feature = "gmac1")`,
   change to **`0x8000_8540`** (byte 5 = 0x85).

9. **`kernel/src/validate.rs:119-143`** (`is_net_mmio_addr`) — add a
   new whitelist range **`0x1303_0000..0x1303_1000`** (SYS SYSCON
   `phy_intf_sel`). The current SYS CRG range
   `0x1302_0000..0x1303_0000` already covers all the GMAC1
   clock+reset offsets. The widened `NET_MMIO_LEN = 0x2_0000`
   already covers `[0x16030000, 0x16050000)`, so both MAC bases fit.

## Validation plan (`drv_log_u32` tags to add)

Each tag is an ASCII 4-letter mnemonic in big-endian, fired right
after the corresponding register write.

| Tag | ASCII | Source line | Fires when | val |
|---|---|---|---|---|
| `0x4731_5247` | `G1RG` | after SYSCRG+0x184 write | gmac1_ahb gated | readback |
| `0x4731_5241` | `G1RA` | after SYSCRG+0x188 write | gmac1_axi gated | readback |
| `0x4731_5273` | `G1Rs` | after SYSCRG+0x300 RMW | gmac1 reset deasserted | readback of +0x310 |
| `0x4731_5070` | `G1Pp` | after 0x13030090 RMW | phy_intf_sel programmed | readback |
| `0x4731_4D31` | `G1M1` | after MAC_ADDR0_HI write | MAC1 programmed | written value |

Pass criterion:
- COM7 shows `[net:drv] tag=0x47315273 val=0x...0` (reset bits cleared)
- COM7 shows `[net:drv] tag=0x47315070 val=0x...4` (bits[4:2]=001)
- `StRf` counter grows when laptop pings `192.168.50.10`
- tcpdump on OpenWrt shows `ARP Reply 192.168.50.10 is-at 6c:cf:39:00:40:85`

## Risk

| Action | Worst case |
|---|---|
| Blind write to SYS CRG `0x300` | Resets DMA/USB/PCIe and friends — board hangs |
| Wrong bit polarity on reset deassert | GMAC1 stays in reset; symptom is no MAC_VERSION read |
| Touch SYS CRG `+0x18C` (shared root) without checking | GMAC0's PTP path breaks → `wari upgrade` over eth0 dies |
| Touch SYS CRG `+0x1B8` (shared MDC) without checking | Both MACs lose MDIO; PHY ID read returns 0xFFFF |
| Touch SYS SYSCON `+0x90` bits outside `0x1C` mask | Could affect GMAC0's settings; trash RMII RX clock routing |
| Touch AON CRG/SYSCON on GMAC1 path | Yanks `eth0` out from under Debian → `wari upgrade` hangs |
| Wrong YT8531C delay value (e.g. `0x680A` from GMAC0) | Reproduces 1% RX symptom on GMAC1 |

All writes to SYS CRG reset/gate registers MUST be RMW with explicit
bitmask. No blind word writes.

## Sources

- `drivers/clk/starfive/clk-starfive-jh7110-sys.c`
- `drivers/clk/starfive/clk-starfive-jh7110-aon.c`
- `drivers/reset/starfive/reset-starfive-jh7110.c`
- `drivers/net/ethernet/stmicro/stmmac/dwmac-starfive.c`
- `drivers/net/phy/motorcomm.c`
- `arch/riscv/boot/dts/starfive/jh7110.dtsi`
- `arch/riscv/boot/dts/starfive/jh7110-starfive-visionfive-2.dtsi`
- `arch/riscv/boot/dts/starfive/jh7110-starfive-visionfive-2-v1.3b.dts`
- `include/dt-bindings/clock/starfive,jh7110-crg.h`
