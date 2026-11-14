<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Cranelift on riscv64 — the two G4 blockers, answered (PM-36)

> **Status:** Investigation complete, 2026-10-21. Both blockers reproduced
> against `cranelift-codegen` 0.134.4 and both have a fix that needs **no
> Cranelift patch**. The fixes change `aot-target-abi.md` §A2 and §A3 and
> therefore need the architect's sign-off before G6 codegen starts.
>
> Reproduce with `cargo run` in [`tools/clif-rv64-probe`](../tools/clif-rv64-probe).

The G6 notes in `aot-parallel-roadmap.md` flagged two issues from the G4
spike and said to confirm them **before any codegen**. With AOT now on the
critical path (it gates the first app), that confirmation is this document.

---

## Blocker 1 — pinned register for WCTX: **NO, and it is not close**

`aot-target-abi.md` §A2 reserves `x27`/`s11` as WCTX and notes Cranelift
"exposes a *single* pinned register (`enable_pinned_reg`)". The roadmap
called rv64 support "unverified". It is not unverified — it is **absent**,
and upstream says so in the source:

```rust
// cranelift-codegen-0.134.4/src/isa/riscv64/lower.rs
fn maybe_pinned_reg(&self) -> Option<Reg> {
    // pinned register is a register that you want put anything in it.
    // right now riscv64 not support this feature.
```

Reproduced end to end. The flag is *accepted* — which is what makes this
trap easy to walk into — and lowering is what fails:

| Probe | Result |
|---|---|
| `enable_pinned_reg=true`, then `get_pinned_reg.i64` | `Unsupported("should be implemented in ISLE: inst = v0 = get_pinned_reg.i64")` |
| same IR, flag **off** (control) | verifier: "GetPinnedReg/SetPinnedReg cannot be used without enable_pinned_reg" |

The control matters: it proves the flag is doing something and that the
failure is specifically the rv64 backend, not a misuse of the API. `x64`
and `aarch64` implement it (`get_machine_env` branches on
`enable_pinned_reg` in their `abi.rs`); `riscv64` does not.

### Fix: WCTX becomes an explicit first parameter

Drop the reserved register. Thread WCTX as a normal first argument, which
is **what Wasmtime itself does with `vmctx`** — `enable_pinned_reg` is an
optional feature for embedders like SpiderMonkey, not the mainstream path.

This is a small edit to the ABI, not a redesign, because §A2 already
hedged: it mandates *only* WCTX and already specifies `a0 = WCTX` as the
explicit first argument on the host-call boundary (§3.6). The change makes
compiled-to-compiled calls follow the convention the host boundary already
uses.

What it costs: WCTX occupies an argument register and is passed on every
internal call instead of living permanently in `s11`. What it buys: no
Cranelift fork, no upstream dependency, and it works today.

What must change in `aot-target-abi.md`: §A2's reserved-register table
(`s11`/`s10`/`s9` as reserved, line ~216) and the §A2 recommendation. The
reasoning at line ~332 — that these survive calls because they are
callee-saved in the RV64 psABI — no longer applies and should go.

---

## Blocker 2 — `trap` lowering: **confirmed, lowers to an illegal instruction**

```
trap  ->  4 bytes: 00 00 00 00      vcode: udf##trap_code=user1
```

Not a call, so the §A3 thunk edge does not exist in the emitted code and
the G7b cert checker cannot see it. The property would be unverifiable.

### Fix: never emit `ir::trap` — branch to a block that calls the thunk

The roadmap assumed we would post-process machine code to rewrite `udf`
into a thunk call. We do not have to: **we write the translator**, so it
can simply never emit `ir::trap`. Emit the bounds check as ordinary
control flow into a block that loads `WCTX.trap_entry` and calls it.

This is strictly better than post-processing: the trap edge is a real call
in the IR, so it is verifiable *by construction* rather than recovered by
pattern-matching bytes — and it is exactly §A3's own Option 4A ("branch to
a thunk that calls `WCTX.trap_entry`").

---

## Both fixes compose, and the result is validated

They are the same change seen twice: with WCTX as an explicit value, the
trap block loads `trap_entry` straight from it. Compiled for
`riscv64gc-unknown-none-elf`:

```
block0:
  bgeu a1,a2,taken(label2),not_taken(label1)   ; the §A3 bounds check
block1:
  ld a0,0(a1)                                  ; in-bounds path
  ret
block2:
  ld a2,16(a0)                                 ; WCTX.trap_entry via explicit a0
  li a1,1                                      ; trap code
  callind a2                                   ; a real call edge, not udf
  ret
```

**72 bytes, 0 relocations** — the zero-reloc property the ABI requires
survives, and the trap edge is visible to the checker.

## What this does not answer

- Fuel accounting (§A2 "if only WCTX is reservable") assumed a pinned
  register too; it needs the same explicit-parameter treatment.
- The translator front-half — the actual wasm→CLIF lowering — is still
  the main G6 risk. G4 established the backend is sound (sub-ms,
  zero-reloc, byte-reproducible RV64); nothing here changes that.
- No performance comparison between pinned-register and explicit-parameter
  WCTX. On rv64 it is moot: there is no pinned-register option to compare.
