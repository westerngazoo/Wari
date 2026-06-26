# Diagnostic Tag Registry

Every `[net:drv] tag=0xXXXXXXXX val=0xYYYYYYYY` line on COM7 comes from a
`wari_drv_log_u32(tag, val)` host-fn call. This file is the single source
of truth for what each tag means.

## How to read a tag

Tags are 32-bit constants chosen to look like ASCII in `xxd`. Decode big-
endian: `0x7258_4672` = `r X F r` = "rXFr". The associated `val` carries
the runtime data (slot index, register value, counter, etc.).

For traces with many tags, use `scripts/wari-trace-decode.sh`:

```
$ pbpaste | scripts/wari-trace-decode.sh
[rXFr] frame received        slot=2 rdes3=0x30010202 (len=514, FD+LD)
[rXCe] consume entered       slot=2
[rRaE] rearm entered         slot=2
[rXCn] descriptor rearmed    slot=2
[rXTl] tail doorbell kicked  pa=0x40620100
[rRaX] rearm exited          slot=2
[rXDr] drop fired            slot=0xFFFFFFFF (already-consumed)
```

## Tag conventions (build 118+)

- **Idx in val, not in tag.** The base tag identifies the EVENT; the val
  carries the slot index in `val >> 24` (top byte) plus any payload in
  the lower 24 bits. Older builds OR'd idx into the low nibble of the
  tag, which collided when the ASCII base byte already had bits set
  (e.g. `0x72 | 2 == 0x72`). New tags keep the ASCII bytes fixed.
- **Counter stats logged once per ~65536 receive() calls** under the
  `St**` tag family so we get periodic visibility into hot-path
  health without flooding.

## Event tags (drivers/net/src/lib.rs)

| Tag hex | ASCII | Source | Fires when | val |
|---|---|---|---|---|
| `0x7258_4672` | `rXFr` | receive() finds OWN=0 | New frame yielded to smoltcp | `(idx<<24) \| (rdes3 & 0xFFFFFF)` |
| `0x7258_4365` | `rXCe` | RxToken::consume entry | smoltcp called consume() | slot idx |
| `0x7258_4472` | `rXDr` | RxToken::drop entry | Rust dropped the token | slot idx or `0xFFFFFFFF` if already-consumed |
| `0x7258_434E` | `rXCn` | vf2_rx_rearm | Descriptor re-armed (OWN \| IOC \| BUF1V) | slot idx |
| `0x7258_546C` | `rXTl` | vf2_rx_rearm | RX_TAIL doorbell kicked | tail PA |
| `0x6450_7262` | `dPrb` | receive() change-detection probe | PREV_YIELDED's value flipped (logged once per flip) | PREV_YIELDED |
| `0x7458_5472` | `tXTx` | TxToken::consume | smoltcp sent a frame | `(idx<<24) \| len` |

Build 119 trimmed the per-step saturation tags `rRaE`/`rRaB`/`rRaW`/`rRaX`/`dPyR`
that we added during the stale-driver hunt. The counters in `St**` subsume
them — if you need step-level breakpoints back, add a new tag for the
specific failure you're chasing rather than reviving the kitchen-sink set.

## Build 129 — net-diag snapshot tags (`N*` family)

Gated behind the `net-diag` cargo feature. Default-off for production
builds. The `'N'` prefix is reserved exclusively for this family — no
other tag in the registry leads with `'N'` (`0x4E`).

## Build 130 — PHY address indicator

| Tag hex | ASCII | Source | Fires when | val |
|---|---|---|---|---|
| `0x5061_4472` | `PaDr` | `driver_start` PHY init | Once at boot, before first PHY register read | `plat::PHY_ADDR` (`0` for GMAC0, `1` for GMAC1). If subsequent PHYID reads come back `0xFFFF`, the MDIO transaction is going to a dead address. |

### Lifecycle markers

| Tag hex | ASCII | Source | Fires when |
|---|---|---|---|
| `0x4E64_6742` | `NdgB` | `diag::boot_dump` | Boot-time deep dump start. val = `0xB007_0000`. Trailer with same tag + val `0xB007_FFFF` marks dump end. |
| `0x4E64_6753` | `NdgS` | `diag::maybe_snapshot` | Periodic snapshot header. val = snapshot counter (`0`, `1`, `2`, ...). |
| `0x4E64_6746` | `NdgF` | `diag::note_first_frame` | First OWN=0 transition. val = slot idx where frame landed. Fires exactly once per boot. |

### MAC layer (`NM_*`)

| Tag hex | ASCII | Register | Reads from |
|---|---|---|---|
| `0x4E4D_5F43` | `NM_C` | MAC_CONFIGURATION | `GMAC_BASE + 0x0000` |
| `0x4E4D_5F46` | `NM_F` | MAC_PACKET_FILTER | `GMAC_BASE + 0x0008` |
| `0x4E4D_5F44` | `NM_D` | MAC_DEBUG | `GMAC_BASE + 0x0114` |
| `0x4E4D_5F50` | `NM_P` | MAC_PHYIF_CTRL_STATUS | `GMAC_BASE + 0x00F8` |
| `0x4E4D_5F56` | `NM_V` | MAC_VERSION | `GMAC_BASE + 0x0110` (boot dump only) |

### MMC RX counters (`Nm**`)

| Tag hex | ASCII | Register | Diagnostic value |
|---|---|---|---|
| `0x4E6D_4742` | `NmGB` | MMC_RX_FRAMECOUNT_GB | Total RX frames hitting MAC. **Stuck at 0 = PHY blocking** |
| `0x4E6D_475F` | `NmG_` | MMC_RX_FRAMECOUNT_G | Good frames only |
| `0x4E6D_4372` | `NmCr` | MMC_RX_CRC_ERROR | **Climbs in lockstep with GB = RGMII timing wrong** |
| `0x4E6D_416C` | `NmAl` | MMC_RX_ALIGN_ERROR | PHY/MAC framing skew |
| `0x4E6D_4C65` | `NmLe` | MMC_RX_LENGTH_ERROR | Length field bad |
| `0x4E6D_466F` | `NmFo` | MMC_RX_FIFO_OVERFLOW | RXQ too small or MTL backed up |

### MTL layer (`NT_*`)

| Tag hex | ASCII | Register | Diagnostic value |
|---|---|---|---|
| `0x4E54_5F4F` | `NT_O` | MTL_RXQ0_OP_MODE | Should match init (RSF, RQS=7) |
| `0x4E54_5F4D` | `NT_M` | MTL_RXQ0_MISSED | **Non-zero = MAC accepted frames, MTL dropped them** |
| `0x4E54_5F44` | `NT_D` | MTL_RXQ0_DEBUG | Bits 5:4 PRXQ = FIFO fill. Stuck non-zero = DMA stalled |

### DMA layer (`ND_*`)

| Tag hex | ASCII | Register | Diagnostic value |
|---|---|---|---|
| `0x4E44_5F52` | `ND_R` | DMA_CH0_RX_CONTROL | Bit 0 SR must = 1 |
| `0x4E44_5F43` | `ND_C` | DMA_CH0_CUR_RXDESC | **Δ across snapshots = engine running.** Stuck = no frames OR RBU |
| `0x4E44_5F42` | `ND_B` | DMA_CH0_CUR_RXBUF | Buffer PA being filled |
| `0x4E44_5F53` | `ND_S` | DMA_CH0_STATUS | Bit 7 RBU = ring full; bits 8:5 RPS = engine state |

### Reading a snapshot — diagnosis table

| Pattern in trace | Layer blocking frames |
|---|---|
| `NM_P` link-up bit stays 0 across snapshots | **PHY** — no link / AN didn't complete |
| Link up, but `NmGB` stays at 0 for many snapshots | **MAC** — RE bit cleared, or DA filter rejecting (check `NM_F` PR=1) |
| `NmGB` climbs, `NmCr` tracks 1:1 with it | **RGMII timing** — re-tune YT8531C ext-reg `0xA003` |
| `NmGB` > 0, `NmCr` = 0, `NT_M` climbs | **MTL** — RXQ dropping |
| `NT_M` = 0, `ND_S` bit 7 (RBU) = 1 | **driver** — descriptor re-arm too slow |
| `ND_C` advancing but `StRf` still 0 | **descriptor handoff bug** — RX_NEXT wrong |

## Counter stats (build 118+)

Emitted as a six-line burst every ~65536 receive() calls. If `St**` lines
appear but, say, `StCc=0` after 30 seconds of ping, you know smoltcp is
NOT calling consume — without per-event log spam.

| Tag hex | ASCII | Counter |
|---|---|---|
| `0x5374_5263` | `StRc` | `receive()` call count |
| `0x5374_5266` | `StRf` | rXFr frames-found count |
| `0x5374_4363` | `StCc` | consume() call count |
| `0x5374_4463` | `StDc` | drop() call count |
| `0x5374_5261` | `StRa` | vf2_rx_rearm() call count |
| `0x5374_5478` | `StTx` | TX frames sent count |

## Build / lifecycle tags

| Tag hex | ASCII | Meaning |
|---|---|---|
| `0x57415_200` | `WARI` boot beacon | Driver `_start` running |
| Various `0x57...` | First-letter `W` family | Init register dumps (one-shot at boot) |

## Adding a new tag

1. Pick a 4-byte ASCII string that's free in this registry.
2. Make sure each byte is non-zero (so the tag survives in `strings(1)`).
3. **Don't** embed slot index in the tag via bitwise OR — put it in val.
4. Add a row to the table above with file:line, fires-when, val schema.
5. If it's a per-event log on a hot path, increment a counter instead
   and rely on the `St**` periodic dump. Per-event logs are for milestones
   (boot init, one-shot diagnostics) not per-packet hot paths.

## Lessons learned

### Stale driver wasm (builds 107–114, May 2026)

Symptom: kernel banner reads "build 114" but new diagnostic tags I just
added in `drivers/net/src/lib.rs` never appeared on COM7.

Root cause: I had added `core::arch::asm!("fence ow,ow")` to driver code
in build 107. That's a RISC-V CPU instruction. The driver compiles to
`wasm32-unknown-unknown`, where inline asm is unstable — the wasm build
silently failed. Cargo kept reusing the last-known-good `wari_driver_net.wasm`
from build 106 while `cd kernel && cargo build` happily relinked the
kernel with that stale blob.

Fix (build 116): added a `WARI-DRV-BUILD-TAG-N` rodata string to the
driver via `concat!("WARI-DRV-BUILD-TAG-", env!("WARI_BUILD"))` and a
`kernel/build.rs` guard that greps the embedded signed wasm for that
string and refuses to compile if N != current `WARI_BUILD`. Also added
`make verify` for operator-visible end-to-end coherence check.

### Tag bit-collision

`let tag = 0x7258_4672 | (idx & 0xF)` aliases idx 0/2, 1/3, 4/6, 5/7,
8/10, 9/11, 12/14, 13/15 because the ASCII byte `0x72` already has bits
1, 4, 5, 6 set. We lost 20 minutes pretending we saw 8 distinct slots
when half of them were the same slots logging as a different tag.

Fix (build 118): put idx in `val >> 24`, leave tag constant.
