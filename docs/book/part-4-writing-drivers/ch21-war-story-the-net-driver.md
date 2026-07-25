---
sidebar_position: 21
sidebar_label: "Ch 21: War Story — The Net Driver"
title: "Chapter 21 — War Story: The Net Driver"
---

# Chapter 21 — War Story: The Net Driver

Everything before this chapter was scaffolding. The trait, the manifest,
the signing, the pipeline — all of it exists so that when you flash a
driver to real hardware and it does not work, you at least know you are
debugging the code you wrote. This chapter is what happens next: the
part where the datasheet is right, the code is right, the build is fresh,
and the board still will not pass a single ping.

The subject is the VisionFive 2 network bring-up — the JH7110's GMAC1 at
`0x16040000`, wired through RGMII to a Motorcomm YT8531C PHY, talking to
an isolated test net at `192.168.50.0/24`. It took fourteen builds, 124
through 138, to get the first stable ping (`docs/net-driver-vf2.md:132`).
It is worth recording not because the bugs were exotic — they were not —
but because the *shape* of the failure is a general lesson about
debugging hardware you cannot single-step: when several faults are live
at once, the scientific method turns against you.

## Three faults, each disproving the others

RX was silently zero. Frames left the switch, the cable was fine, Debian
on the same board saw them perfectly — and the MAC's frame counter stayed
at zero. The reason, discovered only in retrospect, was that **three
independent faults were live simultaneously**, and each one, on its own,
was enough to zero RX. `docs/net-driver-vf2.md:143` names them:

| Fault | What it was | Fixed in build |
|---|---|---|
| **A** | PHY writes went to MDIO address 0; GMAC1's PHY is at address **1** | 130 |
| **B** | SYSCRG clock cluster wrong — `gmac1_rx` (`0x19C`) left at 0 = **no RX clock at all** | 136 |
| **C** | PHY RGMII delays left at U-Boot residue (`0xA003 = 0x00F1`, rx-delay 0) instead of the BSP config | 137 |

Read the failure sequence and feel the trap close (`net-driver-vf2.md:149`):

- Build 127 wrote the *right* PHY delay values — to the *wrong* address,
  through a *dead* clock. Nothing changed. Conclusion drawn: "PHY writes
  don't help."
- Build 130 fixed the address. Clock still dead, so still nothing.
  Conclusion drawn: "the `0x0850` value doesn't help either."
- Build 131 therefore **removed the PHY writes entirely** — an
  "inheritance test," on the reasonable-sounding theory that if they did
  not help, U-Boot's setup must already be fine.
- Build 136 fixed the clocks. Now the PHY was wrong again (its writes had
  been removed), so RX stayed zero. Conclusion available: "the clocks
  didn't help either."
- Only build 137, with A *and* B *and* C fixed at once, worked.

This is the payload of the chapter. **When multiple faults coexist,
one-variable-at-a-time elimination generates false negatives, and you
will revert your own correct fixes.** Every good scientific instinct —
change one thing, observe, conclude — produces a *wrong* conclusion here,
because the observable (RX count) is gated by an AND of three conditions
and stays at zero until the last one flips. The method that works on one
bug actively misleads you on three.

The way out was not more theorizing. It was a **golden-reference diff**:
stop hypothesizing about what the registers *should* be, boot the working
system (Debian, same silicon), dump every register the RX path touches
through `/dev/mem` with `scripts/dump-gmac1-regs.sh`, and make the broken
system byte-identical to it (`net-driver-vf2.md:157`). When you cannot
reason forward from the datasheet because too many things are wrong,
reason backward from a system that works.

### Fault A, and why the PHY hides at a different address

Fault A is the one Chapter 18 foreshadowed: the PHY is not the MAC. The
MAC is on-SoC MMIO at `0x16040000`; the PHY is a separate chip reached
*through* the MAC's MDIO block, addressed on a two-wire management bus.
GMAC0's PHY answers at MDIO address 0; GMAC1's answers at address 1
(`&gmac1 { ethernet-phy@1 }` in the board's device tree). Builds 125–129
hard-coded a literal `0` at all thirteen `mdio_*_phy` call sites
(`drivers/net/src/lib.rs:116`), so a GMAC1 build was writing delay
configuration to an address that, on GMAC1's bus, no PHY owns. The
values were perfect. They went nowhere.

Build 130 added a single diagnostic that made this class of bug
diagnosable ever after — a tag logging `plat::PHY_ADDR` *before* the
first PHY read (`lib.rs:2116`), so a subsequent PHY-ID of `0xFFFF` is
unambiguously "talking to a dead address" rather than "PHY present but
mute." Small tags, placed to make a whole failure mode legible, are the
difference between a five-minute diagnosis and a five-build one.

## The fourth bug: real time, not a loop counter

Faults A/B/C got RX flowing. Ping still stuttered — and this one could
only be *found* once the datapath was clean, because now the diagnostic
counters could prove the frames were arriving while the replies were
still going missing.

smoltcp needs a real clock. It ages its neighbor (ARP) cache, times TCP
retransmits, and schedules delayed ACKs against a millisecond timestamp.
`kernel/src/runtime/tier2_net.rs::next_tick()` supplies it by reading the
RISC-V `time` CSR — `rdtime` — and converting to milliseconds against the
JH7110's 4 MHz timebase (`net-driver-vf2.md:83`). Builds up to 137 had
instead advanced a *fake* clock: a loop counter that ticked ten "ms" per
idle iteration. At roughly a hundred thousand idle iterations per second,
that fake clock ran about a thousand times too fast — a thousand virtual
seconds per real second. smoltcp's 60-second ARP cache therefore expired
every 60 *real milliseconds*, so the stack re-ARPed before almost every
reply it wanted to send.

The fingerprint was in the TX census: 111 ARP frames transmitted against
82 ICMP replies (`net-driver-vf2.md:93`). A stack that is spending more
frames asking "who has .10?" than answering pings is a stack that has
lost track of time. The rule the incident leaves behind is blunt: **never
replace `rdtime` with a spin counter.** Wall-clock time is not an
approximation you can synthesize from loop iterations, because the thing
downstream of it — cache lifetime — is measured in the real seconds you
just threw away.

## The RGMII finale, and the signature of analog timing

Fault C is the most physical of the four, and it teaches the subtlest
diagnostic instinct in the whole bring-up.

RGMII is a source-synchronous parallel bus: the PHY sends receive data
(RXD) alongside a receive clock (RXC), and the MAC latches the data on a
clock edge. For the latch to be reliable, the clock edge has to fall in
the *center* of the data eye — the window where the data lines are
stable. "RGMII-ID" (internal delay) is the PHY inserting a few nanoseconds
of skew between RXC and RXD to center that edge. Too little delay and the
edge drifts to the boundary of the eye, where some fraction of frames get
sampled mid-transition and fail CRC at the MAC.

Here is the tell. When the delay is marginal, the fraction of frames that
happen to sample cleanly depends on the exact phase relationship the link
came up in — and that varies slightly *from boot to boot on byte-identical
code*. Wari saw it as ping success swinging wildly between cold boots with
no code change at all; the code's own comment records one datapoint as
"1/118 ping success with Debian seeing the same frames perfectly on the
same cable — classic RGMII timing margin signature" (`lib.rs:2130`).

**A metric that varies boot-to-boot on identical code is the signature of
analog timing, not logic.** Logic is deterministic; if the code did not
change and the result did, you are looking at something physical — a
clock phase, a delay line, a voltage margin — that the code only
*configures*, and configures wrong. The moment you see a number that
should be a constant behaving like a random variable across resets, stop
looking at the control flow and start looking at the picoseconds.

The fix is per-target, and this is where the honest register values
matter. On **GMAC0** the driver writes a single value to the YT8531C's
RGMII-config-1 extended register (`0xA003`): `0x680A`
(`lib.rs:2171`) — bit 14 set (tx-clk-1000 inverted), RX delay `0x0A` and
GE-TX delay `0x0A`, where each nibble step is 150 ps, so `0x0A` is about
1500 ps of internal delay. On **GMAC1** — the board actually in use — the
BSP does more than a single write, so the driver mirrors StarFive's
`motorcomm.c ytphy_of_config` as a three-step read-modify-write
(`lib.rs:2224`): clear `RXC_DLY_EN` in the chip-config register `0xA001`,
set pad drive strength in `0xA010` (mask `0xF030`, value `0xC030`), and
RMW only the delay nibbles of `0xA003` to land `0x0850` (rx `0x2`, fe
`0x5`, ge `0x0`). The `ext_rmw` closure at `lib.rs:2231` is the whole
extended-register dance in five lines — page-select, read, modify,
page-select, write — because the YT8531C pages its vendor registers
behind `0x1E` (page select) and `0x1F` (page data), the standard MDIO map
only exposing 32.

Two lessons ride along with the value.

First, **the delay latches at link-up.** Writing the register is not
enough; the PHY only samples its new timing config on a fresh
auto-negotiation cycle. The driver computes `needs_relink = rc1r_pre !=
rc1r_post` (`lib.rs:2276`) and forces a re-AN *only* when the config
actually changed — kicking AN unconditionally would drop a link that
U-Boot may already have brought up, costing ~100 ms for nothing.

Second, **always verify-read.** The driver reads the register back after
writing and logs the latched value under the `RC1p` tag (`lib.rs:2271`),
next to the pre-write `RC1R` (`lib.rs:2183`). The boot log therefore
*proves* the PHY accepted the write — you see `RC1R` go in and `RC1p`
come out — rather than merely proving you issued it. On a bus where the
target might be a dead address (fault A), "I sent the write" and "the
write took" are different facts, and only the second one helps.

## Reading the trace: tag words through a keyhole

You debug all of this through a single UART line at 115200 baud. There is
no debugger, no `printf` to a terminal — there is `drv_log_u32(tag,
val)`, which the kernel formats onto COM7 as
`[net:drv] tag=0xXXXXXXXX val=0xYYYYYYYY`. Learning to read that stream is
the actual skill of hardware bring-up, and `docs/diagnostic-tags.md` is
its dictionary.

**Tags are four ASCII bytes packed big-endian into a `u32`**, chosen so
they are legible in a hex dump. `0x5243_3170` is `R C 1 p` — the
verify-read above. `0x4E6D_4742` is `N m G B`. The `val` carries the
runtime payload: a register value, a slot index, a counter. Decoding is
mechanical (`pbpaste | scripts/wari-trace-decode.sh`), but the fluency
worth having is knowing *which* tag answers *which* question.

Two channels, and the distinction is not cosmetic — it is the difference
between a working driver and an 11 ms ping floor. `drv_log_u32` is
**always on** and reserved for boot milestones: register dumps at init,
the MAC address, one-shot markers. `drv_trace_u32` (`lib.rs:228`) is
**compiled out** unless the kernel's `debug-kernel` feature is set, and it
carries the per-frame RX/TX events. That split exists because the
per-frame tags were once on the always-on channel, where each line cost
~3.6 ms of blocking UART — about 14 ms per received frame — capping RX at
~70 frames/second and putting a hard 11 ms floor under ping RTT, with
ring overflow under broadcast bursts (`lib.rs:216`). Hot-path logging on
a production build is not a diagnostic; it is a new bug. Milestones on
`drv_log_u32`, per-packet on `drv_trace_u32`, and the production kernel
pays for neither.

For the hot path you do not read every frame anyway — you read
**counters**. The `St**` family (`diagnostic-tags.md:123`) emits a
six-line burst once per ~65,536 `receive()` calls: `StRc` (receive
calls), `StRf` (frames found), `StCc` (consume calls), `StDc` (drops),
`StRa` (re-arms), `StTx` (frames sent). This is how you get datapath
health without flooding the line. If `St**` lines appear but `StCc` is
still 0 after thirty seconds of ping, you know smoltcp is receiving
nothing to consume — without a single per-frame log.

And the counters localize the fault to a layer. The `net-diag` feature
(the `trace` profile) adds a 17-register RX-path snapshot every ~32K
`receive()` calls, and its diagnosis table (`diagnostic-tags.md:112`)
reads like a fault tree:

- `NmGB` (frames at the MAC) **stuck at 0** → the PHY is blocking. Link
  down, wrong clock, wrong address — faults A and B live here.
- `NmGB` climbing while `NmCr` (CRC errors) **tracks it 1:1** → the
  frames arrive but fail CRC → RGMII timing is wrong. Fault C lives here.
- `NmGB` clean but `NT_M` (MTL missed) climbing → the MAC accepted them,
  the MTL FIFO dropped them.
- All clean but `StRf` still 0 → the frames are landing but the
  descriptor handoff is broken → look in the driver.
- All clean, `StRf` equals `StCc`, replies still missing → **look above
  the driver.** smoltcp config, or the clock — the fourth bug lives here.

That last row is the one that finally isolated the clock. Once the
snapshot proved every hardware layer clean and the counters proved smoltcp
was consuming exactly what arrived, the missing replies could not be a
datapath problem. The fault had to be above the driver, and the TX census
pointed straight at time.

One field note that cost twenty minutes and is worth inheriting for free:
do **not** pack a slot index into a tag with a bitwise OR
(`diagnostic-tags.md:176`). An early build wrote `0x7258_4672 | (idx &
0xF)`, but the ASCII byte `0x72` already has bits set, so indices 0 and 2,
1 and 3, and so on all aliased to the same tag — the trace showed eight
"distinct" slots that were four slots wearing each other's names. Since
build 118 the convention is fixed: the base tag is the *event*, constant;
the slot index rides in `val >> 24`. A diagnostic that lies to you is
worse than no diagnostic, because you trust it.

## The finale: 1500 picoseconds

Build 138 answered a ping, but not *every* ping. The loss settled into a
maddening pattern: whenever a frame got through, its round-trip was
sub-millisecond — clean, fast, no retransmit — yet a fraction of frames
never came back at all. The MMC counters said the RX datapath was
spotless: no CRC errors, no FIFO overflow, no missed frames. Frames that
arrived were perfect; some just didn't arrive.

The tell was the one the previous section warned about. Two consecutive
**cold boots of byte-identical build 150** measured 0.0 % loss, then
27.5 % — a metric that should be constant swinging wildly across identical
code. By the book's own rule, that stops being a logic problem and becomes
an analog-margin problem. And there is exactly one analog knob on this
path: the RGMII receive-clock delay, the phase relationship between the
PHY's RXC clock and the RXD data lines it strobes.

RGMII in *internal-delay* mode (RGMII-ID) asks the PHY to shift RXC by
1.5–2.0 nanoseconds so its rising edge lands in the centre of the data
eye — the widest-open, most forgiving sampling point. The GMAC1 PHY was
programmed with the StarFive BSP default of `rx_delay = 0x2`, which at 150
picoseconds per step is **300 ps** (`drivers/net/src/lib.rs:2163`,
register `0xA003` = `0x0850`). Three hundred picoseconds is nowhere near
the eye centre; it parks the sampling edge near the data-transition
boundary, where any board-to-board or boot-to-boot variation in trace
length, temperature, or link-training outcome decides whether a given
frame samples cleanly or lands a CRC error. Hence the coin-flip loss that
changed on every link-up. The proof was already in the tree: GMAC0, on the
same silicon, uses `rx_delay = 0x0A` — **1500 ps** — and never dropped a
frame.

The fix is one nibble. Bump GMAC1 to the same 1500 ps the sister
interface and the RGMII-ID spec both call for — `rx_delay 0x2 → 0x0A`,
register `0x0850 → 0x2850`. The value is written from named nibbles so the
neighbours in the sweep (`0x08` = 1200 ps, `0x0C` = 1800 ps) are a
one-token change if 1500 overshoots on a particular board revision, and
the `RC1p` verify-read prints the latched value so the boot log proves the
write took. Because the YT8531C only samples RGMII timing at link-up, the
driver forces a fresh auto-negotiation whenever the register changed —
`needs_relink = rc1r_pre != rc1r_post`. This lands as build 152 (PR #71);
silicon confirmation is the cold-boot sweep the diagnosis prescribed —
clean loss on *every* boot, not one lucky link-up.

That this fix could be reasoned to a single value, rather than swept blind,
is the whole point of the section that follows: once you recognise the
analog-margin signature, the register reference tells you where the eye
centre is, and the sister interface tells you it's reachable.

## What the war actually teaches

Strip the register addresses away and three transferable instincts
remain. When one-variable elimination keeps disproving fixes you believe
are correct, suspect *coexisting* faults and switch to a golden-reference
diff — make the broken system byte-identical to a working one before you
theorize. When a number that should be constant varies across identical
boots, stop reading logic and start measuring analog margin. And build
your diagnostics as carefully as your datapath — put the PHY address in
the log before the first read, verify-read every write, keep hot-path
tracing off the critical path, and never let a tag word alias itself.
Build 138 passed a stable ping not because the last fix was clever, but
because by then the trace could finally be trusted.

## Closing hook

The network driver works, and the way it works is a `wasmi` interpreter
walking WASM bytecode instruction by instruction, at roughly a hundred
thousand polls per second on a U74 core. That is fast enough to answer a
ping. It is nowhere near fast enough to saturate a gigabit link, and the
reason is exactly the interpreter loop this whole book has leaned on for
its safety story.

Part 5 confronts that trade directly: the ahead-of-time engine that
compiles the same validated, signed WASM into native RISC-V *before* it
runs — keeping the structural isolation the interpreter gave us for free,
while paying the interpretation cost once, at build time, instead of on
every frame.
