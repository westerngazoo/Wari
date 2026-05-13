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

| Tag hex | ASCII | Source line | Fires when | val |
|---|---|---|---|---|
| `0x7258_4672` | `rXFr` | receive() finds OWN=0 | A new frame is yielded to smoltcp | `(idx<<24) \| (rdes3 & 0xFFFFFF)` |
| `0x7258_4365` | `rXCe` | RxToken::consume entry | smoltcp processed the frame | slot idx |
| `0x7258_4472` | `rXDr` | RxToken::drop entry | Rust dropped the token | slot idx or `0xFFFFFFFF` if already consumed |
| `0x7258_434E` | `rXCn` | vf2_rx_rearm log | Descriptor was re-armed (OWN \| IOC \| BUF1V) | slot idx |
| `0x7258_546C` | `rXTl` | vf2_rx_rearm doorbell | RX_TAIL register kicked | tail PA |
| `0x7252_6145` | `rRaE` | vf2_rx_rearm enter | Rearm function called | slot idx |
| `0x7252_6142` | `rRaB` | vf2_rx_rearm after addr math | Buffer PA computed | bp low 32 |
| `0x7252_6157` | `rRaW` | vf2_rx_rearm after desc write | Descriptor words written | slot idx |
| `0x7252_6158` | `rRaX` | vf2_rx_rearm exit | Function completed | slot idx |
| `0x6450_7952` | `dPyR` | receive() prev-yielded path | About to rearm previously-yielded slot | prev idx |
| `0x6450_7262` | `dPrb` | receive() probe | Periodic snapshot of PREV_YIELDED (throttled) | PREV_YIELDED value |
| `0x7458_5472` | `tXTx` | TxToken::consume | smoltcp sent a frame | TDES3 |

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
