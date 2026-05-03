// SPDX-License-Identifier: AGPL-3.0-only
//! Kernel-side driver-manifest parser.
//!
//! Walks a Tier-2 driver's WASM binary, finds the
//! `wari_driver_manifest` custom section, and returns typed
//! references into its bytes — without allocation, without copying,
//! and with bounded LOC so a Kani harness can prove safety.
//!
//! The parser is deliberately minimal: WASM section walker (preamble
//! + LEB128-prefixed section list), then a fixed-offset header read,
//! then bounds-checked slice extraction for the export and import
//! arrays. No wasmi here — this runs *before* `Module::new` so
//! the kernel can refuse to even parse a binary whose declared kind
//! does not match the slot it is being loaded into.
//!
//! See `docs/driver-interface-design.md` §5 for the design.

use core::mem::size_of;
use wari_driver_iface::{
    DriverKind, DriverManifestError, ExportDecl, FuncSig, ImportDecl,
    ManifestHeader, MAGIC, MANIFEST_ABI_VERSION, SECTION_NAME,
};

/// Borrowed view over a parsed manifest. The kernel never copies
/// the bytes; everything here references the original WASM buffer.
pub struct DriverManifestView<'a> {
    /// 16-byte fixed header.
    pub header: &'a ManifestHeader,
    /// `header.export_count` entries.
    pub exports: &'a [ExportDecl],
    /// `header.import_count` entries.
    pub imports: &'a [ImportDecl],
}

impl<'a> DriverManifestView<'a> {
    /// Driver kind decoded from the header. Returns
    /// [`DriverManifestError::UnknownKind`] if the discriminant
    /// is not in the supported set.
    pub fn kind(&self) -> Result<DriverKind, DriverManifestError> {
        // SAFETY: `self.header` was obtained from a payload of at
        // least `size_of::<ManifestHeader>()` bytes (parse_payload
        // bounds-checks). `addr_of!` skips reference creation —
        // required for packed-field reads.
        let raw = unsafe { unaligned_u16(core::ptr::addr_of!(self.header.kind)) };
        DriverKind::from_raw(raw).ok_or(DriverManifestError::UnknownKind)
    }
}

// ── WASM preamble + section walker ───────────────────────────────

const WASM_MAGIC: [u8; 4] = [0x00, 0x61, 0x73, 0x6d]; // "\0asm"
const WASM_VERSION: [u8; 4] = [0x01, 0x00, 0x00, 0x00];
const WASM_PREAMBLE_LEN: usize = 8;
const SECTION_ID_CUSTOM: u8 = 0;

/// Parse a WASM binary, find the `wari_driver_manifest` custom
/// section, and return a typed view into its bytes.
///
/// # Errors
///
/// - [`DriverManifestError::Missing`] — no matching section
/// - [`DriverManifestError::Truncated`] — section payload too short
///   for the declared `export_count` × stride + `import_count` ×
///   stride, or any LEB128 / section header would read off the end
/// - [`DriverManifestError::BadMagic`] — header magic ≠ `b"WDM\0"`
/// - [`DriverManifestError::UnsupportedAbiVersion`] —
///   `abi_version` ≠ [`MANIFEST_ABI_VERSION`]
/// - [`DriverManifestError::UnknownSig`] — any export or import
///   carries a [`FuncSig`] discriminant the kernel does not know
///
/// Does **not** verify the kind matches what the kernel expects —
/// that is the loader's job (so the same parser serves both the
/// load path and any future general-purpose tooling).
pub fn parse_from_wasm(wasm: &[u8]) -> Result<DriverManifestView<'_>, DriverManifestError> {
    let payload = find_section_payload(wasm, SECTION_NAME)?
        .ok_or(DriverManifestError::Missing)?;
    parse_payload(payload)
}

/// Walk the WASM section table, return the payload bytes of the
/// first custom section whose name matches `target`. `Ok(None)`
/// means "valid WASM, no such section". `Err(...)` means the WASM
/// itself is structurally truncated.
fn find_section_payload<'a>(
    wasm: &'a [u8],
    target: &str,
) -> Result<Option<&'a [u8]>, DriverManifestError> {
    if wasm.len() < WASM_PREAMBLE_LEN {
        return Err(DriverManifestError::Truncated);
    }
    if wasm[0..4] != WASM_MAGIC || wasm[4..8] != WASM_VERSION {
        // Not a WASM v1 binary — not strictly a manifest error but
        // there is no manifest to find here.
        return Err(DriverManifestError::Missing);
    }

    let mut i = WASM_PREAMBLE_LEN;
    while i < wasm.len() {
        let id = wasm[i];
        i += 1;
        let (sec_size, after_size) = read_leb128_u32(wasm, i)?;
        i = after_size;
        let sec_size = sec_size as usize;
        let sec_end = i.checked_add(sec_size).ok_or(DriverManifestError::Truncated)?;
        if sec_end > wasm.len() {
            return Err(DriverManifestError::Truncated);
        }

        if id == SECTION_ID_CUSTOM {
            let (name_len, after_name_len) = read_leb128_u32(wasm, i)?;
            let name_len = name_len as usize;
            let name_end = after_name_len
                .checked_add(name_len)
                .ok_or(DriverManifestError::Truncated)?;
            if name_end > sec_end {
                return Err(DriverManifestError::Truncated);
            }
            let name_bytes = &wasm[after_name_len..name_end];
            if name_bytes == target.as_bytes() {
                let payload_start = name_end;
                let payload = &wasm[payload_start..sec_end];
                return Ok(Some(payload));
            }
        }

        i = sec_end;
    }

    Ok(None)
}

/// Decode the manifest payload bytes into a typed view. Bounds-
/// checks every read; refuses on size overflow.
fn parse_payload(payload: &[u8]) -> Result<DriverManifestView<'_>, DriverManifestError> {
    if payload.len() < size_of::<ManifestHeader>() {
        return Err(DriverManifestError::Truncated);
    }

    // SAFETY: payload is at least `size_of::<ManifestHeader>()`
    // bytes by the check above. `ManifestHeader` is `repr(C, packed)`
    // with byte alignment, so any aligned-or-not pointer into a u8
    // slice is a valid `*const ManifestHeader`. We never construct
    // a reference to a packed field directly — accessors copy out
    // via `unaligned_u16` / `unaligned_u32`.
    let header_ptr = payload.as_ptr() as *const ManifestHeader;
    let header: &ManifestHeader = unsafe { &*header_ptr };

    if header.magic != MAGIC {
        return Err(DriverManifestError::BadMagic);
    }
    // SAFETY: `header` references the parsed payload (≥ 16 bytes,
    // bounds-checked above); `addr_of!` returns a raw pointer
    // without taking a reference to a packed field — required to
    // avoid UB. Reads via `read_unaligned`.
    let abi_v = unsafe { unaligned_u16(core::ptr::addr_of!(header.abi_version)) };
    if abi_v != MANIFEST_ABI_VERSION {
        return Err(DriverManifestError::UnsupportedAbiVersion);
    }

    // SAFETY: same as above for the count fields.
    let export_count =
        unsafe { unaligned_u16(core::ptr::addr_of!(header.export_count)) } as usize;
    let import_count =
        unsafe { unaligned_u16(core::ptr::addr_of!(header.import_count)) } as usize;

    let exports_off = size_of::<ManifestHeader>();
    let exports_size = export_count
        .checked_mul(size_of::<ExportDecl>())
        .ok_or(DriverManifestError::Truncated)?;
    let imports_off = exports_off
        .checked_add(exports_size)
        .ok_or(DriverManifestError::Truncated)?;
    let imports_size = import_count
        .checked_mul(size_of::<ImportDecl>())
        .ok_or(DriverManifestError::Truncated)?;
    let total = imports_off
        .checked_add(imports_size)
        .ok_or(DriverManifestError::Truncated)?;

    if total > payload.len() {
        return Err(DriverManifestError::Truncated);
    }

    // SAFETY: we just bounds-checked that
    //   `exports_off + export_count * size_of::<ExportDecl>() <= payload.len()`
    // and similarly for imports. ExportDecl/ImportDecl are
    // `repr(C, packed)` with byte alignment, so a `*const _` cast
    // from any byte boundary is well-formed. No reads occur until
    // the caller indexes; once they do, every field access goes
    // through the same unaligned-safe pattern as the header.
    let exports: &[ExportDecl] = unsafe {
        core::slice::from_raw_parts(
            payload.as_ptr().add(exports_off) as *const ExportDecl,
            export_count,
        )
    };
    // SAFETY: same as above, for the import slice.
    let imports: &[ImportDecl] = unsafe {
        core::slice::from_raw_parts(
            payload.as_ptr().add(imports_off) as *const ImportDecl,
            import_count,
        )
    };

    // Validate every signature discriminant up front, so the
    // loader can call FuncSig::from_raw without re-checking.
    for e in exports {
        if FuncSig::from_raw(e.sig).is_none() {
            return Err(DriverManifestError::UnknownSig);
        }
    }
    for im in imports {
        if FuncSig::from_raw(im.sig).is_none() {
            return Err(DriverManifestError::UnknownSig);
        }
    }

    Ok(DriverManifestView { header, exports, imports })
}

// ── Loader-side verification ─────────────────────────────────────

/// Resolve every export the manifest declares against the live
/// wasmi instance, asserting the declared signature matches what
/// wasmi typed it as. Called by the loader after instantiate but
/// before any export is actually invoked. Catches the residual
/// case the sign tool would normally have caught: manifest claims
/// `write: (u32,u32) -> i32` but the binary actually exposes
/// `write: (u32) -> i32`.
///
/// Returns the matching [`KernelError`] on mismatch — propagated
/// up to the loader, which surfaces it as the `[net] virtio init
/// failed`-style log line the operator already knows how to read.
pub fn verify_exports<S>(
    view: &DriverManifestView<'_>,
    instance: &wasmi::Instance,
    store: &S,
) -> Result<(), crate::error::KernelError>
where
    S: wasmi::AsContext,
{
    use crate::error::KernelError;

    for export in view.exports {
        let name = trim_nul(&export.name);
        // SAFETY check on FuncSig already happened during parse.
        let sig = FuncSig::from_raw(export.sig)
            .ok_or(KernelError::DriverManifestMalformed)?;
        if !export_matches(instance, store, name, sig) {
            return Err(KernelError::DriverBadExport);
        }
    }
    Ok(())
}

/// Return true iff `instance` exports a function named `name`
/// with the WASM type matching `sig`. Uses `wasmi::Instance`'s
/// typed-export resolution: success means wasmi already type-
/// checked the signature.
fn export_matches<S>(
    instance: &wasmi::Instance,
    store: &S,
    name: &[u8],
    sig: FuncSig,
) -> bool
where
    S: wasmi::AsContext,
{
    // Convert NUL-trimmed bytes into &str. ASCII-only manifest
    // names — non-UTF-8 here means a corrupted manifest, treat as
    // no-match so the caller surfaces DriverBadExport.
    let Ok(name_str) = core::str::from_utf8(name) else {
        return false;
    };
    match sig {
        FuncSig::UnitUnit => instance
            .get_typed_func::<(), ()>(store, name_str)
            .is_ok(),
        FuncSig::U32xU32I32 => instance
            .get_typed_func::<(u32, u32), i32>(store, name_str)
            .is_ok(),
        FuncSig::U32I32 => instance
            .get_typed_func::<u32, i32>(store, name_str)
            .is_ok(),
        FuncSig::U32U32 => instance
            .get_typed_func::<u32, u32>(store, name_str)
            .is_ok(),
        FuncSig::U64I32 => instance
            .get_typed_func::<u64, i32>(store, name_str)
            .is_ok(),
        FuncSig::UnitU64 => instance
            .get_typed_func::<(), u64>(store, name_str)
            .is_ok(),
        FuncSig::U32x5I32 => instance
            .get_typed_func::<(u32, u32, u32, u32, u32), i32>(
                store, name_str,
            )
            .is_ok(),
    }
}

/// Trim trailing NULs from a fixed-size name buffer. Manifest
/// stores names NUL-padded; the rest of the kernel wants a tight
/// slice for string comparison.
fn trim_nul(buf: &[u8]) -> &[u8] {
    let mut end = buf.len();
    while end > 0 && buf[end - 1] == 0 {
        end -= 1;
    }
    &buf[..end]
}

// ── Helpers ──────────────────────────────────────────────────────

/// Copy a u16 out of a packed struct field via a raw pointer.
/// Forming a `&u16` to a packed field is UB even if you then
/// `read_unaligned` it (rustc enforces this); the caller must
/// hand in `core::ptr::addr_of!(field)` to skip reference
/// creation entirely. Read-only.
unsafe fn unaligned_u16(p: *const u16) -> u16 {
    // SAFETY: caller asserts `p` is in-bounds for the manifest
    // payload (header fields, already bounds-checked at parse
    // time). `read_unaligned` is the documented escape hatch for
    // accessing packed-struct fields without alignment guarantees.
    unsafe { core::ptr::read_unaligned(p) }
}

/// Read a single LEB128-encoded u32 starting at `wasm[off]`. WASM
/// spec §5.2.2. Caps at 5 bytes (max for u32). Returns the value
/// and the byte offset just past the encoding.
fn read_leb128_u32(wasm: &[u8], off: usize) -> Result<(u32, usize), DriverManifestError> {
    let mut result: u32 = 0;
    let mut shift: u32 = 0;
    let mut i = off;
    let mut bytes_read = 0;
    loop {
        if i >= wasm.len() {
            return Err(DriverManifestError::Truncated);
        }
        if bytes_read >= 5 {
            // Overlong / hostile encoding.
            return Err(DriverManifestError::Truncated);
        }
        let b = wasm[i];
        i += 1;
        bytes_read += 1;
        // Each iteration contributes 7 bits to the result. Mask
        // the high bit to extract the data.
        let chunk = (b & 0x7f) as u32;
        result |= chunk
            .checked_shl(shift)
            .ok_or(DriverManifestError::Truncated)?;
        if b & 0x80 == 0 {
            return Ok((result, i));
        }
        shift += 7;
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-built minimal valid WASM: preamble + one custom section
    /// named `wari_driver_manifest` whose payload is a UART-shaped
    /// manifest produced by `build_manifest`.
    fn synth_wasm_with_uart_manifest() -> alloc::vec::Vec<u8> {
        use wari_driver_iface::build_manifest;
        let manifest: [u8; 192] = build_manifest::<192>(
            DriverKind::Uart,
            &[
                (b"write", FuncSig::U32xU32I32),
                (b"_start", FuncSig::UnitUnit),
            ],
            &[
                (b"wari", b"wari_mmio_write8", FuncSig::U32xU32I32),
                (b"wari", b"wari_mmio_read8",  FuncSig::U32U32),
            ],
        );

        // Build the custom section: id=0, then LEB128(size), then
        // LEB128(name_len) + name + payload.
        let name = SECTION_NAME.as_bytes();
        let mut sec_payload = alloc::vec::Vec::new();
        // name_len LEB128
        sec_payload.push(name.len() as u8); // fits in 1 byte for short names
        sec_payload.extend_from_slice(name);
        sec_payload.extend_from_slice(&manifest);

        let mut wasm = alloc::vec::Vec::new();
        wasm.extend_from_slice(&WASM_MAGIC);
        wasm.extend_from_slice(&WASM_VERSION);
        wasm.push(SECTION_ID_CUSTOM);
        // section size LEB128 (assume <128 — 213 bytes total here,
        // so two-byte encoding needed)
        let sz = sec_payload.len() as u32;
        // simple LEB128 encode
        let mut s = sz;
        loop {
            let mut b = (s & 0x7f) as u8;
            s >>= 7;
            if s != 0 {
                b |= 0x80;
            }
            wasm.push(b);
            if s == 0 {
                break;
            }
        }
        wasm.extend_from_slice(&sec_payload);
        wasm
    }

    extern crate alloc;

    #[test]
    fn parse_round_trip_uart() {
        let wasm = synth_wasm_with_uart_manifest();
        let view = parse_from_wasm(&wasm).expect("parse ok");
        assert_eq!(view.kind().expect("kind ok"), DriverKind::Uart);
        assert_eq!(view.exports.len(), 2);
        assert_eq!(view.imports.len(), 2);
        assert_eq!(view.exports[0].sig, FuncSig::U32xU32I32 as u8);
        assert_eq!(view.exports[1].sig, FuncSig::UnitUnit as u8);
        assert_eq!(&view.exports[0].name[..5], b"write");
        assert_eq!(&view.exports[1].name[..6], b"_start");
        assert_eq!(&view.imports[0].name[..16], b"wari_mmio_write8");
        assert_eq!(&view.imports[1].name[..15], b"wari_mmio_read8");
    }

    #[test]
    fn parse_rejects_bad_magic() {
        let mut wasm = synth_wasm_with_uart_manifest();
        // Locate manifest payload start: WASM preamble (8) + sec_id
        // (1) + section_size LEB (1 or 2) + name_len LEB (1) + name
        // (20). Just walk it via the parser's section finder.
        let payload = find_section_payload(&wasm, SECTION_NAME)
            .unwrap()
            .unwrap();
        // payload is &[u8] inside wasm; find its start offset.
        let start = (payload.as_ptr() as usize) - (wasm.as_ptr() as usize);
        wasm[start] = b'X'; // corrupt magic
        assert_eq!(
            parse_from_wasm(&wasm).unwrap_err(),
            DriverManifestError::BadMagic
        );
    }

    #[test]
    fn parse_rejects_bad_abi_version() {
        let mut wasm = synth_wasm_with_uart_manifest();
        let payload = find_section_payload(&wasm, SECTION_NAME)
            .unwrap()
            .unwrap();
        let start = (payload.as_ptr() as usize) - (wasm.as_ptr() as usize);
        wasm[start + 4] = 0xff; // bad abi_version low byte
        wasm[start + 5] = 0xff;
        assert_eq!(
            parse_from_wasm(&wasm).unwrap_err(),
            DriverManifestError::UnsupportedAbiVersion
        );
    }

    #[test]
    fn parse_rejects_truncated() {
        let wasm = synth_wasm_with_uart_manifest();
        // Cut the wasm in half — section walker should hit the end
        // mid-section.
        let half = &wasm[..wasm.len() / 2];
        let err = parse_from_wasm(half).unwrap_err();
        assert!(matches!(
            err,
            DriverManifestError::Truncated | DriverManifestError::Missing
        ));
    }

    #[test]
    fn parse_returns_missing_when_no_section() {
        // Minimum-valid WASM (preamble only), no custom sections.
        let mut wasm = alloc::vec::Vec::new();
        wasm.extend_from_slice(&WASM_MAGIC);
        wasm.extend_from_slice(&WASM_VERSION);
        assert_eq!(
            parse_from_wasm(&wasm).unwrap_err(),
            DriverManifestError::Missing
        );
    }

    #[test]
    fn leb128_decodes_one_byte_values() {
        let buf = [0x00, 0x7f, 0x01, 0x42];
        assert_eq!(read_leb128_u32(&buf, 0).unwrap(), (0, 1));
        assert_eq!(read_leb128_u32(&buf, 1).unwrap(), (127, 2));
        assert_eq!(read_leb128_u32(&buf, 2).unwrap(), (1, 3));
        assert_eq!(read_leb128_u32(&buf, 3).unwrap(), (66, 4));
    }

    #[test]
    fn leb128_decodes_multi_byte_values() {
        // 624485 == 0xE5 0x8E 0x26 in LEB128.
        let buf = [0xE5, 0x8E, 0x26];
        assert_eq!(read_leb128_u32(&buf, 0).unwrap(), (624485, 3));
    }

    #[test]
    fn leb128_rejects_overlong() {
        // 6+ continuation bytes.
        let buf = [0x80, 0x80, 0x80, 0x80, 0x80, 0x80];
        assert!(read_leb128_u32(&buf, 0).is_err());
    }
}
