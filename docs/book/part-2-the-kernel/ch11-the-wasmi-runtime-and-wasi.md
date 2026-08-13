---
sidebar_position: 11
sidebar_label: "Ch 11: The wasmi Runtime & WASI"
title: "Chapter 11 — The wasmi Runtime & WASI"
---

# Chapter 11 — The wasmi Runtime & WASI

Chapter 10 left the kernel with a working trap vector and a PLIC that
routes an interrupt to a handler. That is the last piece of purely
native machinery. Everything above it — the UART driver, the network
stack, the customer's "hello world" — is WASM. This chapter is about
the seam: the code that takes a `.wasm` blob and turns it into a
running thing the kernel can call and be called by.

That seam is small. It is three files — `runtime/loader.rs`,
`runtime/wasi.rs`, `runtime/host_fns.rs` — plus a bump allocator and a
handful of boot-time singletons. It is also, per the security model,
part of the trusted computing base: the WASM interpreter runs in
S-mode inside the kernel's own address space
(`docs/security-model.md`, "Load-bearing caveat"). There is no
privilege wall between the interpreter and the rest of Tier 0. So the
seam has to be as auditable as the kernel it belongs to.

## Why wasmi, and why an interpreter

Wari embeds [`wasmi`](https://github.com/wasmi-labs/wasmi), pinned in
`kernel/Cargo.toml:43` as `wasmi = { version = "=0.32.3",
default-features = false }`. Two words in that line carry the whole
argument.

`default-features = false` turns off `std`. wasmi then compiles
against `core` + `alloc` only, which is the precondition for running
in a `no_std` kernel that has no operating system beneath it. wasmi is
one of the very few WASM runtimes that will do this at all; Wasmtime
and V8 assume an OS.

The pinned `=0.32.3` is the reproducible-build discipline (R8) applied
to the single largest third-party dependency Wari admits into Tier 0.
CLAUDE.md's architectural rule is blunt about it: *"No third-party
code except `wasmi` itself."* Everything else in the kernel is
Wari-native Rust. wasmi is the one exception, and it earns the
exception by being an interpreter.

An interpreter, not a JIT. A JIT would be faster per instruction, but
it would also mean generating executable code at runtime inside the
kernel — a W^X nightmare, a much larger attack surface, and a much
harder formal-verification target. The roadmap defers JIT to Phase 2+
and only behind a proof obligation. For now the bet is the one
Cloudflare Workers and Fastly's Compute@Edge made before us: a
well-optimized interpreter is fast enough for the density we want, and
an order of magnitude easier to reason about. Wari counts wasmi's
lines in its audit surface precisely because it cannot hide behind a
hardware boundary the way a Unix process can.

> **Prior art.** The "WASM as the process boundary" pattern is Fastly
> Compute@Edge (2019). The "shared-runtime density, not a process per
> tenant" pattern is Cloudflare Workers. Wari inherits both and adds the
> thing neither has: a hardware MMU underneath the sandbox as defense in
> depth. See `docs/prior-art.md`.

## The load pipeline: verify first, then trust the parser

Loading a WASM module is a pipeline, and the *order* of the stages is
a security property, not a convenience. The canonical Tier-2 path is
`load_tier2` in `kernel/src/runtime/loader.rs:120`, and its first
executable line is the whole point:

```rust
// Step 1 — INV-13: verify signature before any parse runs.
let wasm_bytes = sign::verify(envelope)?;
```

That is `loader.rs:133`. **The signature check runs before wasmi ever
sees the bytes.** This is INV-13 (`docs/invariants.md`): any Tier-2
bytecode passes ed25519 verification against the compiled-in
`ACCEPTED_PUBKEY` before a `Module::new()` is constructed from it. The
reasoning is defense against the parser itself. wasmi's decoder is
complex code processing attacker-influenced input; the cheapest way to
keep a malformed-module exploit from ever reaching it is to refuse to
hand it anything that is not signed. `sign::verify` returns the raw
`.wasm` slice on success and an error otherwise, and on error the
Phase-0 kernel simply has no driver and halts — no I/O is strictly
safer than untrusted I/O.

Only after the signature clears does the rest of the pipeline run
(`loader.rs:127`–`174`):

1. **Manifest gate** (`loader.rs:138`) — parse the driver's embedded
   manifest and reject a wrong-`kind` binary before instantiation.
   This catches an attacker who signs a UART driver but ships it where
   a net driver is expected.
2. **Parse + validate** (`loader.rs:148`) — `Module::new(&engine,
   wasm_bytes)`. wasmi's validator now runs, on bytes we have already
   proven authentic. Any failure folds to `KernelError::BadWasm`.
3. **Assign caps + build the store** (`loader.rs:151`–`153`) —
   `caps_for(Tier::Two, module_id)` picks the capability set, and a
   `Store<Tier2HostState>` is constructed to carry it.
4. **Link host functions** (`loader.rs:154`) —
   `host_fns::register_host_fns(&mut linker)` binds the `wari::*`
   imports the module is allowed to call.
5. **Instantiate + run start** (`loader.rs:157`–`160`) —
   `linker.instantiate(&mut store, &module)?.start(&mut store)?`.

Note that R5 (no panics in the kernel) is visible in every step: every
wasmi `Result` is mapped to a `KernelError`, never `unwrap`ped.

### Two tiers, two host-state types

The Tier-1 path (`load_tier1`, `loader.rs:280`) mirrors the Tier-2 one
with two deliberate differences the module docstring calls out
(`loader.rs:17`–`27`):

- **No signature verification.** Phase 0's Tier-1 module is
  `apps/hello`, a build artifact compiled in the same workspace and
  embedded with `include_bytes!`. There is no third-party Tier-1 path
  yet, so a signature would add cost and no security
  (`loader.rs:250`–`256`). Tier-1 signing arrives with the Phase-1
  manifest registry. This is one of the places the book has to be
  honest: *Tier-1 is unsigned today.*
- **A different host-state type.** Tier-1 gets `Tier1HostState`
  (`wasi.rs:80`), Tier-2 gets `Tier2HostState` (`host_fns.rs:48`).
  They are two distinct structs, not one shared struct with a `tier`
  field, and the loader docstring explains why at length
  (`loader.rs:29`–`43`): each `Linker<T>` is parameterised by exactly
  the state its host functions need, so a Tier-2 host function *cannot
  compile* against Tier-1's capability shape. The type system enforces
  the tier separation before any runtime check does.

So `load_tier1` at `loader.rs:302`–`303` builds a
`Linker<Tier1HostState>` and calls
`wasi::register_wasi_host_fns(&mut linker, proc_id)` — a different
registration function, a different import surface, a different cap
shape, all fixed at the type level.

One subtlety worth its own sentence: `load_tier1` instantiates but
does **not** run `_start` (`loader.rs:269`–`279`). The Tier-1 module
exports `_start` as an ordinary WASI entry, and the kernel calls it
explicitly later (`run_tier1`, `mod.rs:280`) so it can observe the
`proc_exit` trap cleanly. More on that below.

## The host-function boundary *is* the ABI

Here is the thing that surprises people coming from Unix. Wari has no
syscall path for customer code. There is no `ecall` trampoline that a
Tier-1 module can reach, no `SYS_WRITE` number, and — per R7,
non-negotiable — no `SYS_SPAWN_ELF`, ever. The kernel core is native
Rust; customer modules are WASM; and the *only* way a WASM module
affects the world outside its linear memory is by calling a host
function the kernel imported into its linker.

That makes the set of registered host functions the entire ABI. It is
worth reading `register_wasi_host_fns` (`wasi.rs:99`) as exactly that:
an enumerated, closed list of everything a Tier-1 tenant can do. In
the current tree it binds `fd_write` and `proc_exit` under the
standard `wasi_snapshot_preview1` module name, plus a `wari::*`
extension surface — `cap_mint`, `cap_copy`, `cap_revoke`, `cap_lookup`
(`wasi.rs:135`–`194`), the IPC primitives `ipc_send` / `ipc_recv` /
`ipc_call` / `ipc_reply` (`wasi.rs:303`–`350`), the socket calls
(`wasi.rs:223`–`294`), and a `proc_self` identity probe
(`wasi.rs:354`–`360`). If a capability is not reachable through one of
those names, a tenant cannot exercise it. Full stop.

The choice of `wasi_snapshot_preview1` as the module name is itself a
documented decision (`wasi.rs:13`–`20`): it is the standard WASI
Preview 1 name, so any toolchain that targets WASI — wasi-libc, Rust's
`std::io`, Go's WASI target — emits imports Wari can satisfy. The cost
Wari accepts in exchange is that it must implement the *exact* WASI P1
ABI shapes rather than inventing its own. The `wari::*` module carries
the Wari-native extensions that have no WASI equivalent.

## One `fd_write`, all the way down

The clearest way to see the boundary work is to trace a single
`fd_write` from the Tier-1 "hello" module to a byte on the wire. It
crosses two trust boundaries and passes two capability checks, and
every step is in the code.

**The tenant calls.** `hello.wasm` executes `fd_write(fd=1,
iovs_ptr, iovs_len, nwritten_ptr)`. wasmi dispatches to the closure
bound at `wasi.rs:105`, which calls `host_fd_write` (`wasi.rs:391`)
with the `proc_id` the kernel baked into the closure at registration
time.

**Capability check #1 — the crossing into Tier 0** (`wasi.rs:405`):

```rust
use crate::cap::{check_cap, ObjectKind, CAP_RIGHT_WRITE};
if check_cap(proc_id, 0, ObjectKind::Endpoint, CAP_RIGHT_WRITE).is_err() {
    return WASI_EPERM;
}
```

The tenant must hold an `Endpoint` capability with `WRITE` rights at
slot 0 of its CSpace — the "stdout" shape in Wari's cap model. No cap,
no write; the module gets a WASI `EPERM` (`wasi.rs:72`) and nothing
happens. What that `check_cap` actually inspects is the subject of the
next chapter.

**Argument validation.** Only `fd == 1` (stdout) is plumbed in Phase
0, so any other fd returns `EBADF` (`wasi.rs:410`). The handler then
resolves the caller's own linear memory via `caller.get_export(
"memory")` (`wasi.rs:419`) — an out-of-bounds or missing memory
yields `EFAULT`, never a panic (R5).

**Marshalling, on the stack.** The handler reads the first iovec
(`wasi.rs:426`–`434`) and then copies up to `FD_WRITE_MAX = 256`
bytes (`wasi.rs:374`) into an on-stack buffer:

```rust
let n = (buf_len as usize).min(FD_WRITE_MAX);
let mut bytes = [0u8; FD_WRITE_MAX];
```

That `[0u8; 256]` on the stack (`wasi.rs:440`–`441`) is not an
accident of style. It is R2 — *no heap allocation in interrupt or
dispatch context* — enforced at the point it matters most. A host
function is dispatch context; it must not allocate. So the buffer is
fixed-size and on the stack, the byte count is clamped to its size,
and `nwritten` reports what was actually written so a caller can
detect truncation.

**Capability check #2 — the crossing into Tier 2.** The bytes are
pushed to the Tier-2 UART driver (`wasi.rs:454`):

```rust
let written = match unsafe { tier2_uart::write(&bytes[..n]) } {
```

The `unsafe` here is one of the few in the runtime, and its SAFETY
comment cites INV-1 (single-hart), INV-8 (post-init), and INV-14 (the
Tier-2 driver is a boot-installed singleton) — `wasi.rs:450`–`453`.
`kmain` orders `run_tier2_uart` (`mod.rs:218`) before any Tier-1 host
function can fire, so by the time this line runs the singleton is
guaranteed present. `tier2_uart::write` marshals the bytes into the
*driver's* linear memory and calls the driver's typed `write` export —
crossing from Tier 0 into Tier 2.

**Inside the driver, capability check #2 fires for real.** The driver
loops over the bytes and, for each, calls `wari::mmio_write8`. That
lands in `host_mmio_write8` (`host_fns.rs:198`), which is double-gated
(`host_fns.rs:199`–`208`):

```rust
if check_cap(PROC_ID_TIER2_UART, 0, ObjectKind::Endpoint, CAP_RIGHT_READ).is_err() {
    return E_PERM;
}
if !validate::is_uart_mmio_addr(addr as usize) {
    return E_INVAL;
}
```

The driver must hold the receive side of the UART endpoint (an
`Endpoint` cap with `READ` at slot 0), *and* the address must be
inside the NS16550 register window per the pure validator. Only then
does the one raw `write_volatile` in the path execute
(`host_fns.rs:216`–`218`), under a SAFETY comment citing INV-3 (MMIO
address validity, narrowed by the validator). That is R3 in action:
raw volatile MMIO lives in exactly one licensed place, behind a
capability and a range check.

Two boundaries — Tier-1→Tier-0 at `fd_write`, Tier-0/Tier-2→hardware
at `mmio_write8`. Two capability checks. One byte. This is the same
nine-layer chain Chapter 17 watches appear on a serial console; here
we are looking at the two layers where the WASM meets the native
kernel.

## The bump allocator and the R2 discipline

wasmi needs a heap. `Module::new` parses into `Vec`s, `instantiate`
builds `Box`ed state, the `Store` grows. Something has to back
`#[global_allocator]`, and in Phase 0 that something is the smallest
sound thing that satisfies `core::alloc::GlobalAlloc`: a bump
allocator, `kernel/src/runtime/heap.rs`.

It is about eighty lines with one `unsafe impl`
(`heap.rs:95`). `alloc` (`heap.rs:96`) rounds a monotonic cursor up to
the requested alignment, checks it against the arena end with a
`checked_add` guard, and either advances the cursor or returns null.
`dealloc` (`heap.rs:126`) is a no-op. That is the whole allocator.

The design is Simplicity First (`heap.rs:5`–`28`). Phase 0 has exactly
one heap consumer — wasmi's instantiation machinery — running once, at
boot, on a single hart, with no need to free anything because the
arena dies with the kernel image at reset. There is no fragmentation
pressure and no concurrent user (INV-1), so a free-list or buddy
allocator would be paying for flexibility the phase does not need.
Pulling in `linked_list_allocator` or `talc` would also enlarge the
Tier-0 trust base for no benefit. When Phase 1 needs repeated instance
creation, this module retires along with its invariant, INV-12.

INV-12 is the reason the whole thing is sound: *the arena is
initialized exactly once in `kvm::init` and never re-initialized;
after init only `alloc()` moves the cursor, and `HEAP_CURSOR <=
HEAP_END` always holds* (`docs/invariants.md`, INV-12;
`heap.rs:30`–`36`). Combined with INV-1's single-hart guarantee, the
lockless cursor is safe.

And here is how it stays consistent with R2. The runtime's module
docstring (`mod.rs:16`–`20`) states the discipline directly: the bump
allocator is a "heap," but it is exercised only from `kmain` in boot
context, never from a trap or a host-function dispatch. wasmi allocates
during `Module::new` and `instantiate`, which happen once at boot,
before any trap is taken. **No syscall path allocates.** That is why
`host_fd_write` copies into a stack buffer instead of a `Vec` — a
host function runs in dispatch context, and dispatch context does not
touch the heap. The bump allocator existing does not weaken R2; the
two are kept apart by *when* each runs.

## `proc_exit` as a trap you can catch

When the tenant is done it calls `proc_exit(code)`. The WASI spec says
this function does not return, and Wari honors that literally.
`host_proc_exit` (`wasi.rs:491`) does not set a flag and return — it
returns an error that unwinds the wasmi call stack:

```rust
caller.data_mut().exit_code = Some(code);
Err(Error::i32_exit(code as i32))
```

That is `wasi.rs:505`–`506`. `Error::i32_exit` is wasmi's first-class
support for exactly this WASI pattern (the module docstring at
`wasi.rs:34`–`52` weighs the alternatives and rejects them: returning
normally would let the module keep executing past a call the spec says
never returns). The exit is also cap-gated — a tenant needs an
`Endpoint` `WRITE` cap at slot 1 (`wasi.rs:501`); a denied call still
traps, with `i32_exit(-1)`, because the module must not be allowed to
continue without the cap.

The kernel side catches it in `run_tier1` (`mod.rs:280`–`301`). It
resolves `_start`, calls it, and inspects the resulting error:

```rust
Err(e) => {
    if let Some(code) = e.i32_exit_status() {
        kprintln!("[t1:{}] exit({})", proc_id, code);
        Ok(code)
    } else {
        kprintln!("[t1:{}] runtime trap: {:?}", proc_id, e.kind());
        Err(KernelError::BadWasm)
    }
}
```

A clean `proc_exit` surfaces as `i32_exit_status()` returning the
code; anything else is a genuine trap and becomes `BadWasm`. The
kernel never panics on tenant behavior — a customer module that faults
is a `Result`, not a crash. That is the whole of Phase 0 exit criterion
4 ("module calls `proc_exit(0)`; scheduler reaps cleanly") reduced to
one match arm.

## A small aside: `drv_log_u32` vs `drv_trace_u32`

Two Tier-2 host functions are worth noticing because they look
identical and are not (`host_fns.rs:342`–`368`). `drv_log_u32` prints
its `(tag, val)` pair through `kprintln!` unconditionally — it is
meant for boot-time milestones an operator expects to see on a stock
build (the net driver uses it to report the GMAC version register).
`drv_trace_u32` writes the same wire format through `kdebug!`, which
compiles out entirely unless the `debug-kernel` feature is on. Same
shape, same format, different cost: one is always on and belongs on
the cold path; the other is a hot-path probe that leaves no trace in
production. The wire formats match on purpose so an operator can grep
`[net:drv]` and `[debug:drv]` lines together. It is a small thing, but
it is the kind of policy-vs-mechanism separation CLAUDE.md's code
standards keep asking for.

## What the runtime does not decide

Every host function in this chapter opened with the same gesture: a
`check_cap` call. `fd_write` checked for an `Endpoint`/`WRITE` at slot
0; `proc_exit` checked slot 1; `mmio_write8` checked the driver's
`Endpoint`/`READ` at slot 0. The runtime *asks* the question "may this
module do this?" — but it does not answer it. It delegates to
`crate::cap`.

That delegation is the whole security posture. The runtime is the
mechanism; the capability system is the policy. What a capability
actually *is* — a 16-byte unforgeable token, a slot in a per-process
table, a node in a derivation tree the kernel can revoke — and how the
kernel mints the very first ones at boot so that `check_cap` has
something to find, is Chapter 12.

---

*A note on accuracy: `runtime/mod.rs:6` describes the wasmi pin as
`=1.0.9`, and `loader.rs` comments reference a "wasmi 1.0 API." The
authoritative pin is `kernel/Cargo.toml:43` and `Cargo.lock`:
`=0.32.3`. Where a comment and the manifest disagree, the manifest is
the running version — this chapter cites 0.32.3.*
