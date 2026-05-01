# QEMU wasmi `instantiate_and_start` hang — research report

> Bug: `wasmi::Linker::instantiate_and_start` for the Tier-1 hello WASM
> hangs indefinitely on QEMU `virt` RV64 but completes in seconds on
> StarFive VisionFive 2 (JH7110) silicon. Same kernel source, same
> `wasmi = "=1.0.9"`, same 289-byte module. Read-only research; no
> code modified.

## TL;DR

- **Most likely root cause is QEMU TCG-specific behavior**, not a
  wasmi bug. The same wasmi 1.0.9 binary works on real RV64 silicon
  for the identical module; that asymmetry rules out almost every
  wasmi-internal hypothesis (translator, host-fn dispatch, module
  shape) and points at something only QEMU's TCG path exercises
  differently.
- **wasmi 1.0.9 is itself a bug-fix release** (released 2026-02-09;
  fixes a `local.get/local.set/local.tee` + `global.get` translator
  miscompile, issue #1779). The Wari pin is the patched version, so
  this hang is not the bug 1.0.9 fixed. There are no other open or
  recently-closed wasmi-labs issues that match the shape of this
  hang.
- **Top hypothesis [~]**: a tight spinlock in wasmi's no_std
  synchronisation path (`spin::Once`, `spin::Mutex`) interacts with
  QEMU's RISC-V LR/SC reservation handling. QEMU has open and
  historic bugs around RV64 atomic reservation tracking; under TCG
  these can silently invalidate reservations and cause an infinite
  SC-fail loop.
- **Second hypothesis [~]**: the differentiator is the 17-page
  initial linear memory plus a `Vec::resize(N, 0)`-shaped fill on
  the bump allocator, where a ~1 MiB memset hits a slow QEMU TCG
  store path. "Slow" here would still be seconds, not 90+s — ranked
  below #1 but not ruled out.
- **Cheapest next step**: run QEMU with `-d in_asm,exec` for a few
  seconds after `[t1:2] DBG: instantiate_and_start` is logged. If
  the PC orbits a tiny `lr.d`/`sc.d` window, hypothesis #1 is
  confirmed. If it sweeps a memset loop, hypothesis #2.
- **Recommended workaround if a fix is non-obvious**: ship Phase 2's
  wasmi upgrade ahead of schedule. The 0.32.x or post-1.0
  main-branch lineage rework the instantiate path and may sidestep
  the offending `spin` use.

## Reproduction summary

`run_tier1` (`kernel/src/runtime/mod.rs:188`) calls `load_tier1`
(`kernel/src/runtime/loader.rs:225`) which builds an `Engine`,
parses the 289-byte hello blob (succeeds — `Module ok` is logged),
registers seven host fns (`fd_write`, `proc_exit`, five
`wari::cap_*`), then calls `linker.instantiate_and_start(&mut store,
&module)`. On QEMU virt the process hangs at that call until the
90-second test timeout. On VF2 silicon the same code path returns a
`Tier1Instance` in well under a second and the module runs to clean
`proc_exit(0)`.

The Tier-2 UART driver (255-byte WASM, 1 page initial linmem) and
the Tier-2 net driver (31 KiB WASM with smoltcp, larger initial
linmem) both instantiate fine on QEMU before the hello hang, ruling
out broad wasmi-on-QEMU brokenness. The Tier-1 module is the only
one that hangs and the only one that combines (a) WASI imports, (b)
a 17-page initial linear memory (1.0625 MiB; the Rust `cdylib`
default with a 1 MiB shadow stack), and (c) an active data segment.

## Hypothesis ranking

### 1. QEMU TCG mishandling RISC-V LR/SC reservations inside wasmi's `spin` synchronisation [~] — moderate confidence

wasmi 1.0 with `default-features = false` depends on the `spin`
crate for its locks (`spin::Mutex`, `spin::Once`) — confirmed by
wasmi-labs' own docs. `spin::Once::call_once` and
`spin::Mutex::lock` are CAS loops on top of LR/SC on RV64.

QEMU's RISC-V atomic emulation has documented gaps:

- gitlab.com/qemu-project/qemu issue #594: faults from AMO
  instructions are reported as load faults instead of store/AMO
  faults.
- launchpad bug #1908626: atomic test-and-set instruction
  occasionally does not work correctly under QEMU.
- multiple historical reports of LR/SC reservation tracking being
  fragile under TCG.

On real VF2 silicon LR/SC is implemented in hardware exactly as the
spec requires, so a wasmi spinlock that loops a few times on first
use completes immediately. Under QEMU TCG, if a reservation is
silently invalidated by an unrelated memory op in the same
translation block, the SC fails forever and the spinlock spins
forever.

Why this hypothesis fits the asymmetry:

- Works on silicon, hangs on QEMU: matches a TCG-only bug.
- Tier-2 modules instantiate fine: each `load_tier*` constructs a
  fresh `Engine`, so any per-engine `Once` re-fires; if Tier-1 has
  a host-fn surface that touches a different `Once` slot first (the
  cap_lookup variant takes `&mut Caller`), it could be the
  contended one.
- Removing host fns and skipping cap init didn't help: those run
  before instantiate; the hang is downstream of them.

What would falsify it: a `-d exec` trace that shows the PC sweeping
across a wide range (a memset-shaped loop) rather than orbiting a
few LR/SC instructions.

### 2. Slow QEMU TCG store path on a 1 MiB linear-memory zero-fill [~] — lower confidence

`Module::instantiate` in wasmi must allocate the linear memory at
its declared initial size and zero-fill it. Hello declares 17 pages
= 1 088 KiB. wasmi's no_std memory backing is a `Vec<u8>` (or
equivalent) sized via `with_capacity` followed by `resize(N, 0)` —
a memset of N bytes through the bump allocator's freshly-handed
slab.

On QEMU TCG that memset hits the soft-MMU slow path on every page
boundary (TLB miss → page-table walk → fill helper → store). For
~1 MiB of zero-fill across 256 4 KiB pages, that's ~256 TLB-miss
helper calls plus ~270 000 store ops. Even slow TCG should finish
this in single-digit seconds; 90+s is too long for this hypothesis
alone unless something else is also off (e.g. the TLB is being
repeatedly flushed by a sibling code path).

Why this hypothesis fits less well than #1: Tier-2 net is also a
cdylib, so it should have a similar default 1 MiB shadow stack. The
user reports it instantiates fine — though it may be built with a
smaller `--stack-size`. Verifying this is one of the cheapest next
steps; if Tier-2 net has the same 17+ pages, hypothesis #2 weakens
severely.

### 3. wasmi 1.0.9 has a separate latent bug specific to this module shape — low confidence

The wasmi 1.0.x patch series since 1.0.0 (2025-12-03) has been
almost entirely about translator miscompiles for specific
instruction fusions (1.0.5 signed remainder, 1.0.6 missing local
preservation in loops, 1.0.7/1.0.8 wide-arithmetic + `local.set`
fusion, 1.0.9 `copy`-instruction merge — issue #1779). None of
these match an instantiation-time hang. Translator bugs land lazily
under `LazyTranslation` (the default since 0.32), so a translator
hang would only fire when a function is *called*, not at
instantiate. Hello has no `(start)`, so `instantiate_and_start`
should not invoke the translator. This further weakens #3.

### 4. Module-level `(start)` running and looping inside an interpreter step — ruled out

User-confirmed: hello's `_start` is an exported regular function,
not a WASM `(start)` section. `instantiate_and_start` therefore
does not call into the executor for hello.

## Source-trace investigation

I could not read the wasmi 1.0.9 source from
`~/.cargo/registry/src/...` in this environment (sandbox blocked
access to the user-profile cargo cache). The trace below is
reconstructed from public docs and changelog; mark it `[~]`.

`Linker::instantiate_and_start` is a thin wrapper around
`Linker::instantiate(...).and_then(|pre| pre.start(&mut store))`.
For a module with no `(start)`, `pre.start` is a no-op. So the hang
is in `Linker::instantiate`, which is `InstancePre::new(...)`-shaped:

1. **Resolve imports** — for each `(module, name)` pair, look up
   the matching entry in the linker's hashmap and type-check it.
   Hello has 7 imports; bounded.
2. **Allocate tables** — Hello has none.
3. **Allocate memories** — `MemoryEntity::new(initial_pages)`
   allocates the backing store and zero-fills it. Hot candidate
   for hypothesis #2.
4. **Allocate globals** — three i32s, trivial.
5. **Apply element segments** — Hello has none.
6. **Apply data segments** — Hello has one active data segment
   carrying "Hello from Wari\r\n" (17 bytes). `memory.write` for
   17 bytes is trivial.
7. **Invoke `(start)` if present** — not present.

Steps 3 and 6 are where wasmi internally takes locks (`spin::Mutex`
on the engine's resource pool, `spin::Once` for engine-internal
lazy state). The candidate spinlock locations are inside the engine
resource registry that wasmi 1.0 introduced for the new
`Instance::new` low-level path (per the wasmi-labs 1.0 blog post).
That new path uses `spin` primitives in no_std builds and is the
most plausible location for hypothesis #1 to fire.

Marked `[~]` because I have not read the 1.0.9 source line-for-line;
the hypotheses identify the *shape* of the suspect call site rather
than naming a specific function.

## QEMU TCG and RV64 specifics

What I checked:

- QEMU TCG has documented LR/SC bugs and a non-trivial AMO fault
  pathway on RV64 (gitlab #594, launchpad #1908626).
- QEMU TCG soft-MMU forces a helper call on every TLB miss; a
  1 MiB zero-fill across 256 fresh pages is "expensive but
  bounded" — should be seconds, not minutes.
- QEMU virt's OpenSBI handoff and Sv39 are not implicated: prior
  Tier-2 modules complete fine, so paging and SBI work.
- PLIC and cap init were already ruled out by the user.

What I could NOT verify:

- The exact QEMU version in use. Ubuntu's apt package often lags;
  if it is older than 8.x, multiple AMO fixes are missing.
- Whether `-smp 1` is actually being passed. A spurious second
  hart would change the analysis.

## Recommended next steps (cheapest first)

1. **Capture a QEMU `-d in_asm,exec` trace** for ~30 seconds after
   `instantiate_and_start` is logged. If the PC orbits a tight
   4–8-instruction window with `lr.d`/`sc.d`, hypothesis #1 is
   confirmed; if it sweeps a wider memset loop, hypothesis #2.
2. **Diff `wasm-objdump -h` for hello vs. the Tier-2 net driver**.
   Verify net's actual initial linmem. If similar to hello's 17
   pages, hypothesis #2 is severely weakened.
3. **Pin `wasmi = "=0.32.0"`** (or current 0.32.x patch tip) in a
   throwaway branch and rerun. The 0.32 line predates wasmi 1.0's
   register-machine translator/instantiate rework. If the hang
   disappears, ship Phase 2's wasmi upgrade ahead of schedule.
4. **Try `qemu-system-riscv64` from a recent backport** (Ubuntu's
   apt qemu often lags). A 9.x QEMU carries recent AMO/LR-SC
   fixes.
5. **No-code probe**: lower hello's stack reservation via
   `RUSTFLAGS="-C link-arg=-zstack-size=65536"`. That drops initial
   linmem from 17 pages to ~2. If the hang vanishes, hypothesis
   #2 is confirmed; if it persists, hypothesis #1 wins.

## Workarounds

- **Ship the Phase 2 wasmi upgrade now**, not later. The 0.32.x
  series and the post-1.0 main branch (`Instance::new`-based
  instantiate) both rework the no_std synchronisation path.
- **Restrict QEMU runs to Tier-2-only** during Phase 0 boot
  smoke-tests; promote the Tier-1 hello smoke-test to a VF2-only
  CI gate.
- **Switch QEMU CPU flag** from generic `-cpu rv64` to one with
  explicit `a` extension (`-cpu rv64,a=true`) if not already set.
  Some users report atomic emulation improvements after explicit
  extension flags.
- **Do NOT** attempt to manually walk the instantiation steps.
  wasmi 1.0.9 does not expose `instantiate(...)` separately;
  rolling our own would mean re-implementing `MemoryEntity::new`
  plus data-segment application against private types — more
  work and more risk than the upgrade.

## References

- wasmi-labs/wasmi releases page: https://github.com/wasmi-labs/wasmi/releases
- wasmi v1.0.9 release tag: https://github.com/wasmi-labs/wasmi/releases/tag/v1.0.9
- wasmi issue #1779 (the bug v1.0.9 fixed): https://github.com/wasmi-labs/wasmi/issues/1779
- wasmi-labs blog "Wasmi 1.0": https://wasmi-labs.github.io/blog/posts/wasmi-v1.0/
- wasmi-labs blog "Wasmi's New Execution Engine" (0.32 lazy translation): https://wasmi-labs.github.io/blog/posts/wasmi-v0.32/
- wasmi CHANGELOG (master): https://github.com/wasmi-labs/wasmi/blob/master/CHANGELOG.md
- wasmi GHSA-g4v2-cjqp-rfmq (UAF in linear memory): https://github.com/wasmi-labs/wasmi/security/advisories/GHSA-g4v2-cjqp-rfmq
- spin crate docs (no_std primitives wasmi depends on): https://docs.rs/spin
- QEMU virt machine docs: https://www.qemu.org/docs/master/system/riscv/virt.html
- QEMU RISC-V system emulator overview: https://www.qemu.org/docs/master/system/target-riscv.html
- QEMU multi-threaded TCG synchronisation primitives: https://www.qemu.org/docs/master/devel/multi-thread-tcg.html
- QEMU GitLab issue #594 (RV64 AMO fault reporting): https://gitlab.com/qemu-project/qemu/-/issues/594
- Launchpad QEMU bug #1908626 (atomic test-and-set issues): https://bugs.launchpad.net/qemu/+bug/1908626
- WebAssembly/threads issue #62 (data segments re-copied per instantiate): https://github.com/WebAssembly/threads/issues/62
- wasmi issue #732 (lazy compilation context): https://github.com/wasmi-labs/wasmi/issues/732
