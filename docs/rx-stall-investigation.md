# VF2 GMAC RX-DMA stall — investigation report

**Status:** open. Root *trigger* not yet confirmed; the *permanence* is
confirmed and independently fixable. Build 155 (quiet-console, off
`main`), `ping -c 80 192.168.50.10` → ~30% loss, intermittent (0–57%
boot-to-boot on byte-identical code).

## 1. Symptom

The MAC receives frames for a while, then RX **stops permanently**.
Clean frames, no corruption, no recovery. Intermittent per boot.

## 2. Telemetry (build 155, from the driver's stall snapshot ×21)

| tag | register | value | reading |
|---|---|---|---|
| `MRxF` | MMC_RX_FRAMECOUNT_GB `0x0780` | `9→23→35→39` then **frozen at 39** | MAC stopped completing receptions |
| `MRxC` | MMC_RX_CRC_ERROR `0x0794` | **0** throughout | no corruption — physical layer clean |
| `DSts` | DMA_CH0_STATUS `0x1160` | `0x8444` (NIS+RI+ETI+TBU; **RBU bit7 CLEAR**; no AIS/FBE) | RX side quiet, no ring-overflow flag |
| `ROwn` | current RDES3 | `0xc1000000` (**OWN=1**) | descriptor armed, waiting for a writeback that never comes |

## 3. Ruled out

- **Physical / RGMII margin / cabling** — `MRxC` (CRC errors) = 0. The
  frames that arrived were clean.
- **Console-trace flood** — build 155 gated the flood behind `net-diag`;
  console quiet, still 30%. (The quiet fix is real and worth keeping —
  it made this diagnosis possible.)
- **Pure read-side stale-`OWN`** (LLVM hoisting the OWN load) — the
  descriptor accessors are already `volatile` (`desc_rd`/`desc_wr`,
  lib.rs:419-432), and that mechanism predicts `MRxF` *climbing*, not
  frozen.

## 4. The reconciliation the first pass missed

The obvious hypothesis is the INV-25 store→doorbell ordering race, fixed
by a `fence` before the doorbell. **That fence already exists** —
`kernel/src/runtime/host_fns.rs:305` (`fence w,o`, landed in PR #84), on
`main`, so **build 155 was built with it** and the driver rings the tail
doorbell *through* that fenced host fn (`wari_net_mmio_write32`,
lib.rs). The fence is **necessary but not sufficient**: ordering is
handled and the stall still happens. So the initial trigger is *not* a
plain missing barrier.

> Correction to an earlier claim in this thread: I read a receive-process
> "RS-state = 0" out of `DSts`. `DMA_CH0_STATUS` (`0x1160`) does **not**
> carry the 3-bit RS field — it lives in `DMA_DEBUG_STATUS_0` (~`0x100C`),
> which we did not capture. Do not treat "engine Stopped" as established.

## 5. Two-part diagnosis

**(a) The trigger — UNCERTAIN.** With the ordering fence in place, the
remaining candidates for what first wedges the RX engine:
- **Descriptor cache-coherence** — the driver *asserts* the JH7110 is
  IO-coherent (lib.rs:1763,1798; "INV-25's open half", lib.rs:1694). If
  that assertion is wrong for descriptors, the fence orders the doorbell
  but the descriptor image is still in cache and the DMA reads stale RAM
  — a `fence` can't fix that, a CMO (`cbo.clean`) can.
- **Init-time Start-Receive race** — `SR` never effectively latched on
  the bad boots; the channel never truly runs.
- **PHY/link** (lower prob given `MRxC`=0, but not disproven).

**(b) The permanence — CONFIRMED.** The RX recovery kick is gated:
```rust
if st & DMA_STATUS_RBU != 0 {   // drivers/net/src/lib.rs (watchdog)
    // clear RBU + re-ring tail doorbell  ('rXKk')
}
```
Our stall has **RBU clear**, so the kick **never runs**. Whatever
triggers the wedge, the driver has **no path to recover it** — that is
why a rare timing event becomes a permanent 30% loss instead of a
sub-second hiccup.

## 6. Fix plan (ordered by leverage)

1. **Make RX self-healing regardless of trigger (highest leverage).**
   Rework the watchdog so that when RX is wedged — `MRxF` not advancing
   while ping traffic continues, independent of RBU — it performs a full
   RX-channel recovery: stop the channel, re-init the descriptor ring,
   re-issue **Start-Receive** (`DMA_CH0_RX_CONTROL.SR`), re-enable the
   MTL RXQ. This turns *any* stall (whatever the root trigger) from
   permanent into a brief recoverable blip, which alone should drop the
   loss from ~30% to near-0.
2. **Nail the trigger with one measurement (see §7)** so we can also fix
   the *cause*, not just recover from it.
3. **If §7 shows a DMA wedge with descriptor staleness** — add a CMO
   (`cbo.clean`/`fence`-plus-clean) around descriptor publication, or
   re-verify the IO-coherence assumption; close INV-25's "open half".
4. Audit that every descriptor access (RX + TX + ring init) routes
   through the `volatile` `desc_rd`/`desc_wr` helpers — one stray
   non-volatile access reintroduces the visibility half.

## 7. The decisive experiment (turns hypothesis → fact)

During a **live wedge** (a targeted `net-diag` build — ideally a
*low-rate* probe of just these registers, not the full per-frame flood
that itself backpressures the FIFO), read and watch:

| register | addr | what it tells us |
|---|---|---|
| `MTL_RXQ0_MISSED` | `0x0D34` | frames dropped for lack of a descriptor |
| `MMC_RX_FIFO_OVERFLOW` | `0x07D4` | RX FIFO overflowing |
| `MTL_RXQ0_DEBUG` | `0x0D38` | FIFO fill / read-controller state |
| `DMA_CH0_CUR_RXDESC` | `0x114C` | is the DMA descriptor pointer moving? |
| `DMA_CH0_RX_CONTROL` | `0x1108` | `SR` (Start-Receive) still set? |
| `DMA_DEBUG_STATUS_0` | `~0x100C` | the real RS receive-process state |
| `MAC_PHYIF_CTRL_STATUS` | `0x00F8` | link/speed/duplex still up? |

Discriminator:
- **MISSED/OVERFLOW rising + `CUR_RXDESC` frozen** → frames *are*
  arriving but the DMA isn't draining → **DMA wedge** (leading theory);
  the fix is §6.1 + §6.3.
- **MISSED/OVERFLOW also frozen + FIFO empty** → no frames reach the MAC
  → **PHY/link**; different fix entirely.
- **`SR`=0 during the wedge** → init-time start race (§5a variant).

`diag.rs:78-84,222-229` already samples several of these — the probe is
mostly wiring them into the stall-snapshot window.

## 8. Confidence

- RX-DMA wedge vs physical: **leaning DMA** (MRxC=0, intermittent on
  byte-identical code), but **not confirmed** — §7 decides it.
- The recovery gap (RBU-gated, can't handle our RBU-clear stall):
  **confirmed** from the code.
- The permanence being the dominant contributor to the 30% loss:
  **high** — a wedge with no recovery loses everything after it fires.

## Notes on method

The parallel investigation lenses degraded (two hit the structured-output
retry cap, the rest returned stubs), so §4's reconciliation and §5b were
established by direct code reading against the telemetry, not by the
four independent lenses. The single decisive measurement in §7 is the
thing that would replace judgement with data.
