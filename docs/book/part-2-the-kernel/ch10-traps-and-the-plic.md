---
sidebar_position: 10
sidebar_label: "Ch 10: Traps & the PLIC"
title: "Chapter 10 — Traps & the PLIC"
---

# Chapter 10 — Traps & the PLIC

A trap is the hardware asking the kernel a question it did not schedule:
*something happened — a timer fired, a device raised a line, an
instruction faulted — what do you want to do?* Everything reactive about
an operating system funnels through one address, the trap vector, and how
narrow or how sprawling that vector is tells you a great deal about the
kernel's ambitions. Wari's is narrow on purpose. This chapter follows a
trap from the assembly that catches it, through the `scause` dispatch that
classifies it, into the PLIC's claim-and-complete handshake for a device
interrupt — and then it tells you the one thing the code does *not* do
yet, which turns out to be the most important sentence in the chapter.

## The vector: save everything, call Rust

The vector lives in [`kernel/src/trap.S`](../../../kernel/src/trap.S), and
like `boot.S` it is deliberately the smallest thing that works. Its header
records what was *removed* relative to the goose-os original: the U-mode
entry path, the `sscratch` swap convention, the IPC fast-path, the
scheduler-tick branch, the syscall jump table. Phase 0 has no userspace, so
every trap originates in S-mode, and the kernel runs with interrupts
disabled outside the vector, so traps do not nest. That collapses what is,
in a mature kernel, an intricate two-mode dance into a straight line:
*save, call, restore, return.*

`install()` ([`trap.rs:86`](../../../kernel/src/trap.rs)) points the
hardware at it — a single `csrw stvec` with the vector's address
([`:96-98`](../../../kernel/src/trap.rs)). The low two bits of `stvec` are
left `00`, direct mode, so *every* trap — interrupt or exception — enters
at the one symbol `_trap_entry`. The `unsafe` is INV-7: a privileged CSR
write, sound because we are in S-mode.

`_trap_entry` ([`trap.S:27`](../../../kernel/src/trap.S)) then does the
unglamorous, essential work. It carves 288 bytes off the current kernel
stack and stores all 32 general-purpose registers into it
([`:29-67`](../../../kernel/src/trap.S)) — `x0` written as a literal zero
so the struct offsets line up cleanly with `xN * 8`, `sp` reconstructed
as "current sp + 288" and stored separately
([`:70-71`](../../../kernel/src/trap.S)) because the live `sp` has already
moved. Then it reads the four CSRs that describe the trap — `sepc`,
`sstatus`, `scause`, `stval` — into the frame
([`:74-81`](../../../kernel/src/trap.S)), sets `a0` to point at the frame,
and calls the Rust dispatcher ([`:84-85`](../../../kernel/src/trap.S)).

The layout is a contract. The `TrapFrame` struct in
[`trap.rs:34-73`](../../../kernel/src/trap.rs) is `#[repr(C)]` and its
field offsets — `sepc` at `0x100`, `sstatus` at `0x108`, `scause` at
`0x110`, `stval` at `0x118` — must match `trap.S` byte for byte; both
files carry the same offset table in their comments so a change to one
that forgets the other is caught by review, not by a mysteriously wrong
register at runtime. Notice the design choice embedded here: the assembly
reads the CSRs and hands them to Rust *in the frame*, so the Rust side is
pure dispatch and never re-reads a CSR. That keeps the interesting logic in
a function you can reason about.

On return, the handler's possible edits to `sepc` and `sstatus` are
written back ([`:90-93`](../../../kernel/src/trap.S)) — those are the two
the spec lets a handler change — every GPR is reloaded, the 288 bytes are
popped, and `sret` returns to `sepc`
([`:95-130`](../../../kernel/src/trap.S)).

### INV-2, and why the `&mut` doesn't lie

`handle_trap` ([`trap.rs:119`](../../../kernel/src/trap.rs)) takes a `&mut
TrapFrame`. In a preemptible kernel that reference would be a hazard — a
second trap could arrive mid-handler and hand out a second `&mut` to
overlapping state. Wari's INV-2 (Trap Frame Exclusivity) is what makes it
sound: while an S-mode trap is being serviced, interrupts are masked, so
no other execution path touches the frame until `sret`. The mutable
reference genuinely does not alias, and the `SAFETY` comment on the
function cites exactly that. INV-2 is not free — it is *purchased* by the
same masked-interrupt posture that, as we are about to see, also means the
kernel cannot be preempted at all.

## Dispatch: three outcomes

`handle_trap` splits `scause` into "is this an interrupt?" (the top bit,
`SCAUSE_INTERRUPT_BIT`, [`:102`](../../../kernel/src/trap.rs)) and a cause
code, and branches to one of three fates
([`:128-159`](../../../kernel/src/trap.rs)):

**Timer** (`scause` code 5, [`:130-134`](../../../kernel/src/trap.rs)).
The kernel does not arm any timer today — a grep of the tree finds no
`stimecmp` write and no SBI `set_timer` call — so a supervisor timer
interrupt is a stray one OpenSBI delivered. The handler does the minimum
that keeps it from re-firing forever: `ack_timer`
([`:166`](../../../kernel/src/trap.rs)) clears `sip.STIP` with a `csrc
sip` and returns. It is a defensive stub, shaped to grow into the tick a
preemptive scheduler will eventually need, but honest about being a stub.

**External** (`scause` code 9, [`:135-139`](../../../kernel/src/trap.rs)).
This is a PLIC-routed device interrupt, and the handler delegates straight
to `crate::mmio::plic::dispatch()`. This arm is the clearest place where
the file has outgrown its own header: the module docstring at the top of
[`trap.rs:15`](../../../kernel/src/trap.rs) still asserts "No
PLIC/external-interrupt path (no PLIC in Phase 0)," while the body has a
fully wired one, labelled "Phase 1b PR Net-1." The code is the truth; the
header is a Phase 0 scope note that history overtook.

**Everything else** ([`:149-159`](../../../kernel/src/trap.rs)). With no
userspace, every exception is a kernel bug — a bad load, a fault, a
misaligned access. There is nothing to recover *to*, so the handler prints
the diagnosis (`code`, `sepc`, `stval`) and calls `halt()`
([`:174`](../../../kernel/src/trap.rs)), the now-familiar `wfi` park. An
unhandled *interrupt* code takes the same exit. This is the same
philosophy as `kmain`'s per-stage failures in Ch 8: a kernel that limps
forward after touching memory it cannot explain is worse than one that
stops with the address on the wire. When Ch 9's GMAC1 driver read one
register past its mapped window, *this* is the code that printed the
`stval` and parked.

## The PLIC: claim, signal, complete

The Platform-Level Interrupt Controller is RISC-V's standard fan-in for
device interrupts — many source lines multiplexed into the single
"external interrupt" the trap dispatcher sees. Its driver is
[`kernel/src/mmio/plic.rs`](../../../kernel/src/mmio/plic.rs), at the
fixed base `0x0c00_0000` ([`:64`](../../../kernel/src/mmio/plic.rs)) —
identical on QEMU `virt` and the JH7110, because every spec-compliant
RISC-V SoC puts it there. All register access goes through the typed
`VolatilePtr` wrappers under INV-3 (MMIO Address Validity); per absolute
rule R3, raw `read_volatile`/`write_volatile` is confined to
`kernel/src/mmio/`, and this is one of the files that earns the exception.

`init()` ([`:138`](../../../kernel/src/mmio/plic.rs)) does two small
things. It writes the per-hart priority *threshold* to 0, so any source
with priority ≥ 1 is accepted ([`:141-142`](../../../kernel/src/mmio/plic.rs)),
and it sets `sie.SEIE`, bit 9 of the `sie` mask register, so the trap
dispatcher will actually see external interrupts as cause 9
([`:146-148`](../../../kernel/src/mmio/plic.rs), INV-7). One wrinkle worth
carrying: each hart has two PLIC *contexts*, M-mode and S-mode, and Wari
uses the S-mode one — context 1 on QEMU (hart 0), context 3 on the VF2
(hart 1), selected by feature at [`:80-83`](../../../kernel/src/mmio/plic.rs).
The hart-numbering asymmetry from Ch 8 propagates all the way down here.

When a bound line fires, the trap handler calls `dispatch()`
([`:259`](../../../kernel/src/mmio/plic.rs)), and the three steps are the
PLIC's whole protocol:

1. **Claim.** Read the claim register
   ([`:261-262`](../../../kernel/src/mmio/plic.rs)); the PLIC returns the
   highest-priority pending IRQ number, or `0` if none. A `0` is a
   spurious interrupt and the handler simply returns
   ([`:263-266`](../../../kernel/src/mmio/plic.rs)) — reading the claim
   register is itself the acknowledgement, so nothing is left dangling.
2. **Signal.** Look up the notification bound to that IRQ and set its
   signal bit ([`:269-278`](../../../kernel/src/mmio/plic.rs)). More on
   this below.
3. **Complete.** Write the IRQ number back to the claim register
   ([`:285`](../../../kernel/src/mmio/plic.rs)), which tells the PLIC the
   source may fire again. One claim per dispatch; if several lines are
   pending, the next external interrupt re-enters `dispatch` for the next.

### IRQ → Notification, and INV-23

Step 2 is where a hardware event becomes something a capability-based
kernel can hand to a driver. Wari does not let a Tier-2 driver hook a raw
interrupt; instead the kernel translates the IRQ into a signal on a
`Notification` capability object, and the driver *polls* that notification
through a host function. The translation table is a single static array,
`IRQ_NOTIFICATION_BINDINGS` ([`:120-121`](../../../kernel/src/mmio/plic.rs)),
mapping IRQ number → notification pool index, populated at boot by
`bind_irq_to_notification` ([`:220`](../../../kernel/src/mmio/plic.rs)) and
read on every external trap by `dispatch`. A `None` entry means "not
bound," and `dispatch` handles it defensively: it still completes the PLIC
cycle so an unbound stray line cannot wedge the controller, it just
signals nothing ([`:280-285`](../../../kernel/src/mmio/plic.rs)).

That static-mut array is governed by INV-23 (IRQ Routing Determinism),
drafted for PR Net-1. Its guarantee is exactly the kind of property Wari
likes to be able to state in one sentence: the bindings are written only
at boot and only by `bind_irq_to_notification`; after init, no path
mutates them, so the trap-to-notification mapping is deterministic and
read-only. A reader of the trap path can verify "every IRQ that fires
routes to one specific notification, the same way every time" by
inspecting one array — no race, no dynamic rebinding mid-flight. The
catalog is candid about INV-23's shelf life, too: when a `sys_irq_bind`
syscall lands to let drivers register IRQs at runtime, INV-23 is retired
and replaced by INV-1 covering the new write path. Invariants in Wari are
dated; they are meant to be revisited when the thing they assumed changes.

## The seam: nothing preempts anything

Now the sentence the whole chapter has been circling. `plic::init` set
`sie.SEIE` — the per-source *enable* for external interrupts. Nowhere in
the tree is `sstatus.SIE` set — the *global* supervisor interrupt-enable.
(A grep for a `csrs sstatus` that raises SIE finds nothing; the only
`sstatus` traffic is `trap.S` saving and restoring it around a trap.) In
the RISC-V privileged architecture those are two different switches, and
with the global one clear, an interrupt while the kernel is running in
S-mode becomes *pending* but is never *taken*.

The consequence is precise and load-bearing:

- A `wfi` will still **wake** when an enabled interrupt is pending —
  `wfi` responds to a pending enabled interrupt regardless of the global
  mask. So the idle loops and halts sprinkled through the kernel can, in
  principle, be roused by a device.
- But a running kernel path is never **preempted**. Control does not leave
  the current instruction stream to service the interrupt until the code
  reaches a point where it polls — the idle loop's smoltcp poll in
  `kmain`, or a driver spinning on `notification_wait`. The device
  interrupt sets a pending bit and waits its turn.

This is not an accident and it is not, today, a bug — it is a seam, left
deliberately at a clean edge. It is *why* the whole interrupt path is
poll-driven: `plic::init`'s own docstring notes there are "no wait
queues" in Phase 1b, that a driver "that wants to block must poll in a
loop." It is why `kmain`'s idle loop busy-polls the UART for Ctrl-R
instead of sleeping on an interrupt. And it is why INV-2 holds so easily:
with `sstatus.SIE` clear, traps genuinely cannot nest, so the trap frame
genuinely cannot alias.

The seam is exactly where the future preemptive scheduler plugs in. The
day Wari wants timer-driven preemption, three things move together: the
kernel arms a timer (the `stimecmp`/SBI write the timer arm currently does
*not* make), `handle_trap`'s timer arm grows from `ack_timer` into a real
scheduler tick, and `sstatus.SIE` comes on — at which point INV-2 stops
being free and every trap-frame and static-mut reasoning that leaned on
"interrupts are masked" has to be re-audited under nesting. The code today
is arranged so that change is a deliberate, reviewable move rather than an
emergent surprise. Preemption is not missing by neglect; it is held back
at a marked line until the kernel is ready to pay for it.

## Closing hook

We now have a kernel that boots, maps itself, and catches what the
hardware throws at it — but it still has nothing to *run*. The whole point
of Tier 0 is to be the thin, verifiable substrate under untrusted WASM,
and we have not yet loaded a single module. Ch 11 — the wasmi runtime and
the WASI surface: signature-checking a `.wasm` blob, instantiating it, and
wiring `fd_write` down through the Tier-2 driver to the UART we brought up
in Ch 8.
