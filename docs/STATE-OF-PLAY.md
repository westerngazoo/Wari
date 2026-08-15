# State of Play — pick up here

> **Last updated**: 2026-08-15
> **Last build published**: 162 (release, on `phase-1c/wire-format-derived`)
> **Next action**: review and merge that branch — it is large, see
> "Open review" below.

## The milestone

**2026-08-15: 0% packet loss on silicon. The ping-loss saga is closed,
and the cause was a missing `volatile`.**

Every DMA descriptor access in the net driver was a plain Rust
load/store on a `static mut` that the GMAC writes asynchronously. No
Rust code ever stores to the OWN bit the hardware clears, so LLVM was
free to hoist the load out of the RX polling loop and keep it in a
register: `receive()` spun on a stale OWN bit reporting "no frame"
while the MAC delivered packets normally.

This is why the loss metric swung **0–57% boot-to-boot on byte-identical
code** — the observation that made no sense as software and sent the
investigation toward analog RGMII margin for weeks. It *was* software;
it just was not deterministic software, because whether the compiler
hoists depends on inlining decisions that shift with any unrelated
change.

Verified on build 162: `ping -c 80` → **0% loss**, StRf 102 / StRa 102
(1:1), StTx 81, and zero watchdog kicks, zero rejects, zero ring-full
drops. No stall during traffic at any point.

**Caveat on PR #71** (RGMII rx-delay 300 ps → 1500 ps, merged): the
value is defensible on its own — 1500 ps is what GMAC0 uses — but the
evidence it was merged on was this bug. Do not treat #71 as the reason
ping works.

### Other silicon results from the same session

- **Ctrl-R reboots from any state**, including mid-tenant. `sstatus.SIE`
  had never been set since Phase 0, so `trap.rs`'s external-interrupt
  arm and all of `plic::dispatch` were code that had never executed.
  The VF2's PLIC `HART_CONTEXT` was also wrong (3 → 2: JH7110 hart 0 is
  the S7 monitor core with no S-mode, so contexts do not follow
  `2*hart+1`), and the DesignWare busy-detect latch needed a USR read.
- **Double RX re-arm removed.** Builds 110–162 re-armed every descriptor
  twice and silently discarded any frame the DMA delivered in between.
  `StRa` must track `StRf` 1:1; a 2:1 ratio is that bug returning.
- **Remote DoS fixed**: an unvalidated device-controlled RX length could
  panic the driver into its `loop {}` handler from any LAN host.

## Open review

`phase-1c/wire-format-derived` is ~800 lines across four concerns
(derived wire formats + INV-24, DMA correctness + INV-25, interrupts +
Ctrl-R, the volatile fix). Past the PR-size rule and worth splitting
before merge.

## The previous milestone

**2026-07: synchronous IPC runs cross-tenant on silicon.** Two isolated
Tier-1 instances completed a PING→PONG rendezvous on the VF2 (seL4-style
`call`/`recv`/`reply`, the Option-B resumable-suspend model — bricks
2/3a/3b), and a Tier-1 HTTP demo served `200 OK` over the wire. This is
the Phase-1c payoff: a **networked, capability-isolated, IPC-capable OS
on sovereign RISC-V silicon.** The earlier ping milestone (build 137,
ICMP reply; build 138, rdtime clock stabilising it) is now history — the
net path PHY → MAC → MTL → DMA → smoltcp is proven, and the residual
loss was isolated to the RGMII PHY delay (a boot-to-boot analog margin,
fixed in #71 pending cold-boot confirmation).

**Also since:** the extracted-core kernel (pure host-testable crates),
the accept-deadline Ctrl-R fix, the artifact-release flow (binaries in
GitHub Releases, not git), the full dev book (26 chapters), and two
Phase-2 tracks opened — the AOT engine and the AI-OS agentic layer.

<details><summary>Original 2026-07-07 ping milestone note (history)</summary>

2026-07-07: **Wari replied to ICMP ping on the VF2** (build 137,
GMAC1/eth1, isolated OpenWrt net, `192.168.50.10`). Phase-1c silicon
network path is proven end-to-end: PHY → MAC → MTL → DMA → smoltcp →
TX replies. Build 138 (pushed, not yet flashed) adds the rdtime-based
clock that makes replies stable instead of intermittent.
</details>

**Read [`net-driver-vf2.md`](net-driver-vf2.md) first** — it is the
complete reference: architecture, bring-up sequence with golden
register values, the diagnostic system, the three-masked-faults
post-mortem (builds 124→138), and operating instructions. It
supersedes `phase-1c-status.md`.

## Quick context for a fresh clone

```bash
git clone https://github.com/westerngazoo/Wari.git wari
cd wari
scripts/build.sh trace     # the one true pipeline — see build-workflow.md
```

`make` is legacy; `scripts/build.sh <release|debug|trace|qemu>` is
canonical (runs on Git Bash + Linux, self-verifying, archives per
branch/profile under `build/out/`).

## Current state

- ✅ Ping answered on silicon (build 137); stability fix shipped (138)
- ✅ Three-fault root cause closed: PHY MDIO addr 1 (130) + golden
  SYSCRG clock cluster (136) + BSP PHY init (137)
- ✅ smoltcp clock now real ms via `rdtime` (138) — prerequisite for TCP
- ✅ Build pipeline fool-proofed (`scripts/build.sh`, four-way tag verify)
- ✅ Golden-reference dump tooling (`scripts/dump-gmac1-regs.sh`)
- ✅ net-diag register snapshots (trace profile) — layer-by-layer RX diagnosis
- ✅ VF2 `.bashrc` sources `~/wari/scripts/wari-upgrade.sh` from the repo
- ✅ `wari go-branch <br>` flashes feature branches

## Topology (operator's two-cable setup, no cable swapping)

- VF2 `end0` (GMAC0, `…:84`) → home router → internet (`wari upgrade`)
- VF2 `end1` (GMAC1, `…:85`) → isolated OpenWrt (`192.168.50.1`,
  WAN unplugged) → laptop USB-Eth (`192.168.50.4`)
- Wari drives GMAC1 (`gmac1` cargo feature), listens on `.10`
- On Debian before `wari upgrade`:
  `sudo ip route del default via 192.168.50.1 dev end1`

## Test loop

```bash
# VF2 (Debian):
sudo ip route del default via 192.168.50.1 dev end1
wari upgrade && wari status && wari go -y      # flashes main

# Laptop (Windows):
arp -d * && ping -t 192.168.50.10

# Trace (PuTTY logging → C:\projects\putty.log):
grep -a "tag=0x4e6d4742" putty.log    # NmGB — frames at MAC (per-interval rate)
grep -a "tag=0x53745478" putty.log    # StTx — smoltcp replies (cumulative)
```

## Next steps, in order

1. **Flash 138, confirm stable ping** — expect ~0% loss; `StTx` should
   track ICMP 1:1 instead of ARP-storming (137 evidence: 111 ARP tx
   vs 82 ICMP tx = neighbor-cache thrash from the 1000x-fast clock)
2. **Net-6d on silicon**: Tier-1 tenant already binds port 7000;
   kernel resolves `socket_accept`/`socket_send_canned` —
   `curl http://192.168.50.10:7000` from the laptop
3. **JSON-over-HTTP demo** — the Phase-1c north star
4. Housekeeping when convenient: `release`-profile flash (drop
   net-diag); revisit "known remaining deltas" in `net-driver-vf2.md`
   before TCP throughput work; grow RX ring (16 → more) for TCP

## Build-number note

Numbers are monotonic per branch lineage, not globally unique
(parallel-dev deploys minted their own 130s). Identity = branch +
sha + embedded `WARI-BUILD-TAG` (see `build-info.txt` under
`build/out/<branch>/<profile>/`).
