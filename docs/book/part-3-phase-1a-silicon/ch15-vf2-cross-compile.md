---
sidebar_position: 15
sidebar_label: "Ch 15 — VF2 Cross-Compile"
title: "Chapter 15 — VF2 Cross-Compile: One Kernel, Two Linkers"
---

Part 2 ended with a sentence that, in hindsight, was a small lie of
omission: *"The Phase-0 demo runs end-to-end."* True. It ran end-to-end
**inside QEMU.** The kernel ELF that produced `Hello from Wari` had
never touched a real RISC-V die, never asked a JH7110 SoC for a single
byte, never been forced to reckon with the fact that QEMU's `virt`
machine is a generous fiction.

This chapter is where the fiction breaks.

PR 8 is the unglamorous structural work that has to land before any of
the satisfying photo-op work in Chapter 17 is even possible: a second
linker script, a build-script switch, a sibling boot path, and a tiny
linker symbol that papers over the most embarrassing surprise of the
whole sprint — that the VisionFive 2 doesn't boot on hart 0.

We don't run on real silicon yet. We just stop *forbidding* it.

:nerdygoose: :sharpgoose: PR 8 is 100% structural. Zero new behaviour.
That is exactly what makes it the right size of PR — surgical, no
bundled cleverness, no "while we're in there." The behavioural change
is PR 10. PR 8's job is to make PR 10 a small diff.

## The shape of "two binaries from one tree"

The constraint we picked: **one kernel source tree, two release
artefacts.** `wari-qemu` and `wari-vf2` come out of the same `cargo
build`, differing only in cargo features and the linker script the
toolchain reaches for. No `#ifdef VF2` mess, no out-of-tree codegen
step, no DTB parser dragged into Tier 0 just to learn what address
to load at.

```
┌──────────────────────────────────────────────────────────────┐
│  kernel/                                                      │
│  ├── linker.ld          ← QEMU virt   (ORIGIN 0x80200000)    │
│  ├── linker-vf2.ld      ← JH7110 SoC  (ORIGIN 0x40200000)    │
│  ├── build.rs           ← picks one based on CARGO_FEATURE_VF2│
│  ├── src/                                                     │
│  │   ├── boot.S         ← one asm file, both platforms       │
│  │   └── platform/      ← cfg-gated constants per board      │
│  │       ├── mod.rs                                           │
│  │       ├── qemu_virt.rs                                     │
│  │       └── vf2.rs                                           │
│  └── Cargo.toml         ← features = { qemu, vf2 }           │
└──────────────────────────────────────────────────────────────┘

  cargo build --features qemu              cargo build --features vf2 \
                                                       --no-default-features
        │                                              │
        ▼                                              ▼
  linker.ld → 0x80200000                       linker-vf2.ld → 0x40200000
  _boot_hart_id = 0                            _boot_hart_id = 1
  UART_REG_STRIDE = 1                          UART_REG_STRIDE = 4
```

Two artefacts. One source. The diff between them is captured at four
sites — and we'll walk through each one.

## Site 1: the linker scripts

The kernel's QEMU linker has been in the tree since Phase 0. It loads
at `0x80200000` because that's where OpenSBI hands control to S-mode
on QEMU's `virt` machine. The new sibling, `linker-vf2.ld`, loads at
`0x40200000` because that's where U-Boot's `bootm` jumps after OpenSBI
on the JH7110.

Same SECTIONS layout. Same exported symbols. Different `MEMORY` block.

```ld title="kernel/linker-vf2.ld — MEMORY"
MEMORY {
    /* JH7110 has 4–8 GB DDR; reserve 256 MB for the kernel image.
     * Boot stack + heap + runtime arena live inside this window. */
    RAM (rwx) : ORIGIN = 0x40200000, LENGTH = 256M
}
```

The "identical SECTIONS layout" point is load-bearing. `kvm.rs` walks
`_text_start` … `_text_end`, `_rodata_start` … `_rodata_end`, etc., and
identity-maps each section with the right permissions. If the symbol
set or order drifted between linker scripts, the MMU bringup would
diverge per platform — exactly the kind of silent skew that turns into
a three-week debugging session in Chapter 17.

We keep the symbol set identical on purpose. The Phase-0 page allocator
walks `[_end, _heap_end)`. The Phase-0b runtime arena lives in
`[_runtime_heap_start, _runtime_heap_end)`. Both linker scripts export
all six symbols. The pure-logic Rust above the linker doesn't know
which board it's on — and doesn't have to.

:sharpgoose: This is the "make impure things look pure" trick at the
linker layer. The MMU code is the same on both platforms because the
data it consumes (linker symbols) is the same on both platforms. The
divergence is captured *once*, in the `MEMORY` block, where it belongs.

### Why a sibling linker, not a templated one?

We considered three alternatives before picking the sibling-script
approach:

- **Templated linker with `m4` / `sed` pre-step.** Rejected — adds a
  build-time codegen pass the toolchain doesn't need today. R8
  (reproducible builds) becomes harder to argue about: now `Cargo.lock`
  + `rust-toolchain.toml` are no longer the only inputs. One `make`
  switch is simpler and traceable.
- **Single linker with `--defsym` for ORIGIN + LENGTH.** Rejected on
  the spot. GNU ld requires `MEMORY` region origins to be link-time
  constants; `--defsym` values do not satisfy that rule. The toolchain
  forced our hand here.
- **Two scripts, one chosen by build flag.** Picked. Matches goose-os's
  layout (proven across ~100 production builds), keeps each script
  readable in isolation, and makes the diff between QEMU and VF2
  memory maps the first thing a reviewer sees.

The cost we accepted: one update has to land in two places when the
section symbol set changes. Both files are short (~95 LoC each), and
cross-referencing comments at the top of each script flag the drift
risk to anyone editing one without the other.

## Site 2: `build.rs` selects the linker

The original Wari `build.rs` passed `-Tlinker.ld` as a bare relative
path via `.cargo/config.toml`. That worked from the crate directory
but broke from the workspace root, and it had no idea about features.
PR 8 promotes it to a real Rust build script:

```rust title="kernel/build.rs — main()"
let dir = std::env::var("CARGO_MANIFEST_DIR")
    .expect("cargo always sets CARGO_MANIFEST_DIR for build scripts");
let script = if std::env::var("CARGO_FEATURE_VF2").is_ok() {
    "linker-vf2.ld"
} else {
    "linker.ld"
};
println!("cargo:rustc-link-arg=-T{}/{}", dir, script);
println!("cargo:rerun-if-changed=linker.ld");
println!("cargo:rerun-if-changed=linker-vf2.ld");
println!("cargo:rerun-if-changed=src/boot.S");
```

Three lines do the work the previous config.toml hack couldn't:

1. `CARGO_MANIFEST_DIR` resolves to an absolute path, so `cargo build`
   from the workspace root and `cd kernel && cargo build` both link
   correctly.
2. The `CARGO_FEATURE_VF2` env var (cargo sets it when `--features
   vf2` is active) picks the linker without a `cfg!` macro that would
   bake the choice into the source tree.
3. `rerun-if-changed` covers all three files that affect the linker
   step. Edit `boot.S`, the kernel relinks; edit `linker-vf2.ld`, ditto.

:nerdygoose: `cargo:rustc-link-arg=-T<path>` and
`cargo:rustc-link-arg=-Tlinker.ld` look identical to a casual reader,
but the absolute path is the load-bearing word. Cargo's link step has
no concept of "the source tree's root"; it cd's into wherever the
build script ran. Bare relative paths in linker args are a common
papercut, and we paid for it once before fixing it here.

## Site 3: the platform module

The third site is where compile-time facts about the board live:

```
kernel/src/platform/
├── mod.rs            — exactly-one-feature enforcement + re-exports
├── qemu_virt.rs      — UART_BASE, UART_REG_STRIDE, UART_MMIO_LEN, ...
└── vf2.rs            — same constants, JH7110 values
```

Each platform file is pure constants. No `unsafe`. No MMIO. No
`static mut`. Per CLAUDE.md §Code Quality #6 (Pure before impure),
this is host-testable Rust: `cargo test --workspace` compiles
`platform/qemu_virt.rs` on x86-64 and asserts the constants are
what we said they were.

`mod.rs` carries one piece of cleverness:

```rust title="kernel/src/platform/mod.rs — feature gate"
#[cfg(not(any(feature = "qemu", feature = "vf2")))]
compile_error!("wari-kernel requires --features qemu or --features vf2.");

#[cfg(all(feature = "qemu", feature = "vf2"))]
compile_error!("wari-kernel accepts only one of --features qemu / vf2.");
```

Two `compile_error!` macros guarantee the kernel never builds with
zero or two platform features active. The first guards the obvious
mistake (`cargo build` with no flags would otherwise silently miss a
`UART_BASE` definition). The second guards the subtler one (a Cargo
feature unification across crates accidentally turning both on).

:angrygoose: A third invariant we wanted — "exactly one platform's
constants visible at any call site" — falls out for free. Because
each constant lives in `qemu_virt` *or* `vf2` (both gated by exactly
one feature), no caller can accidentally `use` the wrong one.
`platform::UART_BASE` is unambiguous.

### The `default = ["qemu"]` decision

`kernel/Cargo.toml` declares `default = ["qemu"]`. That choice has
a single concrete purpose: workspace-wide host commands.

`cargo test --workspace`, `cargo clippy --workspace`, and `cargo
check --workspace` invoke every crate without per-crate feature
arguments. If the kernel crate had no default platform feature, the
workspace pass would trip the `compile_error!` in `platform/mod.rs`
the moment the workspace tried to `cargo check` the kernel.

The cost is that a developer who types `cd kernel && cargo build`
gets a QEMU build silently. The Makefile's real per-platform targets
use `--no-default-features --features qemu` or `--features vf2`
explicitly, so the default never leaks into a release artefact:

```makefile title="Makefile — VF2 cross-compile target"
build-vf2: sign-uart-driver build-hello
	cd kernel && WARI_BUILD=$(NEXT_BUILD) \
	  cargo build --release --features vf2 --no-default-features
```

:weightliftinggoose: We chose this because cargo's `--features`
unification has burned us before. Picking a default that matches the
Phase-0 demo's behaviour means a `git clone && cargo test` works
out of the box. Picking *no* default would break workspace tests for
every contributor on day one. Pure-logic modules deserve to be
ergonomic.

## Site 4: register stride

This is the smallest piece of the platform module and the most
satisfying. QEMU's NS16550A model and the JH7110's DesignWare 8250
expose the same six logical UART registers (THR, IER, FCR, LCR, MCR,
LSR) at the same base address (`0x1000_0000`, by happy coincidence).
The only difference: how many bytes apart consecutive registers sit.

```
QEMU NS16550A (1-byte stride)         JH7110 DW8250 (4-byte stride)
─────────────────────────────         ─────────────────────────────
  0x1000_0000  THR/RBR                  0x1000_0000  THR/RBR
  0x1000_0001  IER                      0x1000_0004  IER
  0x1000_0002  FCR/IIR                  0x1000_0008  FCR/IIR
  0x1000_0003  LCR                      0x1000_000C  LCR
  0x1000_0004  MCR                      0x1000_0010  MCR
  0x1000_0005  LSR                      0x1000_0014  LSR
```

Same registers, different spacing. We capture this once:

```rust title="kernel/src/platform/qemu_virt.rs"
pub const UART_BASE:       usize = 0x1000_0000;
pub const UART_REG_STRIDE: usize = 1;
pub const UART_MMIO_LEN:   usize = 0x8;   // six 1-byte regs, rounded
```

```rust title="kernel/src/platform/vf2.rs"
pub const UART_BASE:       usize = 0x1000_0000;
pub const UART_REG_STRIDE: usize = 4;
pub const UART_MMIO_LEN:   usize = 0x20;  // six 4-byte regs, cache-aligned
```

And consume it once, in `mmio/uart_ns16550.rs`:

```rust title="kernel/src/mmio/uart_ns16550.rs — reg_addr()"
#[inline(always)]
fn reg_addr(index: usize) -> usize {
    UART_BASE + index * UART_REG_STRIDE
}
```

Six register accesses become six `base + index * stride` computations.
Every register addition in the future is a single `const REG_NAME:
usize = N;` line. No per-register conditional. No platform-specific
register table.

We considered an alternative: declare distinct `UART_LSR_OFFSET`,
`UART_IER_OFFSET`, etc., per platform. Rejected — it duplicates the
register table per board. Future register additions would require two
edits, and the register *names* are the same on both platforms; only
the *spacing* differs. A stride captures that one fact, once.

We also considered using 32-bit volatile reads on QEMU (since most
NS16550A implementations also accept word access). Rejected — it
relies on a quirk that is not architectural. Stride-based 8-bit
access is well-defined on both targets and matches the goose-os
driver that has run on real VF2 hardware for ~100 builds.

:mathgoose: The `mul` instruction per register access (1 vs 4) is the
"cost." On QEMU it's literally one instruction the build doesn't
need. Kernel printk runs maybe a few hundred bytes per boot. The
constant-folding optimizer eats most of it. Simplicity won. Cycles
lost: zero we can measure.

### A note on the platform module's history

PR 8 introduced `kernel/src/platform/{mod,qemu_virt,vf2}.rs` with
`UART_BASE`, `UART_REG_STRIDE`, and `UART_MMIO_LEN` as the canonical
home for these constants, plus `PLIC_BASE`, `KERNEL_LOAD_ADDR`, and
`BOOT_HART` looking forward to Phase-1b PRs. A post-PR-8 cleanup pass
walked some of that abstraction back; today the platform constants
live at multiple sites — the `mmio/uart_ns16550.rs` cfg-gated
`UART_REG_STRIDE` you'll see in Chapter 17 is one of those sites,
duplicated again in `drivers/uart/src/lib.rs` (Chapter 16's subject).

We're surfacing this honestly because it matters for two reasons.
First, the "lockstep maintenance" comment in the Tier-2 driver
(Chapter 16) reads as more puzzling without this history — the
duplication exists *because* the kernel-side single source of truth
got partially reverted. Second, anyone reading the PR 8 body and then
the actual tree will spot the discrepancy; we'd rather name it now
than have a future contributor file an issue called "what happened to
the platform module?"

The pure intent of PR 8 — one home for "where does this board put its
UART?" — survives where it matters most: in the linker scripts and
build script. The C-level data flow (`kernel build → bin → flash`) is
clean even where the Rust-side abstraction has been compressed.

:sarcasticgoose: Engineering as it is, not engineering as the PR body
says. Both are real. Both belong in this book.

## Site 5: `_boot_hart_id` and the QEMU-was-lying lesson

The smallest change in PR 8, the one that earns its line in the diff
the most: a single linker symbol that tells `boot.S` which hart should
proceed past the entry trampoline.

```ld title="kernel/linker.ld — _boot_hart_id"
/* QEMU boots on hart 0. boot.S compares a0 against this symbol
 * (loaded via PC-relative .dword) and parks every non-matching
 * hart. Parallel to linker-vf2.ld's `_boot_hart_id = 1;`. */
_boot_hart_id = 0;
```

```ld title="kernel/linker-vf2.ld — _boot_hart_id"
/* VF2 boots on hart 1 (not hart 0 like QEMU). PR 8's boot.S loads
 * this symbol and parks every hart whose id != _boot_hart_id. */
_boot_hart_id = 1;
```

Wait. *Hart 1?*

:surprisedgoose: This was the most surprising fact in the entire
sprint, and it's the reason PR 8 needed to touch `boot.S` at all. The
JH7110 has five RV64 cores: four U74 application cores numbered 1–4,
and an S7 monitor core at hart 0 with no S-mode at all. OpenSBI on
the VF2 launches S-mode on the *first U74 core*, which is hart 1. The
kernel that hardcoded `bnez a0, _park` (the previous Wari `boot.S`,
inherited from the QEMU-only build) would silently park itself on
real silicon — `a0 = 1`, the branch is taken, the only hart that
could run the kernel goes into a `wfi` loop forever.

QEMU was lying to us. Its `virt` machine boots S-mode on hart 0, just
like a textbook RV64 core. Real RV64 SoCs have monitor cores, boot
routing, errata, and reasons to hand S-mode to a specific
application hart. Our `bnez a0, _park` worked on QEMU for *exactly*
the same reason it would silently fail on the JH7110: by accident.

We don't fix the boot.S in PR 8. We just lay the symbolic groundwork.
The two linker scripts now define `_boot_hart_id` — `0` for QEMU, `1`
for VF2 — and PR 8's boot.S edit (~12 LoC delta) loads it via a
PC-relative `.dword` indirection (the why-not-`la` story is Chapter
17's territory; we'll fully unpack the relocation arithmetic when it
matters most). For now: same `boot.S`, two boards, no `cfg!`-conditional
asm.

The symbol that papers over the embarrassment is one line of linker
script per board.

## What "build-vf2" buys us

`make build-vf2` is the new Makefile target PR 8 ships:

```makefile title="Makefile — build-vf2"
# VF2 cross-compile sanity (no flash). Useful before PR 10 deploy.
build-vf2: sign-uart-driver build-hello
	cd kernel && WARI_BUILD=$(NEXT_BUILD) \
	  cargo build --release --features vf2 --no-default-features
```

It does **not** flash a device. It does **not** boot anything. All it
does is prove the kernel ELF builds for `riscv64gc-unknown-none-elf`
with the `vf2` feature active and the `linker-vf2.ld` script in
effect. If `_boot_hart_id` ever drops out of `linker-vf2.ld`, this
target screams.

That's PR 8's contract: cross-compile parity. QEMU still does
everything Part 2 promised it would. VF2 produces a kernel ELF that
links cleanly. Whether that ELF actually *runs* on a JH7110 is PR
10's question — and the answer is "no, not yet" because the Tier-2
UART driver still bakes in a 1-byte register stride. That's PR 9.

## What We Changed

| Site | File | Direction |
|---|---|---|
| Linker (VF2) | `kernel/linker-vf2.ld` | New (~95 LoC) — JH7110 memory map, `_boot_hart_id = 1` |
| Linker (QEMU) | `kernel/linker.ld` | +4 LoC — adds `_boot_hart_id = 0` |
| Build script | `kernel/build.rs` | Picks linker per `CARGO_FEATURE_VF2`; absolute paths |
| Platform module | `kernel/src/platform/{mod,qemu_virt,vf2}.rs` | New — pure constants, `compile_error!` exactly-one-feature |
| Boot asm | `kernel/src/boot.S` | +~12 LoC — PC-relative `_boot_hart_id` load |
| MMIO printk | `kernel/src/mmio/uart_ns16550.rs` | Routes `UART_BASE`/`STRIDE` through `platform::*` |
| KVM glue | `kernel/src/mem/kvm.rs` | `UART_MMIO_BASE` defers to `platform::*` |
| Validator | `kernel/src/validate.rs` | `UART_MMIO_*` defer to `platform::*` |
| Kernel manifest | `kernel/Cargo.toml` | `default = ["qemu"]` for workspace tests |
| Makefile | `Makefile` | `build-vf2` cross-compile sanity target |
| Invariants | `docs/invariants.md` | INV-3 update note (per-platform stride) |

No new `unsafe`. No new INV-N. Phase-0 demo unchanged.

## What's Next

| PR | Chapter | What it unlocks |
|---|---|---|
| PR 9 | [Ch 16](./ch16-per-platform-drivers.md) | Tier-2 UART driver gains `vf2` flavour + `mmio_read8` host fn for the LSR poll loop |
| PR 10 | [Ch 17](./ch17-hello-from-silicon.md) | Deploy harness + `init()` writes the JH7110 needs — first boot on real silicon |

We've cross-compiled but not run. The kernel ELF for the VF2 sits in
`target/riscv64gc-unknown-none-elf/release/wari`, weighing about
two megabytes. Nothing in the world can stop us from objcopying it
to a binary, copying that binary to a board, and pressing reset.

But if we did that today, the board would boot into a kernel whose
Tier-2 UART driver writes to the wrong register offsets. The JH7110
would receive bytes addressed to the QEMU-stride register map and do
nothing visible. We'd see no banner, no error, no signal — just a
power LED and silence.

We don't have a deploy path yet either. That's two PRs away. But the
silent-boot failure mode is exactly why the Phase-1a sprint is three
PRs and not one: each PR closes one of the three reasons first-boot
on the VF2 would fail invisibly.

PR 9 closes the Tier-2 driver one. The Tier-2 UART driver is its own
WASM module — signed, capability-gated, and now needing to know the
register stride at WASM-build time. That's where we go next.
