<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Wari — Cranelift AOT Spike: Results (G4 / M0 evidence)

> **Status:** measured results for roadmap task **G4**
> ([`aot-parallel-roadmap.md`](aot-parallel-roadmap.md) §4), gated on
> **DG-1 = Cranelift-offline**, which the architect confirmed. Together with
> the G1 benchmark numbers this file is the **M0 gate evidence** for the
> go/kill decision on the AOT workstream.
>
> **Every number in this document was measured on this machine on
> 2026-07-24.** Nothing is estimated, extrapolated or carried over from
> literature. Where a number is missing it says `PENDING` or `NOT MEASURED`
> and names the reason. In particular **no compiled code has ever been
> executed** — see §2.
>
> Code: `tools/wari-aot-spike/` — **throwaway**, deleted when G6 lands.

---

## 1 · Verdict first

**Cranelift's riscv64 backend is viable for G6.** It compiled every fixture
the spike's translator could feed it, in well under a millisecond, to
correct-looking RV64, with **zero residual relocations** and **bit-identical
output across runs** — the two properties `docs/aot-target-abi.md` §5.1 and
§6 depend on and that §8.8 explicitly said "verified by G4, not assumed."

Three findings materially change the G6 plan, none of them fatal:

1. **`cranelift-wasm` no longer exists.** The roadmap names it; it was
   removed from the Cranelift workspace and last published as `0.111.11`
   against a Cranelift ~2 years older than current. G6 must either vendor a
   wasm→CLIF translator or take a `wasmtime`-shaped dependency. §3.
2. **Cranelift's `trap` lowers to an illegal instruction (`unimp`), not to a
   call.** The ABI RFC's recommendation A3 (§4.2, Option 4A: branch to a
   thunk that calls `WCTX.trap_entry`) is therefore *not* what the backend
   does by default. G6 must emit the thunk call explicitly. §6.2.
3. **The interpreter baseline is profile-sensitive by ~19×.** The roadmap's
   G1 acceptance command uses a debug build. Release `wari-bench` puts
   `arith.wasm` at **0.0106 ms**; debug puts it at **0.202 ms**. An M0 gate
   argued from the debug number would overstate the interpreter's cost by
   more than an order of magnitude. §5.1.

**What this spike does *not* establish:** any execution result, any native
wall-clock time, any cycle count, and therefore *anything at all about
whether AOT is faster than `wasmi` on the U74*. That is Phase B (§7) and it
needs the board. The go/kill decision cannot be made from this file alone.

---

## 2 · The environment constraint, and how the spike was restructured

The roadmap's G4 acceptance criterion is:

```bash
qemu-riscv64 /tmp/arith.elf      # prints the same result the oracle records
```

**`qemu-riscv64` — QEMU's user-mode (linux-user) emulation — does not exist
on this host and cannot.** linux-user emulation is a Linux-only QEMU build
target; the dev machine is macOS/arm64. Only the full-system
`qemu-system-riscv64` is installed:

```
$ which qemu-system-riscv64 qemu-riscv64
/opt/homebrew/bin/qemu-system-riscv64
qemu-riscv64 not found
```

Running the harness under `qemu-system-riscv64` would mean booting a whole
riscv64 Linux guest — more moving parts than the measurement is worth, and
still emulated timing, which is useless as a performance gate.

The spike is therefore split:

| | Scope | Status |
|---|---|---|
| **Phase A** | compile, emit, and verify *without executing* | **done — this document** |
| **Phase B** | execute on real silicon (VF2 running Debian riscv64) | **not attempted** — operator instructions in §7 |

Phase B on the board is strictly better evidence than emulation would have
been: it is the actual U74 microarchitecture the ABI RFC §8.1 is asking
about. It is also the **first time any of this code will have executed**.

---

## 3 · What could not be used (a G6 finding, not an excuse)

The roadmap specifies driving "`cranelift-codegen` + `cranelift-wasm` or
`wasmtime-cranelift`'s translator". Checked against the registry:

| crate | latest published | usable? |
|---|---|---|
| `cranelift-codegen` | **0.134.2** (has a `riscv64` feature, MSRV 1.94.0) | yes |
| `cranelift-frontend` | **0.134.2** | yes |
| `cranelift-wasm` | **0.111.11** | **no** — removed upstream; would pin Cranelift ~23 minor versions back |
| `wasmtime-cranelift` | tracks wasmtime | not a standalone translator; drags the wasmtime runtime |

So the spike **hand-wrote a ~300-line wasm→CLIF translator**
(`tools/wari-aot-spike/src/translate.rs`) covering only the opcodes
`arith.wat` and `memory.wat` use: `i32` arithmetic and comparisons, locals,
`block`/`loop`/`br`/`br_if`, and `i32.load`/`i32.store`. No calls, no
`call_indirect`, no `if`/`else`, no `i64`/float, no type-section parsing.

**Consequence for G6, and it is not small:** the wasm→CLIF front end is now
*Wari's code to write and maintain*, not a dependency to pull. That is
several hundred lines of semantically load-bearing translation (every entry
in ABI RFC §4.3's trap table has to be emitted by hand) plus its own
divergence surface against `wasmi`. It should be re-estimated in the G6
sizing before the gate decision. The alternative — depending on
`wasmtime-cranelift` — pulls the wasmtime runtime into the signing pipeline
and was not evaluated here.

---

## 4 · Phase A — what was built and how it was verified

`tools/wari-aot-spike` does, per input `.wasm`:

1. translate `_start` to CLIF;
2. compile to RV64 via `cranelift-codegen` (`riscv64gc-unknown-linux-gnu`,
   `opt_level=speed`, `is_pic=false`, verifier on);
3. write the raw code buffer to `<out>.bin`;
4. wrap it in a hand-built **statically linked, freestanding RV64 Linux
   ELF** with a 72-byte `_start` shim (no libc) that calls the compiled
   function, writes its `i32` result to fd 1 as 4 little-endian bytes, and
   `exit(0)`s;
5. compile a *second* time and assert the bytes are identical (R8).

The full static ELF **was** produced — the `.o`-plus-link-command fallback
the task allowed was not needed. It is hand-emitted (ELF header + 2
`PT_LOAD` + `.text` + section headers) because this host has no RISC-V
assembler or linker driver in the pinned toolchain; every hand-encoded
instruction is cross-checked against `llvm-mc --triple=riscv64
--show-encoding` in a unit test, and the finished file is validated with
`llvm-readobj` and `llvm-objdump`.

The harness image is **R+X and R+W in separate segments — never W+X** (D4),
even though it is a Linux binary and not something Wari would ever load.

### 4.1 Structural validation of the emitted ELF

```
$ llvm-readobj --file-headers --program-headers /tmp/arith.elf
  Type: Executable (0x2)          Machine: EM_RISCV (0xF3)
  Entry: 0x100B0                  Flags [ EF_RISCV_FLOAT_ABI_DOUBLE, EF_RISCV_RVC ]
  PT_LOAD  vaddr 0x10000  filesz 504    memsz 504    [ PF_R PF_X ]  align 4096
  PT_LOAD  vaddr 0x20000  filesz 0      memsz 65536  [ PF_R PF_W ]  align 4096
```

The second segment is the zero-filled 64 KiB stand-in for linear memory
(`memory.wat` declares `(memory 1)`); its base and length are passed to the
compiled function in `a0`/`a1`.

---

## 5 · Measured numbers

**Host:** Apple M5, macOS 26.5 (25F71), arm64, `rustc 1.95.0 (59807616e)`.
**Backend:** `cranelift-codegen 0.134.2` / `cranelift-frontend 0.134.2`,
exact-pinned in `tools/wari-aot-spike/Cargo.toml`. **Parser:**
`wasmparser 0.254.0`. **Interpreter:** `wasmi 0.32.3` — the version
`kernel/Cargo.toml` pins (`wasmi = { version = "=0.32.3" }`).

*All compile-time figures are from a **release** build of the spike, min and
median over 11 process invocations. Each invocation compiles the module
twice; "cold" is the first compile in a fresh process, "warm" the second —
the gap is Cranelift's own lazy first-use initialisation, not input-dependent
work.*

### 5.1 The M0 comparison table

| | `arith.wasm` | `memory.wasm` | `fuel_bomb.wasm` |
|---|---|---|---|
| **wasmi fuel** (deterministic, `wari-bench`) | **4 004** | **1 028** | n/a (non-terminating) |
| **wasmi wall, release build**, min / median of 25 | **0.0106 / 0.0107 ms** | **0.0030 / 0.0030 ms** | n/a |
| **wasmi wall, debug build**, min / median of 25 | 0.202 / 0.209 ms | 0.072 / 0.074 ms | n/a |
| **Cranelift compile, cold**, min / median of 11 | **79 / 88 µs** | **115 / 124 µs** | 45 / 47 µs |
| **Cranelift compile, warm**, min / median of 11 | **37 / 39 µs** | **57 / 61 µs** | 16 / 18 µs |
| wasm→CLIF translate (spike's own front end), min / median | 16 / 17 µs | 19 / 20 µs | 13 / 13 µs |
| **emitted `.text` bytes** | **40** | **68** | **4** |
| source `.wasm` bytes | 74 | 79 | 44 |
| `.text` / `.wasm` ratio | **0.54** | **0.86** | 0.09 |
| residual relocations against `.text` | **0** | **0** | **0** |
| byte-identical across two compiles | **yes** | **yes** | **yes** |
| **native wall time on RV64** | **PENDING — Phase B, needs board** | **PENDING — Phase B** | **PENDING** |
| **native vs wasmi speedup** | **PENDING — Phase B, needs board** | **PENDING** | **PENDING** |

Reference `_start` return values, **measured** by running each fixture under
`wasmi 0.32.3` (a throwaway host runner, not checked in — `wari-oracle` (G2)
does not exist in the tree yet):

| fixture | value | as 4 LE bytes on stdout |
|---|---|---|
| `arith.wasm` | `499500` | `2c a0 07 00` |
| `memory.wasm` | `1020` | `fc 03 00 00` |

These are the values Phase B must reproduce.

**Read the two `wasmi wall` rows together.** The roadmap's G1 acceptance
command is `cargo run -p wari-bench` — a *debug* build — and that is what
produced `0.181–0.235 ms` in casual runs. The release build is **19× faster**
on `arith`. Any M0 argument of the form "the interpreter costs X" must state
which profile X came from. Recommendation: G1 should either default to
`--release` or print the profile in its output and JSON.

### 5.2 Reproducibility (R8)

```
$ wari-aot-spike tests/fixtures/aot/arith.wasm --out /tmp/a.elf --bin /tmp/a.bin
$ wari-aot-spike tests/fixtures/aot/arith.wasm --out /tmp/b.elf --bin /tmp/b.bin
$ shasum -a 256 /tmp/a.bin /tmp/b.bin /tmp/a.elf /tmp/b.elf
0b6cdf9dd29f1c4fd66a6383e98228ae9e639576f6beab5fb38d01b2288ae192  /tmp/a.bin
0b6cdf9dd29f1c4fd66a6383e98228ae9e639576f6beab5fb38d01b2288ae192  /tmp/b.bin
9c31dc68280bb602f098606938ce75202e9287b71fc63c18c777ea1277110c86  /tmp/a.elf
9c31dc68280bb602f098606938ce75202e9287b71fc63c18c777ea1277110c86  /tmp/b.elf
```

This closes ABI RFC §8 item 8 *weakly*: determinism holds across runs of one
binary on one host for these inputs. It says nothing about determinism across
hosts, architectures or Cranelift versions. G6's double-compile check should
be kept and extended to a cross-host comparison in CI.

### 5.3 Backend configuration actually in force

`--print-isa-flags` (recorded here because it is part of what makes the
bytes above reproducible):

```
has_m = true      has_a = true      has_f = true      has_d = true
has_zicsr = true  has_zifencei = true
has_zca = false   has_zcd = false   has_zcb = false     ← no compressed ISA
has_zba = false   has_zbb = false   has_zbs = false     ← no bit-manip
has_v = false     (all Zvl* = false)
```

Two consequences worth carrying into G6:

- **Compressed instructions are off by default.** The U74 is RV64G**C**;
  enabling `has_zca`/`has_zcd` should shrink `.text` measurably, and `.text`
  size is a first-order concern at the 10 000–50 000-instance density target
  (ABI RFC §5.1). **Not measured here.**
- **Zba/Zbb are correctly off** — the JH7110 U74 does not implement them.
  This is why the wasm `i32`→`u64` index zero-extension below costs a
  `slli`/`srli` pair rather than a single `zext.w`.

---

## 6 · Disassembly verdict

Verified with the pinned toolchain's `llvm-objdump -d` (from
`llvm-tools-preview`). **Verdict: sane RV64.** Correct psABI usage (`a0`/`a1`
in, `a0` out, `ret` via `x1`), correct loop structure, correct arithmetic,
correct constant folding on the final load.

### 6.1 `arith.wasm` — the integer hot loop (40 bytes)

```asm
100f8: 00000513   li    a0, 0x0          ; sum = 0
100fc: 00050693   mv    a3, a0           ; i   = 0
10100: 0016859b   addiw a1, a3, 0x1      ;  i + 1                 ┐
10104: 3e800613   li    a2, 0x3e8        ;  1000  (re-materialised) │
10108: 00d5053b   addw  a0, a0, a3       ;  sum += i                │ loop
1010c: 0006061b   sext.w a2, a2          ;  (redundant)             │ body
10110: 00c5d663   bge   a1, a2, 0x1011c  ;  exit if i+1 >= 1000     │
10114: 00058693   mv    a3, a1           ;  i = i + 1               │
10118: fe9ff06f   j     0x10100          ;                          ┘
1011c: 00008067   ret                    ; a0 = sum
```

Correct: it computes `sum += i` before the increment, exactly as the wasm
orders it, and returns Σ(0..999) = 499500 — the value `wasmi` returns (§5.1).

Two observations, offered as observations and not as conclusions:

- **No prologue/epilogue at all.** The function is a leaf with no spills, so
  Cranelift sets up no frame. That means this spike **does not exercise** the
  ABI RFC §3.7 stack-limit prologue check — nothing here tells us what that
  check will cost.
- **`li a2, 0x3e8` and `sext.w a2, a2` are loop-invariant and were not
  hoisted**, at `opt_level=speed`. 2 of the 7 loop-body instructions are
  dead weight. Partly this is the spike's front end emitting the `iconst`
  inside the loop block (the wasm has `i32.const 1000` there); a real
  translator that hoists constants, or Cranelift's own remat/LICM on more
  realistic input, may well remove them. **Do not read this as a Cranelift
  quality verdict** — it is one 8-instruction function.

### 6.2 `memory.wasm` — the ABI RFC §A1 bounds check, as actually emitted

This is the interesting one, because it is the first look at what
recommendation **A1** (explicit bounds checks, no guard pages) costs in real
instructions:

```asm
100fc: 02061693   slli   a3, a2, 0x20     ; ┐ zero-extend wasm i32 index → u64
10100: 0206d793   srli   a5, a3, 0x20     ; ┘ (2 insns; 1 with Zba/Zbb — U74 has neither)
10104: 00478713   addi   a4, a5, 0x4      ;   end = start + 4
10108: 00e5f463   bgeu   a1, a4, 0x10110  ;   if mem_len >= end → ok        (a1 = WMLEN)
1010c: 0000       unimp                   ; ┐ trap: 2× 2-byte illegal insn
1010e: 0000       unimp                   ; ┘
10110: 00f506b3   add    a3, a0, a5       ;   host addr = mem_base + index  (a0 = WMBASE)
10114: 00c6a023   sw     a2, 0x0(a3)      ;   the actual store
10118: 0046061b   addiw  a2, a2, 0x4
1011c: 40000693   li     a3, 0x400        ;   (loop-invariant, not hoisted)
10120: 0006869b   sext.w a3, a3           ;   (redundant)
10124: fcd64ce3   blt    a2, a3, 0x100fc  ;   loop
10128: 40000613   li     a2, 0x400        ; ┐ second, independent bounds check
1012c: 00c5f463   bgeu   a1, a2, 0x10134  ; │ for the final i32.load
10130: 0000       unimp                   ; │
10132: 0000       unimp                   ; ┘
10134: 3fc52503   lw     a0, 0x3fc(a0)    ;   constant index folded into the offset
10138: 00008067   ret
```

**This is line-for-line the shape `docs/aot-target-abi.md` §2.3 sketched**
(`slli`/`srli` zero-extend, add offset, `bltu`-family compare against the
length register, branch to trap, then add the base). The RFC's pseudocode was
written before anything was compiled; the backend independently produced the
same sequence. That is a real, if modest, validation of A1's feasibility.

Three findings:

1. **Static cost of A1 on this fixture: 5 of the 10 loop-body instructions**
   are addressing/checking overhead (`slli`, `srli`, `addi`, `bgeu`, `add`)
   versus 1 instruction of actual work (`sw`). **This is a static instruction
   count, not a measurement.** On an in-order dual-issue U74 with a
   never-taken, perfectly-predicted branch the *cycle* cost will be lower —
   possibly much lower — but that number does not exist yet and this document
   will not invent it. It is exactly ABI RFC §8.1, and Phase B is how it gets
   answered.
2. **Cranelift did not hoist or merge the two bounds checks**, even though
   the loop's trip count is a compile-time constant and every access is
   provably in range. The RFC's §2.3 hope that "Cranelift's redundant
   bounds-check elimination removes many in loops" **is not observed here** —
   though note that elimination lives in `wasmtime-cranelift`'s legalisation
   of `heap_addr`, which this spike does not use, so the fair reading is
   "the spike's hand-emitted checks get no such treatment", not "Cranelift
   cannot do it."
3. **`ins().trap()` lowers to `unimp` (illegal instruction), not to a call.**
   On Linux that is `SIGILL`; on Wari it would be an illegal-instruction
   exception into `trap.rs`. **This is not ABI recommendation A3**, which
   requires a branch to a thunk that calls `WCTX.trap_entry` (§4.2 Option 4A)
   precisely so the trap edge is privilege-agnostic, MMU-free-compatible and
   *visible to the safety certificate*. G6 must emit the thunk call itself
   rather than using Cranelift's `trap`; the checker's ability to see the trap
   edge depends on it. Worth an explicit G6 acceptance criterion.

### 6.3 `fuel_bomb.wasm` — 4 bytes, and a warning

```asm
100f8: 0000006f   j 0x100f8
```

The infinite loop compiles to an infinite loop, with **no fuel check of any
kind** — as expected, since fuel metering (ABI RFC §4.5, recommendation
4F-A) is entirely unimplemented in the spike. Practical consequence:
`fuel_bomb.elf` will spin forever. **Do not run it in Phase B without
`timeout`.**

---

## 7 · Phase B — execution on the VisionFive 2 (operator instructions)

**Not attempted from here.** The board is currently booted into the Wari
kernel, not Debian, and is not reachable from this host. The VF2 runs Debian
riscv64 natively, so the static ELF executes directly — real U74 silicon,
better evidence than any emulator.

Nothing below has been run. **The Phase-A artifacts have never been
executed**; Phase B is simultaneously the performance measurement *and* the
first correctness test of both the compiled code and the hand-built ELF. If
the ELF is malformed, the first symptom will be `Exec format error` or a
`SIGSEGV` at `0x100b0`.

### 7.1 Build the artifacts

```bash
cd /path/to/wari
cargo build --release -p wari-aot-spike

# correctness image: one call, prints the result
./target/release/wari-aot-spike tests/fixtures/aot/arith.wasm  --out /tmp/arith.elf
./target/release/wari-aot-spike tests/fixtures/aot/memory.wasm --out /tmp/memory.elf

# timing image: 1e6 calls, so the wall time is not dominated by exec+exit
./target/release/wari-aot-spike tests/fixtures/aot/arith.wasm \
    --out /tmp/arith_1m.elf --repeat 1000000
```

### 7.2 Copy to the board (boot it into Debian first)

```bash
scp /tmp/arith.elf /tmp/memory.elf /tmp/arith_1m.elf debian@<vf2-ip>:~/
ssh debian@<vf2-ip>
```

### 7.3 Correctness — must match `wasmi` exactly

```bash
chmod +x arith.elf memory.elf arith_1m.elf

./arith.elf  | xxd     # expect: 2ca0 0700   → 499500
./memory.elf | xxd     # expect: fc03 0000   → 1020
```

Any other bytes, a `SIGILL`, or a `SIGSEGV` is a **compiler or harness bug**
and must be reported before any timing number is believed. `SIGILL` in
particular means an emitted bounds check tripped — the `unimp` of §6.2.

### 7.4 Timing — the number the M0 gate actually wants

```bash
# native, 1e6 iterations of arith's 1000-iteration loop
time ./arith_1m.elf > /dev/null
# → divide `real` by 1e6 for per-call native wall time
```

Then, **on the same board**, the interpreter side of the comparison:

```bash
# build wari-bench for riscv64 (release! see §5.1) and run the same fixture
cargo build --release -p wari-bench
./target/release/wari-bench tests/fixtures/aot/arith.wasm --runs 25 --json bench-vf2.json
```

Compare `wall_ms_min` from `wari-bench` against the per-call native time.
**Both sides must be release builds and both must run on the U74** — the
host numbers in §5.1 are Apple M5 numbers and are not comparable to RV64.

Fill the `PENDING` rows of §5.1 with what comes back. Also worth capturing
while the board is up, because they are open ABI-RFC questions (§8.5, §8.6)
and cheap to answer there: `cat /proc/cpuinfo`, and whether misaligned
loads/stores fault, trap-and-emulate, or run natively.

---

## 8 · What this spike did not do — read before quoting it

| | |
|---|---|
| Executed any compiled code | **No.** §2, §7. Every correctness claim here is by disassembly and by `wasmi` reference values, not by running. |
| Measured native performance | **No.** Every performance row is `PENDING`. |
| Compiled a realistic module | **No.** The translator handles `i32` arithmetic, locals, `block`/`loop`/`br`/`br_if`, and `i32.load`/`i32.store`. `calls.wasm` and `hostcall.wasm` are rejected with `spike does not support operator Call {..}`. |
| Exercised host calls | **No.** The ABI RFC §3 trampoline — the thing the `hostcall.wat` fixture exists to price — is completely untested. This is arguably the *most* important unmeasured quantity for the sovereign-cloud orchestration workload. |
| Exercised the trap mapping | **No.** Only `HEAP_OUT_OF_BOUNDS` is ever emitted; the div/rem-by-zero, `INT_MIN/-1` and float→int cases of §4.3 — the ones where RV64 silently succeeds — are untouched. |
| Exercised fuel metering | **No.** §6.3. |
| Exercised the stack-limit prologue | **No.** §6.1 — leaf function, no frame. |
| Produced a WNM, a signature, or a cert | **No.** That is G6/G7. |
| Answered how many registers Cranelift can reserve (ABI §8.3) | **No.** The spike passes `mem_base`/`mem_len` as ordinary arguments; `enable_pinned_reg` was not exercised. **Still open, and it is a G6 blocker for the WCTX design.** |
| Measured the `.text`/`.wasm` ratio on realistic code (ABI §8.7) | **No.** 0.54 and 0.86 on 74- and 79-byte fixtures are not a ratio; they are noise. |

One unrelated observation from using `wari-bench` read-only: it reports
`peak_linmem_pages = 0` for `memory.wasm`, which declares `(memory 1)` and
writes 1 KiB into it. That looks like a gap in G1's measurement rather than a
true zero. Flagged, not touched — different lane.

---

## 9 · Recommendation to the architect

**On DG-1 (backend):** the confirmation of Cranelift-offline is supported by
this spike. The backend drives cleanly as a library, targets RV64 today,
emits relocation-free text, and is byte-reproducible. Nothing found here
argues for bespoke codegen or `wasm2c`.

**On the M0 go/kill decision: not yet decidable.** The single number the gate
turns on — *how much faster is compiled RV64 than `wasmi` on the U74* — is
`PENDING` and needs Phase B. Two things sharpen the question while the board
is being freed up:

1. The release-build interpreter baseline (§5.1) is 19× better than the
   debug number that has been circulating. The bar AOT has to clear is much
   higher than it looked.
2. The static bounds-check overhead of recommendation A1 (§6.2) is real and
   was not eliminated by the optimiser in this configuration. Per ABI RFC
   §2.5's own framing, if that cost proves intolerable the correct response
   is interpreter tuning, not guard pages.

**Concrete G6 acceptance criteria this spike suggests adding:**

- emit trap edges as explicit calls through `WCTX.trap_entry`, and assert no
  `unimp`/illegal-instruction encoding appears anywhere in `.text` (§6.2);
- assert the residual relocation set against `.text` is empty (already ABI
  §5.1 — confirmed achievable, observed 0 on every fixture);
- double-compile `sha256` equality (already ABI §6 — confirmed achievable);
- evaluate `has_zca`/`has_zcd` for `.text` size before freezing the flag set
  (§5.3), since the artifact is signed and the flags are part of what makes
  it reproducible;
- resolve ABI §8.3 (reservable registers / `enable_pinned_reg` on riscv64)
  early — the WCTX design depends on it and this spike did not test it.

---

## 10 · Reproducing Phase A

```bash
cd /path/to/wari
cargo build --release -p wari-aot-spike
cargo test  -p wari-aot-spike           # 4 tests: encodings, ELF header,
                                        # codegen non-empty + reloc-free,
                                        # byte-reproducibility
cargo clippy -p wari-aot-spike --all-targets -- -D warnings
cargo fmt -p wari-aot-spike --check

# the numbers in §5
./target/release/wari-aot-spike tests/fixtures/aot/arith.wasm --out /tmp/arith.elf
./target/release/wari-aot-spike tests/fixtures/aot/arith.wasm --out /tmp/arith.elf --print-isa-flags
cargo run --release -p wari-bench -- tests/fixtures/aot/arith.wasm \
    tests/fixtures/aot/memory.wasm --runs 25 --json /tmp/bench.json

# the disassembly in §6
"$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objdump -d /tmp/arith.elf
```

---

## 11 · Prior art

| Pattern | Source | Used for |
|---|---|---|
| Offline wasm→native codegen backend for RV64 | **Cranelift / Wasmtime** | the backend this spike drives (DG-1) |
| Explicit bounds checks in compiled output | **wasm2c** (wabt); Wasmtime's dynamic-memory path | §6.2 — the shape observed |
| `vmctx`-mediated instance state | **Wasmtime** | the `a0`/`a1` mem_base/mem_len stand-in for WCTX |
| AOT-not-JIT, sign-then-load | **Fastly Lucet** | why this is a spike and not a JIT |
| Reference semantics | **wasmi 0.32.3** | the `_start` values Phase B must reproduce |
| Freestanding `_start` + raw `write`/`exit` harness | standard Linux `nolibc` practice | §4 — no libc in the RV64 image |
