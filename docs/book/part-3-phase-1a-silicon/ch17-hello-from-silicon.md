---
sidebar_position: 17
sidebar_label: "Ch 17 — Hello from Silicon"
title: "Chapter 17 — Hello from Silicon: First Boot on Real Hardware"
---

Chapter 16 closed with two known reasons why the VF2 kernel — fully
cross-compiled, fully linked, sitting in
`target/riscv64gc-unknown-none-elf/release/wari` — would still boot
silently on real hardware. Boot hart parks itself. UART driver writes
to THR with no FIFO and no OUT2, bytes get dropped. Plus the small
matter that we had no way to *get* the kernel onto the device.

PR 10 fixes all three. In one PR. Larger than the 100–400 LoC sweet
spot we usually aim for (~350 LoC across shell + asm + Rust +
Makefile + docs), and we did it on purpose, and we'd do it again.

This is the chapter that justifies every prior chapter of this book.

:happygoose: :weightliftinggoose: :nerdygoose: :sharpgoose: We are
going to walk through every layer between a developer typing `make
deploy` on Friday afternoon and a sentence appearing on a serial
console hooked to COM7. Nine layers. One sentence. Forty thousand
words of book to get here. Strap in.

## Why one PR, not three

The Phase-1a sprint discipline was "small PRs, one concept each."
PR 8 was structural (linker + platform + boot symbol). PR 9 was
Tier-2 (per-platform driver + read host fn). PR 10 should, by that
discipline, have been three PRs: deploy harness, boot.S hart-id
restoration, UART init writes.

We bundled them. The reasoning, copied from the PR body because it
crystallised after the fact:

> The deploy harness exists to produce a bootable VF2 image. Merging
> the harness without the boot.S fix means the first `wari go` after
> merge boots into a kernel where the boot hart parks itself; merging
> the harness + boot.S without the UART init means the kernel runs
> but produces no serial output on the JH7110 (DW8250 idle out of
> reset). Either intermediate is worse than the pre-PR-10 state
> because the device has been flashed and the operator has no signal
> whether the silence is silicon, firmware, or kernel.

:angrygoose: This is the case where "small PRs" hurts. Each piece in
isolation produces a *worse* failure mode than the status quo
(silent device, ambiguous fault). Together they produce the *only*
working state. The right unit of work is the smallest one whose
intermediate states aren't actively misleading.

We named it explicitly in the PR body, took the size hit knowingly,
and merged. Three failure modes had to land together because each
one alone is invisible.

## Layer 1: the deploy harness

Start at the top of the funnel — the developer's terminal. PR 10
adds two surfaces: `make deploy` on the dev machine and `wari go`
on the device.

### `make deploy` — push from dev

```makefile title="Makefile — deploy"
deploy: kernel-vf2
	git add $(DEPLOY_FILES)
	git commit -m "Build $(NEXT_BUILD) (wari deploy)" --allow-empty || true
	git push
	@echo ""
	@echo "========================================="
	@echo "  DEPLOYED: build $(NEXT_BUILD)"
	@echo "========================================="
	@echo "  On the VF2 ($(VF2_IP)), run:"
	@echo "      wari go"
	@echo "  Then watch the COM7 serial console for:"
	@echo "      Wari v0 build $(NEXT_BUILD) boot OK, hart 1"
	@echo "========================================="
```

The flow is: build the VF2 kernel binary (`build/wari.bin`), commit
it along with the source tree that produced it, push to GitHub,
then print a banner telling the operator what to type on the
device and what to watch for on the serial console.

Three things deserve unpacking here.

**First**, `build/wari.bin` is committed. Wari's `.gitignore`
excludes the entire `build/` directory by default — those are
intermediate artefacts, not source. We add an exception for
`build/wari.bin` because the device pulls *it*, not the source. The
binary is the deploy artefact.

This is mildly heretical. Most repos would push the binary to a
release artifact server, S3, or a CDN. We push it to git-tracked
files in the same repo, on the same branch. The reasoning:

- **R8 reproducibility extends to the device.** Every flashed
  kernel is a git commit. An operator on the device can `git log
  /boot/kernel.bin` and recover the exact source state of any
  prior boot. No "what was running on this board last Tuesday?"
  detective work.
- **Subnet-diversity tolerance.** The dev machine and the VF2
  currently sit on different DHCP subnets (dev on `10.0.x`, VF2 on
  `192.168.86.236`). Direct SSH does not route between them.
  GitHub does. Both sides need internet only, no host-to-host
  routing.
- **Audit trail by construction.** The git history *is* the deploy
  log. Every `make deploy` creates a commit named after the build
  number. `git log --oneline -- build/wari.bin` is the device's
  flash history.

:sharpgoose: The cost is repo bloat (~2 MB per build × N builds).
At Phase 1a's deploy cadence — manual, maybe weekly — that's a
rounding error. Phase-1d adds `git lfs` for binary artefacts if
the cadence increases. For now, push it.

**Second**, the terminal banner is the operator's hand-off. This
is small, easy to overlook, and exactly the kind of detail that
matters at 9 PM on a Friday when the developer is alone with the
hardware:

```
=========================================
  DEPLOYED: build 12
=========================================
  On the VF2 (192.168.86.236), run:
      wari go
  Then watch the COM7 serial console for:
      Wari v0 build 12 boot OK, hart 1
=========================================
```

The banner answers three questions before the operator asks them:
*Did it deploy?* *What do I run on the device?* *What should I
expect to see?* That last one is the most important. Without it,
the operator stares at an empty serial console and doesn't know
whether to keep waiting or start debugging.

:happygoose: This is the "what does done look like?" discipline,
applied at the operator level. Define the success criterion in
the same place that initiates the action.

**Third**, `--allow-empty || true` lets `make deploy` succeed
even when the binary hasn't changed. Useful when redeploying a
known-good build to a freshly-flashed SD card, or when the deploy
itself failed mid-push and you're retrying.

### `wari go` — pull from device

The device-side script is a Bash function sourced from `.bashrc`,
ported from the goose-os flow that produced ~100 production flashes
without a regression. Wari rebrands it and adds two subcommands;
otherwise, it's the same shape:

```bash title="scripts/wari-upgrade.sh — go subcommand"
upgrade|up)
    echo "=== Wari Upgrade ==="
    cd "$WARI_DIR" || { echo "ERROR: $WARI_DIR not found"; return 1; }
    echo "Pulling latest..."
    git pull || { echo "ERROR: git pull failed"; return 1; }
    local build=$(cat .build_number 2>/dev/null || echo "?")
    echo "Copying wari.bin (build $build) to /boot/kernel.bin..."
    cp build/wari.bin /boot/kernel.bin || { echo "ERROR: cp failed"; return 1; }
    ;;
go)
    # upgrade + reboot in one shot — the everyday flow
    wari upgrade && wari reboot
    ;;
```

The full subcommand set: `upgrade` (pull + cp), `go` (upgrade +
reboot, the everyday flow), `reboot`, `status` (show build info +
recent commits), `demo` (status + reboot, **no pull** — for live
presentations where you want what's currently deployed, not a race
with a fresh push), `boot-log` (Debian dmesg tail with a pointer
to COM7 since Wari output never reaches Debian's kernel log
buffer), and `help`.

:nerdygoose: `WARI_DIR=/root/wari` is hardcoded. The script lives
under root because it does `cp ... /boot/kernel.bin && reboot` —
operations that root unconditionally needs. The threat model is
"single trusted dev, single VF2 on a developer's network." Phase
1+ adds a signature check before the `cp` step (verifying the
build signature against a key burned into `/etc/wari/trust.pub`).
Today: the dev's GitHub credentials are the trust root.

### Why `/boot/kernel.bin`, not `/boot/wari.bin`?

Small, deliberate decision. The on-disk filename misrepresents the
contents to a casual `ls /boot` — it says "kernel" but it's
specifically the Wari kernel. The reason we kept the goose-os name:

> U-Boot config rewrites are a separate failure mode (typo +
> reboot = brick → re-flash SD card). Phase-1b introduces a
> U-Boot menu (dual-boot Wari/Debian) and that PR owns the
> `extlinux.conf` change as its primary concern.

`/boot/extlinux/extlinux.conf` on the VF2 already says `kernel
/boot/kernel.bin` because that's what goose-os put there.
Renaming the binary in PR 10 would mean editing extlinux.conf in
PR 10. A typo in extlinux.conf bricks the board into a U-Boot
prompt the operator can't easily exit. We do the rename in
Phase-1b, in the PR that owns the U-Boot menu work, where touching
extlinux.conf is the *primary* concern.

:sarcasticgoose: "It's confusing in a directory listing" is a real
cost. "It might brick the board" is a real cost. The bigger one
loses. Documented in `docs/vf2-bringup.md` with one sentence and
moved on.

## Layer 2: the boot.S hart-id mechanism, in detail

Chapter 15 introduced `_boot_hart_id` as a linker symbol that
papers over the QEMU-vs-VF2 boot-hart difference. This chapter
unpacks the asm. PR 10 restores the implementation that PR 8 had
introduced and the post-PR-8 cleanup had walked back.

The naive form, the one a freshly-onboarded engineer would write:

```asm
# DOES NOT WORK
_start:
    la      t0, _boot_hart_id
    ld      t1, 0(t0)
    bne     a0, t1, _park
```

`la` (load address) on RISC-V expands to `auipc + addi` — load the
upper 20 bits of a PC-relative offset, add the lower 12. Both
instructions use 32-bit relocations: `R_RISCV_PCREL_HI20` and
`R_RISCV_PCREL_LO12_I` respectively. Both have a ±2 GiB PC-relative
reach.

That reach is enormous when the target symbol is a normal address
(somewhere in `.text` or `.data`, near the PC). It is *zero* when
the target symbol is an *absolute* value defined in the linker
script — like `_boot_hart_id = 0` or `_boot_hart_id = 1`. Absolute
symbol values sit at addresses 0x0 or 0x1 in the linker's
worldview, which are nowhere near any PC the kernel will ever have.

The link fails:

```
relocation truncated to fit: R_RISCV_PCREL_HI20 against symbol `_boot_hart_id'
```

The fix uses an indirection through a `.dword` storing the absolute
value next to the boot code:

```asm title="kernel/src/boot.S — hart-id selection"
.section .text.entry
.global _start

_start:
    # --- Park non-boot harts ---
    # Compute address of the inline _boot_hart_id_addr constant
    # PC-relative, then load the absolute hart-id value through it.
1:  auipc   t0, %pcrel_hi(_boot_hart_id_addr)
    ld      t1, %pcrel_lo(1b)(t0)
    bne     a0, t1, _park

    # ... (zero bss, set sp, call kmain)

# --- Boot configuration data ---
# Stored next to boot code so `auipc + ld` reaches it PC-relative.
# The .dword uses R_RISCV_64 (no range limit) to embed the absolute
# value of the linker-defined `_boot_hart_id` symbol.
.align 3
_boot_hart_id_addr:
    .dword  _boot_hart_id
```

Two key tricks:

1. **`_boot_hart_id_addr` is a normal symbol** (it sits at a real
   PC-relative address, inside `.text.entry`). `auipc + ld` reaches
   it cleanly — short-distance PC-relative load.
2. **The `.dword _boot_hart_id` uses `R_RISCV_64`**, a 64-bit
   absolute relocation with no range limit. The linker writes the
   absolute value of `_boot_hart_id` (0 on QEMU, 1 on VF2) into
   the `.dword` slot at link time.

So at boot, the sequence is:

- `auipc t0, %pcrel_hi(_boot_hart_id_addr)` — load high 20 bits of
  the PC-relative address of the .dword into t0.
- `ld t1, %pcrel_lo(1b)(t0)` — load 8 bytes from `(t0 + low12)`,
  which is the .dword. t1 now holds the absolute hart-id value.
- `bne a0, t1, _park` — branch to park if the SBI-supplied hart id
  in `a0` doesn't match.

```
                      RAM at runtime
   ┌──────────────────────────────────────────┐
   │ .text.entry:                              │
   │   _start:                                 │
   │     auipc t0, %pcrel_hi(...)  ┐           │
   │     ld    t1, %pcrel_lo(1b)(..)│ short PC │
   │     bne   a0, t1, _park       │ relative │
   │     ...                       │          │
   │   _boot_hart_id_addr:         ◄          │
   │     .dword (linker fills 0 or 1)         │
   │   _park:                                  │
   │     wfi                                   │
   │     j _park                               │
   └──────────────────────────────────────────┘
```

:nerdygoose: This is exactly the pattern goose-os used across ~100
production builds. We considered three alternatives:

- **`la _boot_hart_id` directly.** Doesn't link, as above.
- **Per-platform `bnez a0, _park` via `cfg!`.** Doesn't work in
  `boot.S` — it's GAS-included via `global_asm!`, not Rust. Cargo
  features don't reach into asm without a `build.rs` codegen pass.
- **Build-time const injection via `build.rs`.** Possible, but
  cargo's asm-source mechanism does not cleanly support build-script-
  generated constants without writing a `.S` file out and including
  it. Substantially more plumbing for the same outcome.

The `.dword` indirection costs 8 bytes of `.text.entry` per kernel
and one extra `ld` instruction per boot — which means it adds
roughly *one nanosecond* to boot latency on the slowest U74. Both
costs are noise. The PC-relative-load-of-an-absolute-symbol
pattern is the right level of abstraction.

## Layer 3: the UART init that QEMU let us skip

Phase 0 had an `init()` function in `mmio/uart_ns16550.rs` that
was, structurally, a no-op. The QEMU NS16550A model accepts writes
to THR regardless of the rest of the register state — no FIFO
configuration, no line-control setup, no modem-control bits. You
push bytes, they show up on stdout. The model is a generous lie.

The JH7110 DW8250 is not a generous lie. Out of reset (and after
U-Boot's NS16550 driver disables FIFOs and drops OUT2 on its way
out), the device is in a state where bytes pushed to THR are
either dropped or never reach the line driver. The `wari go` flow
would complete, the kernel would boot, and the COM7 console would
stay empty — silent boot.

PR 10 cherry-picks the goose-os init sequence verbatim (byte
values; the *structure* uses Wari's typed `VolatilePtr<u8>` via
the `reg()` helper, not raw `ptr::write_volatile`):

```rust title="kernel/src/mmio/uart_ns16550.rs — init()"
pub fn init() {
    // SAFETY: INV-3. Each `reg(i)` returns a typed wrapper for a
    // fixed NS16550/DW8250 register (THR..LSR); writes are hardware
    // register operations, not arbitrary memory access.
    unsafe {
        // Disable all interrupts during setup.
        reg(IER_REG).write(0x00);
        // 8N1.
        reg(LCR_REG).write(LCR_8N1);
        // FIFOs: enable + clear, 1-byte RX trigger.
        reg(FCR_REG).write(FCR_FIFO_RESET);
        // Modem control: DTR + RTS + OUT2.
        reg(MCR_REG).write(MCR_DTR_RTS_OUT2);
        // Re-enable RX-available interrupt to match the goose-os
        // proven sequence. TX stays poll-driven (ETBEI clear).
        reg(IER_REG).write(IER_RX_AVAIL);
    }
}
```

Five writes:

| Step | Register | Value | Purpose |
|---|---|---|---|
| 1 | IER  | `0x00` | Disable all interrupts during setup |
| 2 | LCR  | `0x03` | 8 data bits, 1 stop, no parity (8N1) |
| 3 | FCR  | `0x07` | Enable + clear both FIFOs, 1-byte RX trigger |
| 4 | MCR  | `0x0B` | DTR + RTS + OUT2 — gate IRQs to PLIC; DTR/RTS for RX |
| 5 | IER  | `0x01` | Re-enable RX-data-available interrupt |

The MCR write is the load-bearing one. OUT2 gates the UART's IRQ
output to the PLIC, but more importantly on the JH7110 it's part
of the line driver enable — without it, bytes written to THR don't
reach the wire. DTR + RTS together are what keeps the line in a
state where the receiver acknowledges incoming data; goose-os
discovered the hard way (around build 30) that *transmit* on the
JH7110 is sensitive to DTR/RTS too because the line driver shares
state with the modem-control pins.

QEMU's NS16550A model ignores all five writes. It transmits
regardless. So the QEMU regression test still produces byte-for-
byte identical output before and after PR 10:

```
Wari v0 build N boot OK, hart 0
[kvm] heap ...
mmu OK, traps installed
tier-2 uart driver loaded
Hello from Wari
[hello] exit(0)
```

On the JH7110, those same five writes are the difference between
a silent boot and the same banner showing up on COM7.

:sharpgoose: This is the "platform abstraction has no leakage" win
in concrete. The `init()` function does not know whether it's
talking to a 1-byte-stride QEMU model or a 4-byte-stride DW8250.
It calls `reg(IER_REG).write(0x00)`, the `reg_addr` helper does
`UART_BASE + IER_REG * UART_REG_STRIDE`, and the right register
gets written. Five identical lines of Rust, two different physical
register addresses, both correct.

The cost: four extra MMIO writes on every boot of every platform.
On QEMU, each write is a few nanoseconds. On the VF2, the whole
init sequence completes in well under a microsecond. Not
measurable next to the cost of the kernel-printk banner that
follows it. Free.

## The Moment

The dev machine, Friday afternoon, terminal open:

```bash
make deploy
```

About forty seconds of `cargo build`, an `objcopy` to produce
`build/wari.bin`, a git commit, a git push. Terminal prints:

```
=========================================
  DEPLOYED: build 12
=========================================
  On the VF2 (192.168.86.236), run:
      wari go
  Then watch the COM7 serial console for:
      Wari v0 build 12 boot OK, hart 1
=========================================
```

Switch to the SSH session into the VF2 (Debian Bookworm, root
shell). Type:

```bash
wari go
```

The function pulls, copies `build/wari.bin` to `/boot/kernel.bin`,
prints `Build 12 ready in /boot/kernel.bin`, and reboots. The SSH
connection drops as the device shuts down.

Switch to the COM7 serial console (PuTTY, screen, minicom — the
operator's choice). Watch.

OpenSBI banner scrolls past. U-Boot banner scrolls past. U-Boot
counts down its boot timer, executes `bootm`, jumps into
`/boot/kernel.bin` at `0x40200000`. PC lands in `_start`.

Then this:

```
Wari v0 build 12 boot OK, hart 100000 ...
[kvm] heap 0x402b4000 - 0x402c4000 (16 pages)
[kvm] root pt at 0x402b4000
mmu OK, traps installed
tier-2 uart driver loaded
Hello from Wari
[hello] exit(0)
```

:happygoose: :weightliftinggoose: :happygoose: :weightliftinggoose:
:happygoose:

That output is real. Verbatim. The exact characters that appeared
on the COM7 console of a VisionFive 2 sitting on a developer's
desk in April 2026, after a `wari go` flashed build 12.

We were going to go straight to the celebration but there is one
small embarrassment to name first: `hart 100000` is wrong. The
boot hart on the VF2 is hart 1, not hart 100000. The number is a
`kprintln!` formatting bug — somewhere in the banner format string
or the integer decoder, we're printing the value with the wrong
base or padding, and `1` came out as `100000`. The actual `a0`
value at boot is correct (the kernel got past the `bne` check; if
`a0` had been `100000`, the hart would have parked itself in the
`_park` loop and never printed anything). The bug is cosmetic, in
the print path, not in the kernel logic.

It's a Phase-1b cleanup item. Logged. Will be a one-line fix in
some `kprintln!`-related module the moment we look at it.
Acknowledged here so future readers don't think we were boasting
about supporting one hundred thousand harts.

The other small thing: `[hello] exit(0)` is indented one space
relative to the other lines. Same printk-formatting cleanup; the
exit message and the rest of the banner went through different
formatting paths and they don't quite agree on column-zero. Phase
1b housekeeping.

Two cosmetic bugs. Zero kernel bugs. The kernel **boots on real
silicon**. The whole 9-layer chain works.

## The 9-layer chain, made concrete

Here's what just happened, layer by layer, between the `wari go`
keystroke and `Hello from Wari` appearing on COM7:

```
┌────────────────────────────────────────────────────────────────┐
│ 1. OpenSBI (M-mode firmware on JH7110 ROM)                     │
│    - Initializes the SiFive S7 monitor and U74 cores            │
│    - Hands S-mode control to U-Boot at 0x40200000               │
├────────────────────────────────────────────────────────────────┤
│ 2. U-Boot                                                       │
│    - Reads /boot/extlinux/extlinux.conf                         │
│    - Loads /boot/kernel.bin into RAM at 0x40200000              │
│    - bootm → jumps to 0x40200000 with a0=hart_id, a1=DTB_addr   │
├────────────────────────────────────────────────────────────────┤
│ 3. boot.S _start (hart 1)                                       │
│    - auipc + ld _boot_hart_id_addr → t1 = 1                     │
│    - bne a0(=1), t1(=1), _park  →  not taken, fall through      │
│    - Zero .bss, set sp=_stack_top, call kmain                   │
├────────────────────────────────────────────────────────────────┤
│ 4. kmain — kvm bringup                                          │
│    - page_alloc init over [_end, _heap_end)                     │
│    - root page table at 0x402b4000, identity-map sections       │
│    - "[kvm] heap 0x402b4000 - 0x402c4000 (16 pages)"            │
│    - "[kvm] root pt at 0x402b4000"                              │
├────────────────────────────────────────────────────────────────┤
│ 5. trap install + UART init (the silent-boot fix)               │
│    - stvec ← trap_entry                                          │
│    - mmio/uart_ns16550.rs::init() — IER=0, LCR=03, FCR=07,      │
│      MCR=0B, IER=01 → DW8250 ready to transmit                  │
│    - "mmu OK, traps installed"                                  │
├────────────────────────────────────────────────────────────────┤
│ 6. wasmi runtime + Tier-2 UART driver load                      │
│    - include_bytes!("uart-vf2.signed.wasm") → linker-vf2 blob   │
│    - sig check, wasmi instantiate, CAP_MMIO_UART granted        │
│    - "tier-2 uart driver loaded"                                │
├────────────────────────────────────────────────────────────────┤
│ 7. Tier-1 hello.wasm load + start                               │
│    - include_bytes!("apps/hello.wasm") → unsigned Tier-1 blob   │
│    - wasmi instantiate, fd_write WASI export bound              │
│    - call _start → hello.wasm runs                              │
├────────────────────────────────────────────────────────────────┤
│ 8. fd_write → kernel host fn → tier-2 driver write              │
│    - hello.wasm: fd_write(fd=1, "Hello from Wari\n", ...)       │
│    - kernel WASI host fn dispatches to Tier-2 UART driver       │
│    - Tier-2: for each byte: mmio_read8(LSR) loop, mmio_write8   │
├────────────────────────────────────────────────────────────────┤
│ 9. host_mmio_write8 → JH7110 UART0 → wire → COM7                │
│    - validator: is_uart_mmio_addr(0x10000000) → true            │
│    - cap check: caps.mmio_uart → true                            │
│    - write_volatile(0x10000000 as *mut u8, byte)                │
│    - DW8250 line driver → TX pin → USB-UART → COM7              │
│    - "Hello from Wari" appears in PuTTY                          │
└────────────────────────────────────────────────────────────────┘
                        ↓
                 [hello] exit(0)
                  scheduler reaps
                   board halts
```

Nine layers. Three pieces of WASM (Tier 0 native + Tier 2 driver +
Tier 1 hello). Two trust-boundary crossings on every byte
(fd_write into Tier 2, mmio_write8 into Tier 0). Two capability
checks per crossing. One absolute symbol resolved through a
PC-relative `.dword` indirection. Zero panics.

:happygoose: :nerdygoose: This is *exactly* what the architecture
diagram in `docs/architecture.md` promised. Tier 1 talks WASI to
Tier 0. Tier 0 dispatches to Tier 2 over a capability gate. Tier 2
talks MMIO to the SoC over another capability gate plus a range
validator. The diagram and the runtime trace are byte-for-byte
the same shape.

We did not invent any of these layers under pressure. The
cherry-pick discipline ("copy what makes sense, rewrite what
doesn't") meant the boot.S structure came from goose-os, the
init sequence came from goose-os, the deploy script template came
from goose-os. The Wari-native parts — two-tier WASM, capability
gates on host fns, structural sandbox separation between Tier-1
and Tier-2 — sit on top of that proven boot path. Nothing
load-bearing was new. Everything new was load-bearing.

## What this proves

Sovereign WASM-on-RISC-V is no longer a thesis.

Until today, every claim in this book about "WASM-native operating
system for RISC-V targeting sovereign cloud infrastructure" was a
QEMU result. QEMU is a generous, forgiving environment that
papers over a hundred kinds of silicon truth. A demo that runs in
QEMU and only in QEMU is, charitably, a research prototype.

`wari go` produces `Hello from Wari` on a board you can buy from
the StarFive store for eighty US dollars. The boot ROM is
OpenSBI, MIT-licensed, auditable. The bootloader is U-Boot, GPL,
auditable. The kernel is Wari, AGPL, auditable byte-for-byte from
the linker script up. The Tier-2 UART driver is a signed `.wasm`
whose entire MMIO surface is six hardcoded addresses. The Tier-1
"hello world" is an unsigned `.wasm` that talks to the UART
through a capability-gated WASI call.

You can hold the entire trust chain in one pull request review.

That is the proposition Phase 0 made and Phase 1a delivered. The
proposition is real. The board is real. The byte-for-byte audit
trail from boot ROM to `Hello from Wari` is real and on disk in
this repo.

:happygoose: :weightliftinggoose: :sharpgoose: :nerdygoose:
:mathgoose: :happygoose:

The Phase-1b work — capability mint/grant/revoke as a real Tier-0
service, a Tier-2 net driver doing TCP through smoltcp-in-WASM,
the dual-boot U-Boot menu, the print-formatting bugs above — is
what you build *on top of this*.

You do not build a Phase-1b on top of a thesis. You build it on
top of a kernel that boots on silicon you can hold in your hand.

## What We Changed

| Site | File | Direction |
|---|---|---|
| Deploy harness | `Makefile` | `deploy` target — build, commit, push, hand-off banner |
| Device script | `scripts/wari-upgrade.sh` | New — `wari` shell function: go/upgrade/reboot/status/demo/boot-log/help |
| Bringup doc | `docs/vf2-bringup.md` | New — one-time VF2 setup procedure |
| Hart selection | `kernel/src/boot.S` | `_boot_hart_id` PC-relative `.dword` load (restored from PR 8) |
| Linker symbol | `kernel/linker.ld` | `_boot_hart_id = 0` so QEMU build still parks non-hart-0 harts |
| UART init | `kernel/src/mmio/uart_ns16550.rs` | `init()` now writes IER/LCR/FCR/MCR/IER (cherry-picked from goose-os) |
| Build var | `Makefile` | `VF2_IP = 192.168.86.236` (the user's actual board lease) |
| Invariants | `docs/invariants.md` | INV-3 + INV-7 per-file-sites rows updated |

No new `unsafe`. No new INV-N. Phase-0 QEMU demo unchanged
byte-for-byte.

## What's Next

| Phase | Work | Why it now becomes possible |
|---|---|---|
| Phase 1b | Capability mint/grant/revoke as a real Tier-0 service | Silicon validates the design; per-instance caps now have a concrete attack surface to defend |
| Phase 1b | Tier-2 net driver (smoltcp-in-WASM) | Same per-platform-blob discipline; dwmac-vs-virtio is the new DW8250-vs-NS16550 |
| Phase 1b | Dual-boot U-Boot menu + `wari rollback` | The deploy harness exists; rollback is the natural next subcommand |
| Phase 1b | `kprintln!` cleanup — fix the `hart 100000` and the indented `[hello] exit(0)` | One-line printk-formatting bugs, surfaced by silicon |
| Phase 1c | Module attestation + signing pipeline (per-module hashes, not single dev key) | Two signed Tier-2 blobs in production = the right time to formalise the signing root |

But Part 3 ends here.

The kernel boots on silicon. The book has a proof of concept that
fits on a desk. Sovereign infrastructure for Latin America is no
longer a sentence in a mission statement; it is a sequence of
characters appearing on a serial console because a kernel you can
audit chose to write them.

Hello from Wari.
