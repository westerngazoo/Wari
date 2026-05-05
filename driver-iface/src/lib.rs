// SPDX-License-Identifier: AGPL-3.0-only
//! Wari Tier-2 driver interface — manifest types and trait
//! declarations shared by the kernel and every signed driver.
//!
//! See `docs/driver-interface-design.md` for the full design.
//!
//! # The contract in 60 seconds
//!
//! Every Tier-2 driver embeds a [`ManifestHeader`] + N
//! [`ExportDecl`] + M [`ImportDecl`] in a WASM custom section
//! named exactly `wari_driver_manifest` (no leading dot — WASM
//! custom-section convention). The bytes are produced by the
//! [`wari_driver!`] macro from a trait impl; the kernel parses
//! them at load time, checks magic + ABI version + kind + every
//! export's signature; refuses to load on any mismatch.
//!
//! # What lives here vs. elsewhere
//!
//! - **Here**: pure data types — the manifest layout, the closed
//!   set of supported function signatures, driver-kind discriminant,
//!   the `UartDriver` / `NetDriver` traits the macro lowers.
//! - **In `kernel/src/runtime/manifest.rs`** (PR DI-3): the parser
//!   that walks WASM section headers and returns `&ManifestHeader`
//!   + slices of `ExportDecl` / `ImportDecl` (no allocation).
//! - **In each driver crate** (`drivers/uart`, `drivers/net`,
//!   PRs DI-2 / DI-4): a `wari_driver!` macro invocation that emits
//!   `#[no_mangle] extern "C"` shims AND the manifest bytes static.
//! - **In `scripts/sign-module.rs`** (PR DI-5): a pre-sign verifier
//!   that walks both the WASM exports/imports and the embedded
//!   manifest, refuses to sign on disagreement.
//!
//! # Stability
//!
//! Bumping any of:
//!  - the manifest layout
//!  - the [`FuncSig`] discriminants
//!  - the [`DriverKind`] discriminants
//!  - the trait method shapes (`UartDriver`, `NetDriver`)
//!
//! requires bumping [`MANIFEST_ABI_VERSION`]. The kernel rejects any
//! manifest with a non-supported `abi_version`. Old kernels stay
//! safe; vendor recompiles produce new manifest bytes.
//!
//! # Why no `alloc`
//!
//! Manifest bytes are statically sized and emitted at compile time.
//! The kernel parser hands back slices into the input buffer. No
//! heap, no `Vec`, no `String`. Same constraint that lets the
//! parser be Kani-checkable.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

/// WASM section walker + manifest parser. Shared by the kernel
/// loader (`kernel/src/runtime/manifest.rs`) and the host-side
/// sign tool (`scripts/sign-module.rs`) so the two cannot drift.
/// See `parse::parse_from_wasm`.
pub mod parse;

// ── Manifest framing ─────────────────────────────────────────────

/// Magic bytes at the start of every manifest. ASCII "WDM\0".
/// Wari Driver Manifest — distinguishes our section payload from
/// any other custom section that happens to share its name.
pub const MAGIC: [u8; 4] = *b"WDM\0";

/// ABI version of the manifest format. Bump on any breaking change
/// to the layout, the [`FuncSig`] discriminants, or the trait
/// method shapes. Kernel rejects manifests with an unsupported
/// version.
pub const MANIFEST_ABI_VERSION: u16 = 1;

/// Custom-section name (UTF-8) the kernel scans WASM binaries for.
/// Must match the `#[link_section = ...]` the macro emits. Stored
/// here so kernel and macro cannot drift.
pub const SECTION_NAME: &str = "wari_driver_manifest";

/// Maximum export name length, including trailing NUL. Sized to fit
/// every export currently used by Wari drivers (`write`, `_start`,
/// `poll`, `tx_send`, `rx_pop`, `rx_recycle`) with comfortable
/// headroom. Increase requires an ABI version bump.
pub const NAME_MAX: usize = 32;

/// Maximum host-fn module name length (the `wari` in
/// `linker.func_wrap("wari", "cap_mint", ...)`). 16 bytes is more
/// than enough for the single namespace Phase 2 uses.
pub const MODULE_MAX: usize = 16;

// ── Discriminants ────────────────────────────────────────────────

/// Driver kind. The kernel asserts this matches the slot it is
/// loading the binary into; a UART binary loaded into the Net slot
/// fails with [`DriverManifestError::WrongKind`] before any code
/// runs. Discriminants are stable: never renumber, only append.
#[repr(u16)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DriverKind {
    /// Tier-2 UART driver. Exports `write`, `_start`.
    Uart = 1,
    /// Tier-2 network driver. Exports `_start`, `poll`, `tx_send`,
    /// `rx_pop`, `rx_recycle`.
    Net = 2,
    /// Tier-2 block-device driver. Reserved for Phase 3.
    Block = 3,
    // Append-only. New kinds get the next id.
}

impl DriverKind {
    /// Decode a discriminant from its raw u16 form. Returns `None`
    /// for any value the parser does not recognize; the kernel
    /// turns that into [`DriverManifestError::UnknownKind`].
    pub fn from_raw(v: u16) -> Option<Self> {
        match v {
            1 => Some(DriverKind::Uart),
            2 => Some(DriverKind::Net),
            3 => Some(DriverKind::Block),
            _ => None,
        }
    }
}

/// Closed set of WASM function signatures the manifest can declare.
/// Adding a signature shape is an ABI change (bump
/// [`MANIFEST_ABI_VERSION`]) — but in practice the host-fn surface
/// grows slowly and the same shapes recur across drivers, so the
/// table stays small.
///
/// The encoding deliberately spells out param/result lists rather
/// than using a numeric `(arity, types_index)` scheme: the kernel
/// can do `match sig` and pull a typed `get_typed_func` call out
/// directly, no dynamic dispatch.
///
/// Naming convention: `<params>_<result>` where each component is
/// `U32` / `U64` / `I32` / `Unit`, joined with `x` for multi-arg
/// params (e.g. `U32xU32I32` = `(u32, u32) -> i32`).
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FuncSig {
    /// `() -> ()` — `_start`, init shims.
    UnitUnit = 1,

    /// `(u32, u32) -> i32` — UART `write`, MMIO writes, `tx_send`.
    U32xU32I32 = 2,

    /// `u32 -> i32` — `notification_ack`, `rx_recycle`,
    /// `irq_register`, `nic_queue_notify`.
    U32I32 = 3,

    /// `u32 -> u32` — `mmio_read8`, `mmio_read32`.
    U32U32 = 4,

    /// `u64 -> i32` — net `poll`.
    U64I32 = 5,

    /// `() -> u64` — `rx_pop` (packed `(buf_off, len)`),
    /// `lin_mem_base`.
    UnitU64 = 6,

    /// `(u32, u32, u32, u32, u32) -> i32` — `nic_attach_queue`.
    U32x5I32 = 7,

    /// `(u32, u32, u32) -> i32` — `socket_bind` (handle, ip_be,
    /// port), `socket_send` / `socket_recv` (handle, buf_off, len).
    /// Added in PR Net-6c.
    U32x3I32 = 8,
    // Append-only. Renumbering breaks every signed driver.
}

/// WASM-level shape of a function signature: (param value types,
/// result value types). The `i32` slot is the only thing WASM
/// itself cares about — `u32` vs `i32` is a manifest-level
/// semantic distinction. Two `FuncSig` variants share the same
/// shape iff they encode the same `(i32 / i64)` pattern.
///
/// Used by the sign-tool verifier (PR DI-5) to compare a
/// manifest declaration against the WASM binary's actual type
/// section: the comparison uses shape equality, so a manifest
/// declaring `U32I32` against a WASM `(i32) -> i32` is accepted
/// (both shapes are `[I32] -> [I32]`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WasmSigShape {
    /// Parameter value-type pattern. Each element is one of
    /// `WasmValType` below.
    pub params: &'static [WasmValType],
    /// Result value-type pattern.
    pub results: &'static [WasmValType],
}

/// Subset of WASM ValType the Wari ABI actually uses (no f32 /
/// f64 / v128 / refs in the host-fn surface).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WasmValType {
    /// 32-bit integer. Covers manifest U32 and I32 alike.
    I32,
    /// 64-bit integer.
    I64,
}

impl FuncSig {
    /// Decode a discriminant from its raw u8. `None` means the
    /// kernel sees a manifest from a future driver-iface ABI; the
    /// driver is rejected.
    pub fn from_raw(v: u8) -> Option<Self> {
        match v {
            1 => Some(FuncSig::UnitUnit),
            2 => Some(FuncSig::U32xU32I32),
            3 => Some(FuncSig::U32I32),
            4 => Some(FuncSig::U32U32),
            5 => Some(FuncSig::U64I32),
            6 => Some(FuncSig::UnitU64),
            7 => Some(FuncSig::U32x5I32),
            8 => Some(FuncSig::U32x3I32),
            _ => None,
        }
    }

    /// The WASM-level shape this signature encodes. Used by the
    /// sign-tool to verify the embedded manifest agrees with the
    /// binary's actual type section. Multiple `FuncSig` variants
    /// can share a shape (e.g. `U32I32` and `U32U32`) — the
    /// difference is semantic to Wari, invisible to WASM.
    pub fn wasm_shape(self) -> WasmSigShape {
        use WasmValType::{I32, I64};
        const E: &[WasmValType] = &[];
        const I32_1: &[WasmValType] = &[I32];
        const I32_2: &[WasmValType] = &[I32, I32];
        const I32_3: &[WasmValType] = &[I32, I32, I32];
        const I32_5: &[WasmValType] = &[I32, I32, I32, I32, I32];
        const I64_1: &[WasmValType] = &[I64];
        match self {
            FuncSig::UnitUnit   => WasmSigShape { params: E,     results: E      },
            FuncSig::U32xU32I32 => WasmSigShape { params: I32_2, results: I32_1  },
            FuncSig::U32I32     => WasmSigShape { params: I32_1, results: I32_1  },
            FuncSig::U32U32     => WasmSigShape { params: I32_1, results: I32_1  },
            FuncSig::U64I32     => WasmSigShape { params: I64_1, results: I32_1  },
            FuncSig::UnitU64    => WasmSigShape { params: E,     results: I64_1  },
            FuncSig::U32x5I32   => WasmSigShape { params: I32_5, results: I32_1  },
            FuncSig::U32x3I32   => WasmSigShape { params: I32_3, results: I32_1  },
        }
    }
}

// ── Wire types ───────────────────────────────────────────────────

/// 16-byte fixed header at the start of every manifest. Followed
/// by `export_count` [`ExportDecl`]s then `import_count`
/// [`ImportDecl`]s.
///
/// `repr(C, packed)` chosen so the on-wire layout matches the
/// in-memory layout exactly. Two recompiles of the same trait impl
/// must produce byte-identical manifest bytes — the signed
/// envelope hashes them and any drift breaks reproducibility (R8).
#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
pub struct ManifestHeader {
    /// Always [`MAGIC`]. Distinguishes a Wari manifest from any
    /// other custom section that happens to share the name.
    pub magic: [u8; 4],

    /// Always [`MANIFEST_ABI_VERSION`] for Phase 2.
    pub abi_version: u16,

    /// [`DriverKind`] discriminant. Kernel asserts this matches
    /// the slot it is loading into.
    pub kind: u16,

    /// Number of [`ExportDecl`]s that follow the header.
    pub export_count: u16,

    /// Number of [`ImportDecl`]s that follow the exports.
    pub import_count: u16,

    /// Reserved for forward-compatible flags (e.g. bit 0 = "driver
    /// declares a (start) section the kernel must invoke"). Phase
    /// 2 always emits zero; Phase 3+ may set bits without an ABI
    /// version bump if the meaning is "ignored when set" or "extra
    /// behaviour".
    pub flags: u32,
}

const _: () = assert!(core::mem::size_of::<ManifestHeader>() == 16);

/// Per-export descriptor. Kernel resolves the export by name and
/// verifies the signature matches before invoking it.
#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
pub struct ExportDecl {
    /// Export name as a NUL-padded ASCII byte string. Length up
    /// to [`NAME_MAX`] - 1; the trailing NUL is mandatory so the
    /// kernel can use it as a string terminator without a
    /// separate length field.
    pub name: [u8; NAME_MAX],

    /// [`FuncSig`] discriminant. Kernel uses this to pick the
    /// correct typed `get_typed_func` instantiation.
    pub sig: u8,

    /// Padding to 4-byte alignment. Always zero.
    pub _pad: [u8; 3],
}

const _: () = assert!(core::mem::size_of::<ExportDecl>() == NAME_MAX + 4);

/// Per-import (host-fn) descriptor. Kernel asserts the linker has
/// a host fn registered with the declared `(module, name, sig)`;
/// fails fast on a missing or mistyped registration. Importing
/// a host fn the kernel does not provide is a load-time error,
/// not a "the driver crashes the first time it calls it."
#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
pub struct ImportDecl {
    /// Module name (always `"wari\0..."` in Phase 2; reserved for
    /// future namespacing).
    pub module: [u8; MODULE_MAX],

    /// Import name, NUL-padded.
    pub name: [u8; NAME_MAX],

    /// Required [`FuncSig`] discriminant.
    pub sig: u8,

    /// Padding. Always zero.
    pub _pad: [u8; 3],
}

const _: () = assert!(
    core::mem::size_of::<ImportDecl>() == MODULE_MAX + NAME_MAX + 4
);

// ── Errors ───────────────────────────────────────────────────────

/// Errors the kernel-side parser surfaces. Each maps to a distinct
/// `KernelError::Driver*` variant — see PR DI-3.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DriverManifestError {
    /// No `wari_driver_manifest` custom section in the WASM.
    Missing,

    /// Section payload too short to even hold the header, or the
    /// declared `export_count` × stride + `import_count` × stride
    /// overflows the payload.
    Truncated,

    /// Header `magic != [`MAGIC`]`. Wrong-kind file or junk in the
    /// section name slot.
    BadMagic,

    /// `abi_version` outside the kernel's supported set.
    UnsupportedAbiVersion,

    /// `kind` is not a known [`DriverKind`].
    UnknownKind,

    /// `kind` is known but does not match the slot the kernel is
    /// loading the driver into (e.g. UART binary in Net slot).
    WrongKind,

    /// An [`ExportDecl::sig`] or [`ImportDecl::sig`] is not a
    /// known [`FuncSig`]. Driver was built against a newer
    /// driver-iface ABI than this kernel supports.
    UnknownSig,

    /// A declared export does not exist in the WASM, or exists
    /// but with a different signature than declared.
    ExportMismatch,

    /// A declared import is not registered on the kernel's
    /// linker — the driver expects a host fn the kernel does not
    /// provide.
    MissingHostFn,
}

// ── Driver traits ────────────────────────────────────────────────

/// Tier-2 UART driver contract. The `write` method delivers bytes
/// to the underlying serial controller; everything else (reset,
/// init, format) is internal driver state.
///
/// Driver authors implement this for a unit-style struct and wrap
/// the impl with `wari_driver_iface::declare_uart_driver!{ ... }`
/// (see PR DI-2). The macro emits the `#[no_mangle] extern "C"`
/// `write(buf_ptr, len) -> i32` shim and the matching manifest.
pub trait UartDriver {
    /// Push `buf` to the UART. Returns bytes written on success
    /// (always == `buf.len()` for the line-buffered Phase-2 driver),
    /// or a negative errno on failure.
    fn write(buf: &[u8]) -> i32;
}

/// Tier-2 network driver contract. Exposes the surface the kernel
/// uses to drive smoltcp from its idle loop and to wire RX/TX to
/// Tier-1 socket host fns (Phase-1b PR Net-6).
pub trait NetDriver {
    /// Run the driver's one-shot init. Called by the kernel before
    /// `poll`. Failure leaves `Net.initialized = false` and the
    /// kernel surfaces the failure via its log line.
    fn start();

    /// Drive smoltcp's `Interface::poll`. `timestamp_ms` is a
    /// monotonic millisecond tick; the kernel passes its idle-loop
    /// counter. Returns 1 if state changed, 0 if nothing happened.
    fn poll(timestamp_ms: u64) -> i32;

    /// Queue a TX descriptor for `buf` and notify the device.
    /// Returns 0 on success, negative errno on failure.
    fn tx_send(buf: &[u8]) -> i32;

    /// Drain one RX descriptor from the used ring. Returns the
    /// packed `(buf_off << 32) | len` as a u64, or 0 if the ring
    /// is empty.
    fn rx_pop() -> u64;

    /// Recycle an RX buffer back to the device. Called after the
    /// kernel-side smoltcp has consumed the bytes from `rx_pop`.
    fn rx_recycle(desc_idx: u32) -> i32;

    // ── Socket API (PR Net-6a) ─────────────────────────────────
    //
    // Synchronous driver-RPC path: the kernel's Tier-1-facing
    // `wari::net_socket_*` host fns dispatch directly into these
    // exports. Per-call: kernel validates the calling tier's
    // caps, calls the driver, mints/revokes Socket caps in the
    // calling tier's CSpace as appropriate.

    /// Allocate a new smoltcp socket of the given protocol.
    /// Returns a positive smoltcp handle on success, or a negative
    /// errno (`E_NOMEM` if the smoltcp socket pool is full,
    /// `E_INVAL` if `proto` is not a known [`SocketProto`]).
    fn socket_create(proto: u32) -> i32;

    /// Tear down a smoltcp socket previously returned by
    /// `socket_create`. Returns 0 on success, negative errno on
    /// failure (e.g. `E_INVAL` if the handle is unknown).
    fn socket_close(handle: u32) -> i32;

    /// Bind a TCP socket to a local IPv4 address + port. `ip_be`
    /// is big-endian IPv4 (0 = unspecified / wildcard). Phase-1b
    /// scope: stores intent inside the driver; the actual smoltcp
    /// listen call happens in `socket_listen`. Returns 0 on
    /// success, negative errno otherwise.
    fn socket_bind(handle: u32, ip_be: u32, port: u32) -> i32;

    /// Mark a TCP socket as listening on the port supplied via
    /// `socket_bind`. `backlog` is currently ignored by the
    /// smoltcp backing (single-pending-conn). Returns 0 on
    /// success, negative errno otherwise (`E_INVAL` if the
    /// socket has no bound port yet, smoltcp listen failure).
    fn socket_listen(handle: u32, backlog: u32) -> i32;
}

/// Socket protocol selector — passed as the `proto` arg of
/// [`NetDriver::socket_create`]. Matches the
/// `wari::net_socket_create` host fn ABI from
/// `docs/net-driver-design.md` §6.
#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SocketProto {
    /// TCP socket. Backed by smoltcp's `socket-tcp` feature.
    Tcp = 1,
    /// UDP socket. Backed by smoltcp's `socket-udp` feature.
    Udp = 2,
}

impl SocketProto {
    /// Decode a raw u32 into a known protocol; returns `None` for
    /// any unknown value (callers turn that into `E_INVAL`).
    pub fn from_raw(v: u32) -> Option<Self> {
        match v {
            1 => Some(SocketProto::Tcp),
            2 => Some(SocketProto::Udp),
            _ => None,
        }
    }
}

// ── Build helpers (compile-time use by the macro) ────────────────

/// Total size in bytes of the manifest payload for a given
/// (export count, import count). Computed at compile time so the
/// `[u8; N]` static the macro emits has a known length.
pub const fn manifest_size(export_count: usize, import_count: usize) -> usize {
    16 + export_count * 36 + import_count * 52
}

/// Build the byte image of a manifest at compile time. Driver
/// crates hand a fixed-size descriptor array to this and get back
/// the exact bytes the WASM custom section will carry.
///
/// `EXPORTS` and `IMPORTS` are the compile-time count parameters;
/// `N` is the resulting buffer length (must equal
/// `manifest_size(EXPORTS, IMPORTS)`). The const-eval engine
/// catches any mismatch as a compile error.
///
/// Each export/import descriptor is `(name_bytes, sig)` —
/// the function NUL-pads the name, packs the sig byte, leaves
/// 3 bytes of padding, and writes the result at the right
/// offset. Imports additionally carry a module name (always
/// `"wari"` in Phase 2, but spelled out per descriptor for
/// future namespacing).
pub const fn build_manifest<const N: usize>(
    kind: DriverKind,
    exports: &[(&[u8], FuncSig)],
    imports: &[(&[u8], &[u8], FuncSig)],
) -> [u8; N] {
    let need = manifest_size(exports.len(), imports.len());
    if need != N {
        panic!("driver manifest: const-buffer size disagrees with descriptor counts");
    }

    let mut buf = [0u8; N];
    let mut i = 0;

    // Header: magic + abi_version + kind + export_count +
    //         import_count + flags.  Little-endian per repr(C).
    buf[0] = MAGIC[0];
    buf[1] = MAGIC[1];
    buf[2] = MAGIC[2];
    buf[3] = MAGIC[3];
    buf[4] = (MANIFEST_ABI_VERSION & 0xff) as u8;
    buf[5] = (MANIFEST_ABI_VERSION >> 8) as u8;
    buf[6] = ((kind as u16) & 0xff) as u8;
    buf[7] = ((kind as u16) >> 8) as u8;
    let ec = exports.len() as u16;
    buf[8] = (ec & 0xff) as u8;
    buf[9] = (ec >> 8) as u8;
    let ic = imports.len() as u16;
    buf[10] = (ic & 0xff) as u8;
    buf[11] = (ic >> 8) as u8;
    // flags u32 = 0 (already zeroed)
    i = 16;

    // Exports — NAME_MAX-padded name + 1 sig byte + 3 pad bytes.
    let mut e = 0;
    while e < exports.len() {
        let (name, sig) = exports[e];
        if name.len() >= NAME_MAX {
            panic!("driver manifest: export name does not fit in NAME_MAX bytes");
        }
        let mut k = 0;
        while k < name.len() {
            buf[i + k] = name[k];
            k += 1;
        }
        // NUL-padding already in place; sig at offset NAME_MAX.
        buf[i + NAME_MAX] = sig as u8;
        // 3 bytes of padding remain zero.
        i += NAME_MAX + 4;
        e += 1;
    }

    // Imports — module + name + sig + pad.
    let mut m = 0;
    while m < imports.len() {
        let (module, name, sig) = imports[m];
        if module.len() >= MODULE_MAX {
            panic!("driver manifest: import module name does not fit in MODULE_MAX bytes");
        }
        if name.len() >= NAME_MAX {
            panic!("driver manifest: import name does not fit in NAME_MAX bytes");
        }
        let mut k = 0;
        while k < module.len() {
            buf[i + k] = module[k];
            k += 1;
        }
        let mut k2 = 0;
        while k2 < name.len() {
            buf[i + MODULE_MAX + k2] = name[k2];
            k2 += 1;
        }
        buf[i + MODULE_MAX + NAME_MAX] = sig as u8;
        i += MODULE_MAX + NAME_MAX + 4;
        m += 1;
    }

    let _ = i; // silence "unused at end" — kept for future asserts
    buf
}

/// Pad an ASCII byte string into a fixed-size NUL-terminated buffer.
/// Used by the `wari_driver!` macro to lower string literals into
/// the manifest's `[u8; N]` name fields. Marked `const` so it runs
/// at compile time and the manifest bytes end up in `.rodata`.
///
/// # Panics
///
/// Compile-time panic if `name.len() >= N` (i.e. the trailing NUL
/// would not fit). This catches over-long export names at build
/// time, before the manifest is ever signed.
pub const fn pad_name<const N: usize>(name: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    let mut i = 0;
    // Reserve at least one byte for the trailing NUL.
    if name.len() >= N {
        // Compile-time abort — surfaced as a clear const-eval panic.
        panic!("driver manifest: export/import name does not fit in fixed buffer");
    }
    while i < name.len() {
        out[i] = name[i];
        i += 1;
    }
    out
}

// ── Driver-side macros ───────────────────────────────────────────
//
// The macros below are the driver author's only required surface.
// They:
//   - emit `#[no_mangle] extern "C"` shims that translate the WASM
//     ABI (linmem offsets) into Rust slices and dispatch into the
//     trait impl;
//   - emit a `WARI_DRIVER_MANIFEST` static in the
//     `wari_driver_manifest` WASM custom section, byte-exactly
//     matching the trait the driver is implementing.
//
// One macro per `DriverKind`. Adding a new kind = adding a new
// trait, a new manifest descriptor list, and a new macro. The
// repetition keeps each macro auditable in isolation.

/// Declare a Tier-2 UART driver.
///
/// Usage:
/// ```ignore
/// use wari_driver_iface::{wari_uart_driver, UartDriver};
///
/// pub struct Driver;
/// impl UartDriver for Driver {
///     fn write(buf: &[u8]) -> i32 { /* push to UART */ }
/// }
/// wari_uart_driver!(Driver);
/// ```
///
/// Expands to:
///
/// - `extern "C" fn write(buf_ptr: u32, len: u32) -> i32` — the
///   wasm-ABI shim. Reads `len` bytes from linmem at `buf_ptr` and
///   dispatches into `<$t as UartDriver>::write(slice)`.
/// - `extern "C" fn _start()` — empty WASI command entrypoint, so
///   the kernel's explicit `_start.call()` succeeds.
/// - A 192-byte `WARI_DRIVER_MANIFEST` static in section
///   `wari_driver_manifest`, declaring kind = Uart and the two
///   exports (`write`, `_start`) and two imports
///   (`wari_mmio_write8`, `wari_mmio_read8`).
#[macro_export]
macro_rules! wari_uart_driver {
    ($t:ty) => {
        // ─── exports ────────────────────────────────────────────
        //
        // Empty WASI-command entrypoint. The kernel calls _start
        // explicitly post-instantiate (wasmi 0.32 does not auto-
        // run exported _start the way wasmi 1.0's
        // instantiate_and_start did).
        #[no_mangle]
        pub extern "C" fn _start() {}

        /// `wari::write(buf_ptr: u32, len: u32) -> i32` — wasm-ABI
        /// shim, dispatches to the trait impl.
        #[no_mangle]
        pub extern "C" fn write(buf_ptr: u32, len: u32) -> i32 {
            // SAFETY: kernel-validated linear-memory slice; length
            // fits in u32 by ABI; the kernel's host-fn surface is
            // the only path by which this is invoked, so the
            // pointer is in-bounds for the driver's linmem.
            let slice = unsafe {
                core::slice::from_raw_parts(
                    buf_ptr as *const u8,
                    len as usize,
                )
            };
            <$t as $crate::UartDriver>::write(slice)
        }

        // ─── manifest ──────────────────────────────────────────
        //
        // 192-byte image, computed at compile time from the
        // descriptors below. `#[link_section]` places it in a WASM
        // custom section named `wari_driver_manifest`. `#[used]`
        // keeps the linker from stripping it under LTO.
        #[link_section = "wari_driver_manifest"]
        #[used]
        #[no_mangle]
        pub static WARI_DRIVER_MANIFEST: [u8; 192] =
            $crate::build_manifest::<192>(
                $crate::DriverKind::Uart,
                &[
                    (b"write",  $crate::FuncSig::U32xU32I32),
                    (b"_start", $crate::FuncSig::UnitUnit),
                ],
                &[
                    // Names match the driver's #[link_name = "..."]
                    // attributes — those become the actual WASM
                    // import names. The `wari_` prefix is the Rust
                    // symbol, not the WASM-level name.
                    (b"wari", b"mmio_write8", $crate::FuncSig::U32xU32I32),
                    (b"wari", b"mmio_read8",  $crate::FuncSig::U32U32),
                ],
            );
    };
}

/// Total manifest size for the Tier-2 net driver. Computed from
/// `manifest_size(9, 6)` = 16 + 9*36 + 6*52 = 652 bytes. Exported
/// so the macro and external tooling agree on the byte length.
///
/// 9 exports: `_start`, `poll`, `tx_send`, `rx_pop`, `rx_recycle`
/// (Phase-1b PR Net-4/5b), `socket_create`, `socket_close`
/// (PR Net-6a), `socket_bind`, `socket_listen` (PR Net-6c —
/// TCP server side of the socket API).
///
/// 6 imports cover what the smoltcp-backed VirtIO driver actually
/// calls today: `net_mmio_write32`, `net_mmio_read32`,
/// `nic_set_mac`, `nic_attach_queue`, `nic_queue_notify`,
/// `lin_mem_base`. The `notification_wait` / `notification_ack`
/// host fns are declared in the driver source for a future PR
/// but not yet invoked, so LTO strips the imports from the WASM —
/// adding them to the manifest would make the sign-tool refuse
/// the binary as "manifest declares an import the wasm does not
/// request". Re-add them when the driver actually calls them.
pub const NET_MANIFEST_SIZE: usize = manifest_size(9, 7);

/// Declare a Tier-2 network driver.
///
/// Usage mirrors [`wari_uart_driver!`]:
///
/// ```ignore
/// use wari_driver_iface::{wari_net_driver, NetDriver};
///
/// pub struct Driver;
/// impl NetDriver for Driver {
///     fn start() { /* virtio init */ }
///     fn poll(t: u64) -> i32 { /* smoltcp poll */ }
///     fn tx_send(buf: &[u8]) -> i32 { /* virtqueue tx */ }
///     fn rx_pop() -> u64 { /* drain used ring */ }
///     fn rx_recycle(i: u32) -> i32 { /* recycle desc */ }
/// }
/// wari_net_driver!(Driver);
/// ```
///
/// Expands to the 5 wasm-ABI shims (`_start`, `poll`, `tx_send`,
/// `rx_pop`, `rx_recycle`) and a 612-byte `WARI_DRIVER_MANIFEST`
/// static in section `wari_driver_manifest`, declaring kind = Net
/// and the 8 host-fn imports the smoltcp-backed virtio driver
/// requires.
#[macro_export]
macro_rules! wari_net_driver {
    ($t:ty) => {
        // ─── exports ────────────────────────────────────────────

        /// Driver-init entrypoint. Kernel calls this explicitly
        /// post-instantiate (see `runtime::run_tier2_net`). On
        /// success the trait's `start` has populated the kernel-
        /// side `Net.initialized = true` via `wari_nic_set_mac`.
        #[no_mangle]
        pub extern "C" fn _start() {
            <$t as $crate::NetDriver>::start();
        }

        /// `wari::poll(timestamp_ms: u64) -> i32` — drive smoltcp's
        /// Interface::poll for one tick. Kernel calls per idle loop.
        #[no_mangle]
        pub extern "C" fn poll(timestamp_ms: u64) -> i32 {
            <$t as $crate::NetDriver>::poll(timestamp_ms)
        }

        /// `wari::tx_send(buf_off, len) -> i32` — queue + notify.
        #[no_mangle]
        pub extern "C" fn tx_send(buf_off: u32, len: u32) -> i32 {
            // SAFETY: kernel-validated linmem slice (same shape as
            // the UART shim).
            let slice = unsafe {
                core::slice::from_raw_parts(
                    buf_off as *const u8,
                    len as usize,
                )
            };
            <$t as $crate::NetDriver>::tx_send(slice)
        }

        /// `wari::rx_pop() -> u64` — packed `(buf_off, len)`.
        #[no_mangle]
        pub extern "C" fn rx_pop() -> u64 {
            <$t as $crate::NetDriver>::rx_pop()
        }

        /// `wari::rx_recycle(desc_idx: u32) -> i32`
        #[no_mangle]
        pub extern "C" fn rx_recycle(desc_idx: u32) -> i32 {
            <$t as $crate::NetDriver>::rx_recycle(desc_idx)
        }

        /// `wari::socket_create(proto: u32) -> i32` (PR Net-6a)
        #[no_mangle]
        pub extern "C" fn socket_create(proto: u32) -> i32 {
            <$t as $crate::NetDriver>::socket_create(proto)
        }

        /// `wari::socket_close(handle: u32) -> i32` (PR Net-6a)
        #[no_mangle]
        pub extern "C" fn socket_close(handle: u32) -> i32 {
            <$t as $crate::NetDriver>::socket_close(handle)
        }

        /// `wari::socket_bind(handle, ip_be, port) -> i32` (Net-6c)
        #[no_mangle]
        pub extern "C" fn socket_bind(handle: u32, ip_be: u32, port: u32) -> i32 {
            <$t as $crate::NetDriver>::socket_bind(handle, ip_be, port)
        }

        /// `wari::socket_listen(handle, backlog) -> i32` (Net-6c)
        #[no_mangle]
        pub extern "C" fn socket_listen(handle: u32, backlog: u32) -> i32 {
            <$t as $crate::NetDriver>::socket_listen(handle, backlog)
        }

        // ─── manifest ──────────────────────────────────────────
        #[link_section = "wari_driver_manifest"]
        #[used]
        #[no_mangle]
        pub static WARI_DRIVER_MANIFEST: [u8; $crate::NET_MANIFEST_SIZE] =
            $crate::build_manifest::<{ $crate::NET_MANIFEST_SIZE }>(
                $crate::DriverKind::Net,
                &[
                    (b"_start",       $crate::FuncSig::UnitUnit),
                    (b"poll",         $crate::FuncSig::U64I32),
                    (b"tx_send",      $crate::FuncSig::U32xU32I32),
                    (b"rx_pop",       $crate::FuncSig::UnitU64),
                    (b"rx_recycle",   $crate::FuncSig::U32I32),
                    (b"socket_create",$crate::FuncSig::U32I32),
                    (b"socket_close", $crate::FuncSig::U32I32),
                    (b"socket_bind",  $crate::FuncSig::U32x3I32),
                    (b"socket_listen",$crate::FuncSig::U32xU32I32),
                ],
                &[
                    // Names match the driver's #[link_name = "..."]
                    // attributes (the WASM-level import name). LTO
                    // strips unused imports — the manifest must list
                    // ONLY what the wasm actually requests, so the
                    // sign-tool's bidirectional check passes. See
                    // NET_MANIFEST_SIZE doc.
                    (b"wari", b"net_mmio_write32", $crate::FuncSig::U32xU32I32),
                    (b"wari", b"net_mmio_read32",  $crate::FuncSig::U32U32),
                    (b"wari", b"nic_set_mac",      $crate::FuncSig::U32xU32I32),
                    (b"wari", b"nic_attach_queue", $crate::FuncSig::U32x5I32),
                    (b"wari", b"nic_queue_notify", $crate::FuncSig::U32I32),
                    (b"wari", b"lin_mem_base",     $crate::FuncSig::UnitU64),
                    (b"wari", b"drv_log_u32",      $crate::FuncSig::U32xU32I32),
                ],
            );
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_16_bytes_packed() {
        // Sanity: the wire layout assumption the parser relies on.
        assert_eq!(core::mem::size_of::<ManifestHeader>(), 16);
        assert_eq!(core::mem::align_of::<ManifestHeader>(), 1);
    }

    #[test]
    fn export_decl_is_36_bytes_packed() {
        assert_eq!(core::mem::size_of::<ExportDecl>(), 36);
        assert_eq!(core::mem::align_of::<ExportDecl>(), 1);
    }

    #[test]
    fn import_decl_is_52_bytes_packed() {
        assert_eq!(core::mem::size_of::<ImportDecl>(), 52);
        assert_eq!(core::mem::align_of::<ImportDecl>(), 1);
    }

    #[test]
    fn driver_kind_round_trips() {
        for k in [DriverKind::Uart, DriverKind::Net, DriverKind::Block] {
            assert_eq!(DriverKind::from_raw(k as u16), Some(k));
        }
        assert_eq!(DriverKind::from_raw(0), None);
        assert_eq!(DriverKind::from_raw(0xFFFF), None);
    }

    #[test]
    fn func_sig_round_trips() {
        let all = [
            FuncSig::UnitUnit,
            FuncSig::U32xU32I32,
            FuncSig::U32I32,
            FuncSig::U32U32,
            FuncSig::U64I32,
            FuncSig::UnitU64,
            FuncSig::U32x5I32,
        ];
        for s in all {
            assert_eq!(FuncSig::from_raw(s as u8), Some(s));
        }
        assert_eq!(FuncSig::from_raw(0), None);
        assert_eq!(FuncSig::from_raw(0xFF), None);
    }

    #[test]
    fn pad_name_writes_nul_terminator() {
        let n: [u8; 8] = pad_name(b"write");
        assert_eq!(&n[..5], b"write");
        assert_eq!(n[5], 0);
        assert_eq!(n[6], 0);
        assert_eq!(n[7], 0);
    }

    #[test]
    fn build_manifest_uart_round_trip() {
        // Reproduce what the wari_uart_driver! macro emits and
        // verify byte-by-byte that the header is correct + the
        // export/import names land at the right offsets.
        const M: [u8; 192] = build_manifest::<192>(
            DriverKind::Uart,
            &[
                (b"write",  FuncSig::U32xU32I32),
                (b"_start", FuncSig::UnitUnit),
            ],
            &[
                (b"wari", b"wari_mmio_write8", FuncSig::U32xU32I32),
                (b"wari", b"wari_mmio_read8",  FuncSig::U32U32),
            ],
        );
        // Header
        assert_eq!(&M[0..4], b"WDM\0");
        assert_eq!(u16::from_le_bytes([M[4], M[5]]), MANIFEST_ABI_VERSION);
        assert_eq!(u16::from_le_bytes([M[6], M[7]]), DriverKind::Uart as u16);
        assert_eq!(u16::from_le_bytes([M[8], M[9]]), 2);
        assert_eq!(u16::from_le_bytes([M[10], M[11]]), 2);
        // First export = "write", sig U32xU32I32
        let off = 16;
        assert_eq!(&M[off..off + 5], b"write");
        assert_eq!(M[off + 5], 0); // NUL pad
        assert_eq!(M[off + NAME_MAX], FuncSig::U32xU32I32 as u8);
        // Second export = "_start"
        let off = 16 + 36;
        assert_eq!(&M[off..off + 6], b"_start");
        assert_eq!(M[off + NAME_MAX], FuncSig::UnitUnit as u8);
        // First import: module "wari", name "wari_mmio_write8"
        let off = 16 + 2 * 36;
        assert_eq!(&M[off..off + 4], b"wari");
        assert_eq!(M[off + 4], 0);
        assert_eq!(&M[off + MODULE_MAX..off + MODULE_MAX + 16], b"wari_mmio_write8");
        assert_eq!(M[off + MODULE_MAX + NAME_MAX], FuncSig::U32xU32I32 as u8);
    }

    #[test]
    fn worked_uart_manifest_size() {
        // Per design doc §3.5: UART = 16 + 2*36 + 2*52 = 192.
        let h = core::mem::size_of::<ManifestHeader>();
        let e = core::mem::size_of::<ExportDecl>();
        let i = core::mem::size_of::<ImportDecl>();
        assert_eq!(h + 2 * e + 2 * i, 192);
    }
}
