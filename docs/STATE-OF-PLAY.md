# State of Play — pick up here on macOS

> **Last updated**: 2026-05-14
> **Last build shipped**: 121
> **Last build flashed on VF2**: 120 (build 121 not yet flashed)
> **Working on**: Phase 1c-9 — YT8531C RGMII delay calibration

## Quick context for a fresh clone

```bash
git clone https://github.com/westerngazoo/Wari.git wari
cd wari
make verify    # confirms tree=121 across all four artifacts
```

If `make verify` reports OK, your local checkout is coherent. If not,
something is stale and `make kernel-vf2` will rebuild everything.

## Where we are

We've been bringing up real silicon (StarFive VisionFive 2, JH7110)
through Phase 1c. Wari boots, the GMAC0 driver loads, smoltcp comes up
on `192.168.50.10/24`, but ARP replies are ~1/118 reliable. Every
other layer is verified working:

- ✅ Kernel boot + WASM driver embedded with build tag (`make verify` enforces)
- ✅ GMAC0 hardware path (Debian on the same cable replies to ping perfectly)
- ✅ Wari's receive() / consume() / Drop / re-arm chain (counters confirm)
- ✅ Stale-driver guard (`kernel/build.rs` + `make verify`)
- ✅ Diagnostic counter overhaul (St** tags — one screenshot tells you which layer breaks)
- ❓ **YT8531C PHY RGMII delay calibration** — the fix that build 121 contains, not yet tested

## What build 121 changed

Two surgical fixes to `drivers/net/src/lib.rs` based on the
`docs/phase-1c-9-plan.md` audit:

1. **YT8531C extended-register `0xA003` ← `0x680A`** at PHY init
   (right after PHY ID read, before AN restart). Sets RX delay = 1500 ps,
   TX delay = 1500 ps, TX_CLK_SEL_INVERTED — matches mainline VF2 v1.3
   device tree.
2. **MAC_CONFIG full-duplex bit (DM=1)** — was `0x3`, now `0x2003`.
3. **Forces fresh AN cycle** whenever `0xA003` changes, because YT8531C
   latches RXC delay at link-up (only takes effect on next link cycle).

Two new diagnostic tags appear in COM7:

| Tag | ASCII | val | Meaning |
|---|---|---|---|
| `0x5243_3152` | `RC1R` | `0x____` | 0xA003 pre-write (whatever U-Boot left) |
| `0x5243_3170` | `RC1p` | should be `0x0000_680A` | 0xA003 post-write — pass if matches |

## What to do on macOS, in order

### 1. Sanity check the clone

```bash
git clone https://github.com/westerngazoo/Wari.git wari
cd wari
make verify
```

You should see all four tags = 121.

### 2. Test in QEMU (no silicon, ~30s)

```bash
make run
```

Boot Wari in QEMU. Confirms the build 121 changes didn't regress anything
on the qemu path. Exit with `Ctrl-A` then `X`. The qemu driver doesn't
exercise the YT8531C code (cfg-gated to vf2 only) but it does exercise
the kernel/driver embed + smoltcp init.

### 3. Flash on VF2 and test

```
# On VF2 via SSH:
wari upgrade        # pulls 121 from origin, verifies, flashes
wari status         # confirm embedded-build = 121
wari go -y
```

### 4. Run the spare-router L2 test

The OpenWrt spare router is already configured. Topology:

```
[OpenWrt @ 192.168.50.1] -- LAN port --> [VF2 eth0 (end0) @ 192.168.50.10]
                       \-- LAN port --> [Laptop USB-Ethernet @ 192.168.50.4]
```

On the laptop (macOS now — adjust syntax):

```bash
# Set static IP on USB-Ethernet adapter
sudo ifconfig en7 192.168.50.4/24   # replace en7 with actual adapter
# Test
arp -d -a
ping 192.168.50.10
```

On the OpenWrt router (already has tcpdump installed):

```bash
ssh root@192.168.50.1
tcpdump -ni br-lan arp or icmp
```

### 5. Read the result

**Pass scenario** — what we expect:

COM7:
```
[net:drv] tag=0x52433152 val=0x????????   <- whatever U-Boot left
[net:drv] tag=0x52433170 val=0x0000680a   <- our value, post-write
...
[net] smoltcp interface up, listening on 192.168.50.10/24
...
[net:drv] tag=0x53745266 val=0x000000XX   <- StRf climbing
[net:drv] tag=0x53745478 val=0x000000YY   <- StTx climbing
```

tcpdump:
```
ARP, Request who-has 192.168.50.10 tell 192.168.50.4, length 28
ARP, Reply 192.168.50.10 is-at 6c:cf:39:00:40:84, length 28
IP 192.168.50.4 > 192.168.50.10: ICMP echo request ...
IP 192.168.50.10 > 192.168.50.4: ICMP echo reply ...
```

Ping:
```
Reply from 192.168.50.10: bytes=32 time=Xms TTL=64
```

**Fail scenario A** — `RC1p ≠ 0x680A`: the write didn't land. Means the
MDIO write protocol is wrong or the page-select is being undone between
the page-select and the data write. Look at `mdio_write_phy` (line ~1775)
and verify it can write to reg 0x1F (page-data register).

**Fail scenario B** — `RC1p = 0x680A` but `StRf` still ~0: the delay
value is wrong for THIS specific board. Try alternatives:
- `0x4806` (RX=8 = 1200 ps, GE_TX=6 = 900 ps — Linux's "RGMII_TXID" preset)
- `0x4801` (RX=8 = 1200 ps, GE_TX=1 = 150 ps — minimum TX skew)
- `0x000A` (no inversion + just RX delay)

**Fail scenario C** — RX works but TX doesn't (ARPs come in, replies
don't come out): consider Phase-1c-10 — flip AONCRG +0x14 from
mux=0 (gtxclk) to mux=1 (rmii_rtx) to use PHY-sourced TX clock.

## Useful references already in the repo

| File | Purpose |
|---|---|
| `docs/phase-1c-status.md` | Full register cheat-sheet, what works on silicon today |
| `docs/phase-1c-9-plan.md` | The audit that produced build 121 |
| `docs/diagnostic-tags.md` | Every COM7 tag, ASCII name, where it fires |
| `scripts/wari-trace-decode.sh` | Pipe COM7 paste through this for human-readable output |
| `scripts/wari-upgrade.sh` | The `wari` shell function on the VF2 |
| `Makefile` | `make verify`, `make kernel-vf2`, `make run` |

## macOS-specific gotchas to watch for

- `llvm-objcopy` lives at `$(rustup show home)/toolchains/<toolchain>/lib/rustlib/<host>/bin/llvm-objcopy`.
  The Makefile's `find` should handle this but verify with `make verify` first.
- Line-endings: the repo is already in LF (we've been editing from
  Windows-PowerShell-but-Git-Bash). Should be clean.
- The `scripts/wari-trace-decode.sh` is bash; works on macOS unchanged.

## What's NOT done

- Build 121 not flashed on the VF2 yet (still on 120)
- The actual silicon test of the YT8531C fix
- If 121 fixes RX, then we move to Net-6c (TCP data path) and the
  echo demo. That's where the work goes next.

## Past lessons that matter

1. **Never run `cd kernel && cargo build` alone** — it embeds the
   last-known-good driver wasm regardless of source changes. Use
   `make kernel-vf2` (rebuilds driver first). The build.rs guard
   will refuse to compile if the embedded driver tag doesn't match
   WARI_BUILD, but this only catches *some* cases — `make verify`
   is the operator-visible safety net.

2. **`core::arch::asm!` is unstable on `wasm32-unknown-unknown`.**
   That's how builds 107-114 silently shipped a stale driver. The
   driver compiles to WASM; only pure Rust + host-fn calls work.

3. **Counter dump (`St**` tags) tells you the answer in one
   screenshot.** When debugging, look at those first before diving
   into per-event tags.
