---
sidebar_position: 19
sidebar_label: "Ch 19: The Manifest & Signing"
title: "Chapter 19 — The Manifest and Signing"
---

# Chapter 19 — The Manifest and Signing

Chapter 18 ended with a macro invocation that emitted, alongside the
export shims, a byte array in a WASM custom section named
`wari_driver_manifest`. That byte array is the most important thing in
the binary that is not code. It is the driver's **contract** — a
machine-checked declaration of what the driver claims to be — and the
kernel refuses to run a driver whose claim it cannot verify.

This chapter is about that contract: what it says, why it is a packed
`repr(C)` byte string and not a serialization format with a name, how
the sign tool makes it impossible to lie, and where the whole thing sits
in the chain of trust the kernel walks before a driver's first
instruction executes.

## The gap the manifest closes

Before the manifest existed, a Tier-2 driver exposed its surface by
*convention*. The driver exported a function it happened to call
`write`, the kernel asked `wasmi` for a function literally named
`"write"` with signature `(u32, u32) -> i32`, and both sides *hoped* the
other had it right. `docs/driver-interface-design.md` lays out what that
hope failed to catch: a typo'd export name, a drifted signature, a
forgotten `#[no_mangle]` — each surfacing as the same opaque
`KernelError::DriverError` at boot. Worse, the net driver and the UART
driver shared the same signed-envelope format and the same tier. Nothing
structurally prevented loading `net.signed.wasm` into the UART slot; it
was prevented only because `kmain` happened to call the loaders with the
right hard-coded arguments.

For an OS that means to be *provable*, "we hard-code the right loader
call" is not a safety property. It is a comment. The manifest turns four
questions the kernel wants to ask about an unknown binary — and one it
wants for informational safety — into fields it can check before any
code runs:

1. Is this even a Wari driver? (the magic number)
2. Does it speak my ABI version? (`abi_version`)
3. Is it the *kind* of driver I am loading? (`kind`)
4. Does its export list match what I am going to call? (export
   descriptors)
5. What host functions does it expect me to provide? (import
   descriptors)

The manifest is emphatically **not** a behavioral contract. It does not
promise that `write` delivers bytes without side effects — that kind of
promise is what a *signed Tier-2 vendor* is, established by review and
signing, not by a byte string. The manifest is a *structural* contract:
the binary exposes exactly the surface it claims, and that surface is the
one the kernel was compiled against.

## What the manifest says

The types live in `driver-iface/src/lib.rs` — the one crate every Tier-2
driver depends on, and the one that pins the ABI version a driver binary
speaks. The layout is a fixed 16-byte header followed by export
descriptors then import descriptors, all `#[repr(C, packed)]` so the
on-wire bytes are the in-memory bytes exactly.

The header (`lib.rs:278`):

```rust
#[repr(C, packed)]
pub struct ManifestHeader {
    pub magic: [u8; 4],       // b"WDM\0" — Wari Driver Manifest
    pub abi_version: u16,     // MANIFEST_ABI_VERSION, currently 1
    pub kind: u16,            // DriverKind discriminant
    pub export_count: u16,
    pub import_count: u16,
    pub flags: u32,           // reserved, zero in Phase 2
}
const _: () = assert!(core::mem::size_of::<ManifestHeader>() == 16);
```

`DriverKind` (`lib.rs:96`) is the identity field: `Uart = 1`, `Net = 2`,
`Block = 3` (reserved for Phase 3). The kernel asserts the declared kind
matches the slot it is loading into; a UART binary in the Net slot fails
with `WrongKind` before a single instruction runs. The discriminants are
append-only — never renumber, only add — because a signed binary in the
field encodes them by value.

Each export is an `ExportDecl` (`lib.rs:310`): a 32-byte NUL-padded name
plus a one-byte signature code plus three bytes of padding, 36 bytes
total. Each import is an `ImportDecl` (`lib.rs:334`): a 16-byte module
name (`"wari"` today), the 32-byte name, the signature byte, padding — 52
bytes. The signature byte is a `FuncSig` discriminant (`lib.rs:137`), a
*closed* set of the function shapes Wari's ABI actually uses:
`UnitUnit` for `() -> ()`, `U32xU32I32` for `(u32, u32) -> i32`,
`U64I32` for the net `poll`, and so on. The set is deliberately small —
the host-fn surface grows slowly and the same shapes recur — and adding
a shape is an ABI change.

The net driver's manifest is assembled at compile time by the
`wari_net_driver!` macro (`lib.rs:802`) from two descriptor lists: eleven
exports (`lib.rs:886`) — `_start`, `poll`, `tx_send`, `rx_pop`,
`rx_recycle`, and the six socket calls — and eight imports (`lib.rs:906`)
— `net_mmio_write32`, `net_mmio_read32`, `nic_set_mac`,
`nic_attach_queue`, `nic_queue_notify`, `lin_mem_base`, `drv_log_u32`,
and `drv_trace_u32`. The total size is a compile-time constant,
`NET_MANIFEST_SIZE` (`lib.rs:776`):

```rust
pub const NET_MANIFEST_SIZE: usize = manifest_size(11, 8);
```

`manifest_size(11, 8)` is `16 + 11×36 + 8×52 = 828` bytes, and a host
test (`lib.rs:1030`) locks that number down: an edit to the export or
import table that forgets to update the constant — or vice versa — fails
in `cargo test` before it ever reaches the sign tool. The UART manifest
is the same machinery at `manifest_size(2, 2) = 192` bytes.

One subtlety that will matter for the rest of this chapter: the import
name in the manifest is the *WASM-level* name, matching the driver's
`#[link_name]`, not the Rust symbol. The macro's list carries `(b"wari",
b"net_mmio_write32", …)`; the driver's `extern` block carries
`#[link_name = "net_mmio_write32"]` over a Rust fn called
`wari_net_mmio_write32`. The two sides have to agree on the WASM name,
because that is the name the sign tool and the kernel both see.

## Why a packed byte string, not protobuf

`docs/driver-interface-design.md` §2.3 makes the case, and it is worth
restating because it is a values statement as much as a technical one:

- **Auditability.** The kernel's manifest parser is a few dozen lines of
  bounds-checked indexing. No third-party decoder, no schema compiler,
  nothing to pull into the audit window of a kernel that means to be
  formally verified.
- **No allocation.** The parser hands back slices into the input buffer.
  No heap, no `Vec`, no `String` — the same constraint that keeps the
  parser Kani-checkable.
- **Determinism.** Every supported manifest has exactly one byte-level
  encoding. Two recompiles of the same trait impl produce byte-identical
  manifest bytes, which is what lets the signed envelope hash them
  without reproducibility drift (Rule R8).
- **Forward-compatibility through versioning.** Bumping `abi_version` is
  the migration path. An old kernel rejects a newer manifest cleanly
  rather than misreading it.

The cost is that every new function-signature shape adds a `FuncSig`
discriminant. That cost is bounded by the host-fn surface, not by the
number of drivers, and the surface has about ten shapes across
everything Wari runs. The long-term answer is the WASM Component Model
and WIT — the design doc is explicit that the manifest is a stripped-down
precursor, forward-compatible at the trait level — but that needs a newer
`wasmi` and a build-pipeline rewrite. Until then, declaration beats
inference, and thirty lines of section walker buys the stronger property.

## The bidirectional check: you cannot lie

Here is the mechanism that makes the manifest a *tamper-evident*
contract rather than a hopeful annotation. The signer,
`scripts/sign-module.rs`, will not produce a signed envelope unless the
manifest and the actual WASM binary agree — **in both directions.**

`verify_manifest` (`sign-module.rs:155`) parses the embedded manifest
with the very same `driver-iface::parse` module the kernel uses, then
walks the WASM's real export and import sections with `wasmparser`, and
checks six things (`sign-module.rs:190`):

1. Every manifest-declared export is present in the WASM, with a matching
   signature shape.
2. Every WASM export is present in the manifest — *no undeclared
   surface.*
3. Every manifest-declared import is requested by the WASM, with a
   matching shape.
4. Every WASM-requested import is present in the manifest — *no
   undeclared host-fn need.*
5. and 6. the magic and ABI version parse.

Directions two and four are the ones that matter. It is not enough that
everything the manifest promises exists; nothing may exist that the
manifest did *not* promise. A driver cannot smuggle in an extra export
the kernel might resolve by accident, or an extra host-fn import that
implies a capability the manifest never disclosed. On any mismatch the
tool prints a specific error and exits non-zero:

```
sign-module: refusing to sign — wasm imports "wari"::"drv_trace_u32"
but manifest does not declare it
```

### The `drv_trace_u32` episode

This is not hypothetical; it is exactly how the eighth import got added,
and it is the cleanest illustration of the check firing. The per-frame
RX/TX diagnostic tags originally went through the always-on
`drv_log_u32`. That turned out to cost roughly 3.6 ms of blocking
115200-baud UART per line on a production build — about 14 ms per
received frame — which capped RX service and put a hard floor under ping
latency (Chapter 21 tells that half of the story). The fix was to demote
hot-path logging to a new, debug-gated host function, `drv_trace_u32`
(`drivers/net/src/lib.rs:228`).

Adding that one call touched *both* sides, and the sign tool is the
reason you cannot forget either:

- The driver's `extern` block gained the `#[link_name = "drv_trace_u32"]`
  import — so the compiled WASM now *requests* it.
- The macro's manifest import list gained `(b"wari", b"drv_trace_u32",
  …)` and `NET_MANIFEST_SIZE` went from `manifest_size(11, 7)` to
  `manifest_size(11, 8)`.

Miss the manifest edit, and the WASM imports a function the manifest does
not declare — check (4) fails, "wasm imports … but manifest does not
declare it," no signature. Add it to the manifest but never actually
*call* it, and a subtler trap springs: LTO strips unused imports from the
WASM entirely, so the binary no longer requests a function the manifest
still declares — check (3) fails, "manifest declares import … but wasm
does not import it." This is why the `NET_MANIFEST_SIZE` doc comment
(`driver-iface/src/lib.rs:766`) insists the manifest list *only* what the
WASM genuinely requests. It is also why platform-asymmetric imports —
one that the QEMU build calls but the VF2 build does not, or the reverse
— must be `#[used]`-pinned on the platform that does not call them, so
both platform WASMs request the identical import set and the same
manifest can bless both. `drv_trace_u32` is pinned this way on the QEMU
side; the comment at `lib.rs:226` says so in as many words.

The lesson generalizes: **a new host-fn call is a two-file edit — the
driver's `extern` block and the macro's manifest list — and the sign tool
will not let you land one without the other.**

## ABI versioning

`MANIFEST_ABI_VERSION` (`driver-iface/src/lib.rs:72`) is `1`. Its module
doc (`lib.rs:33`) lists precisely what a bump means: any change to the
manifest layout, the `FuncSig` discriminants, the `DriverKind`
discriminants, or the trait method shapes. The kernel rejects any
manifest whose `abi_version` is outside the set it supports, so an old
kernel stays safe against a driver built with a newer contract; a vendor
recompile produces new manifest bytes and a new signature. This is the
one-way door that keeps "add a trait method" from silently mismatching a
kernel that predates it — the mismatch becomes an `UnsupportedAbiVersion`
load error with a name, not a wrong-signature call at first use.

## The trust chain: signature first

The manifest is a *structural* gate. It sits behind an older,
*cryptographic* one, and the order is not negotiable. The trust chain for
loading a Tier-2 driver, from `driver-interface-design.md` §2.2:

```
1. kernel: verify ed25519 signature over the envelope payload  (INV-13)
2. kernel: parse WASM, find the wari_driver_manifest section
3. kernel: verify manifest magic + abi_version
4. kernel: verify manifest.kind == the slot's expected kind
5. kernel: instantiate the WASM (wasmi)
6. kernel: resolve each declared export at its declared signature
7. kernel: assert each declared import is registered on the linker
8. kernel: call _start
```

Step 1 is the root. **The signature is verified first, before the kernel
parses a single WASM section.** This is INV-13
(`docs/invariants.md:303`): *any `.wasm` bytecode loaded at Tier 2 passes
ed25519 verification against the kernel's compiled-in `ACCEPTED_PUBKEY`
before instantiation.* `kernel/src/runtime/sign.rs::verify` is the first
gate before any Tier-2 `wasmi` parse (`invariants.md:367`). Only bytes
that a trusted key signed are ever handed to the manifest parser, and
only a manifest that passes gets instantiated.

The signer's envelope is deliberately plain (`sign-module.rs:11`): 32
bytes of ed25519 public key, 64 bytes of signature over the trailing
WASM, then the raw WASM. The signature covers the *whole* WASM, manifest
section included — so the manifest the kernel verifies at step 3 is
byte-for-byte the manifest the sign tool cross-checked against the code
at signing time. The two checks compose: the sign tool guarantees "the
manifest matches the code," the signature guarantees "these bytes are
the ones the sign tool blessed," and the kernel's step-4 check guarantees
"this blessed driver is the kind I meant to load here." A malicious or
merely careless vendor cannot ship a binary whose manifest is a lie the
kernel later trusts, because the lie is caught at signing and the
signature is caught at loading.

Phase 2 signs every envelope with a single dev key; per-vendor keys and a
manifest issuer field are noted as Phase 3 work in the design doc's open
questions. The structure is already in place to carry them.

## Closing hook

The manifest tells the kernel what a driver *is*; the signature tells it
the manifest can be trusted. But both of those checks run against bytes
that have to *exist* first — a signed blob sitting at exactly the path
the kernel's `include_bytes!` expects, built for exactly the platform the
kernel was compiled for, tagged with exactly the build number on the
banner.

Getting those bytes into place is its own discipline, with its own scar
tissue. Chapter 20 walks the full pipeline — build the driver WASM twice,
sign each, embed the cfg-selected blob, verify the build tags line up,
publish — and tells the story of the week Wari spent debugging dead code
because a stale blob slipped through the one gap this pipeline now closes.
