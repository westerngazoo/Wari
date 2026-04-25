# Wari — Invariants (INV-N catalog)

> This document is the **source of truth** for what makes Wari's unsafe
> code sound. Every `unsafe` block in the kernel carries a
> `// SAFETY: INV-N` comment citing an invariant below. When an
> invariant is violated (e.g., SMP lands and INV-1 changes), every
> citing site needs revisiting.

Format inherited from `../goose-os/docs/unsafe-audit.md`.

---

## Loaded-bearing invariants (Phase 0 baseline)

### INV-1 · Single-Hart Kernel

> Only one hart executes kernel code at a time. Interrupts are disabled
> on entry to the trap vector and not re-enabled until sret back to
> userspace.

**Consequence**: `static mut` access without synchronization is sound
for scheduler-owned state (`PROCS`, `CURRENT_PID`, `TICKS`, etc.).

**When this breaks**: SMP. Every INV-1 citation needs per-hart or
locked access.

### INV-2 · Trap Frame Exclusivity

> While a syscall handler runs, the current hart owns the `TrapFrame`
> it was handed. No other code path touches it until sret.

**Consequence**: `&mut TrapFrame` parameters in syscall handlers do
not alias.

**When this breaks**: nested interrupts (reentrant traps). Prevented
by SIE=0 during S-mode trap service.

### INV-3 · MMIO Address Validity

> Hardcoded MMIO bases are fixed by hardware spec. Writes/reads to
> these addresses are hardware register operations, not arbitrary
> memory access.

**Consequence**: `VolatilePtr`/`VolatileRef` wrapping of fixed MMIO
addresses is sound.

**When this breaks**: porting to a different SoC layout. MMIO bases
move behind `platform::` module.

**Update (PR 8)**: MMIO base addresses are no longer hardcoded at use
sites; they are exported from `kernel/src/platform/{qemu_virt,vf2}.rs`
and selected at compile time via the `qemu` / `vf2` feature gate
(exactly one must be active; `platform::mod` `compile_error!`s
otherwise). INV-3's claim — "hardcoded MMIO bases are fixed by
hardware spec" — applies to the `platform::*` constants, not their
call sites. Sites that previously hardcoded `0x1000_0000`
(`mmio/uart_ns16550.rs`, `mem/kvm.rs`, `validate.rs`) now defer to
`platform::UART_BASE`. The `mmio/uart_ns16550.rs` file additionally
sources `platform::UART_REG_STRIDE` so the same kernel printk path
works on QEMU's NS16550A (1-byte stride) and VF2's DesignWare 8250
(4-byte stride). PR 8 introduces no new `unsafe` blocks; the
platform module is pure constants.

### INV-4 · Linker Symbol Addresses Are Valid

> Linker script exports symbols (`_end`, `_heap_end`, etc.) whose
> addresses are bound at link time. Taking `&X as *const u8 as usize`
> yields that address.

**Consequence**: reading linker symbol addresses is sound; no deref.

**When this breaks**: linker script renames or symbol-stripping builds.
CI asserts the symbols exist in the final binary.

### INV-5 · Page Allocator Returns Kernel-Writable PAs

> `BitmapAllocator::alloc()` returns a PA in the range `[_end,
> _heap_end)`. The kernel identity-maps this entire range RW.

**Consequence**: writes through allocator-returned PAs don't clobber
kernel text.

### INV-6 · Page-Table Walker Returns Installed PAs

> `page_table::walk(root, va, cb)` invokes the callback only when VA
> resolves to a present leaf PTE whose PA was installed via validated
> mapping.

**Consequence**: callbacks receive PAs owned by the caller's process.

### INV-7 · Privileged ASM Is Privileged

> Inline assembly touching CSRs, `sret`, `ecall`, `wfi`, `sfence.vma`
> is sound because the kernel executes in S-mode.

**Consequence**: unsafe-block reason is "Rust requires `unsafe` around
asm"; the instruction itself is permitted at this privilege level.

### INV-8 · Static-Mut Singleton Accessors Are Called Post-Init

> `page_alloc::get()`, `runtime::get()`, driver accessors return
> `&'static mut` to statics initialized once in boot. Callers obtain
> these references only after the corresponding `init()` has run.

**Consequence**: returned references are to initialized state.

### INV-9 · Bytewise Struct Reinterpretation Is Bounds-Checked

> Reinterpreting `&[u8]` as a `&StructT` is preceded by a length check
> (`slice.len() >= size_of::<StructT>()`) AND alignment verification
> (or `read_unaligned`).

**Consequence**: struct reads don't extend past the slice, don't cause
unaligned access faults.

**Open**: goose-os followed this for length but not alignment — see
`../goose-os/docs/unsafe-audit.md` follow-up #1. Wari cherry-picks
with the alignment fix.

### INV-12 · Bump-Allocator Arena Is Boot-Only

> The runtime bump allocator's arena `[HEAP_CURSOR, HEAP_END)` is
> initialized exactly once during `kvm::init()` and never re-initialized.
> After init, the arena is mutated only by `alloc()` calls; `dealloc()`
> is a no-op (Phase 0: arena-per-boot, no free).

**Consequence**: bump allocator's `unsafe` blocks rely on INV-1 (single-
hart) for cursor exclusivity AND on the post-init guarantee that
`HEAP_CURSOR <= HEAP_END` is the only relevant invariant.

**When this breaks**: Phase 1's real allocator lands. INV-12 retires; a
new INV covers the replacement allocator's invariants (free-list
integrity, etc.).

---

## Phase-1 invariants (added when capability system lands)

### INV-10 · Capability Monotonicity *(Phase 1)*

> A process's capability table is append-only within a single IPC
> call. Capabilities are revoked only by explicit `SYS_CAP_REVOKE`,
> never implicitly.

### INV-11 · Tier-2 Grants Are Signed *(Phase 1)*

> A Tier-2 module is loaded only with a matching signature on its
> manifest. The signature is verified against a compiled-in public key
> before any bytecode executes.

### INV-13 · Tier-2 Bytecode Is Signature-Verified Before Instantiation *(Phase 0; generalizes into INV-11 in Phase 1)*

> Any `.wasm` bytecode loaded at Tier 2 passes signature verification
> against the kernel's compiled-in ed25519 `ACCEPTED_PUBKEY` before a
> wasmi `Module::new()` is constructed from it. Verification failure
> aborts the load and the kernel halts in Phase 0 (no Tier-2 driver =
> no I/O).

**Consequence**: every `Tier::Two` instance reachable by the runtime
has passed signature check in this boot.

**When this breaks**: Phase 1 adds pubkey registries and signed
manifests (INV-11's full form). Phase 0's single-pubkey fast path is
replaced.

### INV-14 · Tier-2 Driver Instance Is a Boot-Initialized Singleton

> The Tier-2 UART driver's `wasmi::Instance` (with its `Store` and the
> typed `write` function handle) is held in a static
> `Option<Tier2UartHandle>` installed exactly once at boot via
> `tier2_uart::install`. Subsequent `tier2_uart::write` calls obtain
> a `&mut TIER2_UART` and rely on INV-1 (single-hart) for exclusivity.

**Consequence**: the WASI `fd_write` host fn (called from Tier-1) safely
reaches into the singleton without locks. Cross-tier marshaling
(`Memory::write` into the driver's linear memory + typed-call into
its `write` export) becomes a synchronous, single-threaded sequence.

**When this breaks**: SMP. INV-1's failure mode propagates here;
every `tier2_uart::write` call site needs a per-hart or locked
discipline. INV-14 also breaks if a second `install` ever lands —
the second handle would silently shadow the first, leaking the
previous Store. Enforced structurally by the `unsafe fn install`
contract: caller must guarantee one-time invocation pre-runtime use.

---

## Per-file sites

*(Populated as the kernel is cherry-picked.)*

| File                                    | Site                               | Invariant | Rationale |
|-----------------------------------------|------------------------------------|-----------|-----------|
| `kernel/src/main.rs` (`kmain` wfi loop) | `wfi` after banner, pre-runtime    | INV-7     | S-mode WFI |
| `kernel/src/main.rs` (`panic` handler)  | `wfi` in panic halt loop           | INV-7     | S-mode WFI |
| `kernel/src/boot.S`                     | Boot asm: `.bss` zero, stack setup, call into `kmain`, `wfi` park | INV-7 | Privileged asm in S-mode |
| `kernel/src/mmio/volatile.rs`           | `VolatilePtr::new` construction; `read` / `write` volatile ops    | INV-3 | Typed MMIO access — the one module where raw volatile lives (R3) |
| `kernel/src/mmio/uart_ns16550.rs`       | `VolatilePtr::new` calls for THR / LSR at `platform::UART_BASE + index * platform::UART_REG_STRIDE` | INV-3 | NS16550A on QEMU virt (stride 1); DesignWare 8250 on VF2 / JH7110 (stride 4). PR 8 routed both through `platform::*`. |
| `wari-mem/src/page_alloc.rs` (`get`)    | `&mut *addr_of_mut!(ALLOC)` returns global allocator               | INV-1, INV-8 | Single-hart kernel + post-init accessor |
| `wari-mem/src/page_alloc.rs` (`install`)| `addr_of_mut!(ALLOC).write(..)` boot-time install                  | INV-1, INV-8 | Called once during boot, interrupts off |
| `wari-mem/src/page_alloc.rs` (`zero_page`) | `write_volatile` over a 4 KiB page                              | INV-5 | Allocator-returned PA is identity-mapped RW |
| `wari-mem/src/page_table.rs`            | *No `unsafe` blocks.* INV-9 has no site: `walk()` takes a `read: FnMut(usize) -> u64` closure rather than reinterpreting `&[u8]` as `&Pte`, so the slice-to-struct alignment caveat from goose-os `unsafe-audit.md` follow-up #1 (which targets `elf.rs`, not cherry-picked into Wari) is structurally avoided. | — | — |
| `kernel/src/mem/kvm.rs` (`init`, ~120)  | `csrw satp` write                                                  | INV-7        | Privileged S-mode CSR write that turns paging on |
| `kernel/src/mem/kvm.rs` (`init`, ~120)  | `sfence.vma zero, zero`                                            | INV-7        | TLB flush ordering after satp write (R6) |
| `kernel/src/mem/kvm.rs` (`init`, ~70)   | `page_alloc::install` from linker syms                             | INV-4, INV-5, INV-8 | Heap range `[_end,_heap_end)` is kernel-writable; one-time post-init install |
| `kernel/src/mem/kvm.rs` (`read_pte`/`write_pte`, ~190/~200) | `read_volatile`/`write_volatile` on PTE slots          | INV-5        | PTE slot lives in an allocator-owned identity-mapped page |
| `kernel/src/trap.rs` (`handle_trap`, ~115) | `&mut TrapFrame` parameter                                      | INV-2        | Trap-frame exclusivity during S-mode service |
| `kernel/src/trap.rs` (`install`, ~95)   | `csrw stvec` write                                                 | INV-7        | Privileged S-mode CSR write |
| `kernel/src/trap.rs` (`ack_timer`, ~150)| `csrc sip` clear                                                   | INV-7        | Privileged S-mode CSR clear |
| `kernel/src/trap.S` (`_trap_entry`)     | Privileged register save/restore + `sret`                          | INV-7        | S-mode trap-vector asm |
| `kernel/src/mem/kvm.rs` (`init`, runtime-heap symbols) | `sym_addr(&_runtime_heap_start/_end)`             | INV-4        | Linker-symbol addresses for the bump arena |
| `kernel/src/mem/kvm.rs` (`init`, end)   | `runtime::heap::init(runtime_heap_start, runtime_heap_end)`        | INV-1, INV-12 | One-time boot install of the bump arena |
| `kernel/src/runtime/heap.rs` (`init`)   | `static mut HEAP_CURSOR/HEAP_END` write                            | INV-1, INV-12 | Single-hart boot-only init of the arena |
| `kernel/src/runtime/heap.rs` (`alloc`)  | `static mut HEAP_CURSOR` read/write                                | INV-1, INV-12 | Single-hart cursor advance, post-init |
| `kernel/src/runtime/sign.rs` (`verify`) | ed25519 verify of envelope vs. `ACCEPTED_PUBKEY`                   | INV-13       | First gate before any Tier-2 wasmi parse |
| `kernel/src/runtime/host_fns.rs` (`host_mmio_write8`) | `core::ptr::write_volatile` byte write to MMIO       | INV-3        | Validator-narrowed: only `is_uart_mmio_addr` addresses reach the volatile write; capability gate (`mmio_uart`) precedes |
| `kernel/src/cap/static_caps.rs`         | `Caps` construction + `caps_for` lookup                            | INV-1        | Plain-value caps; immutable post-load on a single-hart kernel |
| `kernel/src/runtime/tier2_uart.rs` (`install`) | `addr_of_mut!(TIER2_UART)` write of the `Option<Tier2UartHandle>` singleton | INV-1, INV-8, INV-14 | One-time boot install of the Tier-2 UART driver handle, called from `runtime::run_tier2_uart` before any Tier-1 host fn dispatch |
| `kernel/src/runtime/tier2_uart.rs` (`write`)   | `addr_of_mut!(TIER2_UART)` mutable read; `Memory::write` into driver lin-mem; `TypedFunc::call` into driver `write` export | INV-1, INV-8, INV-14 | Single-hart post-init access to the singleton; cross-tier marshaling is bounds-checked by wasmi |
| `kernel/src/runtime/wasi.rs` (`host_fd_write`) | `unsafe { tier2_uart::write(&bytes[..n]) }` call from Tier-1 host fn dispatch | INV-1, INV-8, INV-14 | The Tier-1 → Tier-2 → MMIO marshaling chain; capability gate (`caps.stdout`) and fd validation precede |
| `kernel/src/runtime/loader.rs` (`load_tier1`)  | *No `unsafe` blocks.* The Tier-1 path is pure wasmi orchestration; the unsafe surface lives in `tier2_uart` (singleton) and `wasi` (delegating to that singleton). | — | — |

---

## Non-contributing crates (audit-exempt)

Crates in this list contain **no `unsafe`** and **no MMIO**. They are
pure data and logic, host-testable, and therefore introduce no
invariants. Phase-gate audits skip them for unsafe-block coverage but
still review them for R4 (API contracts) and test coverage.

| Crate       | Rationale |
|-------------|-----------|
| `wari-abi`  | Pure ABI constants + `SyscallError` enum + `into_retval`. No `unsafe`, no allocation, no MMIO. Host-testable with `cargo test -p wari-abi`. |

If any of these crates ever grow an `unsafe` block, they move out of
this list and every block gets an INV-N citation the same PR it lands.

---

## Enforcement

- `cargo clippy -- -D warnings` with `undocumented_unsafe_blocks = "warn"`
- Every PR that adds `unsafe` must update this file (CLAUDE §PR Workflow)
- Phase gate audits cross-check: for every `unsafe` in the codebase,
  is there a matching row in this file?

---

*Last audited: Phase 0 scaffold, April 2026.*
