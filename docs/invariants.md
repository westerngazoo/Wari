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

## Phase-1b invariants (capability system)

These invariants govern the dynamic capability system introduced in
Phase 1b (cap-primitive PR 1, kernel-objects PR 2, syscall-surface
PR 3). Architectural contract in `docs/cap-system-design.md`.

### INV-10 · Capability Monotonicity *(Phase 1b)*

> For any successful `Cap::derive` invocation that produces a child
> cap from a parent, `child.rights & !parent.rights == 0`. The
> kernel never produces a child cap with rights its parent does not
> hold.

**Consequence**: rights cannot be silently amplified through a chain
of mints. The audit story for Phase 4: a static analysis of every
mint site verifies the `requested & !parent.rights == 0` check.

**Enforcement**: `kernel/src/cap/types.rs::Cap::derive` rejects
violations with `KernelError::PermissionDenied`. Property-checked by
`cap::proofs::derive_preserves_rights_monotonicity` and
`derive_rejects_rights_amplification` (PR 1).

**When this breaks**: never legitimately. Any code path that
constructs a `Cap` value without going through the rights check is a
soundness bug.

### INV-11 · Tier-2 Grants Are Signed *(Phase 1b)*

> A Tier-2 module's CSpace is populated only from caps declared in
> its signed manifest (the existing `runtime::sign::verify` gate at
> the binary level extends to the cap manifest in Phase 1b PR 2).
> Tier-1 modules are similarly populated from compiled-in manifests
> in Phase 1b; Phase 2+ moves Tier-1 manifests to signed
> distribution.

**Consequence**: every cap reachable by a Tier-2 instance traces
back, via parent-chain or IPC delegation, to a kernel-issued root
cap that was authorized by signature.

**Enforcement**: PR 2's boot-time root-cap construction is the only
producer of root caps and consults the signed manifest as input.

### INV-15 · Capability Forgery Prevention *(Phase 1b)*

> No userspace code path produces a `Cap` value that the kernel did
> not construct. The `Cap` type's only public constructors are
> `Cap::empty()` (for unused slots) and `Cap::derive()` (which
> requires a parent and goes through the rights check). Userspace
> WASM cannot construct a `Cap` value at all — it manipulates
> capabilities only via syscall slot indices.

**Consequence**: a Tier-1 or Tier-2 WASM module passing untrusted
bytes to a syscall cannot smuggle a synthetic cap; the kernel only
ever reads cap data from its own static memory, indexed by syscall
arguments that are themselves bounds-checked.

**Enforcement**: Rust privacy + an internal-use convention that
never adds a public all-fields constructor. Property-checked by
`cap::proofs::derive_rejects_reserved_rights_bits` (PR 1) and the
Phase 1b PR 3 syscall trampoline tests.

**When this breaks**: a `mem::transmute<[u8; 16], Cap>` or
equivalent slipping past review. Caught by the `unsafe` audit — every
`transmute` requires INV-N citation.

### INV-16 · Derivation Chain Integrity *(Phase 1b)*

> For every successful derivation, the child cap has the same
> `kind` and `pool_index` as its parent. The mint operation never
> retargets the underlying kernel object.
>
> Equivalently, after `child = Cap::derive(parent, parent_id, r,
> b).unwrap()`, both `child.kind == parent.kind` and
> `child.pool_index == parent.pool_index` hold.

**Consequence**: revocation is sound. A depth-first walk from any
cap following `parent`-equality finds every descendant; no
descendant can escape revocation by claiming a different kernel
object than its ancestor.

**Enforcement**: `Cap::derive` (PR 1) copies `kind` and `pool_index`
from the parent without modification. Property-checked by
`cap::proofs::derive_preserves_kind_and_pool_index` and
`derive_records_parent_id` (PR 1).

**When this breaks**: SMP, where a mint and a revoke could race on
the same parent slot. INV-1 covers Phase 1b's single-hart
guarantee; the Phase 2+ SMP migration revisits this invariant.

### INV-17 · Generation-Counter Anti-ABA *(Phase 1b)*

> Every CSpace slot has a 16-bit generation counter that
> monotonically increases on each transition occupied → empty →
> occupied. A child cap whose `parent` field references generation
> `N` of a slot becomes orphaned when the slot is reused at
> generation `N+1`; the next revocation walk detects the mismatch
> and clears the orphan.

**Consequence**: a cap's parent reference is valid only as long as
the slot it points to retains the same generation. ABA attacks
(slot freed, refilled with an unrelated cap, child claims to be
derived from the new occupant) are structurally impossible.

**Enforcement**: `kernel/src/cap/cspace.rs::CSpace::bump_generation`
uses `saturating_add` so the counter never wraps; PR 3's mint path
refuses operations on a slot whose counter saturated at
`u16::MAX`. Property-checked partially in PR 1
(`cspace::tests::bump_generation_*`); fully proven in PR 3 once the
mint syscall lands.

**When this breaks**: a single slot is re-occupied 65,535 times in
one boot. PR 3's mint path returns `KernelError::OutOfHandles` when
this happens.

### INV-18 · CSpace Slot Index Bounds *(Phase 1b)*

> Every kernel access to a CSpace slot bounds-checks `slot <
> CSPACE_SLOTS` before indexing. The two access paths
> (`CSpace::lookup` and `CSpace::lookup_mut`) return `None` for
> out-of-bounds; syscall trampolines map `None` to
> `KernelError::InvalidArgument`.

**Consequence**: `CSpace::slots[i]` is always sound (`i < 256`).
No raw `slots[]` indexing exists outside `cap::cspace`.

**Enforcement**: `cspace.rs` is the only module that directly
indexes `CSpace.slots`; downstream consumers receive
`Option<&Cap>` or `Option<&mut Cap>`. Currently checked at
PR-1-test level (`cspace::tests::lookup_*`); will be Kani-proven
once a slot-indexing fast path lands in PR 3.

**When this breaks**: `CSPACE_SLOTS` ever increases past 256. Then
the slot-index type widens past `u8`; ABI-breaking change requiring
a versioned syscall set.

### INV-19 · Tier-Shape Compatibility *(Phase 1b reserved)*

> A Tier-1 process cannot hold a cap to a kernel-object kind that
> is Tier-2-only. Phase 1b ships no Tier-2-only kinds (Endpoint,
> Notification, Untyped, Frame are all kind-agnostic from a cap
> perspective; Tier-2-ness applies to module loading, not to cap
> objects). Reserved against Phase 2+ when Tier-2-only kinds appear
> (`IrqHandler`, `MmioWindow`, etc.).

**Consequence**: a Tier-1 module cannot, today or in the future,
acquire a cap whose mint path is gated on "caller is Tier-2".

**Enforcement**: per-kind mint paths inspect the caller's tier and
refuse if the kind is incompatible. Phase 1b's mint path is
uniform across the four kinds; the check is structurally a no-op
today, shaped to grow.

**When this breaks**: an attacker minting a Tier-2-only kind from a
Tier-1 process. Caught structurally.

### INV-23 · IRQ Routing Determinism *(Phase 1b PR Net-1)*

> The PLIC dispatcher reads the static array
> `IRQ_NOTIFICATION_BINDINGS: [Option<u16>; MAX_BOUND_IRQS]` to map
> a hardware IRQ source to the kernel `Notification` pool index it
> should signal. Phase 1b binds at boot only (via
> `mmio::plic::bind_irq_to_notification`); after the kernel finishes
> initialization, no path mutates the bindings. The trap handler's
> claim → signal-notification → complete cycle is therefore
> deterministic and read-only after init.

**Consequence**: a reader of the trap path can verify "every IRQ
that fires routes to one specific notification, the same way every
time" by inspecting the static binding table. No race, no dynamic
re-binding mid-flight.

**Enforcement**: `IRQ_NOTIFICATION_BINDINGS` is `static mut` but
the only writer is `bind_irq_to_notification`, called exclusively
from boot-time setup paths (`cap::boot::init_root_caps` extends to
this in PR Net-3). The trap handler's `dispatch()` only reads.

**When this breaks**: Phase 1c when a `sys_irq_bind` syscall lands
to allow drivers to register IRQs at runtime. INV-23 is then
replaced by INV-1 (single-hart) covering the binding write path.

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
| `kernel/src/boot.S`                     | Boot asm: hart-id select via `auipc`+`ld` of `_boot_hart_id_addr` (PC-relative `.dword` of linker-defined `_boot_hart_id`), `.bss` zero, stack setup, call into `kmain`, `wfi` park | INV-7 | Privileged asm in S-mode; `R_RISCV_64` reach lets the hart-id constant address absolute-0/1 symbols where `la` would overflow |
| `kernel/src/mmio/volatile.rs`           | `VolatilePtr::new` construction; `read` / `write` volatile ops    | INV-3 | Typed MMIO access — the one module where raw volatile lives (R3) |
| `kernel/src/mmio/uart_ns16550.rs`       | `VolatilePtr::new` calls for THR / IER / FCR / LCR / MCR / LSR via `reg(index)` (`UART_BASE + index * UART_REG_STRIDE`); `init()` writes IER/LCR/FCR/MCR to bring the device up | INV-3 | NS16550 (QEMU, stride 1) / DW8250 (VF2, stride 4) UART registers; init sequence required by JH7110, no-op-safe on QEMU |
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
| `kernel/src/cap/types.rs` (`Cap::derive`) | Pure-function rights check + kind/pool preservation                | INV-10, INV-15, INV-16 | Mint primitive; no `unsafe`; Kani-proven in `cap::proofs` |
| `kernel/src/cap/cspace.rs` (`lookup`, `lookup_mut`)  | Bounds-checked slot access                              | INV-18       | Single point of indexed access into `CSpace.slots[]` |
| `kernel/src/cap/cspace.rs` (`bump_generation`) | `saturating_add` on per-slot generation counter             | INV-17       | Anti-ABA; saturates at `u16::MAX` so PR 3's mint can refuse |
| `kernel/src/mmio/plic.rs` (`init`)             | `csrs sie` to enable S-mode external interrupts             | INV-7        | Privileged S-mode CSR write; matches `trap::install` pattern |
| `kernel/src/mmio/plic.rs` (priority/enable/threshold/claim/complete) | `VolatilePtr<u32>::new` over PLIC_BASE-derived addresses | INV-3 | PLIC at fixed RV64 base 0x0c000000 |
| `kernel/src/mmio/plic.rs` (`bind_irq_to_notification`, `notification_for_irq`, `dispatch`) | `addr_of[_mut]!(IRQ_NOTIFICATION_BINDINGS)` static-mut access | INV-1, INV-8, INV-23 | Single-hart, post-init, read-only-after-bind boot table |
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

*Last updated: Phase 1c, May 2026. Last formal gate audit: Phase 0 —
`docs/audits/phase-0.md`. The Phase-1b capability invariants (INV-10,
11, 15–19, 23) are drafted and enforced in `kernel/src/cap/`; a Phase-1
gate audit is still pending.*
