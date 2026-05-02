# Driver Interface Design — Phase 2 PR DI

> **Status**: design draft v1, May 2026.
> **Scope**: replace today's convention-based Tier-2 driver ABI
> ("driver exports a function called `write` with signature
> `(u32,u32) -> i32`, kernel hopes it does") with an explicit,
> machine-checked **Driver Manifest** embedded in every signed
> Tier-2 binary, verified by the kernel before the driver runs.
> **Author**: Wari co-architect.

---

## 1 · Why this exists

### 1.1 The gap today

Every Tier-2 driver currently exposes its surface by **convention**:

```rust
// drivers/uart/src/lib.rs:127
#[no_mangle]
pub extern "C" fn write(buf_ptr: u32, len: u32) -> i32 { ... }
```

```rust
// kernel/src/runtime/mod.rs:154
let write_fn = instance
    .get_typed_func::<(u32, u32), i32>(&store, "write")
    .map_err(|_| KernelError::DriverError)?;
```

The kernel asks wasmi for a function literally named `"write"` with
the signature `(u32, u32) -> i32`. If the driver author:

- typoes the export name → `KernelError::DriverError` at boot time
- changes the signature → same
- forgets `#[no_mangle]` → same
- omits an export the kernel needs → same
- **lies about being a UART driver while shipping net code** → the
  signature might still match by accident; nothing detects the
  identity mismatch until the device misbehaves at runtime

Worse: the Tier-2 net driver and Tier-2 UART driver share the same
signed-envelope format and the same `Tier::Two` enum value. The
kernel has no way to refuse to load `net.signed.wasm` into the UART
slot. Today this is prevented only because `kmain` happens to call
`load_tier2(uart_blob, ModuleId::Tier2Uart)` and `load_tier2_net(
net_blob, ModuleId::Tier2Net)` at the right hard-coded offsets.

For an OS that aims to be **provable**, "we hard-code the right
loader call" is not a safety property — it is a comment.

### 1.2 What a fix must achieve

1. **Identity** — the binary declares "I am a UART driver, ABI v1",
   and the kernel verifies this matches the slot it is being
   loaded into. A net driver loaded into the UART slot fails at
   parse time, before any code runs.
2. **Surface** — the binary declares its full export list with
   typed signatures. The kernel verifies every declared export
   resolves to the declared signature before `_start` runs. A typo
   or signature drift fails at parse time, not at first use.
3. **Tamper-evident** — a driver author cannot ship a binary whose
   manifest disagrees with its actual exports. The signing tool
   extracts and validates the manifest at sign time and refuses to
   produce a signed envelope on mismatch.
4. **Static + machine-readable** — the manifest is fixed-size,
   `repr(C)`, no allocation, no parsing of arbitrary WASM type
   sections at runtime. The kernel's manifest parser is small
   enough to fit in the audit window.
5. **Rust-native ergonomics** — driver authors write a normal
   `impl UartDriver for MyDriver { ... }` block. A macro generates
   the `#[no_mangle] extern "C"` shims AND the manifest static.
   The manifest is never written by hand.

### 1.3 Non-goals (Phase 2)

- **WASM Component Model / WIT** — the right long-term answer, but
  needs wasmi 2.x and a build-pipeline rewrite. We are still on
  wasmi 0.32 (Phase 2 wasmi pin). The design here is forward-
  compatible: a Phase 4+ migration to Component Model can keep
  the same driver-side trait surface and replace only the manifest
  encoding.
- **Driver hot-swap** — drivers are still load-once-at-boot.
- **Capability declarations** — the manifest could one day declare
  "this driver requires `Net+READ+WRITE` cap" and the kernel could
  derive the cap grant from it, but Phase 2 keeps `caps_for` static
  in the kernel as today. Manifest carries the import list as
  informational metadata (so the kernel can fail fast if the driver
  imports a host fn the kernel didn't register).

---

## 2 · Conceptualization

### 2.1 The manifest as a contract

A **Driver Manifest** is a single static byte string embedded in
every Tier-2 driver's `.rodata`, in a known location, with a
known shape. It answers four questions a kernel wants to ask
before running an unknown binary:

1. **Is this even a Wari driver?** (magic number)
2. **Does it speak my ABI version?** (`abi_version` field)
3. **Is it the kind of driver I am loading?** (`kind` field)
4. **Does its export list match what I'm going to call?** (export
   descriptors)

Plus one informational question:

5. **What host fns does it expect me to provide?** (import
   descriptors — used to fail fast on missing host-fn registration,
   future use for cap derivation)

The manifest is **not** a behavioral contract. It does not say
"this `write` function will deliver bytes to a UART without
side effects." That kind of contract is what Tier-2 drivers
*are* — signed code from a trusted vendor whose behavioral
correctness is established by review + signing, not by the
manifest. The manifest is a *structural* contract: the binary
exposes the surface it claims to expose, and that surface is
the surface the kernel was compiled against.

### 2.2 Position in the trust chain

The trust chain for a loading Tier-2 driver becomes:

```
   signed envelope
   ──────────────
1. kernel: check ed25519 sig over envelope.payload  (existing INV-13)
2. kernel: parse WASM, find .wari_driver_manifest section
3. kernel: verify manifest magic + abi_version
4. kernel: verify manifest.kind == expected kind for this slot
5. kernel: instantiate WASM (wasmi)
6. kernel: for each manifest.exports[i]:
            resolve get_typed_func(name, sig); fail load on mismatch
7. kernel: for each manifest.imports[i]:
            assert linker has a host fn registered with that name+sig
8. kernel: call _start (if manifest declares it)
9. kernel: stash typed-func handles for later use
```

Steps 2-4 and 6-7 are the new gates. Step 1 stays unchanged.
Step 5 is unchanged (same wasmi instantiate path). The signing
tool extracts the manifest at sign time and refuses to sign if
manifest claims don't match the actual `.wasm` exports — so a
malicious or buggy driver author can't ship a binary whose
manifest is a lie that the kernel later trusts.

### 2.3 Why static `repr(C)` and not protobuf / CBOR

- **Auditability**: the kernel's parser is ~30 lines of
  bounds-checked indexing. No third-party encoder.
- **No allocation**: kernel parses without touching the heap.
- **Determinism**: every supported manifest has exactly one
  byte-level encoding. A signed envelope's bytes either match
  the embedded manifest exactly, or the binary is rejected.
- **Forward-compat through versioning**: bumping `abi_version`
  is the supported migration path. Old kernels reject newer
  manifests cleanly.

The cost: every new function signature shape adds a `FuncSig`
discriminant. This is fine — Wari has ~10 signature shapes
across all drivers; growth is bounded by the host-fn surface,
not by driver count.

---

## 3 · Wire format

### 3.1 Section placement

The manifest lives in a custom WASM section named exactly
`wari_driver_manifest` (no leading dot — WASM custom-section
naming convention). The section payload is the manifest bytes.

Why a custom WASM section, not a `.rodata` static the kernel has
to find by symbol scan: WASM custom sections are first-class in
the WASM binary format, parseable in O(n) without a symbol
table, and survive `wasm-opt` / `lld` stripping if marked. The
driver-iface macro emits the static AND the section.

### 3.2 Manifest header (`repr(C, packed)`)

```rust
#[repr(C, packed)]
pub struct ManifestHeader {
    /// b"WDM\0" — Wari Driver Manifest. Rejects non-Wari binaries
    /// that happen to have a section named the same way.
    pub magic: [u8; 4],

    /// Bumps on breaking format changes. Phase 2 = 1.
    pub abi_version: u16,

    /// DriverKind discriminant. Kernel asserts this matches the
    /// slot it is loading into. Wrong-kind = refuse to load.
    pub kind: u16,

    /// Number of ExportDecl entries that follow this header.
    pub export_count: u16,

    /// Number of ImportDecl entries that follow the exports.
    pub import_count: u16,

    /// Reserved for forward-compatible flags (e.g. bit 0 = "driver
    /// declares a (start) section"). Zero in Phase 2.
    pub flags: u32,
}
```

Total: 16 bytes.

### 3.3 Export and Import descriptors

```rust
#[repr(C, packed)]
pub struct ExportDecl {
    /// Export name, NUL-padded. 32 bytes accommodates every
    /// existing driver export (`write`, `poll`, `tx_send`,
    /// `rx_pop`, `rx_recycle`, `_start`).
    pub name: [u8; 32],

    /// Function signature (see FuncSig below).
    pub sig: u8,

    /// Padding to 4-byte alignment for ImportDecl that follows.
    pub _pad: [u8; 3],
}
```

Total: 36 bytes per export.

```rust
#[repr(C, packed)]
pub struct ImportDecl {
    /// Module name (always "wari" in Phase 2; reserved for
    /// future namespacing).
    pub module: [u8; 16],

    /// Import name, NUL-padded.
    pub name: [u8; 32],

    /// Required signature.
    pub sig: u8,

    pub _pad: [u8; 3],
}
```

Total: 52 bytes per import.

### 3.4 FuncSig encoding

```rust
#[repr(u8)]
pub enum FuncSig {
    /// () -> ()  — _start, init shims
    Unit = 1,

    /// (u32, u32) -> i32  — UART write, MMIO writes, tx_send
    U32U32_I32 = 2,

    /// u32 -> i32  — notification_ack, rx_recycle, irq_register
    U32_I32 = 3,

    /// u32 -> u32  — mmio_read8, mmio_read32
    U32_U32 = 4,

    /// u64 -> i32  — net poll
    U64_I32 = 5,

    /// () -> u64  — rx_pop (packed return), lin_mem_base
    Unit_U64 = 6,

    /// (u32, u32) -> i32 with two more u32 — nic_attach_queue
    /// (placeholder; full N-arg signatures get explicit variants
    /// as needed)
    U32x5_I32 = 7,

    /// Allow forward extension; unknown values mean "newer driver
    /// than this kernel supports" → kernel rejects.
}
```

The signature space is closed and small (~10 entries today). New
host fns or driver exports add one variant. The full signature
table lives in `abi-shared/src/driver_iface.rs` so kernel and
drivers always agree.

### 3.5 Worked example: UART manifest size

The Tier-2 UART driver:

- 2 exports: `write` (U32U32_I32), `_start` (Unit)
- 2 imports: `wari_mmio_write8` (U32U32_I32), `wari_mmio_read8` (U32_U32)

Manifest size: 16 (header) + 2×36 (exports) + 2×52 (imports) = **192 bytes**.

The Tier-2 net driver, with 5 exports + 8 imports, lands at
**612 bytes**. Both fit comfortably below the 1 KiB I/O threshold
where size starts to matter.

---

## 4 · Driver-side ergonomics

### 4.1 The trait

```rust
// driver-iface/src/uart.rs
pub trait UartDriver {
    /// Write `buf` to the UART. Return bytes written, or a
    /// negative errno on failure. Driver-side panics convert
    /// to E_DRIVER_FAULT before unwinding.
    fn write(buf: &[u8]) -> i32;
}
```

```rust
// driver-iface/src/net.rs
pub trait NetDriver {
    fn start();
    fn poll(timestamp_ms: u64) -> i32;
    fn tx_send(buf: &[u8]) -> i32;
    fn rx_pop() -> u64;
    fn rx_recycle(desc_idx: u32) -> i32;
}
```

These are **declarative**: the trait body is what the driver
*must* expose. Adding a method = bumping `abi_version`.

### 4.2 The macro

```rust
// In drivers/uart/src/lib.rs
use wari_driver_iface::{wari_driver, UartDriver};

pub struct Driver;

#[wari_driver(kind = Uart)]
impl UartDriver for Driver {
    fn write(buf: &[u8]) -> i32 {
        // ... actual hardware push ...
    }
}
```

The `#[wari_driver(kind = Uart)]` attribute macro expands to:

```rust
impl UartDriver for Driver {
    fn write(buf: &[u8]) -> i32 { ... }   // unchanged
}

// Generated:
#[no_mangle]
pub extern "C" fn write(buf_ptr: u32, len: u32) -> i32 {
    // SAFETY: linmem-backed slice, validated by kernel.
    let slice = unsafe {
        core::slice::from_raw_parts(buf_ptr as *const u8, len as usize)
    };
    <Driver as UartDriver>::write(slice)
}

#[no_mangle]
pub extern "C" fn _start() {}

#[link_section = "wari_driver_manifest"]
#[used]
pub static WARI_DRIVER_MANIFEST: [u8; 192] = {
    // ... assembled from compile-time evaluation ...
};
```

The driver author writes 1 trait impl. The macro emits 2 extern
shims + 1 manifest. The manifest is byte-exact; two recompiles
of the same trait impl produce identical manifest bytes
(reproducibility / R8).

### 4.3 First-cut implementation: declarative macro, not proc-macro

Phase-2 PR DI ships with a `macro_rules!`-based implementation
of `wari_driver!` rather than a full proc-macro. Reasoning:

- proc-macros need a `proc-macro = true` crate, a host-target
  build during compilation of the wasm32 driver, and `syn`/`quote`
  pulled into the build graph.
- Wari's drivers are few (UART, Net) with stable trait shapes.
  A `macro_rules!` invocation per driver kind is enough.

Rough shape:

```rust
wari_driver_iface::declare_uart_driver! {
    Driver => {
        write(buf) -> i32 { /* body */ }
    }
}
```

where the macro fans out the shims + manifest. A future Phase-3
PR can replace this with a proc-macro that derives from a real
`impl UartDriver for Driver` block, with no on-disk binary
changes (manifest bytes remain identical).

---

## 5 · Kernel-side parser + verifier

### 5.1 New file: `kernel/src/runtime/manifest.rs`

```rust
pub struct DriverManifestRef<'a> {
    header: &'a ManifestHeader,
    exports: &'a [ExportDecl],
    imports: &'a [ImportDecl],
}

pub fn parse_from_wasm(
    wasm: &[u8],
) -> Result<DriverManifestRef<'_>, KernelError>;
```

`parse_from_wasm`:

1. Walk WASM section headers (preamble: 8 bytes magic+version,
   then sections: `id u8`, `size LEB128`, payload).
2. For each section with `id == 0` (custom), read its name; if
   name == `"wari_driver_manifest"`, treat payload as manifest.
3. Bounds-check payload length matches `16 + export_count*36 +
   import_count*52`.
4. Verify magic == `b"WDM\0"`.
5. Verify `abi_version == 1` (Phase 2).
6. Verify every `ExportDecl::sig` and `ImportDecl::sig` is a
   known `FuncSig` discriminant.
7. Return `DriverManifestRef` with raw slices into the input.

No allocation. ~40-60 LOC of indexing.

### 5.2 New verifier in `loader.rs`

```rust
pub fn verify_against_kernel_expectations(
    manifest: &DriverManifestRef,
    expected_kind: DriverKind,
    instance: &Instance,
    store: &impl AsContext,
    linker: &Linker<...>,
) -> Result<(), KernelError>;
```

1. `manifest.header.kind == expected_kind as u16` (else `WrongKind`).
2. For each declared export, `instance.get_typed_func(name, sig)`
   resolves with the declared signature (else `BadExport`).
3. For each declared import, `linker.get(...).is_some()` (else
   `MissingHostFn`).

### 5.3 Loader rewrite

`load_tier2(envelope, expected_kind)` becomes:

```rust
let wasm = sign::verify(envelope)?;                    // INV-13
let manifest = manifest::parse_from_wasm(wasm)?;       // NEW
if manifest.header.kind != expected_kind as u16 {
    return Err(KernelError::DriverWrongKind);
}
if manifest.header.abi_version != DRIVER_ABI_VERSION {
    return Err(KernelError::DriverAbiVersion);
}
let module = Module::new(&engine, wasm)?;
let mut linker = ...;
register_host_fns(&mut linker, manifest.imports, ...)?;  // NEW: imports drive registration
let instance = linker.instantiate(...)?.start(...)?;
verify_exports(&manifest, &instance, &store)?;          // NEW
Ok(Tier2Instance { instance, store, manifest, .. })
```

`load_tier2` and `load_tier2_net` collapse into one
`load_tier2_driver(envelope, expected_kind)` — the per-kind
host-fn registration is driven by the manifest's imports, not
by the call site.

### 5.4 New error variants

```rust
pub enum KernelError {
    // ... existing ...
    DriverManifestMissing,    // No wari_driver_manifest section
    DriverManifestMalformed,  // Bad magic / size mismatch / unknown sig
    DriverAbiVersion,         // abi_version not in supported set
    DriverWrongKind,          // manifest.kind != expected
    DriverBadExport,          // declared export missing or wrong sig
    DriverMissingHostFn,      // declared import not registered
}
```

---

## 6 · Sign-tool integration

`scripts/sign-module.rs` gains a pre-sign verification step:

1. Parse the input `.wasm` for `wari_driver_manifest` custom section.
2. Refuse to sign if missing or malformed.
3. Independently walk the WASM exports + imports.
4. For every manifest export decl, assert the WASM `.wasm` actually
   exports that name with a type-section signature matching the
   declared FuncSig. (Uses a tiny inline FuncSig → wasmparser-style
   `(params, results)` mapping.)
5. Same for imports.
6. Refuse to sign on any mismatch.

This is the **tamper-evident** layer: it makes "ship a manifest
that lies about my exports" a sign-time error, not a kernel-time
trust hole.

---

## 7 · Migration

### 7.1 PR sequence

| PR | Title | Scope | LOC |
|---|---|---|---|
| **DI-0** (this doc) | Driver interface design draft v1 | Doc only | ~600 lines doc |
| **DI-1** | `driver-iface` crate + manifest types + sig table | New crate, no_std | ~250 |
| **DI-2** | `wari_driver!` macro + UART driver migration | Macro + migrate `drivers/uart` | ~350 |
| **DI-3** | Kernel loader rewrite + manifest parser + new errors | `kernel/src/runtime/manifest.rs` + `loader.rs` collapse | ~400 |
| **DI-4** | Net driver migration | `drivers/net` migrate to macro | ~200 |
| **DI-5** | Sign-tool manifest verifier | `scripts/sign-module.rs` adds pre-sign check | ~150 |
| **DI-6** | Tests: manifest fuzz + wrong-kind rejection + bad-sig rejection | `tests/integration` + Kani harness for parser | ~300 |

Total: ~1700 LOC across 6 PRs.

### 7.2 Backward compatibility

Phase 2 starts with `abi_version = 1`. The kernel rejects any
manifest with `abi_version != 1`. There are no pre-manifest
binaries in the field — every existing driver is rebuilt during
this PR series.

### 7.3 What this PR series does NOT change

- Signing format (96-byte ed25519 header + payload — unchanged)
- ModuleId enum (still names the slots; manifest verifies the
  binary fits the slot)
- caps_for static authority table (manifest *informs* kernel of
  what host fns the driver imports; cap grants stay declarative
  for now)
- wasmi version (still 0.32.3 from Phase 2 wasmi pin)

---

## 8 · Verification + tests

### 8.1 Round-trip property tests (host-side, `tests/`)

For every supported `DriverKind`:

- Build a known-good manifest at compile time
- Parse it via `parse_from_wasm` round-trip
- Assert byte-identical re-encoding

### 8.2 Adversarial inputs (negative tests)

- Manifest with bad magic → `DriverManifestMalformed`
- Manifest with `abi_version = 99` → `DriverAbiVersion`
- Manifest declaring export not present in WASM → sign tool refuses
- Manifest declaring `kind = Uart` loaded into Net slot → kernel refuses
- Truncated manifest section → `DriverManifestMalformed`
- Manifest with `export_count = 0xFFFF` (size overflow) → caught at parse

### 8.3 Kani harness (manifest parser)

The parser is the new attack surface. Kani proof:

- For any byte slice ≤ 4 KiB, `parse_from_wasm` either returns
  `Err` or returns `Ok` with a `DriverManifestRef` whose slice
  bounds are within the input.
- No panics, no out-of-bounds reads.

### 8.4 Smoke tests

- `make run` — kernel boots, both drivers load with manifests,
  hello runs, net comes up, Ctrl-R reboots.
- `make test-security` — existing P1-P8 still pass.
- A new "ship a wrong-kind binary" test: build the net driver
  with `kind = Uart` in its manifest, attempt to load into the
  Net slot, assert `DriverWrongKind` is logged.

---

## 9 · Open questions

1. **Capability declaration in manifest?** The manifest could
   declare "I require `Net+READ+WRITE` cap to function." Phase 2
   keeps cap grants in `caps_for` static; Phase 3 could derive
   them from the manifest, with the manifest verifier asserting
   the kernel's grant covers the manifest's request. Defer to
   Phase 3.

2. **Per-driver private keys?** Currently every signed envelope
   is signed by the same dev key. The manifest could carry an
   issuer field that maps to one of N accepted keys. Defer to
   Phase 3 (multi-vendor driver story).

3. **Driver self-test export?** A `selftest() -> i32` export the
   kernel runs after instantiate, before any production call,
   to surface boot-time hardware failures. Optional — declared
   in manifest if present, kernel runs it if so. Suggest:
   include in DI-1 manifest type but make it optional; defer
   actual driver implementation to Phase 3.

4. **Manifest in the signed envelope vs. inside the WASM
   binary?** Today's design puts the manifest inside the WASM
   custom section. Alternative: copy the manifest bytes into
   the envelope header (so the kernel can read it without
   parsing WASM). Pro: simpler kernel parser. Con: now there
   are two copies of the manifest and they must agree (sign
   tool ensures that). Suggest: keep manifest in WASM section
   for Phase 2; revisit if kernel boot time becomes an issue.

---

## 10 · Prior art

- **seL4** — uses CapDL for static capability layout, but
  drivers themselves are conventional ELF binaries. Manifest
  concept is new at the driver level.
- **Component Model / WIT** — the long-term replacement.
  Defines interfaces declaratively, kernel-equivalent (the
  component runtime) verifies. Wari's manifest is a stripped-
  down precursor, byte-compatible at the trait level.
- **Linux kernel modules** — `modinfo` field declares
  `vermagic`, `depends`, etc. Linux's check is "vermagic
  matches", which is a weaker structural property than
  signature-checked exports. Wari's manifest goes further.
- **Fuchsia FIDL** — full IDL with code generation. Same
  trajectory as Component Model; Wari follows that path in
  Phase 4+.

---

## 11 · Summary

This PR series replaces "the driver exports a function called
`write`, the kernel hopes it does" with "the driver embeds a
signed, machine-checked manifest declaring its kind, ABI
version, and export/import surface; the kernel verifies before
running it." The cost is ~1700 LOC across 6 PRs, one new
`driver-iface` crate, and a small kernel-side parser. The
benefit is that a critical class of bugs — typo'd exports,
wrong-kind binaries loaded into the wrong slot, drift between
driver and kernel — becomes a load-time hard failure with a
specific error code, not a runtime mystery.
