<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Writing a Tier-2 Driver

> **Audience:** someone adding a new Tier-2 (system) WASM driver, or
> trying to understand the net/UART drivers we already have. This is
> the how-to; the *why* lives in
> [`driver-interface-design.md`](driver-interface-design.md) (the
> manifest contract) and [`net-driver-vf2.md`](net-driver-vf2.md) (the
> VF2 bring-up war story). Worked example throughout: the net driver,
> `drivers/net/src/lib.rs`.

---

## 0 · What a Tier-2 driver *is*

A Tier-2 driver is a **WASM module** (`wasm32-unknown-unknown`) that the
kernel loads, verifies, and runs — same sandbox as a customer app, but
granted **capabilities** ordinary apps don't have (MMIO, IRQ). It is
**not** native kernel code. That's the whole bet: a driver bug can't
escape the WASM sandbox into the kernel.

```
Tenant app  ──fd_write──►  Kernel  ──host fn──►  Tier-2 driver (WASM)  ──MMIO host fn──►  hardware
                           (Tier 0)              (Tier 2, sandboxed)
```

Three hard rules follow from "the driver is WASM," and every one of them
has drawn blood already (see §6):

1. **The driver compiles to `wasm32-unknown-unknown`.** No inline
   `asm!`, no RISC-V intrinsics, no `core::arch::asm!`. If you need a
   CPU instruction, you're doing it wrong — cross the boundary instead.
2. **Hardware is touched only through host functions** the kernel
   imports for you (`wari_net_mmio_read32`, `wari_net_mmio_write32`,
   `mdio_*`, `lin_mem_base`, …). The driver has no raw pointers to
   device memory.
3. **The driver is built as a *separate cargo crate*** and embedded into
   the kernel as a signed blob. You **must** build through `make` /
   `scripts/build.sh` or you will ship a stale driver under a
   fresh-looking kernel (this is `CLAUDE.md`'s loudest warning).

---

## 1 · The contract: trait + macro + manifest

You do **not** hand-write the WASM ABI. You implement a trait and invoke
a macro; the macro emits the `#[no_mangle] extern "C"` shims *and* the
signed-manifest bytes.

`drivers/net/src/lib.rs` (~line 3346):

```rust
pub struct Driver;                       // zero-sized; per-call dispatch

impl wari_driver_iface::NetDriver for Driver {
    fn start()                { driver_start(); }
    fn poll(t: u64) -> i32    { driver_poll(t); }
    fn tx_send(buf: &[u8])    -> i32 { … }
    fn rx_pop()               -> u64 { … }
    fn socket_create(p: u32)  -> i32 { … }
    // … the rest of the NetDriver surface
}

wari_driver_iface::wari_net_driver!(Driver);   // ← emits shims + manifest
```

The macro produces two things:

- **Export shims** (`_start`, `poll`, `tx_send`, …) — the functions the
  kernel calls into.
- **A `WARI_DRIVER_MANIFEST` byte array** in a custom WASM section
  (`wari_driver_manifest`) declaring the driver's *kind*, its *exports*
  (name + signature), and the *host-fn imports* it needs.

The **sign tool** (`scripts/sign-module.rs`) refuses to sign unless the
manifest's declared imports/exports match what the WASM binary actually
requests — a bidirectional check. This is why adding a new host-fn call
(like `drv_trace_u32`) also means adding it to the manifest import list
in `driver-iface/src/lib.rs`; forget one side and signing fails loudly.
See [`driver-interface-design.md`](driver-interface-design.md) for the
wire format.

---

## 2 · Features and `#[cfg]` — the platform-selection system

This is the part that looks like magic until it clicks. **One source
tree compiles into several different drivers, and cargo features pick
which.** There is no runtime `if platform == vf2` — the wrong
platform's code is *deleted at compile time*.

### 2.1 Where features are *declared*

`drivers/net/Cargo.toml`:

```toml
[features]
default  = ["qemu"]      # a bare `cargo build` targets QEMU
qemu     = []            # VirtIO-net on QEMU virt
vf2      = []            # JH7110 GMAC on the VisionFive 2
gmac1    = ["vf2"]        # target GMAC1 (eth1) not GMAC0 — implies vf2
net-diag = []            # opt-in 17-register RX diagnostic snapshots
```

Read the syntax literally:
- A feature is a **name** with a list of **other features it turns on**.
- `gmac1 = ["vf2"]` means "enabling `gmac1` also enables `vf2`" — you
  can't target GMAC1 without being on the VF2 platform. That dependency
  is enforced by cargo, not by a comment.
- `default = ["qemu"]` is what you get from a plain `cargo build`. The
  real builds override it (`--no-default-features`, below).

### 2.2 Where features are *set*

**You almost never type `--features` yourself** — `scripts/build.sh`
does, keyed by profile. `scripts/build.sh` (~line 85):

```sh
release) DRV_FEATURES="vf2 gmac1";          KRN_FEATURES="vf2"              ;;
debug)   DRV_FEATURES="vf2 gmac1";          KRN_FEATURES="vf2,debug-kernel" ;;
trace)   DRV_FEATURES="vf2 gmac1 net-diag"; KRN_FEATURES="vf2"              ;;
qemu)    DRV_FEATURES="qemu";               KRN_FEATURES="qemu"             ;;
```

and then (~line 159):

```sh
cargo build --release --features "$DRV_FEATURES" --no-default-features
```

`--no-default-features` is the important half: it *drops* `default =
["qemu"]` so a vf2 build doesn't accidentally compile both platforms.
The net driver is actually built **twice** every run — once `vf2 gmac1`,
once `qemu` — and the kernel `include_bytes!`s the matching signed blob
(the cfg-selected `net_blob.rs`). So `scripts/build.sh release` →
`DRV_FEATURES="vf2 gmac1"` is how the RX-delay you just changed reaches
silicon.

**Feature cheat-sheet:**

| Feature | Turns on | Set by profile |
|---------|----------|----------------|
| `qemu` | VirtIO-net, MMIO base `0x1000_8000` | `qemu` |
| `vf2` | JH7110 GMAC, DMA rings, PHY init | `release` / `debug` / `trace` |
| `gmac1` | GMAC1 (`0x1604_0000`, PHY @1) instead of GMAC0 | all vf2 profiles |
| `net-diag` | periodic RX-path register snapshots | `trace` only |
| `debug-kernel` *(kernel feature)* | `kdebug!` lines fire | `debug` only |

### 2.3 How `#[cfg]` *selects* code — the `plat` module trick

Now the payoff. `drivers/net/src/lib.rs:93`:

```rust
#[cfg(feature = "qemu")]
mod plat {
    pub const NIC_BASE: u32 = 0x1000_8000;         // VirtIO on QEMU
}

#[cfg(feature = "vf2")]
mod plat {
    #[cfg(not(feature = "gmac1"))]
    pub const NIC_BASE: u32 = 0x1603_0000;         // GMAC0
    #[cfg(feature = "gmac1")]
    pub const NIC_BASE: u32 = 0x1604_0000;         // GMAC1

    #[cfg(not(feature = "gmac1"))]
    pub const PHY_ADDR: u32 = 0;                    // GMAC0's PHY
    #[cfg(feature = "gmac1")]
    pub const PHY_ADDR: u32 = 1;                    // GMAC1's PHY
}
```

What's happening:

- **Two modules named `plat`.** They don't collide, because
  `#[cfg(...)]` deletes the one whose feature is off *before* the
  compiler sees a duplicate. Exactly one survives. The rest of the code
  just writes `plat::NIC_BASE` and never knows which platform it's on.
- **cfg nests.** Inside the `vf2` module, `#[cfg(not(feature =
  "gmac1"))]` vs `#[cfg(feature = "gmac1")]` picks GMAC0 vs GMAC1. This
  is why `gmac1` *implies* `vf2` (§2.1) — the inner cfg only exists
  inside the vf2 module.
- **`#[cfg(not(feature = "…"))]`** is "compile this when the feature is
  OFF." The GMAC0 default and the GMAC1 opt-in are a `cfg` / `cfg(not)`
  pair — a common idiom for "A unless the flag says B."

This is the mental model for the whole file: **a `#[cfg]` is a compile-
time delete.** When you read `#[cfg(feature = "vf2")]` above a function,
read it as "this function does not exist in the QEMU build." The RX-delay
constant you just edited lives under `#[cfg(feature = "gmac1")]`
(`lib.rs:2190`) — it is literally absent from the GMAC0 and QEMU drivers.

### 2.4 Adding your own platform / variant

To add, say, a third NIC target:
1. Add `mynic = ["vf2"]` (or a bare platform) to `[features]`.
2. Add a `#[cfg(feature = "mynic")]` arm anywhere the base address,
   register layout, or init sequence differs — the `plat` module is the
   natural home for constants.
3. Add a profile line to `scripts/build.sh` so it can be built, and (if
   the kernel embeds it) a `#[cfg]` arm in `kernel/src/runtime/net_blob.rs`.
4. Keep the pure logic **outside** the cfg arms — only the
   platform-specific values/sequences should be gated, so the shared
   datapath stays single-source.

---

## 3 · Touching hardware: MMIO and MDIO

The driver has no pointers to device registers. It calls host functions
the kernel bound into its linker. They're declared as `extern "C"`
imports at the top of `drivers/net/src/lib.rs` (~line 200):

```rust
extern "C" {
    #[link_name = "net_mmio_write32"]
    fn wari_net_mmio_write32(addr: u32, val: u32) -> i32;
    #[link_name = "net_mmio_read32"]
    fn wari_net_mmio_read32(addr: u32) -> u32;
    #[link_name = "drv_log_u32"]
    fn wari_drv_log_u32(tag: u32, val: u32) -> i32;   // boot milestones (always on)
    #[link_name = "drv_trace_u32"]
    fn wari_drv_trace_u32(tag: u32, val: u32) -> i32; // hot path (debug-kernel only)
    // …
}
```

- **`addr` is an absolute physical register address** (`plat::NIC_BASE +
  offset`). The kernel's `net_mmio_*` host fn is capability-gated: it
  checks the driver holds a Net cap covering that MMIO window before it
  pokes the register (see `kernel/src/cap/` and the validator). A driver
  can't reach memory it wasn't granted.
- **CPU fence/ordering comes for free** at the wasm→native boundary —
  the host-fn call is a natural serialization point. That's *why* rule
  #1 forbids inline `asm!`: you never need a manual `fence`.

**MDIO (PHY registers)** ride on top of MMIO. `mdio_write_phy` /
`mdio_read_phy` (`lib.rs:1912`) drive the GMAC's MDIO controller
registers to reach the external PHY chip over the management bus. PHY
registers ≠ MAC registers: the MAC is on-SoC MMIO; the PHY is a separate
chip you talk to *through* the MAC's MDIO block.

---

## 4 · Worked example: PHY bring-up (the RGMII delay)

The single most instructive sequence in the driver, because it shows
MDIO, extended registers, cfg-gating, and hardware-timing reasoning all
at once. `drivers/net/src/lib.rs` §"YT8531C extended-register RGMII delay
config" (~line 2122).

The YT8531C PHY hides its RGMII timing config behind an **extended-
register** protocol (standard MDIO only exposes 32 registers; vendors
page the rest):

```
write PHY reg 0x1E (PAGE_SELECT) = <extended reg address, e.g. 0xA003>
write/read PHY reg 0x1F (PAGE_DATA) = <value>
```

`0xA003` (RGMII Config 1) packs three delay nibbles:

| bits | field | step |
|------|-------|------|
| 13:10 | RX_DELAY | 150 ps |
| 7:4   | FE_TX_DELAY (100M) | 150 ps |
| 3:0   | GE_TX_DELAY (1G) | 150 ps |

The value is computed from named nibbles so it's readable and tunable
(`lib.rs:2191`):

```rust
#[cfg(feature = "gmac1")]
const YT8531_RC1R_VF2_VALUE: u16 = {
    const RX_DELAY: u16 = 0x0A;   // 10 × 150 ps = 1500 ps
    const FE_TX_DELAY: u16 = 0x5;
    const GE_TX_DELAY: u16 = 0x0;
    (RX_DELAY << 10) | (FE_TX_DELAY << 4) | GE_TX_DELAY   // = 0x2850
};
```

Three lessons a driver author should carry:

1. **RGMII-ID needs ~1.5–2 ns of RX-clock delay** to center RXC in the
   RXD data eye. Too little (the old 300 ps) → a fraction of frames
   sample on the timing boundary and fail CRC. The failure *signature*
   was ping loss swinging 0 %..57 % **boot-to-boot on identical code** —
   if you ever see a metric vary across cold boots with no code change,
   suspect analog timing, not logic.
2. **PHY timing latches at link-up.** Writing the register isn't enough;
   you must force a fresh auto-negotiation. The driver does this via
   `needs_relink = rc1r_pre != rc1r_post` (`lib.rs:2299`) — only kick AN
   when the config actually changed.
3. **Always verify-read.** The `'RC1p'` log tag (`lib.rs:2294`) prints
   the value the PHY *latched*, so the boot log proves the write took
   (`val=0x2850`), not just that you issued it.

---

## 5 · Build → sign → embed → flash

Never `cd drivers/net && cargo build` and expect the kernel to pick it
up. The pipeline (all inside `scripts/build.sh`):

```
1. build net driver wasm  (twice: "vf2 gmac1" and "qemu")   → *.wasm
2. sign each              (scripts/sign-module.rs)          → *.signed.wasm
3. build kernel           (include_bytes! the signed blob)  → wari.bin
4. verify                 (embedded WARI-DRV-BUILD-TAG == build number)
5. publish                (GitHub Release; git tracks build/wari.release)
```

The **stale-driver guard** (`kernel/build.rs`) greps the embedded blob
for `WARI-DRV-BUILD-TAG-<N>` and fails the build if `N` ≠ the kernel's
build number. If that fires, you bypassed `make` — run `scripts/build.sh`,
don't "fix" `build.rs`. This guard exists because builds 107–114 shipped
a stale driver under a fresh banner for a *week* (an inline `asm!` had
silently broken the wasm32 build; cargo reused the last-good blob).

Deploy: `scripts/build.sh <profile> --publish`, then on the board
`wari go` (main) or `wari go-branch <branch>` downloads the release
asset named by `build/wari.release` and flashes it.

---

## 6 · The wasm32 tripwires (learn these the easy way)

| Tripwire | Symptom | Rule |
|----------|---------|------|
| Inline `asm!` in driver code | wasm32 build breaks, cargo silently reuses stale blob, banner lies | Never. Cross the host-fn boundary. |
| Bypassing `make` | kernel embeds a stale driver | Build via `scripts/build.sh`; heed the stale-driver guard. |
| New host-fn call without manifest entry | sign tool refuses the binary | Add the import to *both* the driver `extern` block and the macro's manifest list in `driver-iface/src/lib.rs`. |
| Per-frame `drv_log_u32` on the hot path | ~11 ms RTT floor, RX-ring overflow | Hot-path logging uses `drv_trace_u32` (debug-gated). Boot milestones use `drv_log_u32`. |
| Assuming `#[cfg]` is a runtime branch | confusion | It's a compile-time delete. The other platform's code isn't there. |

---

## 7 · Diagnostics convention

Tag-word logging keeps trace greppable and cheap. `drv_log_u32(tag,
val)` formats `[net:drv] tag=<hex> val=<hex>`; the `tag` is four ASCII
bytes packed into a u32 (`'RC1p'` = `0x5243_3170`). Two channels:

- **`drv_log_u32`** — always on. Boot milestones only (register dumps at
  init, MAC address, milestone markers). Low, fixed cost.
- **`drv_trace_u32`** — `debug-kernel` only, compiled out otherwise. Per-
  event / hot-path (per-frame RX/TX tags). Free on production builds.

The `net-diag` feature adds a periodic 17-register RX-path snapshot
(MMC counters, MTL debug, DMA status) every ~32K `receive()` calls — the
"which layer dropped the frame" one-screen dump. Opt-in via the `trace`
profile.

---

## 8 · Checklist for a new driver

1. New crate under `drivers/<name>/`, `crate-type = ["cdylib"]`,
   `wasm32` target, `#![no_std]`.
2. `[features]` for each platform/variant; keep platform-specific values
   in a cfg-gated `plat` module, shared logic outside.
3. Implement the `wari_driver_iface` trait; invoke the `wari_*_driver!`
   macro.
4. Declare every host-fn import in an `extern "C"` block *and* the
   macro's manifest list.
5. MMIO/MDIO only through host fns. No `asm!`. No raw device pointers.
6. Add a `scripts/build.sh` profile line and (if embedded) a
   `net_blob.rs`-style cfg blob switch in the kernel.
7. `drv_log_u32` for milestones, `drv_trace_u32` for the hot path.
8. Build via `scripts/build.sh`, never bare `cargo build`.
9. An adversarial test for any new trust-boundary surface (per
   `CLAUDE.md`'s security-test rule) before it merges.

---

## Prior art

| Pattern | Source |
|---------|--------|
| WASM modules as drivers | Wari original bet (Singularity/Tock language-isolation lineage) |
| Manifest-declared, signed driver contract | `driver-interface-design.md` (seL4 capability discipline applied to load) |
| Two-blob per-platform build + cfg blob switch | `book/part-3-phase-1a-silicon/ch16-per-platform-drivers.md` |
| RGMII-ID delay / MDIO extended registers | Linux `motorcomm.c` (`ytphy_of_config`), StarFive BSP |
