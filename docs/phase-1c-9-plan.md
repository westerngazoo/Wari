# Phase 1c-9 — YT8531C RGMII delay calibration (planned)

> **Status**: not yet applied to source. Ready to land as build 121 on
> the next coding session. Authored 2026-05-13 after the spare-router
> L2 test confirmed Wari's GMAC0 receives ~1 frame per 100+ ARP attempts
> while Debian on the same cable/port receives all frames perfectly.

## Diagnosis

**Root cause: missing YT8531C extended-register `0xA003` write.**

Linux mainline `jh7110-starfive-visionfive-2-v1.3b.dts` configures the
VF2 rev 1.3 PHY with:

```dts
rx-internal-delay-ps = <1500>;
tx-internal-delay-ps = <1500>;
motorcomm,tx-clk-adj-enabled;
motorcomm,tx-clk-1000-inverted;
```

Wari's PHY init at `drivers/net/src/lib.rs:~1963` writes only PHY reg 0
(AN restart) — it never touches the YT8531C extended page where RGMII
clock-edge skew lives. U-Boot leaves the PHY in whatever state the
strap-pins set; most of the time the RXC sample point lands in the
data-bit transition zone → CRC fail → MAC silently drops every frame.

The 1/118 ping success pattern is the signature of RGMII timing margin:
occasionally a frame slips through with valid CRC; the rest die at the
MAC.

## Patch (to insert ~line 1937, after the PHY ID read, before AN restart)

```rust
// PR Phase-1c-9 — YT8531C extended-register RGMII delay config.
//
// VF2 rev 1.3+ mainline DT: rx-internal-delay-ps = 1500,
// tx-internal-delay-ps = 1500, tx-clk-1000-inverted.
//
// Extended-register protocol (motorcomm.c ytphy_write_ext):
//   1. write PHY reg 0x1E (PAGE_SELECT) = extended-reg addr
//   2. write/read PHY reg 0x1F (PAGE_DATA)
//
// YT8521_RGMII_CONFIG1_REG = 0xA003:
//   bit  14    TX_CLK_SEL_INVERTED  (set: tx-clk-1000-inverted)
//   bits 13:10 RX_DELAY             (4-bit, 150 ps/step)
//   bits  7:4  FE_TX_DELAY (100M)   (4-bit, 150 ps/step)
//   bits  3:0  GE_TX_DELAY (1G)     (4-bit, 150 ps/step)
//
// 1500 ps / 150 ps = 10 = 0x0A.
// Final value: (1<<14) | (0x0A<<10) | (0x0A<<0) = 0x680A.

const YTPHY_PAGE_SELECT:        u32 = 0x1E;
const YTPHY_PAGE_DATA:          u32 = 0x1F;
const YT8521_RGMII_CONFIG1_REG: u16 = 0xA003;
const YT8531_RC1R_VF2_VALUE:    u16 = 0x680A; // INV | RX=10 | GE_TX=10

// Pre-read so we know U-Boot's starting value.
let _ = mdio_write_phy(plat::NIC_BASE, 0, YTPHY_PAGE_SELECT,
                       YT8521_RGMII_CONFIG1_REG);
let rc1r_pre = mdio_read_phy(plat::NIC_BASE, 0, YTPHY_PAGE_DATA);
let _ = unsafe { wari_drv_log_u32(0x5243_3152, rc1r_pre) }; // 'RC1R'

// Write delays + TX clock inversion.
let _ = mdio_write_phy(plat::NIC_BASE, 0, YTPHY_PAGE_SELECT,
                       YT8521_RGMII_CONFIG1_REG);
let _ = mdio_write_phy(plat::NIC_BASE, 0, YTPHY_PAGE_DATA,
                       YT8531_RC1R_VF2_VALUE as u32);

// Verify-read.
let _ = mdio_write_phy(plat::NIC_BASE, 0, YTPHY_PAGE_SELECT,
                       YT8521_RGMII_CONFIG1_REG);
let rc1r_post = mdio_read_phy(plat::NIC_BASE, 0, YTPHY_PAGE_DATA);
let _ = unsafe { wari_drv_log_u32(0x5243_3170, rc1r_post) }; // 'RC1p'

// Force re-AN if we changed 0xA003 — YT8531C latches RXC delay
// at link-up, so the new delay only takes effect on next link cycle.
let needs_relink = (rc1r_pre as u32) != (YT8531_RC1R_VF2_VALUE as u32);
```

Then patch the `already_linked` line at ~1976-77:

```rust
// Old:
let already_linked = (bs_pre & BS_LINK_UP) != 0
                  && (bs_pre & BS_AN_COMPLETE) != 0;
// New:
let already_linked = !needs_relink
                  && (bs_pre & BS_LINK_UP) != 0
                  && (bs_pre & BS_AN_COMPLETE) != 0;
```

## Secondary fixes worth same patch round

- **MAC_CONFIG missing duplex bit.** Currently `0x3` (TE | RE), should
  be `0x2003` (DM=1 full-duplex) at 1 Gbps. Single bit, low risk.
  Update at the MAC_CONFIG write site (~line near `0x000 <- 0x00000003`).

- **NOT** TX clock muxing (AONCRG +0x14 mux=0 → mux=1). Defer to
  Phase 1c-10 — do the 0xA003 fix first to isolate variables. If
  RX flakes after that, revisit.

- **NOT** pad drive strength (0xA010). Only attempt if `RC1p = 0x680A`
  confirms but RX still flakes.

## Validation plan

COM7 output should show:

| Tag | ASCII | Expected | Meaning |
|---|---|---|---|
| `0x5243_3152` | `RC1R` | varies | 0xA003 pre-write value |
| `0x5243_3170` | `RC1p` | **`0x680A`** | 0xA003 post-write (must match) |
| `StRf` | frames found | **≥10/sec under ping** | recovery vs the ~0/30s baseline |

Pass criterion: `StRf` increments at least once per second while Windows
pings the VF2 at 1 Hz over the spare OpenWrt router, AND tcpdump shows
`ARP Reply 192.168.50.10 is-at 6c:cf:39:00:40:84`.

## Risk

- Wrong code in `0xA003` (e.g. swapped nibbles) could drop from 1% → 0%
  success. Patch is verified against `GENMASK(13,10)` and `GENMASK(3,0)`
  in upstream `drivers/net/phy/motorcomm.c`.
- Bit 14 affects TX only; if wrong, breaks outgoing ARPs but not RX.
- Do **not** also write `0xA000` (keep-pll / sleep) in this patch.
- Do **not** touch `0xA010` (pad drive) — default is adequate.

## Sources

- `drivers/net/phy/motorcomm.c` — YT8521/YT8531 driver
- `drivers/net/ethernet/stmicro/stmmac/dwmac-starfive.c` — JH7110 glue
- `arch/riscv/boot/dts/starfive/jh7110-starfive-visionfive-2-v1.3b.dts`
