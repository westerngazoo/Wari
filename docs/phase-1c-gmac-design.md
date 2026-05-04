# Phase 1c — JH7110 GMAC Driver Design

> **Status**: design draft v1, May 2026.
> **Scope**: bring up `eth0` on the StarFive VisionFive 2 (JH7110
> SoC) so a Tier-1 tenant can open a TCP socket via the existing
> Net-6 surface and exchange packets with the local LAN.
> **Author**: Wari co-architect.
> **Hardware target**: StarFive VisionFive 2 rev. 1.3+, JH7110 SoC,
> GMAC0 at `0x1603_0000`, IRQ 7 (PLIC).
>
> This is NOT a port of Linux's `stmmac` driver — it is a fresh
> minimal implementation against the same hardware, sized for the
> Wari trust boundary (Tier-2 signed WASM driver, ~2-3 KLOC max).

---

## 1 · Why this matters

After Phase 1b + Phase 2 land:

- The Wari Tier-1 socket API (`wari::net_socket_create/bind/listen/
  close`) works end-to-end on QEMU with a virtio-net NIC
- The same Tier-1 binary runs on VF2 silicon but every socket call
  returns `E_INVAL` because the vf2 net driver is a Phase-1c stub
  (`drivers/net/src/lib.rs::driver_socket_*` returns `-2` on vf2)

Phase-1c lifts the stub: replace the `wari_net_mmio_*`-via-VirtIO
plumbing with the JH7110 GMAC equivalents, keep the smoltcp +
socket API on top unchanged. The SAME signed `tier-1 hello` (or
the upcoming Net-6d echo demo) will then exchange packets on real
silicon Ethernet.

This is the first time a Wari tenant talks to a real NIC. It is
the milestone the project has been building toward since Phase 0.

---

## 2 · The hardware

### 2.1 SoC GMAC instance

The JH7110 has two GMAC blocks:

| Block | MMIO base   | IRQ | RGMII pinout         | DTS node          |
|-------|-------------|-----|----------------------|-------------------|
| GMAC0 | `0x1603_0000` | 7 | RGMII-ID, 1 Gb max | `ethernet@16030000` |
| GMAC1 | `0x1604_0000` | 78| RGMII-ID, 1 Gb max | `ethernet@16040000` |

Phase 1c targets **GMAC0 only** — eth0 is the upper RJ45 jack on
the VF2 silkscreen. eth1 stays unbound (Phase 2+ multi-NIC).

### 2.2 IP block

The JH7110 GMAC is a Synopsys DesignWare DWMAC v5.10a. Public
docs: ARM SBSA-equivalent — register set defined in the DWMAC
databook (NDA-only from Synopsys, but the upstream Linux
`drivers/net/ethernet/stmicro/stmmac/dwmac4.h` carries the
canonical offsets we will mirror.

Key register groups (offsets from MMIO base):

- `0x000` — MAC configuration / control
- `0x038` — MAC address high/low
- `0x100` — MMC counters (tx/rx packet counts; useful for debug)
- `0x200` — MTL queue control
- `0x1000` — DMA engine: bus mode, channel-N tx/rx descriptor
              base addrs, current desc pointers, IRQ status

Phase 1c uses one TX channel + one RX channel (DMA channel 0).

### 2.3 PHY

The VF2 board wires a **Motorcomm YT8531C** GbE PHY on RGMII
to GMAC0, MDIO address `0`. PHY init is a small register
sequence (auto-neg + RGMII delay calibration) reachable through
the GMAC's MDIO subblock at offset `0x200..0x208`.

---

## 3 · Trust boundary

Phase 1c stays inside the existing Tier-2 driver mold:

- The driver is a **signed WASM module** at
  `drivers/net/build/net-vf2.signed.wasm`
- It runs in its own WASM linmem
- It declares a **driver manifest** (Phase 2 contract) with kind
  `Net`, ABI version 1, and the same exports as the qemu net
  driver (`_start`, `poll`, `tx_send`, `rx_pop`, `rx_recycle`,
  `socket_create/close/bind/listen` + Net-6c-3 send/recv when
  those land)
- It calls only the kernel host fns the manifest declares
- The kernel's `validate::is_net_mmio_addr` widens to the GMAC0
  window `[0x1603_0000, 0x1604_0000)`

No new trust mechanism — the existing manifest gate covers GMAC0
the same way it covers VirtIO-net.

---

## 4 · The boot sequence

Per DWMAC databook §6 + cross-referenced with Linux `dwmac4_lib.c`:

1. **Soft-reset the DMA**
   - Write `0x1` to `DMA_BUS_MODE` (offset `0x1000`)
   - Poll bit 0 until clear (chip says "reset done")
   - Timeout: 100 ms; abort if not clear

2. **MAC core configuration**
   - Set MAC address via `MAC_ADDRESS_HIGH` (`0x040`) +
     `MAC_ADDRESS_LOW` (`0x044`). The address comes from the
     EEPROM via U-Boot env (`ethaddr`) or, in Phase 1c, a hard-
     coded `02:00:c0:a8:01:0a` (locally-administered, matching
     QEMU's `192.168.122.10`/24 default for parity)
   - Configure `MAC_CONFIG` (`0x000`):
     - bit 0: TE (transmit enable) = 0 until queues set up
     - bit 1: RE (receive enable) = 0 until queues set up
     - bit 13: DM (duplex mode) = 1 (full-duplex; matches PHY
       auto-neg result)
     - bit 14-15: PS (port speed) = 11 (1 Gb/s)

3. **DMA channel-0 setup**
   - Allocate TX descriptor ring (16 entries × 16 bytes = 256 B,
     16-byte aligned)
   - Allocate RX descriptor ring (16 entries × 16 bytes = 256 B)
   - Allocate per-descriptor buffers (16 × 1536 B = 24 KiB for
     each direction; total 48 KiB)
   - All buffers and descriptors live in the driver's WASM linmem
     and are passed to the kernel via `wari_nic_attach_queue` so
     the kernel can hand the GMAC their physical addresses
   - Write descriptor ring base addresses to `DMA_CH0_TX_BASE_ADDR`
     (`0x1114`) + `DMA_CH0_RX_BASE_ADDR` (`0x1118`)
   - Write ring length to `DMA_CH0_TX_RING_LEN` (`0x112C`) +
     `DMA_CH0_RX_RING_LEN` (`0x1130`)

4. **PHY bring-up**
   - Power up the PHY: write `0x1140` (auto-neg + 1000 Mb cap) to
     PHY register 0 via MDIO
   - Wait for link: poll PHY status register 1 bit 2 (link up)
     with 5 s timeout
   - Negotiate: PHY register 4 (auto-neg advertise) — set 100/1000
     full-duplex bits; restart auto-neg
   - On link-up, read PHY register 5 to learn the negotiated
     speed/duplex; reflect into MAC_CONFIG

5. **Enable**
   - Set TE + RE in `MAC_CONFIG`
   - Set ST (start transmit) + SR (start receive) in
     `DMA_CH0_TX_CONTROL` (`0x1104`) + `DMA_CH0_RX_CONTROL` (`0x1108`)
   - The MAC is now up; smoltcp can attach via the existing
     `nic_iface::init` pathway

---

## 5 · Mapping to the existing Tier-2 driver scaffold

Today's `drivers/net/src/lib.rs` has clean cfg-gated platform
modules:

```rust
#[cfg(feature = "qemu")] mod plat { ... }
#[cfg(feature = "vf2")]  mod plat { pub const NIC_BASE: u32 = 0x1603_0000; }
```

Phase 1c grows the vf2 path to mirror the qemu path's structure.
Concretely, the file gains four cfg-gated modules:

- `vf2::regs` — every GMAC offset constant we touch
- `vf2::dma` — descriptor ring layout, init, recycle
- `vf2::phy` — MDIO + auto-neg
- `vf2::driver_start` — orchestrates §4 above

The `nic_iface` (smoltcp) module stays platform-neutral. The
`smoltcp::phy::Device` trait impl currently hard-codes
`SOCKET_BACKING_LEN = 4`; Phase 1c keeps that.

`driver_tx_send` / `driver_rx_pop` / `driver_rx_recycle` get vf2
implementations that move bytes through DMA descriptors instead
of VirtIO virtqueues — same shape, different ring management.

---

## 6 · Host-fn surface

Phase 1c adds **no new** host fns. The existing surface covers
GMAC0:

| Host fn (manifest import) | What it gives the driver |
|---|---|
| `net_mmio_read32`         | GMAC register reads |
| `net_mmio_write32`        | GMAC register writes |
| `nic_set_mac`             | Communicate the MAC address up to the kernel |
| `nic_attach_queue`        | Hand the kernel the linmem offsets of the TX + RX descriptor rings |
| `nic_queue_notify`        | Kick the DMA engine |
| `lin_mem_base`            | Get the linmem physical base for descriptor PA computation |

The kernel-side `validate::is_net_mmio_addr` widens its allowed
window to `[0x1603_0000, 0x1604_0000)` for vf2 builds. The cap
gate (`Net + WRITE`) is unchanged.

---

## 7 · PR sequence

| PR | Title | Scope | LOC |
|---|---|---|---|
| **1c-0 (this doc)** | GMAC driver design draft | Doc only | 350 lines |
| **1c-1** | Validator widening + EEPROM MAC plumbing | `kernel/src/validate.rs` window grows; driver reads MAC from `lin_mem_base`-relative scratch supplied by U-Boot | ~150 |
| **1c-2** | DMA reset + MAC config + register-poke smoke test | `vf2::regs` + `vf2::driver_start` minimum; logs `[net] GMAC reset OK` | ~250 |
| **1c-3** | PHY init + link-up | `vf2::phy` MDIO module; logs `[net] phy link 1000 Mb full` | ~200 |
| **1c-4** | DMA descriptor rings + TX path | `vf2::dma` setup + `driver_tx_send` for vf2 | ~350 |
| **1c-5** | RX path + smoltcp wire-up | `driver_rx_pop` + `driver_rx_recycle` for vf2; nic_iface activates on vf2 | ~250 |
| **1c-6** | First-packet integration test (ARP + ICMP echo) | Tier-1 demo pings the LAN gateway from VF2 silicon | ~150 |
| **1c-7** | Net-6d TCP echo demo | Tier-1 echo server bound to port 7000, connect from a laptop, exchange "ping/pong" | ~200 |

Total: ~1700 LOC across 7 PRs. **Each PR ends in a silicon
test** — no QEMU shortcut for these (QEMU's virt has no GMAC).

### 7.1 Iteration loop

For Phase 1c, the dev loop is:

```
edit -> deploy.bat vf2 "msg" -> wari go on VF2 -> watch COM7
        -> if MMIO trap or hang, reboot, edit, repeat
```

This is slower than QEMU's instant feedback, so each PR keeps the
register-poke surface narrow and uses kprintln liberally on the
GMAC bring-up path. Once data is flowing the chatty logs go
behind a `debug-kernel` cfg.

---

## 8 · Risks + open questions

1. **Cache coherency on RV64**: the JH7110's GMAC DMA accesses
   memory directly. Wari currently doesn't manage CPU caches
   beyond the boot-time fence. We may need explicit
   `fence.i`/`fence rw,rw` around descriptor writes. Defer to
   first-packet tests; if RX works without explicit fences, the
   SiFive cores (U74) on this SoC are likely IO-coherent for the
   GMAC (DT property `dma-coherent`).

2. **PHY init quirks**: the YT8531C has an undocumented bug
   where RGMII delay must be re-calibrated after reset. Linux
   handles this via an out-of-tree YT8531 PHY driver. Phase-1c
   transcribes the magic register sequence from
   `linux/drivers/net/phy/motorcomm.c` into a Wari constant.

3. **Clock + reset domains**: GMAC0's clocks come from the
   StarFive `aon` and `stg` clock controllers (`0x1700_0000`
   and `0x1718_0000`). U-Boot leaves them ENABLED on the VF2
   image, so Phase 1c does not touch them — but if a future
   power-management pass disables them on suspend, this driver
   will need to learn the clock framework. Out of scope here.

4. **Multi-vendor signing**: still deferred per Phase 2 open
   question #2. The vf2 GMAC binary is signed by the same dev
   key as the qemu binary.

5. **GMAC1 (eth1)**: ignored. Phase 2+ multi-NIC.

---

## 9 · Why a Wari driver, not a Linux port

Two reasons:

1. **Trust surface**: `stmmac` is ~30 KLOC. Wari's vf2 GMAC will
   be ~2-3 KLOC. The smaller surface is auditable; the Linux
   driver is a Tier-3 acquired dependency.
2. **Sandbox boundary**: the Wari driver runs in a signed WASM
   sandbox under the manifest contract. Bringing in Linux code
   would either need a cross-compile to wasm32 (large +
   speculative) or breaking the trust boundary entirely. The
   greenfield driver fits the architecture by construction.

The cost is real: Phase-1c bring-up will be 1-2 weeks of
careful silicon iteration. The payoff is the first end-to-end
demo of Wari talking to the world over real hardware.
