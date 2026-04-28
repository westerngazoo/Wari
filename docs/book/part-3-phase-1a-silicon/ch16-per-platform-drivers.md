---
sidebar_position: 16
sidebar_label: "Ch 16 — Per-Platform Drivers"
title: "Chapter 16 — Per-Platform Drivers: Two Signed Blobs, One Source"
---

Chapter 15 closed with a kernel ELF that links cleanly for the
VisionFive 2 and a smug little observation that nothing was stopping
us from flashing it. Then it spoiled the punchline: if we did flash
it, the JH7110 would receive UART writes addressed to the
QEMU-stride register map and produce nothing visible. Power LED.
Silence.

PR 9 closes that gap on the Tier-2 side.

The Tier-2 UART driver is a `.wasm` module — separately compiled,
separately signed, loaded by the kernel via `include_bytes!` and
launched into a `wasmi` instance with the `CAP_MMIO_UART` capability
bit set. Its only superpower is calling two host functions:
`wari::mmio_write8` and (after this PR) `wari::mmio_read8`. Both are
double-gated by capability + range validator. Everything else the
driver does is pure WASM, validated at load time.

The Phase-0 driver baked a 1-byte register stride into its
arithmetic. PR 9's job: make the stride a build-time constant the
WASM module knows about, ship two signed blobs (one per board), and
add the missing read host fn so the driver can actually poll
LSR.THRE before each byte instead of yelling into the void and
hoping for the best.

:nerdygoose: :sharpgoose: There are two interesting decisions in
this PR. One is *why per-platform blobs* (instead of one blob with a
runtime stride argument). The other is *why a `default = ["qemu"]`
feature snuck in here too*. Both have load-bearing answers.

## The architectural fork: one blob or two?

When we sat down to make the Tier-2 UART driver platform-aware, we
had two real options:

**Option A — one blob, runtime stride.** The driver imports a host
fn (or reads a host-set memory cell at startup) that tells it
"stride = 1" or "stride = 4." All MMIO arithmetic is computed at
runtime. One signed `.wasm` ships, runs on both boards.

**Option B — per-platform blobs.** The driver hardcodes
`UART_REG_STRIDE` (and `UART_BASE`) at WASM-build time, gated by
cargo features. Two signed `.wasm`s ship; the kernel
`include_bytes!`s the one matching its own platform feature.

We picked B. Here's the reasoning, the way it actually played out
in design discussion:

```
Option A: one blob, runtime stride
                                            Audit cost
┌──────────────────────────────┐             ─────────
│  Driver linear memory:       │   The driver's MMIO surface, as
│   [stride: u32] ←────────────┤   visible to a code reviewer or
│   ...                        │   formal-verification tool, is
│                              │   "any address the host hands me
│  put_byte(b):                │   through stride * index". That's
│    addr = base + LSR*stride  │   the entire low 4GiB of physical
│    mmio_read8(addr)  ◄───────┤   address space, in principle —
│                              │   the host range-validator catches
│  ⇒ MMIO surface = "wherever  │   the actual writes, but the
│      stride*N happens to     │   *driver-side* surface is
│      land"                   │   unbounded. Rejected.
└──────────────────────────────┘

Option B: per-platform blobs
                                            Audit cost
┌──────────────────────────────┐             ─────────
│  uart-vf2.signed.wasm:       │   Each blob has exactly one MMIO
│   const STRIDE: u32 = 4;     │   surface visible at audit time.
│   const BASE:   u32 = 0x1..; │   A reviewer reads the .wasm,
│                              │   reads the constants, and knows
│  put_byte(b):                │   the entire set of addresses the
│    addr = 0x1...0014  (LSR)  │   driver can ever ask the host to
│    mmio_read8(addr)          │   touch — six of them, hardcoded.
│                              │
│  ⇒ MMIO surface = exactly    │   The kernel-side range validator
│     six fixed addresses      │   is still the security gate, but
└──────────────────────────────┘   the driver is now self-evidently
                                   well-behaved without needing it.
```

:sharpgoose: The deciding factor is *what a reviewer sees when they
audit the signed blob*. Tier-2 modules live above the structural
WASM isolation barrier (no pointer escapes, no kernel VA reads —
that's all the validator's job). But Tier-2 modules talk to MMIO
through capability-gated host fns. We want the audit story for
"what addresses can this driver reach?" to be **the constant table
inside the blob**, not "whatever the host marshals through to it."

Per-platform blobs make the audit story trivial: hash the blob,
disassemble it, count the literal addresses. Six. Done.

The cost we accepted is that the signing pipeline runs twice. The
Makefile handles it:

```makefile title="Makefile — sign-uart-driver"
sign-uart-driver: build-uart-driver
	cargo run --manifest-path scripts/Cargo.toml --bin sign-module -- \
	  build/drivers/uart-qemu.wasm build/drivers/uart-qemu.signed.wasm
	cargo run --manifest-path scripts/Cargo.toml --bin sign-module -- \
	  build/drivers/uart-vf2.wasm  build/drivers/uart-vf2.signed.wasm
```

Two signed blobs in `build/drivers/`. Two corresponding signatures.
A future Phase-1d module-attestation PR catalogs both hashes; for
now they coexist, and the kernel picks one at compile time.

## How the driver knows its stride

The driver's manifest declares two mutually-exclusive features:

```toml title="drivers/uart/Cargo.toml — features"
[features]
default = ["qemu"]
qemu = []
vf2  = []
```

Yes, `default = ["qemu"]`. Same pattern as the kernel crate from
PR 8, same reason: `cargo test --workspace` and `cargo clippy
--workspace` need *some* feature active to dodge the `compile_error!`
in `lib.rs`. The Makefile's real per-platform builds use
`--no-default-features --features vf2` explicitly.

:angrygoose: This decision deserves a footnote because it bit us
mid-PR. The first cut of `drivers/uart/Cargo.toml` had no default
feature — the driver author's instinct was "don't pick a platform
silently." That worked locally but broke `cargo test --workspace`
the moment the workspace tried to type-check the driver crate
without per-crate feature args. Workspace-wide host commands need
defaults; the alternative is rewriting every contributor's mental
model of "I just ran `cargo test`, why is one crate yelling about
features?" Pure-logic ergonomics won.

The constants themselves live in a tiny `mod plat`:

```rust title="drivers/uart/src/lib.rs — plat module"
#[cfg(feature = "qemu")]
mod plat {
    /// NS16550 base on QEMU `virt`.
    pub const UART_BASE: u32 = 0x1000_0000;
    /// 8-bit registers — 1 byte per logical register index.
    pub const UART_REG_STRIDE: u32 = 1;
}

#[cfg(feature = "vf2")]
mod plat {
    /// JH7110 UART0 base.
    pub const UART_BASE: u32 = 0x1000_0000;
    /// DesignWare 8250 — 32-bit-aligned registers (4 bytes per index).
    pub const UART_REG_STRIDE: u32 = 4;
}
```

And the address arithmetic uses them once:

```rust title="drivers/uart/src/lib.rs — addr helpers"
#[inline]
fn lsr_addr() -> u32 {
    plat::UART_BASE + UART_LSR_REG * plat::UART_REG_STRIDE
}

#[inline]
fn thr_addr() -> u32 {
    plat::UART_BASE + UART_THR_REG * plat::UART_REG_STRIDE
}
```

Both functions inline. Both fold to a single 32-bit literal at
WASM-build time after `cargo build --release`. The signed blob's
disassembly contains the literal address `0x1000_0014` for
`lsr_addr` on the VF2 build and `0x1000_0005` on the QEMU build.
Six addresses, hardcoded, audit-trivial.

## The "lockstep maintenance" problem

The platform constants in `drivers/uart/src/lib.rs::plat` *duplicate*
the kernel-side ones in `kernel/src/mmio/uart_ns16550.rs` (and, in
PR 8's original intent, in `kernel/src/platform/{qemu_virt,vf2}.rs`).
Two sources of truth for the same six numbers.

This is structural. A `wasm32-unknown-unknown` cdylib cannot depend
on the `wari-kernel` crate; the targets are different, the linkage
is different, the build pipeline is different. We can't `use
wari_kernel::platform::UART_BASE` from inside a WASM driver any more
than we could `#include <linux/printk.h>` from a userspace program.
The duplication is a structural fact, not a refactor candidate.

:sarcasticgoose: "We could codegen the constants from a TOML file
during the build" — yes, we could, and then we'd have a build-time
codegen pass to model in the R8 reproducible-builds argument, plus
a TOML file to keep in sync, plus a generator script to maintain.
We've added one rope to two-finger-knot the constants together; we
have not made the knot smaller.

What we did instead: a paragraph at the top of `drivers/uart/src/lib.rs`
that names the duplication, names the lockstep maintenance
discipline, and points out that the security argument doesn't
depend on the constants matching:

```rust title="drivers/uart/src/lib.rs — module doc"
//! ## Lockstep maintenance
//!
//! The platform constants below duplicate the kernel's
//! `kernel/src/platform/{qemu_virt,vf2}.rs` exports. The duplication
//! is structural — a `wasm32-unknown-unknown` cdylib cannot depend on
//! the kernel crate. If hardware moves, update both sides in the same
//! PR. The kernel-side validator (`validate::is_uart_mmio_addr`)
//! enforces that the driver only ever writes to the agreed UART
//! window regardless.
```

The validator is the security gate. If the driver and the kernel
disagree about `UART_BASE`, the driver writes to addresses the
validator rejects, the host fn returns `E_INVAL`, and the driver
gets `-2` back instead of bytes-on-wire. The mismatch is loud, not
silent. The lockstep comment exists to prevent the *correctness*
failure (no UART output), not the *security* failure (which the
validator catches independently).

:sharpgoose: Defense in depth. The audit story for "the driver
can't write outside the UART window" rests on two independent
mechanisms: the driver's own constants (audit-checkable in the
signed blob) *and* the kernel's range validator (audit-checkable in
Tier-0 source). Either one alone is sufficient. Both holding is the
discipline.

## The new host fn: `mmio_read8`

Phase 0's Tier-2 host-fn surface was exactly one function:
`wari::mmio_write8(addr, val) -> i32`. The driver could push bytes
into the UART's THR register and trust the QEMU NS16550A model to
transmit them more or less synchronously. Reading anything? Not
allowed. Not needed. Tier-2 was write-only.

This works on QEMU because QEMU's NS16550A model is generous: writes
to THR succeed instantly regardless of LSR.THRE state. The model
"transmits regardless." The driver could fire bytes blindly and they
would all show up.

The JH7110 DW8250 has a real shift register. Bytes pushed to THR
faster than the line drains overrun. The driver *must* poll
LSR.THRE before each byte:

```
   ┌───────────────────────────┐
   │ Tier-2 UART driver (WASM) │
   │                           │
   │  loop:                    │
   │    lsr = mmio_read8(LSR)  ◄─── new in PR 9
   │    if lsr & THRE != 0:    │
   │      break                │
   │  mmio_write8(THR, byte)   │
   │                           │
   └─────────────┬─────────────┘
                 │ host call
                 ▼
   ┌───────────────────────────┐
   │ Kernel host_mmio_read8    │
   │                           │
   │  if !caps.mmio_uart:      │
   │    return u32::MAX        │
   │  if !validate(addr):      │
   │    return u32::MAX        │
   │  read_volatile(addr)      │
   └─────────────┬─────────────┘
                 │
                 ▼
            UART LSR register
```

The new host fn mirrors `mmio_write8`'s gating exactly:

```rust title="kernel/src/runtime/host_fns.rs — host_mmio_read8"
fn host_mmio_read8(caller: Caller<'_, Tier2HostState>, addr: u32) -> u32 {
    let host = caller.data();

    if !host.caps.mmio_uart {
        return u32::MAX;
    }
    if !validate::is_uart_mmio_addr(addr as usize) {
        return u32::MAX;
    }

    // SAFETY: INV-3 (validator-narrowed MMIO address) + capability
    // check above. The 8-bit read of a UART register is non-mutating
    // and well-defined for the entire NS16550/DW8250 register window.
    let byte = unsafe { core::ptr::read_volatile(addr as usize as *const u8) };
    byte as u32
}
```

The capability check is the same `mmio_uart` bit `mmio_write8` uses.
The range check is the same `validate::is_uart_mmio_addr`. The
unsafe block cites the same INV-3. This is intentional symmetry —
"read" and "write" are the same trust-boundary crossing, just with
opposite data direction.

### The `u32::MAX` sentinel

One small ABI debt PR 9 takes on knowingly: `mmio_read8` returns
`u32::MAX` (`0xFFFFFFFF`) on permission failure or range failure.

```rust title="kernel/src/runtime/host_fns.rs — sentinel rationale"
/// **Sentinel**: returns `u32::MAX` on permission or range failure. A
/// legitimate UART status read would not produce `0xFFFFFFFF`, but the
/// driver should treat this value as "stop polling" defensively. A
/// richer error encoding lands when the ABI gains result-tuple shapes
/// (Phase 2+).
```

Why a sentinel and not a `Result`? Because the WASM ABI for a host
fn returning a single `i32` or `u32` is what wasmi natively supports.
Result-tuple returns (returning `(value, error)` pairs through
multi-value returns or through a memory-cell convention) need ABI
plumbing we haven't built yet. Phase-0 Tier-2 is one host fn; Phase
1a Tier-2 is two; Phase 2 will probably refactor the whole ABI to a
richer error encoding once we have more than two host fns to share
it across.

For now: `u32::MAX` is the sentinel. The driver's `put_byte` reads
LSR in a tight loop and treats `0xFFFFFFFF` as "permission denied"
implicitly — the THRE bit (`0x20`) won't ever be set in a value of
`0xFFFFFFFF`, but the bit *would* be set in many *legitimate* LSR
reads. So a paranoid driver should mask and compare; ours, today,
just spins until THRE.

:mathgoose: A real attacker scenario: a malicious signed Tier-2
driver loaded without `CAP_MMIO_UART` calls `mmio_read8` in a tight
loop and gets `u32::MAX` every time. Its `lsr & 0x20 != 0` test
evaluates true (because `0xFFFFFFFF & 0x20 != 0`), so it falls
through to `mmio_write8`, which also returns an error. No bytes go
out. The driver wedges in an infinite loop of denied reads and
denied writes. It does not, importantly, leak any information or
access any address it doesn't have. The sentinel is *correctness*
debt, not *security* debt.

We document the debt in the host fn doc comment and move on. ABI
v2 fixes it.

## The kernel-side blob switch

One file changes shape in `runtime/`:

```rust title="kernel/src/runtime/uart_blob.rs — cfg-gated includes"
#[cfg(feature = "qemu")]
pub static UART_DRIVER_SIGNED: &[u8] =
    include_bytes!("../../../build/drivers/uart-qemu.signed.wasm");

#[cfg(feature = "vf2")]
pub static UART_DRIVER_SIGNED: &[u8] =
    include_bytes!("../../../build/drivers/uart-vf2.signed.wasm");
```

Two `include_bytes!`, one cfg-gate per platform feature. The kernel
binary contains exactly one signed blob — the one matching its
platform. The other blob still sits in `build/drivers/` after
`make sign-uart-driver`, but only one ever gets linked into the
kernel image.

This means the QEMU kernel ELF contains the QEMU-flavoured signed
blob (1-byte stride hardcoded) and the VF2 kernel ELF contains the
VF2-flavoured signed blob (4-byte stride hardcoded). Mismatched
combinations are unrepresentable: cargo's feature unification
guarantees exactly one of `qemu`/`vf2` is active at any given
build, and the cfg gates pick the matching blob.

:sharpgoose: This is the third "exactly one" enforcement on the
chain. The kernel crate enforces exactly-one-platform via
`compile_error!`. The driver crate enforces exactly-one-platform via
`compile_error!`. The blob include enforces exactly-one-blob via
cfg. Three independent guards, same invariant: a kernel binary
can never accidentally embed the wrong driver.

## A small Makefile complication

`make sign-uart-driver` now does double duty:

```makefile title="Makefile — build-uart-driver"
build-uart-driver:
	mkdir -p build/drivers
	# QEMU variant
	cd drivers/uart && cargo build --release --features qemu --no-default-features
	cp target/wasm32-unknown-unknown/release/wari_driver_uart.wasm \
		build/drivers/uart-qemu.wasm
	# VF2 variant
	cd drivers/uart && cargo build --release --features vf2 --no-default-features
	cp target/wasm32-unknown-unknown/release/wari_driver_uart.wasm \
		build/drivers/uart-vf2.wasm
```

Two consecutive `cargo build` invocations of the same crate, with
different features. Cargo handles the feature flip cleanly because
each `--features` change invalidates the build cache for the
affected target — small thrash, no correctness risk.

The Makefile's `build` and `build-vf2` targets both depend on
`sign-uart-driver`. So whether the developer is running the
Phase-0 QEMU smoke test or cross-compiling for the VF2, both
signed blobs are fresh in `build/drivers/` before the kernel
link's `include_bytes!` resolves.

## The reverted-and-restored business

PR 9 has an awkward bookkeeping detail worth surfacing because it
ties to Chapter 15's "platform module history" footnote. After PR 8
merged, a cleanup pass walked back three of PR 8's structural
fixes: the `build.rs` platform-aware linker selection, the
`linker-vf2.ld` script itself, and the Makefile's `RUSTFLAGS`
removal. That cleanup left the VF2 build path broken.

PR 9 needed VF2 cross-compile to verify the per-platform driver
worked end-to-end. So PR 9 *re-applied* those three pieces — not as
new work, but as restoration of PR 8's intent. The boot.S hart-id
mechanism (PR 8's other structural fix) was *not* re-applied in PR
9 because PR 9 doesn't run on real silicon; the hart-0 hardcode
still works fine in QEMU. That left the boot.S restoration as a
known follow-up for PR 10.

:sarcasticgoose: This is what the truth-on-the-ground engineering
log looks like, and the book is poorer if it pretends every PR
landed clean. Cleanup passes sometimes overcorrect. Subsequent PRs
sometimes have to undo prior undos. The discipline is to **name
the bookkeeping in the PR body** so the next reader knows why a
"new" change touches files the PR's nominal subject shouldn't be
near.

PR 10's body opens with the same disclosure: "restoring the boot.S
PC-relative `_boot_hart_id` mechanism so VF2 (hart 1) is selected
as boot hart." Same restoration discipline, applied where it bites
hardest.

## What We Changed

| Site | File | Direction |
|---|---|---|
| Driver manifest | `drivers/uart/Cargo.toml` | `qemu`/`vf2` features, `default = ["qemu"]` |
| Driver source | `drivers/uart/src/lib.rs` | `mod plat` cfg-gated, `mmio_read8` import, `put_byte` polls LSR |
| Driver host fn | `kernel/src/runtime/host_fns.rs` | New `host_mmio_read8`, `u32::MAX` sentinel on denial |
| Driver blob | `kernel/src/runtime/uart_blob.rs` | cfg-gated `include_bytes!` per platform |
| Makefile | `Makefile` | `build-uart-driver` + `sign-uart-driver` build both variants |
| Restored from PR 8 | `kernel/build.rs`, `kernel/linker-vf2.ld`, Makefile | Re-applied — post-PR-8 cleanup had reverted them |
| Invariants | `docs/invariants.md` | INV-3 narrowed range applies to `mmio_read8` too |

No new INV-N. No new `unsafe` outside the documented `host_mmio_read8`.
QEMU smoke test (`make test`) passes byte-for-byte unchanged.

## What's Next

| PR | Chapter | What it unlocks |
|---|---|---|
| PR 10 | [Ch 17](./ch17-hello-from-silicon.md) | Deploy harness + boot.S hart-id restoration + DW8250 init writes — first boot on real silicon |

After PR 9, both signed blobs sit in `build/drivers/`:
`uart-qemu.signed.wasm` and `uart-vf2.signed.wasm`. The kernel cross-
compiles for the VF2 with `make build-vf2` and links cleanly. The
Phase-0 QEMU demo still runs, byte-for-byte unchanged.

But two things still stand between us and `Hello from Wari` on COM7:

1. The kernel's `boot.S` still hardcodes `bnez a0, _park`. On the
   VF2 (`a0 = 1` at S-mode entry), the boot hart parks itself
   immediately. PR 10 has to restore the `_boot_hart_id` PC-relative
   load.

2. The kernel's `mmio/uart_ns16550.rs::init()` is currently a
   no-op. The DW8250 needs the IER/LCR/FCR/MCR sequence written
   before bytes pushed to THR will reach the line. PR 10 has to
   cherry-pick the sequence from goose-os and wire it through the
   stride-aware `reg()` helper.

Plus the entire deploy harness — `make deploy`, `wari go`, the
shell function on the device, the GitHub-mediated flow. None of
that exists yet.

PR 10 lands all four. It's the widest PR of the sprint and the one
that, when it merges, produces the only output this whole book has
been building toward.

Chapter 17 is the celebration.
