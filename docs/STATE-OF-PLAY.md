# State of Play — pick up here

> **Last updated**: 2026-07-16
> **Last build published**: 152 (release, GMAC1 RGMII rx-delay fix — PR #71)
> **Diagnostic build**: 151 (debug, IPC-wedge tracers — PR, on-branch)
> **Next action**: cold-boot sweep to confirm the RGMII rx-delay fix
> (`wari go-branch phase-1c/gmac1-rgmii-rx-delay`, then `ping -c60` ×4
> power-cycles); if clean every boot, merge #71 and close Phase 1c.

## The milestone

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
