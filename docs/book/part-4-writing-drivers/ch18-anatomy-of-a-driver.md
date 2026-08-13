---
sidebar_position: 18
sidebar_label: "Ch 18: Anatomy of a Driver"
title: "Chapter 18 — Anatomy of a Tier-2 Driver"
---

# Chapter 18 — Anatomy of a Tier-2 Driver

A driver in most operating systems is the most dangerous code in the
building. It runs in the kernel's own address space, holds the kernel's
own privileges, and — historically — is written by whoever had the
datasheet and a deadline. When a NIC driver corrupts a pointer, the
kernel dies with it.

Wari makes a different bet. A Tier-2 driver is a **WASM module** —
`wasm32-unknown-unknown`, the same instruction set a customer app
compiles to — that the kernel loads, verifies, and runs inside the
`wasmi` interpreter. It is not native kernel Rust. It cannot form a
pointer outside its own linear memory, because the WASM validator
proved at load time that it cannot. The whole architecture rests on that
one sentence: **a driver bug cannot escape the WASM sandbox into the
kernel.** A driver that miscomputes a DMA address gets an `E_INVAL` back
from a host function; it does not scribble on the scheduler.

This chapter is the anatomy lesson. What a driver *is*, the three rules
that fall out of "the driver is WASM," the trait-and-macro contract you
actually write, and — the part most readers come here for — the
`#[cfg]`/features system that compiles one source tree into several
different drivers. The worked example throughout is the network driver,
`drivers/net/src/lib.rs`, the same 3,357-line file that took Wari's
network path from nothing to a stable ping on real silicon.

## What a driver *is*: WASM with capabilities

The two-tier model (Chapter 4) puts customer code in Tier 1 — U-mode,
double-sandboxed by the WASM validator *and* the Sv39 MMU — and system
code in Tier 2. A Tier-2 driver runs in S-mode inside the WASM sandbox
only: no MMU wall separates it from the kernel, because it is not
supposed to need one. Its isolation is *structural*. The WASM type
system guarantees it generates no out-of-linear-memory pointers, and
that guarantee is checked once, at load, before the first instruction
runs.

What makes a driver a driver rather than a customer app is not a
different sandbox — it is a **capability**. Ordinary Tier-1 apps reach
the world through WASI host functions. A driver is handed a narrow extra
grant: the right to touch a specific MMIO window, and (later) to take an
IRQ. It exercises that grant through host functions the kernel imports
into its linker — never through raw pointers to device memory, which it
could not form anyway.

```
Tenant app  ──fd_write──►  Kernel  ──host fn──►  Tier-2 driver (WASM)  ──MMIO host fn──►  hardware
                           (Tier 0)              (Tier 2, sandboxed)
```

So the driver sits behind *two independent gates*, and this is the
defense-in-depth story Chapter 16 tells about the UART: the structural
WASM barrier (no pointer escapes, proven at load) and the kernel's own
capability check plus range validator on every host-function call. Wari
wants the answer to "what addresses can this driver reach?" to be
readable off the signed blob's constant table — six literal addresses
for the UART, a bounded window for the NIC — *and* independently
enforced by Tier-0 code. Either gate alone would hold. Both holding is
the discipline.

## The three hard rules

Three rules follow from "the driver is WASM," and every one of them has
drawn blood in Wari's own build history (the scars are in Chapters 20
and 21).

**Rule 1 — the driver compiles to `wasm32-unknown-unknown`.** No inline
`asm!`, no RISC-V intrinsics, no `core::arch::asm!`. If you find yourself
reaching for a CPU instruction, you are solving the problem at the wrong
layer — cross the host-function boundary instead. This is not a style
preference. Inline assembly does not compile to wasm32, and when the
wasm build fails, cargo will quietly reuse the last artifact that *did*
compile. Builds 107–114 shipped a week's worth of "fixes" against dead
code for exactly this reason (Chapter 20).

**Rule 2 — hardware is touched only through host functions.** The kernel
imports a small set of functions into the driver's linker
(`net_mmio_read32`, `net_mmio_write32`, `nic_attach_queue`,
`lin_mem_base`, and so on). The driver has no pointer to a device
register; it passes an *address* to a host function that is
capability-gated and range-validated before it pokes anything. More on
the mechanics below.

**Rule 3 — the driver is a separate cargo crate, embedded as a signed
blob.** `drivers/net` and `drivers/uart` are their own crates,
`crate-type = ["cdylib"]`, built for a different target than the kernel.
The kernel `include_bytes!`s the *signed* output. You must build through
`scripts/build.sh` (or `make`); building the kernel alone after editing
driver source embeds a stale driver under a fresh-looking banner. The
kernel's `build.rs` now refuses to link a stale blob (Chapter 20), but
the rule stands on its own: never `cd kernel && cargo build` after
touching a driver.

## The contract: a trait and a macro

You do not hand-write the WASM ABI. You implement a trait, and you invoke
a macro. The macro emits the `#[no_mangle] extern "C"` shims the kernel
calls into *and* the signed-manifest bytes that describe them.

At the bottom of `drivers/net/src/lib.rs:3274`:

```rust
/// Tier-2 net driver instance (zero-sized; per-call dispatch).
pub struct Driver;

impl wari_driver_iface::NetDriver for Driver {
    fn start()                       { driver_start(); }
    fn poll(timestamp_ms: u64) -> i32 { driver_poll(timestamp_ms) }
    fn tx_send(buf: &[u8]) -> i32     { driver_tx_send(buf.as_ptr() as u32, buf.len() as u32) }
    fn rx_pop() -> u64                { driver_rx_pop() }
    fn rx_recycle(desc_idx: u32) -> i32 { driver_rx_recycle(desc_idx) }
    // … the socket surface: socket_create, socket_bind, socket_listen, …
}

wari_driver_iface::wari_net_driver!(Driver);   // lib.rs:3313
```

`Driver` is a zero-sized struct; every method is a static dispatch into
the existing `driver_*` functions. The trait — `NetDriver` in
`driver-iface/src/lib.rs:416` — is the *declarative* surface: its method
list is exactly what a net driver must expose, and adding a method is an
ABI change (Chapter 19). Note the small but load-bearing detail in
`tx_send`: `buf.as_ptr() as u32` is the WASM linear-memory *offset*,
because wasm32 pointers are 32 bits wide. The slice the kernel handed in
lives in the driver's own linear memory, and its address is an offset
into it.

The `wari_net_driver!(Driver)` invocation on the last line does two
things. It emits the export shims — `_start`, `poll`, `tx_send`,
`rx_pop`, `rx_recycle`, and the socket calls — which are the functions
the kernel resolves by name and calls. And it emits a
`WARI_DRIVER_MANIFEST` byte array, placed in a WASM custom section named
`wari_driver_manifest`, that declares the driver's *kind*, its *exports*
with their signatures, and the *host-fn imports* it needs. That manifest
is the subject of Chapter 19; for now, know that it is generated, never
written by hand, and that the sign tool refuses to sign a binary whose
manifest and actual code disagree.

The UART driver is the same shape in miniature: `impl
wari_driver_iface::UartDriver for Driver` with a single `write` method
(`drivers/uart/src/lib.rs:135`), then `wari_uart_driver!(Driver)`. One
trait method, one macro line.

## Features and `#[cfg]`: the platform-selection system

Here is the part that looks like magic until it clicks. **One source
tree compiles into several different drivers, and cargo features pick
which one.** There is no runtime `if platform == vf2` anywhere in the
driver. The wrong platform's code is *deleted at compile time*, before
the compiler ever sees a conflict.

There are three distinct verbs here, and keeping them separate is the
whole trick: features are *declared* (in `Cargo.toml`), *set* (by the
build script), and *selected against* (by `#[cfg]` in the source).

### Where features are declared

`drivers/net/Cargo.toml`:

```toml
[features]
default  = ["qemu"]      # a bare `cargo build` targets QEMU
qemu     = []            # VirtIO-net on QEMU virt
vf2      = []            # JH7110 GMAC on the VisionFive 2
gmac1    = ["vf2"]       # target GMAC1 (eth1) not GMAC0 — implies vf2
net-diag = []            # opt-in 17-register RX diagnostic snapshots
```

Read this literally. A feature is a *name* with a list of *other
features it turns on*. `gmac1 = ["vf2"]` means "enabling `gmac1` also
enables `vf2`" — you cannot target GMAC1 without being on the VF2
platform, and cargo enforces that, not a comment. `default = ["qemu"]`
is what a plain `cargo build` gives you; it exists so that
workspace-wide host commands (`cargo test --workspace`, `cargo clippy
--workspace`) can type-check the driver crate without anyone passing
per-crate feature flags. The real builds override it — you will see
`--no-default-features` in the next section, and it matters.

### Where features are set

You almost never type `--features` yourself. `scripts/build.sh` does,
keyed by a build *profile* (`build.sh:84`):

```sh
release) DRV_FEATURES="vf2 gmac1";          KRN_FEATURES="vf2"              ;;
debug)   DRV_FEATURES="vf2 gmac1";          KRN_FEATURES="vf2,debug-kernel" ;;
trace)   DRV_FEATURES="vf2 gmac1 net-diag"; KRN_FEATURES="vf2"              ;;
qemu)    DRV_FEATURES="qemu";               KRN_FEATURES="qemu"             ;;
```

and then builds the driver with (`build.sh:158`):

```sh
cargo build --release --features "$DRV_FEATURES" --no-default-features \
    --target wasm32-unknown-unknown
```

`--no-default-features` is the important half. It *drops* `default =
["qemu"]`, so a VF2 build does not accidentally drag QEMU code along.
The net driver is in fact built **twice** on every run — once with
`vf2 gmac1`, once with `qemu` — and the kernel later `include_bytes!`s
whichever signed blob matches its own platform. So `scripts/build.sh
release` is the reason the RX-delay constant you just edited actually
reaches silicon.

| Feature | Turns on | Set by profile |
|---------|----------|----------------|
| `qemu` | VirtIO-net, MMIO base `0x1000_8000` | `qemu` |
| `vf2` | JH7110 GMAC, DMA rings, PHY init | `release` / `debug` / `trace` |
| `gmac1` | GMAC1 (`0x1604_0000`, PHY @1) instead of GMAC0 | all vf2 profiles |
| `net-diag` | periodic RX-path register snapshots | `trace` only |
| `debug-kernel` *(kernel feature)* | hot-path trace prints fire | `debug` only |

### How `#[cfg]` selects code: the `plat` module trick

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

Three things are happening, and each is a piece of the mental model:

- **Two modules named `plat`.** They do not collide, because `#[cfg]`
  deletes the one whose feature is off *before* the compiler notices two
  modules share a name. Exactly one survives. Everything else in the
  file writes `plat::NIC_BASE` and never learns which platform it is on.
  That is the point: the platform difference is confined to one module,
  and the rest of the datapath is single-source.

- **`#[cfg]` nests.** Inside the `vf2` module, the `#[cfg(not(feature =
  "gmac1"))]` / `#[cfg(feature = "gmac1")]` pair picks GMAC0 vs GMAC1.
  This is *why* `gmac1` implies `vf2` back in the `Cargo.toml` — the
  inner selection only exists inside the `vf2` module, so a GMAC1 build
  that was not also a VF2 build would be nonsense.

- **`#[cfg(not(feature = "…"))]`** means "compile this when the feature
  is OFF." The GMAC0 default and the GMAC1 opt-in are a `cfg` /
  `cfg(not)` pair — the standard idiom for "A, unless the flag says B."

Internalize one sentence and the whole 3,357-line file becomes readable:
**a `#[cfg]` is a compile-time delete.** When you see `#[cfg(feature =
"vf2")]` above a function, read it as "this function does not exist in
the QEMU build." The RGMII RX-delay constant that Chapter 21 spends a war
on lives under `#[cfg(feature = "gmac1")]` at `lib.rs:2172` — it is
literally *absent* from the GMAC0 and QEMU drivers. There is no runtime
branch to trace, because there is no runtime choice.

Two guards make the exactly-one-platform rule loud. At `lib.rs:58`:

```rust
#[cfg(not(any(feature = "qemu", feature = "vf2")))]
compile_error!("wari-driver-net requires --features qemu or --features vf2.");

#[cfg(all(feature = "qemu", feature = "vf2"))]
compile_error!("wari-driver-net accepts only one of --features qemu / vf2.");
```

Neither "no platform" nor "both platforms" compiles. The kernel crate
and the blob-include carry the same exactly-one enforcement (Chapter 16
calls it "three independent guards, same invariant"), so a kernel image
can never embed a driver for the wrong board.

### Adding your own platform or variant

To add a third NIC target: declare it in `[features]` (a bare platform,
or `mynic = ["vf2"]` if it is a VF2 variant); add a `#[cfg(feature =
"mynic")]` arm wherever the base address, register layout, or init
sequence differs — the `plat` module is the natural home for the
constants; add a profile line to `scripts/build.sh` so it can be built;
and, if the kernel embeds it, a `#[cfg]` arm in the blob switch. Keep the
pure datapath *outside* the cfg arms — only the platform-specific values
and sequences should be gated, so the shared logic stays single-source
and host-testable.

## Touching hardware: MMIO and MDIO through host functions

The driver has no pointers to device registers. It calls host functions
the kernel bound into its linker, declared as `extern "C"` imports at
`drivers/net/src/lib.rs:193`:

```rust
#[link(wasm_import_module = "wari")]
extern "C" {
    #[link_name = "net_mmio_write32"]
    fn wari_net_mmio_write32(addr: u32, val: u32) -> i32;   // lib.rs:196
    #[link_name = "net_mmio_read32"]
    fn wari_net_mmio_read32(addr: u32) -> u32;              // lib.rs:201
    #[link_name = "drv_log_u32"]
    fn wari_drv_log_u32(tag: u32, val: u32) -> i32;         // boot milestones (always on)
    #[link_name = "drv_trace_u32"]
    fn wari_drv_trace_u32(tag: u32, val: u32) -> i32;       // hot path (debug-kernel only)
    // … nic_set_mac, nic_attach_queue, nic_queue_notify, lin_mem_base
}
```

Two things to notice. The `#[link_name]` is the *WASM-level* import name
(`net_mmio_write32`); the `wari_` prefix is only the Rust symbol. And the
`addr` argument is an **absolute physical register address** —
`plat::NIC_BASE + offset`, assembled by the caller. The kernel's
`net_mmio_*` host function is capability-gated: it checks that the driver
holds a Net capability covering that MMIO window before it touches the
register, and the validator narrows the reachable range further. A
driver cannot read memory it was not granted; the worst it can do with a
bad address is get `u32::MAX` back.

There is a quiet gift in this design. **CPU fence and ordering semantics
come for free at the wasm→native boundary.** The host-function call is a
natural serialization point — control leaves the interpreter, the native
kernel performs the volatile access, control returns — so the driver
never needs a manual `fence`. That is the deeper reason Rule 1 forbids
inline `asm!`: the one thing you might reach into assembly *for* is
already handled by crossing the boundary.

**MDIO rides on top of MMIO.** The PHY — the physical-layer chip on the
end of the RJ45 — is not on the SoC; it is a separate chip you reach
*through* the MAC's MDIO management block. `mdio_read_phy`
(`lib.rs:1875`) and `mdio_write_phy` (`lib.rs:1912`) drive the DWMAC's
`MAC_MDIO_ADDRESS` (offset `0x200`) and `MAC_MDIO_DATA` (`0x204`)
registers: encode a `(phy_addr, reg)` tuple, set the busy bit, poll for
completion, read the low 16 bits back. Every one of those register
accesses is still a `wari_net_mmio_*` host call. PHY registers are not
MAC registers — the MAC is on-SoC MMIO, the PHY is a chip at the other
end of a two-wire bus — and that distinction is the entire plot of
Chapter 21's first fault.

## Closing hook

You have the anatomy: a driver is WASM with a narrow capability, written
as a trait plus a macro, specialized per platform by compile-time
deletion, and wired to hardware through host functions that fence for
free. But the macro emitted something we have only gestured at — a
manifest, embedded in the binary, that *declares* what the driver
exports and imports, and a sign tool that refuses to bless a binary
whose declaration is a lie.

Chapter 19 opens the envelope: the manifest as the driver's structural
contract, why it is a packed `repr(C)` byte string and not protobuf, the
bidirectional check that makes "ship a manifest that disagrees with my
code" a sign-time error, and where the signature sits in the trust chain
that the kernel walks before your `_start` ever runs.
