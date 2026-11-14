# Orange Pi R2S (Ky X1 / SpacemiT K1) — bring-up plan

> **Status:** planning draft. The per-SoC constants below are sourced
> from **mainline Linux device-tree work** for `spacemit,k1`, not yet
> from the physical board. Every value is marked *confirmed-from-DT-needed*
> or *sourced-provisional*. Nothing here is committed to a
> `BoardDescriptor` until the board's own device tree confirms it.
>
> Advances roadmap **p3b** (Orange Pi R2S port; board descriptor first)
> on the B3 foundation (PRs #98/#99/#100).

## 0. The RISC-V gate — confirm this FIRST

Wari is RISC-V-only (R7 / architectural invariant). There is **no ARM
path**. The name "R2S" is ambiguous *across vendors*, so before any work:

| | RISC-V — Wari can run | ARM — Wari **cannot** run |
|---|---|---|
| Brand | **Orange Pi** (Xunlong) | **NanoPi** (FriendlyElec) |
| SoC silkscreen | **Ky X1** (= SpacemiT K1) | Rockchip RK3328 |
| CPU | 8× SpacemiT X60, RV64GCVB | 4× Cortex-A53 |

> Correction to an earlier claim: there is **no Allwinner-H5 "Orange Pi
> R2S."** The Orange Pi R2S is RISC-V. The ARM look-alike is the
> *NanoPi* R2S (a different company). The board you have, if it's an
> Orange Pi, is the RISC-V one.

**Definitive check** (from the booted vendor OS — silkscreen is
strong but not proof):

```bash
cat /proc/cpuinfo                     # expect: isa : rv64...  + "SpacemiT X60"
cat /proc/device-tree/compatible; echo # expect: ...spacemit,k1
```

If those say `aarch64` / `Cortex-A53` / `rockchip,rk3328` → it's ARM, and
we stop. Anything else about the port is moot until this passes.

## 1. Serial console

- **3.3 V USB-to-TTL adapter — required.** A 5 V-logic adapter can damage
  the SoC (vendor-documented).
- 3-pin debug header: **GND / TXD / RXD only** (no VCC pin). Power the
  board from its own supply; do **not** feed VCC from the adapter.
- Wiring is crossed: adapter **RX ← board TXD**, adapter **TX → board
  RXD**, GND↔GND. If you get no output, swap TX/RX (vendor says either
  orientation is safe to try). Flow control: none.
- Line settings: **115200 8N1** *(sourced from the RV2 SBC docs; confirm
  on the R2S from its vendor wiki or U-Boot `printenv baudrate`)*.
- Read the header silkscreen for pin order — the vendor only shows it as
  an image, so I couldn't extract the exact left-to-right order.

Start the logger **before** powering the board so U-Boot's banner is
captured (macOS device is `/dev/tty.usbserial-*`; Linux `/dev/ttyUSB0`):

```bash
ls /dev/tty.usbserial-*                       # find the adapter (macOS)
picocom -b 115200 -g boot.log /dev/tty.usbserial-XXXX
# then power on the board
```

## 2. Pull the device tree + identity proof

From the booted vendor Linux:

```bash
cp /sys/firmware/fdt board.dtb                    # exact blob firmware booted
dtc -I fs /proc/device-tree -O dts -o board.dts   # decompile live tree
#   (apt-get install device-tree-compiler  if dtc is missing)
cat /proc/cpuinfo > cpuinfo.txt
cat /proc/device-tree/compatible; echo            # the gate proof
```

Read the bring-up anchors:

```bash
grep -nE 'memory@|serial@|uart@|plic@|clint@|ethernet@|timebase-frequency|reg-io-width|reg-shift|riscv,ndev|interrupts' board.dts
```

**Send back:** `boot.log`, `board.dts`, `board.dtb`, `cpuinfo.txt`, plus
the `serial@…`, `plic@…`, `cpus`, `memory@…`, and `ethernet@…` nodes
(and any clock/reset/syscon nodes they `phandle` to). A photo of the SoC
silkscreen + board model marking is a nice extra confirmation.

## 3. Provisional `BoardDescriptor` (sourced from `spacemit,k1` DT)

These are strong (mainline Linux DT) but **provisional** — the board's
own DT overrides. Notice how sharply the addresses differ from QEMU/VF2:
this is exactly what the B3 descriptor was built to absorb as data.

| Field | Value | Source / confidence |
|---|---|---|
| `name` | `"orangepi-r2s"` | — |
| `uart_base` | `0xd401_7000` | DT `serial@d4017000` · HIGH |
| `uart_stride` | `4` | `reg-shift=<2>`, `reg-io-width=<4>` · HIGH (same DW8250 family as VF2) |
| `plic_base` | `0xe000_0000` | DT `plic@e0000000`, `sifive,plic-1.0.0`, `ndev=159` · HIGH |
| `timebase_hz` | `24_000_000` | DT `timebase-frequency=<24000000>` · HIGH |
| `dram_origin` | `0x0020_0000` (DRAM base `0x0`) | U-Boot TEXT_BASE · HIGH-ish — **a load decision, cross-check the OpenSBI/U-Boot handoff.** ⚠ `0xC000_0000` is the DDR *controller regs*, NOT DRAM. |
| MMU | Sv39 | `mmu-type="riscv,sv39"` — matches Wari · HIGH |
| `boot_hart_id` | **read from board** | `a0`/hart id at S-mode entry in `boot.log`. QEMU=0, VF2=1; K1 unknown. |
| `plic_hart_context` | **read from board** | K1 pattern is `2*hart+1` (all 8 harts have S-mode, unlike JH7110) — but confirm against `interrupts-extended` for the observed boot hart. |
| `uart_irq` | **read from board** | uart0 node's `interrupts=<N>`. Ctrl-R depends on it. |
| `net_windows` | **read from board** | the router NIC MAC window(s) — see §4. |
| `mmio_regions` | **read from board** | MAC(s) + clock/reset/syscon deps (superset of `net_windows`). |
| `dma_coherent` | **experiment** | not in any DT; the one field that's a decision, not a transcription. Set from a bring-up test. |

## 4. Architect decisions (Gustavo) — not mine to make

1. **New Tier-2 net driver — the bulk of the port.** The R2S is a
   *router*: 2× 2.5 GbE + 2× GbE, a MAC complex that is almost certainly
   **not** the JH7110 GMAC the current driver + smoltcp path targets.
   Reusing `drivers/net` is unlikely; this is a fresh Tier-2 driver
   bring-up (VF2 networking took builds ~107→162). **Scope question:**
   bring up *one* NIC first (single-link parity with VF2), or design for
   the 4-port complex up front? Recommend one-NIC-first.
2. **UART register compatibility.** The console is `spacemit,k1-uart` /
   `intel,xscale-uart` — an 8250 variant, same *family* as VF2's DW8250,
   so `uart_ns16550.rs` with `stride=4` *probably* works unchanged.
   Verify the FCR/LCR/USR (busy-detect) semantics match before trusting
   it. Low risk, must-check.
3. **CLINT base differs (`0xe400_0000`) — but this is a NON-issue.**
   Verified: the kernel never touches CLINT MMIO — it uses **SBI** for
   reset and the `sip`/`stimecmp` CSRs for the timer (K1 has Sstc).
   OpenSBI owns the CLINT in M-mode. **No `clint_base` descriptor field
   is needed.**
4. **Misaligned-access trap risk.** Community reports some K1/M1 silicon
   traps on misaligned scalar/vector loads. If `wasmi` or Tier-0 does
   unaligned accesses, they'd fault here. Investigate before assuming the
   VF2 image "just runs." Flag, not a blocker.
5. **8 harts, all with S-mode.** Initial bring-up stays single-hart
   (INV-1) — park harts 1–7, as VF2 parks its extra U74s. This is the
   natural first target for ADR-001 multikernel *later*, not now.

## 5. Port sequence (once the gate passes + DT is in)

1. `r2s` cargo feature + `linker-r2s.ld` (ORIGIN from the confirmed
   handoff) + `build.rs` arm + `board.rs` selector arm.
2. `pub const R2S: BoardDescriptor` filled from the board's DT (§3).
3. **Boot to banner** on serial — UART-only, no net. Proves
   uart/plic/dram/mmu constants. This is the first real milestone and
   isolates every constant from the NIC work.
4. Ctrl-R (SBI reset) + timer interrupt — proves PLIC context + SBI path.
5. New Tier-2 net driver → smoltcp → `ping` (the long pole).
6. `dma_coherent` experiment; wire CMO hooks if `false`.

Milestone 3 (boot-to-banner) is achievable from the descriptor + linker
alone and is the honest "R2S runs Wari" first light. Networking is a
separate, larger effort after it.
