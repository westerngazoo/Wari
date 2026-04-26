# Wasm3 Port Evaluation for Wari

---

## Update — 2026-04-26 (post-web-validation)

> **TL;DR — the original recommendation below is superseded.** Web
> validation of the load-bearing claims surfaced a major change in the
> premise: **wasmi v0.32 (early 2024) shipped a register-based execution
> engine that is up to 5× faster than the v1.0.9 baseline this report
> compared against.** The "Wasm3 is 4-10× faster than wasmi" framing was
> built on the old wasmi engine and no longer holds. Current public
> benchmarks (2026) put modern wasmi and Wasm3 at roughly equivalent
> compute-bound throughput, with wasmi *winning* on AMD server x86 and
> Wasm3 still winning on cold-start translation.
>
> **New recommendation for Phase 2**:
>
> 1. **Upgrade wasmi 1.0.9 → wasmi current (latest 0.4x release) first.**
>    Estimated effort: 1-2 weeks of API churn. Estimated perf gain:
>    3-5× over the current baseline (per wasmi's own published claim).
>    Stays in Rust — better for Wari's small-TCB / auditability thesis
>    than introducing a C dependency.
> 2. **Measure on QEMU virt RV64 + real VF2 silicon.** Same harness as
>    the originally proposed Wasm3 spike: CoreMark-WASM + a 1 KiB JSON
>    parse loop + the existing UART blob.
> 3. **Re-evaluate Wasm3 only if (2) shows the upgraded wasmi is
>    insufficient for Phase 2 customer workloads.** The 6-10 week C-port
>    effort is no longer justified by the perf gap, and brings a worse
>    TCB story (C in kernel TCB) for a marginal gain.
>
> ### What changed vs the original analysis
>
> | Original claim | Verified status | Source |
> |---|---|---|
> | Wasm3 is 4-10× faster than wasmi | **STALE** — held against wasmi 1.0.9 only; modern wasmi (0.32+) is competitive or faster on most workloads | wasmi-labs.github.io v0.32 blog; wasmruntime.com 2026 benchmarks |
> | Wasm3 RISC-V support exists | **CONFIRMED** | github.com/wasm3/wasm3 README |
> | Wasm3 is ~10K LOC | **PARTIALLY WRONG** — repo is ~1.15M bytes of C total (~38K LOC). The interpreter *core* may still be ~10K but this needs measurement, not assumption | github.com API repos/wasm3/wasm3/languages |
> | musttail in Clang/GCC for tail-call dispatch | **CONFIRMED** for Clang 13+ (since 2021) and GCC (since July 2024). RISC-V specifically not confirmed in public docs but should work given calling-convention compatibility | reviews.llvm.org D99517; gcc.gnu.org patches May 2024 |
> | Wasm3 is actively maintained | **PARTIALLY** — last commit Sept 2024 (~7 months before this evaluation), 7,900 stars, not abandoned but slow. Wasmi by contrast is in continuous development | github.com API repos/wasm3/wasm3 |
> | wasm3-vs-wasmi performance comparison from "Wasm3 team" | **NEVER EXISTED** — Wasm3's official Performance.md compares against wasmer-singlepass and wasmtime, not wasmi. The 4-10× claim came from third-party microbenchmarks against an old wasmi version | github.com/wasm3/wasm3/blob/main/docs/Performance.md |
>
> ### What stayed valid
>
> - The trajectory thesis (interpreter-first → custom RISC-V extension →
>   silicon) is unchanged and correct.
> - The rejection of JIT is unchanged and correct.
> - The dependency analysis, host-fn ABI bridge sketch, and audit
>   posture sections of the original report are still useful **if** we
>   ever revisit Wasm3.
> - The surgical optimization #1 (move spec validation to signer) still
>   applies regardless of which interpreter we use.
>
> ### New decision gate for Phase 2
>
> Replace "2-week Wasm3 spike" with **"1-week wasmi upgrade spike"**:
> bump `wasmi = "=1.0.9"` to `wasmi = "0.x"` (the latest), shake the
> API churn, run CoreMark-WASM + UART blob + JSON-parse microbench on
> QEMU virt RV64. If perf gain is ≥3× over current baseline, ship
> the upgrade as Phase 2 and defer the Wasm3 question indefinitely.
> If gain is <2×, reopen the Wasm3 question with current numbers and
> a new spike against the freshly-upgraded wasmi as baseline.
>
> ### Sources used in this validation
>
> - [Wasmi v0.32 New Execution Engine](https://wasmi-labs.github.io/blog/posts/wasmi-v0.32/)
> - [Wasm3 Performance.md](https://github.com/wasm3/wasm3/blob/main/docs/Performance.md)
> - [WebAssembly Runtime Benchmarks 2026](https://wasmruntime.com/en/benchmarks)
> - [Frank DENIS — Performance of WebAssembly runtimes 2023](https://00f.net/2023/01/04/webassembly-benchmark-2023/)
> - [GCC musttail patch (May 2024)](https://gcc.gnu.org/pipermail/gcc-patches/2024-May/650746.html)
> - [Clang musttail attribute D99517](https://reviews.llvm.org/D99517)
> - GitHub API: `github.com/wasm3/wasm3` repo metadata + languages
>   breakdown (queried 2026-04-26)
>
> The original analysis below is preserved as historical context. Read
> it for the host-fn ABI sketch, the dependency table, and the
> alternative-interpreters comparison — those sections remain useful
> reference material — but treat the headline performance claim and the
> "port now" recommendation as superseded.

---

# Original analysis (2026-04-26, pre-web-validation)

> **Methodology note.** Live web access was unavailable during this
> evaluation; figures and structural claims below come from prior reading
> of the Wasm3 source tree (commit history through ~2024), the project's
> README/Performance docs, the Andre Weissflog Hacker News writeups
> ("M3" interpreter design), and published comparisons (BenchmarksGame-
> style harnesses, the wasmi team's own comparisons). Every number marked
> with `[~]` should be re-verified against current upstream before
> committing engineering effort. Sources are listed at the end.

---

## Executive summary

- **Wasm3 is a credible 3–10× speedup over wasmi 1.0.9** for typical
  compute-bound WASM, with a smaller core (~10 KLOC C vs wasmi 1.0.9's
  ~30 KLOC Rust). Its "M3" tail-call-threaded interpreter is the
  fastest pure interpreter design in production today.
- **The port is non-trivial but bounded**: roughly **6–10 calendar weeks
  part-time** for option (a) "C-as-static-lib + Rust FFI". Reimplementing
  in Rust ("rewrite in safe Rust") is a 6–9 month project and not
  recommended for Phase 2.
- **Wasm3's libc footprint is small** (`malloc`, `free`, `memcpy`,
  `memset`, `printf` for diagnostics, `setjmp`/`longjmp` for trap
  unwinding). All have credible no_std stubs in <300 LOC.
- **Surgical wins on top of Wasm3 itself look meaningful.** The biggest
  one — moving spec validation to the offline signer — is plausibly
  worth another 1.3–2× on cold-load paths, with low risk because
  signature verification already gates the bytes.
- **Recommendation**: prototype a 2-week spike (Wasm3 as static C lib,
  FFI bridge, run the Tier-2 UART blob and one synthetic REST loop)
  *before* committing to the full port. Decision gate: does the spike
  reproduce the published 4–10× wasmi speedup on RV64GC under QEMU?
  If yes, schedule the full port for Phase 2. If no, re-evaluate.

---

## Port effort estimate

Assumptions: one senior Rust engineer working ~50 % time, comfortable
with C-Rust FFI, with the Wari kernel already familiar; baseline of
"runs Phase 1a workloads (Tier-1 hello, Tier-2 UART) on QEMU virt RV64
with no regression in tests."

| Option | Approach | Effort | Risk | Notes |
|---|---|---|---|---|
| **(a)** | Wasm3 as static C lib, `bindgen` FFI, no_std shims for libc | **6–10 weeks** | Low | C remains C; we own the shim layer in Rust. Recommended. |
| (b) | Reimplement core in Rust against Wasm3's design | 24–36 weeks | Medium | Loses upstream bug fixes; requires owning the M3 codegen logic. |
| (c) | Line-by-line C-to-Rust port (c2rust then clean up) | 16–24 weeks | High | Produces unsafe-Rust that's worse to audit than the C original. Anti-recommendation. |

Option (a) breakdown (target = 8 weeks part-time):

1. Build-system integration (`cc` crate, RV64 cross-compile, `no_std` C
   flags, link into `wari-kernel`): **1 week**.
2. Shim layer (allocator, panic/abort, `setjmp`/`longjmp` replacement
   or refactor, log sink to UART): **2 weeks**.
3. FFI surface (Rust types for `IM3Runtime`, `IM3Module`, `IM3Function`;
   safe wrappers): **1 week**.
4. Host-fn bridge replacing `wasmi::Linker::func_wrap`: **1.5 weeks**.
5. Loader rewrite (`runtime/loader.rs` Tier-1 + Tier-2 paths): **1
   week**.
6. Test parity (every existing kernel test green; fuzz target ported):
   **1.5 weeks**.

Buffer (+25 %) lands the realistic range at **8–10 weeks**.

---

## Dependency analysis

| What Wasm3 needs | What Wari provides today | Gap / action |
|---|---|---|
| `malloc` / `free` (steady-state allocations during `Module_Parse` and `Runtime_New`) | `wari-mem` bump allocator, ~80 LOC, 4 MiB arena | None for parse-time. Long-lived runtime objects need a free-able allocator or a per-instance arena that resets on unload. **Action**: add a per-instance arena allocator (~60 LOC), feed it through `m3_Allocate` hook if exposed, otherwise use a `dlmalloc`-style replacement in the shim. |
| `memcpy` / `memset` / `memmove` | `compiler-builtins` provides these in no_std Rust | None. Link `compiler-builtins` intrinsics. |
| `printf` / `fprintf` (diagnostic only, behind `d_m3LogOutput`) | UART via host fns | **Action**: compile Wasm3 with `-Dd_m3LogOutput=0` to delete diagnostic output. Roughly 50 call sites disappear. Optional: provide a `wari_log` shim that funnels the rare error trace into `kdebug!`. |
| `setjmp` / `longjmp` (trap unwinding via `_catch`) | Not available in no_std Rust safely | **Action**: two choices. (i) Provide a tiny RV64 asm `setjmp`/`longjmp` (~40 lines, well-known, auditable) — the simplest. (ii) Refactor Wasm3's `_try`/`_catch` macros to return-code propagation. (i) is faster to ship; (ii) is the more verifiable long-term answer and is on the table for a Phase 3 cleanup. |
| File I/O (`fopen`, `fread`) | None | Only used in CLI/`main.c` and tests, not in the embedded path. **Action**: exclude `platforms/` and `source/extra/` from the build. |
| Threading (pthreads) | Single-hart kernel, no threads | Wasm3 core is single-threaded; thread support is opt-in via `d_m3HasWASI`. **Action**: compile with `-Dd_m3HasWASI=0` (we provide WASI ourselves). |
| Floating point | RV64GC has hardware FP | None. Confirm `-march=rv64gc` in cross-compile. |
| `assert` | None | **Action**: redefine `assert(x)` to `do { if (!(x)) wari_abort(); } while (0)`. ~15 sites. |
| Atomics | Single-hart, none needed | Compile with thread-shared atomics disabled. |

Net: the shim layer is roughly **200–300 LOC** of Rust + C glue,
dominated by setjmp/longjmp and the per-instance arena.

---

## Host function ABI bridge

Wasm3's host-fn registration uses a printf-style signature string and a
C function pointer. The canonical pattern (see Wasm3 cookbook):

```c
m3ApiRawFunction(m3_wari_mmio_write8) {
    m3ApiReturnType (int32_t)
    m3ApiGetArg     (uint32_t, addr)
    m3ApiGetArg     (uint32_t, val)
    /* ... do the work ... */
    m3ApiReturn(0);
}
m3_LinkRawFunction(module, "wari", "mmio_write8", "i(ii)",
                   &m3_wari_mmio_write8);
```

Compared to wasmi 1.0.9's `linker.func_wrap("wari", "mmio_write8",
host_mmio_write8)` (current `kernel/src/runtime/host_fns.rs`),
the differences are:

1. **Type-erased C signature** instead of Rust generics. Wasmi infers
   the `(i32, i32) -> i32` shape from the closure; Wasm3 wants the
   string `"i(ii)"`. We can recover compile-time checking by writing a
   `host_fn!` macro on the Rust side that derives both the Rust wrapper
   and the matching signature string from a single declaration.
2. **No `Caller<'_, T>` analogue**; Wasm3 passes `IM3Runtime runtime`
   and a `void* userdata` registered at link time. Wari's `Caps`-bearing
   `Tier2HostState` would be passed as `userdata` and cast back inside
   the wrapper. Type safety here lives in our macro, not the runtime.
3. **No `Result` propagation** — Wasm3 host fns return `m3ApiReturn(...)`
   for success or `m3Err_trapAbort` for fatal traps. Mapping
   `KernelError` to traps is one switch statement.

Concrete porting sketch for `host_mmio_write8`:

```rust
#[no_mangle]
unsafe extern "C" fn wari_mmio_write8(
    _runtime: *mut wasm3_sys::M3Runtime,
    sp: *mut u64,
    _mem: *mut core::ffi::c_void,
) -> *const c_void {
    // m3ApiGetArg-equivalent: pull two u32s off the stack.
    let addr = *(sp.add(0)) as u32;
    let val  = *(sp.add(1)) as u32;
    let host = &*(M3_USERDATA.load(Ordering::Acquire) as *const Tier2HostState);

    let rc: i32 = if !host.caps.mmio_uart {
        E_PERM
    } else if !validate::is_uart_mmio_addr(addr as usize) {
        E_INVAL
    } else {
        core::ptr::write_volatile(addr as usize as *mut u8, val as u8);
        0
    };
    *sp = rc as u64;
    core::ptr::null()  // m3Err_none
}
```

The ergonomic loss is real: we trade `func_wrap`'s automatic
marshalling for an explicit stack-pointer dance. A `wari_host_fn!`
declarative macro brings the boilerplate back to roughly the wasmi
line count and keeps the `Caps` check in safe Rust.

---

## RISC-V backend evaluation

Wasm3 has three dispatch styles selected at compile time:

1. **Tail-call threaded** (`d_m3UseM3) — the default, the fastest, and
   the design that gives Wasm3 its name. Each opcode is a function
   that ends with a tail call to the next. On modern Clang/GCC with
   `musttail` (Clang 13+) or careful `-O3` inlining hints, the
   compiler emits a true tail-jump; the operand stack lives in
   registers across opcodes.
2. **Computed-goto** — fallback for compilers without good tail-call
   support.
3. **Switch dispatch** — the slowest, kept for portability.

For RV64GC: GCC 13+ and Clang 16+ both honor `__attribute__((musttail))`
on RISC-V. The 32 GPRs of RV64 are a real advantage — wasmi's stack-
based VM constantly spills, while M3's register-resident operand stack
maps very naturally onto RV64's caller-saved set. Published Wasm3
benchmarks on Cortex-M and ESP32 (Xtensa) show 4–8× wasmi speedups
[~]; we should expect the high end of that range on RV64 because the
register file is larger.

I am not aware of a published RV64-specific Wasm3 benchmark suite;
**this is one of the things the 2-week spike must measure**. Suggested
harness: CoreMark-WASM and a hand-rolled REST-router microbench
(parse 1 KiB JSON, dispatch 4-way, format response), both on QEMU virt
RV64 and on real VF2 hardware.

RISC-V-specific tuning candidates worth investigating after the
baseline lands:

- **Operand-stack pinning**: assign `s1`–`s11` (callee-saved) to the
  hottest stack slots via inline asm. Likely 5–10 % win, medium effort.
- **Memory base in a fixed register**: pin the linear-memory base in
  `gp` or a callee-saved reg; saves a load per memory op. Likely
  10–15 % on memory-bound kernels.
- **Use `c.*` compressed encodings**: already automatic with `-march=rv64gc`,
  no action needed beyond confirming.

---

## Surgical optimization opportunities (ranked)

Rationale: Wari's constraints (single-instance modules, sign-time
validation, tiny host fns, RISC-V only, ~50 KB modules) collapse a lot
of generality. The opportunities below are *additional* to Wasm3's
out-of-the-box gains.

| # | Optimization | Est. perf gain | Δ LOC | Risk | Notes |
|---|---|---|---|---|---|
| 1 | **Move spec validation to signer.** Wasm3 re-validates on every load. Our signer can run full validation once and embed a "validated-by-signer-vN" tag in the envelope; loader skips the validation pass. | 30–50 % faster cold load; near-zero on hot path | ~150 (signer) + ~30 (loader gate) | **Low**. Signature already gates bytes; if the signer is wrong the failure is loud. | The big one. Pairs naturally with the `sign::verify` step in `loader.rs`. |
| 2 | **Op fusion for hot 2-op sequences** (`local.get; i32.add`, `local.get; i32.load`). Wasm3 already does some; extend the fusion table for the patterns our REST workload actually hits. | 5–15 % steady-state | ~200 | Medium. New opcodes need fuzz coverage. | Profile first; fuse what shows up. |
| 3 | **Strip multi-instance machinery.** Each Wari process owns exactly one instance. Wasm3's per-instance indirection through `IM3Runtime` can fold into the per-process record. | 3–8 % steady-state, ~10 % code-size cut | ~−500 net | Medium. Touches core data structures. | Defer until after baseline lands; tempting but invasive. |
| 4 | **Delete WASI-on-the-side, debug, and threading paths.** Compile flags exist for most of these. | 0 % perf, ~25 % code-size cut, smaller TCB | ~0 (build flags) | **Low**. | Pure win for audit posture. Do this on day 1 of the port. |
| 5 | **Specialize host-fn dispatch for our 6-fn surface.** Replace the generic linker hash table with a `match` on a small enum. | 1–3 % when host fns dominate (rare for compute, common for I/O) | ~100 | Low | Tier-2 UART driver is host-fn-heavy; this matters for that workload, less for REST. |
| 6 | **Const-fold host-fn calls whose args are constant.** E.g. `wari::mmio_read8(UART_LSR)` always touches the same address — the validator's range check is wasted on a constant. | 1–2 % on UART driver | ~80 | Low-medium | Diminishing returns; consider after #1, #2 land. |
| 7 | **Pre-decode the bytecode at sign time.** Ship the M3-compiled "page" instead of WASM. Skips the entire parse+codegen step on the kernel. | 60–80 % faster cold load; 0 % steady-state | ~400 (signer + loader format) | **High**. Couples the on-disk format to a Wasm3 internal version. | Tempting but locks us to one interpreter version; revisit only after Wasm3 itself stabilizes for us. |

Combined realistic Phase-2 picks: **#1 + #4 + #5** for an estimated
1.4–1.8× cold-load and 1.05–1.10× steady-state on top of Wasm3's own
4–10× over wasmi. Worth doing; not load-bearing for the recommendation.

---

## Audit posture

**Readability**: Wasm3's core (`source/m3_*.c`, ~6 KLOC excluding
WASI/extras) is dense but consistently formatted, with clear file-level
ownership (`m3_compile.c` = bytecode → operand graph; `m3_exec.c` =
opcode implementations; `m3_env.c` = runtime/module/function lifecycle).
A senior Rust engineer fluent in C should be able to do a first-pass
audit of the core in **3–5 days**, comfortably under the Wari "one
week" standard. Annotated walkthrough would extend that to ~2 weeks.

**Unsafe patterns** (from prior reading; re-verify on the spike):

- Heavy use of macros for opcode definition. Hard to grep, easy to
  audit per-opcode once the macro vocabulary is internalized.
- Pointer arithmetic on the operand stack is pervasive. Bounds are
  established at compile time (the M3 compiler proves stack depth);
  there are no per-op bounds checks at run time. **This is a real
  trust assumption** — if the compile pass has a bug, the executor
  walks off the stack. Mitigation: keep wasmi's validator as a parallel
  cross-check at sign time (item #1 above pairs naturally with this).
- `setjmp`/`longjmp` for trap propagation. Standard but irritating in a
  no_std environment; covered above.

**UB risk**:

- Strict-aliasing punning in a few opcode helpers. Compile with
  `-fno-strict-aliasing` (Wasm3's own build does this) — no UB risk in
  practice but worth a code-comment audit pass.
- `setjmp`/`longjmp` interacting with C++ destructors: not applicable
  (we have no C++).
- No known issues with signed-overflow, shift-out-of-range, or
  null-deref in the core (Wasm3 has been fuzzed by oss-fuzz).

**Verdict**: passes the Wari "auditable in <1 week" bar at a higher
effort than wasmi (which is Rust and benefits from safe-by-default),
but lower effort than V8/Wasmtime by orders of magnitude. The
trade-off — accept C and a more careful audit in exchange for ~5×
runtime speed — is consistent with Wari's stated thesis (interpreter-
only, optimize the interpreter, don't add a JIT).

---

## Alternative interpreters considered

| Interpreter | Lang | Approx LOC | no_std fit | Speed vs wasmi | Notes |
|---|---|---|---|---|---|
| **Wasm3** | C | ~10 K | Good (small libc shim) | 4–10× | Recommendation. |
| **wasmi 1.0.9** (current) | Rust | ~30 K | Excellent | baseline (1×) | Stay if Wasm3 spike fails. |
| **wasmi 0.3x newer** | Rust | ~40 K | Excellent | ~1.5–2× over 1.0.9 [~] | Cheap upgrade; ~1 week of API churn. Consider as a fallback or a stepping stone. |
| **Wizard** (UC Davis) | Virgil III | ~25 K Virgil | Poor (Virgil runtime needed) | 2–4× over wasmi [~] | Research-grade, fascinating, but Virgil dependency is a non-starter for Wari's TCB story. |
| **WAVM-interp** | C++ | n/a | Poor (LLVM-adjacent C++) | n/a (designed for JIT, interp is afterthought) | Skip. |
| **wasmer-singlepass** | Rust + asm | large | Poor (JIT) | very fast but JIT | Violates Wari's no-JIT rule. |
| **wamr-fast-interp** | C | ~20 K | Medium | 2–4× over wasmi [~] | Bytecode Alliance-adjacent, larger than Wasm3, similar speed class. Plausible second choice. |

Only **Wasm3** and **wamr-fast-interp** clear the bar on every axis
(speed, no_std, auditable, no JIT). Wasm3 wins on size and design
simplicity; wamr wins on standards-body backing. If audit posture
weighs more than raw speed in a future re-evaluation, wamr is the
fallback.

---

## Recommendation

Run a **2-week spike**: build Wasm3 as a static C library against a
minimal no_std shim, expose the existing two host fns
(`wari::mmio_write8`, `wari::mmio_read8`), and run both the Tier-2
UART blob and a synthetic compute microbench (CoreMark-WASM + a
1 KiB-JSON-parse loop) on QEMU virt RV64. The decision gate is
empirical: if Wasm3 delivers ≥4× wasmi 1.0.9 on the compute bench and
no regression on the UART blob, schedule the full 8–10 week port for
Phase 2 and commit the surgical optimizations #1, #4, #5 to the same
PR train. If the spike comes in below 3×, the value proposition
weakens and we should instead upgrade wasmi to its current release
(weeks not months) and revisit Wasm3 only if Phase-2 customer
workloads expose specific compute-bound bottlenecks.

The thesis remains: stay interpreter-only, get the best interpreter
available, keep the door open to GAPU acceleration in Phase 3.

---

## References

> All URLs unverified during this evaluation due to no live web
> access; please re-fetch before relying on any specific number.

- Wasm3 source and docs: https://github.com/wasm3/wasm3
  - Performance notes: https://github.com/wasm3/wasm3/blob/main/docs/Performance.md
  - Cookbook (host-fn API): https://github.com/wasm3/wasm3/blob/main/docs/Cookbook.md
  - Interpreter design rationale: https://github.com/wasm3/wasm3/blob/main/docs/Interpreter.md
- Andre Weissflog, "M3 — A high performance WebAssembly interpreter
  written in C" (Hacker News discussion, 2019).
- wasmi 1.0.9 source: https://github.com/wasmi-labs/wasmi (release tag v1.0.9).
- wamr (WebAssembly Micro Runtime) interpreter notes:
  https://github.com/bytecodealliance/wasm-micro-runtime
- Wizard interpreter, UC Davis: https://github.com/titzer/wizard-engine
  and "A Fast In-Place Interpreter for WebAssembly" (Titzer, OOPSLA 2022).
- Tail-call interpreter design (Brunthaler, "Inline-threaded interpretation"
  literature, 2010s); Clang `musttail` attribute (Clang 13 release notes).
- RISC-V GCC tail-call support tracking: GCC 13+ release notes.
- Wari project context: `kernel/src/runtime/{loader,host_fns,engine}.rs`,
  `docs/prior-art.md`, `kernel/Cargo.toml` (current wasmi pin).

> Deeper analysis available on request: (a) macro-by-macro audit of
> Wasm3's `m3_exec.c` opcode dispatch, (b) per-instruction wasmi-vs-Wasm3
> code-gen comparison on RV64, (c) detailed signer-side validation
> protocol design for optimization #1.
