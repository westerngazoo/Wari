# RFC — The Board Descriptor (audit blocker B3)

> **Status**: proposed 2026-08-16, awaiting architect approval. This is
> the structural refactor the platform-composability audit flagged as
> the single highest-leverage prerequisite for the Orange Pi R2S port.
> Per the Co-Architect Protocol it is not started until approved.
>
> **Scope class**: structural (touches how the kernel, the validator,
> and `build.rs` read platform constants). Pure refactor + host tests —
> **no behavior change on the VF2 or QEMU**, and it does not need the
> R2S plugged in.

---

## 1. The problem, in one sentence

Everything a board *is* — its UART base and register stride, its PLIC
base and hart context, its DRAM origin, its timebase, its MMIO windows —
is scattered across ~15 `cfg`-gated sites and a ~1,250-line inline
`cfg(vf2)` block, so adding a third board means editing all of them and
hoping the constants that *coincide* between QEMU and the VF2 today
(UART base `0x1000_0000`, PLIC base `0x0c00_0000`) happen to be right on
a different SoC. They will not be: the Ky X1 is a different SoC family.

The audit's B1/B5 (silent-fallback → compile error) already landed
(#85). This RFC is B2 + B3: fold the scattered and the coincidental
constants into one per-platform record, so **adding a board becomes
`pub mod r2s { … }` plus one arm** instead of an archaeology exercise.

## 2. The seed we already have

`wari-validate` holds both platforms' NIC MMIO windows as *data*, both
compiled simultaneously, host-tested, with `cfg` confined to two lines
in `kernel/src/validate.rs`:

```rust
pub mod windows {
    pub mod qemu { pub const NET_WINDOWS: &[MmioWindow] = &[ … ]; }
    pub mod vf2  { pub const NET_WINDOWS: &[MmioWindow] = &[ … ]; }
}
```

This is the pattern to widen. It is already the right shape — the
descriptor is the same idea applied to *every* board constant, not just
the NIC windows.

## 3. Proposed design

A pure `BoardDescriptor` in `wari-validate` (host-tested, no `unsafe`),
one instance per platform, selected by exactly one `cfg` in the kernel:

```rust
pub struct BoardDescriptor {
    pub name: &'static str,        // "qemu-virt", "starfive-vf2", "orangepi-r2s"
    pub uart_base: usize,
    pub uart_stride: usize,        // 1 (NS16550) | 4 (DW8250)
    pub plic_base: usize,
    pub plic_hart_context: usize,  // S-mode context for the boot hart
    pub boot_hart_id: usize,
    pub dram_origin: usize,        // linker ORIGIN cross-check
    pub timebase_hz: u64,          // rdtime frequency
    pub net_windows: &'static [MmioWindow],
    // + a per-platform DMA-coherence policy flag (see §6)
}

pub const QEMU: BoardDescriptor = BoardDescriptor { … };
pub const VF2:  BoardDescriptor = BoardDescriptor { … };
// later:  pub const R2S: BoardDescriptor = BoardDescriptor { … };
```

The kernel exposes one selector:

```rust
// kernel/src/board.rs — the ONLY cfg ladder for platform constants
#[cfg(feature = "qemu")] pub const BOARD: &BoardDescriptor = &wari_validate::QEMU;
#[cfg(feature = "vf2")]  pub const BOARD: &BoardDescriptor = &wari_validate::VF2;
```

Every current constant site reads `board::BOARD.field` instead of a
local literal or its own `cfg`. `build.rs`'s linker-script and
driver-blob selection reads one `WARI_PLATFORM` env var instead of
probing `CARGO_FEATURE_VF2` twice (audit B6, folded in).

## 4. Migration — reviewable slices, each behavior-preserving

1. **Define** `BoardDescriptor` + `QEMU`/`VF2` instances in
   `wari-validate`, with a host test asserting each field equals the
   value the kernel uses today (pins the refactor — a wrong transcription
   fails the test, not the board). *Additive; no kernel change.*
2. **Kernel selector** `board::BOARD`; migrate the UART (base+stride),
   PLIC (base+context), and boot-hart constants to read from it. Delete
   the now-dead per-site `cfg`s. *Behavior identical; the compile guards
   from #85 still hold.*
3. **`build.rs`** reads `WARI_PLATFORM`; linker + driver-blob selection
   go through it. *Behavior identical.*
4. **The NIC `plat` block** (audit B4) is explicitly **out of this RFC** —
   it is the ~1,250-line MAC bring-up, and splitting DesignWare-GMAC vs
   k1x-emac behind a trait is its own large, pre-approved brick. This
   RFC makes the *descriptor* exist; the driver split consumes it later.

Each slice is one PR, ≤400 lines, host-tested, with QEMU + VF2 proving
no behavior changed.

## 5. What adding the R2S then costs

`pub const R2S: BoardDescriptor = …` (UART base/stride, PLIC base +
its S-mode context, DRAM origin `0x…`, timebase, NIC windows), one kernel
`cfg` arm, one `build.sh` profile, a linker script. The **YT8531C PHY is
shared with the VF2**, so all the MDIO/RGMII code transfers; only the MAC
layer (B4) is new. Single-hart bring-up (park 7 of 8 cores) is the first
target; full SMP is the multikernel (ADR-001, deferred).

## 6. One field that is a real decision, not a transcription

`BoardDescriptor` should carry a **DMA-coherence policy** flag. The
JH7110 GMAC path is coherent (proven on silicon — the ping bug was a
missing `volatile`, not coherence). The Ky X1's DMA-master coherence is
**unverified**, and it is a per-SoC property. Encoding it as a descriptor
field — with CMO hooks that are no-ops on coherent boards — means the
R2S bring-up flips one flag rather than discovering the hard way. This
is the field most likely to save a week; it is called out because its
value for the R2S is a bring-up experiment, not a datasheet lookup.

## 7. Out of scope

The NIC MAC-driver split (B4, separate pre-approved brick); network
identity from device tree / EEPROM (B7 — the MAC is still a constant);
any actual R2S code (this RFC makes the port *tractable*, it does not
start it); multi-hart (ADR-001).

## 8. The decision requested

Approve this scope (slices 1–3), and I execute it as three small
host-tested PRs, each proving QEMU + VF2 unchanged. Slice 1 is purely
additive and could land first on its own as a low-risk proof of the
shape. B4 (the MAC split) and the R2S constants come after, as their own
approvals.
