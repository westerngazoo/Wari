---
sidebar_position: 25
sidebar_label: "Ch 25: The GAPU"
title: "Chapter 25 — The GAPU"
---

# Chapter 25 — The GAPU

Chapter 21 ended on a limit. The network driver answers a ping because a
`wasmi` interpreter can walk WASM bytecode about a hundred thousand times
a second on a U74 core — fast enough for control-plane work, nowhere near
fast enough to saturate a gigabit link. Part 5's answer to that limit is
to compile the WASM ahead of time: keep the structural isolation, pay the
interpretation cost once, at build time. It is the right answer for the
network fast path.

It is the wrong question for a large matrix multiply.

There is a class of work where the fix is not "run the WASM faster" but
"do not run this in WASM at all." Transformer inference is the headline
case: a forward pass is billions of multiply-accumulates, and no
interpreter, no AOT compiler, no U74 core is going to make a
general-purpose scalar CPU competitive with silicon built for the shape
of the arithmetic. For that work the WASM should not be the engine. It
should be the *orchestrator* — deciding what to run, marshalling the
inputs, reading the outputs — while the arithmetic happens somewhere
built to do it. That somewhere, in Wari's long-term plan, is the GAPU:
an FPGA accelerator on the PCIe bus, driven by a signed Tier-2 driver,
reached by a tenant through a capability-gated host function. This chapter
argues why that shape is the natural one — and is blunt about the fact
that, of everything in this book, the GAPU is the least built.

## The Nitro analogy, made specific

The strategic ancestor is AWS Nitro (2017–, `docs/prior-art.md:57`).
Nitro's move was to take the work a hypervisor used to do in software —
network, storage, security — and push it onto dedicated hardware,
shrinking the trusted software that runs on the main CPU. The offload
card is not a performance afterthought bolted onto a general-purpose
host; it is a first-class architectural peer, and the shrinking of the
host's TCB is the point as much as the speed.

Wari's analog is the GAPU FPGA coprocessor over PCIe, named in the
Phase-3 roadmap and claimed in the prior-art file as one of the project's
three genuine bets: "GAPU FPGA as architectural peer to GPU — not just
'we happen to also support FPGA'" (`docs/prior-art.md:227`). The name is
chosen to sit beside "GPU" deliberately — the accelerator is meant to be
the *canonical* AI-inference target for workloads where sovereignty
matters more than per-TOPS cost, not a fallback for when no GPU is
present. A government that can audit an open FPGA bitstream but cannot
audit a proprietary GPU driver stack has a reason to prefer the slower,
inspectable path. That preference is the whole bet.

## Why an accelerator fits the capability model without a special case

Here is the part that is already decided, because the decisions were made
in Phase 0 and Phase 1. An accelerator, to Wari, is just another device
behind a signed Tier-2 driver — the same shape as the UART and the
network MAC. The driver holds the accelerator's MMIO capability; the
tenant never touches the device; the tenant holds a capability to *ask*
the driver to run something. There is no new trust tier, no new privilege
mode, no bespoke pathway. The same sentence that describes how a Tier-1
app sends a byte to a serial port describes how it runs an inference: a
cap-gated request crosses into a signed driver that owns the hardware.

The surface that carries the request already has a design on disk. The
WASI-NN surface (`docs/wasi-nn-surface.md`) mirrors the WASI-NN proposal —
`nn_load`, `nn_init_context`, `nn_set_input`, `nn_compute`,
`nn_get_output` — so standard toolchains emit compatible imports, and it
adds `ExecutionTarget::Gapu` beside `Cpu` and `Gpu`
(`docs/wasi-nn-surface.md:51`). Two adaptations make it Wari-shaped
rather than generic, and both are pure capability discipline:

- **Models are attested capabilities, not raw bytes.** WASI-NN's `load`
  takes model bytes out of the caller's memory. Wari makes the model a
  `Model` capability referencing *attested* weights — signed and measured
  at provisioning, like any Tier-2 blob (`docs/wasi-nn-surface.md:61`). A
  tenant can only load a model it was granted; the weights never transit
  untrusted WASM linear memory, because the driver reads them from the
  attested store directly. This is the same attestation grammar Chapter
  24's CoVE story plugs into — the model is one more attested unit.
- **An inference context is a derived capability.** `nn_load` mints an
  `ExecCtx` object into the caller's CSpace and revokes it on drop
  (`docs/wasi-nn-surface.md:66`). A subverted Planner can run only the
  models it holds and cannot forge a context, under the same invariants
  that govern every other capability. `nn_compute` on the GAPU requires
  the *driver* to hold the accelerator's MMIO capability; the tenant's
  reach ends at the request.

Nothing here is an exception to the model. The accelerator is expensive,
physical, and fast, and the trust story around it is identical to the
trust story around a UART. That identity is not luck. It is what the
host-function boundary was drawn for.

## The hot path is not WASM — which is the whole point

Part 5 opens with a gate it calls M0: before building an AOT compiler,
*measure whether you need one* — the differential oracle may say "don't
build it" (`docs/aot-build-plan.md:208`). The M0 discipline is a refusal
to assume the interpreter is the bottleneck before proving it is. The
WASI-NN design applies the same refusal to AI, and reaches a sharper
conclusion: for inference, the interpreter's speed is *irrelevant*,
because the heavy step never runs in the interpreter at all.

The Planner is a control loop — decide, infer, act — and only `infer` is
compute-heavy, and `infer` leaves WASM entirely
(`docs/wasi-nn-surface.md:20`):

```
Planner (Tier-2 WASM)  ──wari_nn::compute──▶  accelerator driver (Tier-2)
   orchestration only                            GPU / GAPU — the math
        ▲                                              │
        └────────────── output tensor ◀────────────────┘
```

Read that diagram against the anxiety it dissolves. An "AI-first OS"
sounds like it demands a blisteringly fast runtime — a JIT, a
supercomputer interpreter, something the density bet cannot afford. It
demands no such thing. The assistant's speed comes from AOT for the
orchestration and offload for the arithmetic, never from running a model
inside `wasmi` (`docs/wasi-nn-surface.md:30`). The WASM core stays thin
precisely *because* the math is off-WASM and off-CPU, sitting behind a
cap-gated host function. This is the same lesson the net driver taught in
reverse: there, the interpreter was fast enough for the control plane and
too slow for the data plane; here, the data plane is simply moved off the
interpreter entirely, and the interpreter is left doing the one thing it
is genuinely good at.

The batching detail makes the boundary efficient. An inference step is
`set_input × N → compute → get_output × M`, a natural fit for the cap
fast-path submission ring: register the `ExecCtx` once, then batch the
tensor ops so the Planner's inner loop pays one kernel crossing per
inference step instead of one per tensor (`docs/wasi-nn-surface.md:100`).
Validate-once, reference-many — the same discipline the cap system uses
everywhere — applied to the shape of an inference call.

## The deeper co-design bet: the accelerator's logic is an artifact too

There is one more turn, and it is the reason the *F* in FPGA matters
rather than a GPU alone. An FPGA is programmable: the accelerator's
function is itself a signed artifact, a bitstream loaded by the driver at
boot. That closes a loop the GPU path cannot. With a proprietary GPU, a
sovereign tenant can audit the model and, with effort, the driver — but
the silicon's behavior is a vendor's black box. With a GAPU, the same
signing-and-attestation discipline that governs Tier-2 *code* can govern
the accelerator's *logic*. Sovereign AI, followed all the way down, means
being able to inspect not just the weights and not just the driver but the
arithmetic unit itself. The FPGA is the only accelerator architecture
where that inspection is even possible, which is why the prior-art file
insists the GAPU is a peer to the GPU and not a substitute for it.

## The honesty: named, not designed

Everything above is architecture. Almost none of it is code, and the gap
is wider here than anywhere else in the book. This section exists so no
reader mistakes a coherent plan for a running system.

There is no GAPU design document on disk at the level of the network
driver's. The project's own adversarial review is blunt about it: the
GAPU is "*named*, not designed" (`docs/research/heli-adversarial-review.md:172`).
What exists today is the pure-ABI layer — the WASI-NN enums and opcodes
landed in `wari_abi::nn` — and nothing below it: the runtime and driver
path are explicitly Phase 2/3 (`docs/wasi-nn-surface.md:8`). Between the
enums and a working inference call sit whole subsystems Wari does not
have:

- **A PCIe stack.** Wari's on-silicon experience is SoC-local MMIO. PCIe
  is a different subsystem; `smoltcp` does not help here. It does not
  exist in the tree.
- **DMA-coherent memory management.** The Phase-1c memory model is a
  single identity-mapped virtual address space. Feeding tensors to a bus
  device wants coherent DMA buffers, which that model does not provide.
- **MSI-X interrupt routing.** The PLIC handles SoC interrupts only.
  PCIe message-signalled interrupts are, again, a subsystem that is not
  present.
- **An FPGA bitstream signing and loading pipeline.** The co-design bet of
  the previous section — the accelerator's logic as a signed artifact —
  presupposes a signing pipeline for bitstreams that has not been built.

Each of those is multi-month work, and they compound. This is why the
GAPU sits in Phase 3, the furthest reach of the roadmap, and why an
honest account of the project's cost puts reaching Phase 3 — CoVE, GAPU,
external audit, multi-board clustering, per-module formal verification —
at somewhere between fifteen and forty person-years of additional
engineering, with no funding mechanism on the page
(`docs/research/heli-adversarial-review.md:87`). Calling the GAPU
"designed" would be the kind of overstatement this book is trying not to
make. It is named because the shape it must take is already fixed; it is
undesigned because the subsystems that would give it that shape are not
written.

## So why name it now at all

Because naming the endpoint is what keeps the earlier decisions honest.
Every host-function boundary drawn in Phase 0, every capability minted in
Phase 1, every signature checked on a Tier-2 blob was drawn so that a
device like the GAPU could slot in *without a special case* — as a signed
driver holding an MMIO cap, reached through a cap-gated host fn, running
models that are themselves attested capabilities. If the accelerator
required tearing up the trust model to accommodate it, the trust model
would have been wrong. It does not. The GAPU is undesigned in its
mechanism and fully constrained in its shape, and that constraint is the
evidence that the architecture underneath it was built to reach this far
even though it has not yet.

## Closing hook

CoVE would hide a tenant from the operator; the GAPU would accelerate it
without breaking the sandbox; both live behind the same discipline — an
attested unit, reached through a capability, mediated by a kernel small
enough to have earned that trust. That kernel is the last thing left to
account for. The whole stack terminates in it, and the project's ordering
of virtues — correct, then secure, then small — was aimed at one endpoint
from the beginning: a kernel small enough to prove, frozen enough to
trust, and finally burned into silicon. The last chapter follows that
ordering to where it was always pointing.
