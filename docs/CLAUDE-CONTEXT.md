# Claude session context — pickup brief

> **For Claude on macOS continuing this work.** Read this AFTER `CLAUDE.md`
> (project rules) and `docs/STATE-OF-PLAY.md` (current build state).
>
> Distilled from the May 12-14 2026 Phase-1c silicon bring-up session.

## Operator profile

- **Name**: Gustavo Delgadillo (gustavo.delgadillo@gmail.com)
- **Goal**: World-class, formally-verifiable, WASM-native RISC-V OS for
  sovereign cloud infrastructure in Latin America. See `CLAUDE.md` §
  Mission for the long version.
- **Style preferences** (learned this session):
  - Terse over verbose. He asks "do X" — do X, don't restate.
  - Tables and labeled lists land better than walls of prose.
  - Says "yeah" / "go ahead" / "ok" to greenlight; means proceed.
  - Says "stop" / "pause" / "hardstop" — full stop, no extra work.
  - Hates being asked questions he can answer himself; loves being
    shown verified facts.
  - Switches devices and contexts (Windows ↔ macOS, dev ↔ client work).
    Build artifacts and state-of-play docs must work across both.

## What Wari is right now (build 121, May 14 2026)

Phase 1c: bringing up the JH7110 GMAC0 on a StarFive VisionFive 2.
Boot → smoltcp comes up on `192.168.50.10/24` → ARP/ICMP should work.

**Build 121 contains the fix that hasn't been silicon-tested yet:**
- YT8531C PHY extended-register `0xA003 ← 0x680A` (RGMII RX/TX delay +
  TX_CLK_SEL_INVERTED, matching mainline VF2 v1.3 device tree).
- MAC_CONFIG full-duplex bit set (`0x3 → 0x2003`).
- Forces fresh AN cycle when `0xA003` changes (YT8531C latches RXC delay
  at link-up).

If this works, ping succeeds and we move to **Net-6c** (TCP data path).

## The architecture in 60 seconds

```
[U-Boot] → [Wari kernel (Rust, riscv64, no_std)]
                                     ↓
                              wasmi 0.32.3 (pure interpreter)
                                     ↓
                  ┌─────────────────────────────────────┐
                  │  Tier-2 drivers (signed WASM):       │
                  │    - wari-driver-uart  (UART)        │
                  │    - wari-driver-net   (GMAC0/smoltcp)│
                  └─────────────────────────────────────┘
                                     ↓
                              Tier-1 apps (WASM): hello, …
```

**Critical**: the Tier-2 net driver is a separate `cdylib` crate compiled
to `wasm32-unknown-unknown`, signed, then `include_bytes!`'d into the
kernel. Driver-side bugs and build cache bugs are SEPARATE from kernel-
side ones. See `kernel/build.rs` for the stale-driver guard.

## Traps we've already fallen into (don't repeat)

### 1. Stale driver wasm
Builds 107-114 silently ran the build-106 driver because:
- I added `core::arch::asm!("fence ow,ow")` in driver code (RISC-V CPU instruction)
- That's **unstable on `wasm32-unknown-unknown`**
- The driver wasm build failed silently
- Cargo reused the last-known-good `.wasm` artifact
- The kernel banner read "build 114" while running build-106 driver code

**Mitigations now in place:**
- `drivers/net/build.rs` embeds `WARI-DRV-BUILD-TAG-N` rodata string
- `kernel/build.rs` greps the embedded signed wasm and refuses to compile
  on tag mismatch
- `make verify` is the operator-visible end-to-end check (~1 sec)

**Lesson:** never use `core::arch::asm!` in driver code. Host-fn MMIO
calls (`wari_net_mmio_write32`) cross the wasm→native boundary and
naturally serialize — that's the only "fence" we have.

### 2. Bypassing `make`
`cd kernel && cargo build` will happily relink with whatever stale
driver wasm is in `build/drivers/`. **Always use `make kernel-vf2`** —
it rebuilds the driver first, then signs, then builds the kernel.

The `make verify` target will catch this if you forget — it greps the
build tag from every artifact (driver wasm, signed wasm, kernel bin,
.build_number) and refuses if any drift.

### 3. Tag bit collision in diagnostic logging
Don't write `let tag = 0x7258_4672 | (idx & 0xF)` — the base ASCII byte
`0x72` already has bits set, so OR with idx aliases slots 0/2, 1/3, etc.
**Put idx in `val.b3` (top byte), keep the tag constant.** See
`docs/diagnostic-tags.md` for the established convention.

### 4. Per-event log spam
On a hot path (millions of calls/sec), per-event tags drown the UART.
Instead:
- Increment a counter (`vf2_state::C_*`)
- Dump the six counters as a `St**` burst every ~65k receive() calls
- Per-event tags only on actual events (`rXFr` when frame found, not on every poll)

The 6-counter dump in one screenshot tells you which layer is broken —
this is exactly what surfaced the YT8531C bug.

## Conventions Claude must follow when editing

- **Mission > correctness > security > size > convenience.** Phase-1c
  driver code follows R5 (no panics on kernel paths). Use `Result`-style
  early returns.
- **WASM-tier code uses only `wari_*` host-fn calls for MMIO.** No
  direct memory access, no inline asm, no platform intrinsics. Pure
  Rust + the documented host-fn surface.
- **Builds use `WARI_BUILD=N`** env var. Bump `.build_number` after a
  full build cycle. Both `drivers/net/build.rs` and `kernel/build.rs`
  embed it via `env!()`.
- **Match the existing comment style** when adding code. Reference
  Linux mainline source paths in comments where applicable — we
  cross-check against `drivers/net/phy/motorcomm.c` and
  `drivers/net/ethernet/stmicro/stmmac/dwmac-starfive.c` for VF2 work.
- **Use the build tags convention** in commit messages:
  `Build N [vf2]: <one-line summary>` for VF2-specific changes,
  `Build N: <summary>` for cross-platform.

## Diagnostic tags Claude should know

See `docs/diagnostic-tags.md` for the full table. The seven that matter:

| Tag | ASCII | When | What |
|---|---|---|---|
| `0x7258_4672` | `rXFr` | DMA delivered a frame | `(idx<<24) \| rdes3` |
| `0x7258_4365` | `rXCe` | smoltcp called consume() | slot idx |
| `0x7258_4472` | `rXDr` | Drop fired on RxToken | slot idx |
| `0x7258_434E` | `rXCn` | Descriptor re-armed | slot idx |
| `0x7258_546C` | `rXTl` | RX_TAIL doorbell kicked | tail PA |
| `0x7458_5472` | `tXTx` | smoltcp transmitted | `(idx<<24) \| len` |
| `0x5374_5263..78` | `St**` | Periodic 6-counter dump | counter value |

The `St**` family is the killer feature. One screenshot answers:
- Is the kernel polling? (StRc growing)
- Are frames reaching the DMA ring? (StRf)
- Is smoltcp consuming? (StCc)
- Is Drop running? (StDc)
- Are descriptors recycling? (StRa)
- Did smoltcp transmit? (StTx)

If StRf=0 in millions of polls, the bug is below smoltcp (PHY/MAC/DMA).
If StRf>0 but StCc=0, the bug is in smoltcp dispatch.
If StCc>0 but StTx=0, the bug is in TX path or smoltcp can't generate a reply.

## Where the silicon test happens

OpenWrt spare router (already configured) on `192.168.50.0/24`:

```
[OpenWrt @ 192.168.50.1, WAN unplugged, Wi-Fi off, DHCP off]
    │
    ├─ LAN port → VF2 eth0 (end0) — Wari listens on 192.168.50.10
    └─ LAN port → laptop USB-Eth — 192.168.50.4
```

Tcpdump on OpenWrt: `tcpdump -ni br-lan arp or icmp` (already installed).

Pass criterion for build 121:
- COM7 shows `[net:drv] tag=0x52433170 val=0x0000680a` (RC1p post-write
  = 0x680A — proves PHY write landed)
- StRf counter grows ≥1/sec under 1Hz ping
- tcpdump shows `ARP Reply 192.168.50.10 is-at 6c:cf:39:00:40:84`
- Ping replies arrive

## What's NOT done after build 121 (in order of priority)

1. **Silicon test of build 121** — first thing to do on macOS
2. If 121 doesn't fix it: alternate `0xA003` values listed in `docs/STATE-OF-PLAY.md`
3. **Net-6c** — TCP data path (smoltcp socket plumbing for Tier-1)
4. **Net-6d** — echo demo (Tier-1 binds port 7000, accepts, echoes)
5. **JSON-over-HTTP demo** — the Phase-1c north star
6. **Kani harnesses** — `kernel/src/validate.rs` is the safest first target
7. **GMAC1 bring-up** — Phase-1c-10+, needed if dual-NIC scenarios matter

## Files that capture the journey

- `docs/STATE-OF-PLAY.md` — current build state + pickup steps (read first)
- `docs/diagnostic-tags.md` — every tag, ASCII, what it means
- `docs/phase-1c-status.md` — register cheat-sheet, what works on silicon
- `docs/phase-1c-9-plan.md` — the audit that produced build 121
- `scripts/wari-trace-decode.sh` — pipe COM7 paste for human-readable output
- `scripts/wari-upgrade.sh` — the `wari` shell function for VF2 SSH

## Communication style for this user

When something works: state it plainly with the evidence. No emoji.
When something fails: name the bug, show the data, propose one or two
ranked fixes. Don't enumerate every possibility — pick the most likely
and say why.

When the user pivots ("ok pause", "switch to X", "do all that") —
believe him, finish cleanly, push, summarize state, stop.

The user has earned credit by working through 14+ builds today. Treat
him as a senior collaborator: assume he's read the docs, knows the
hardware, and just wants the next concrete step.
