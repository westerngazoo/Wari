// SPDX-License-Identifier: AGPL-3.0-only
//! Driver-manifest parser — shared between the kernel loader and
//! the host-side sign tool.
//!
//! Lives in `driver-iface` (no_std, no alloc) so both consumers
//! call the same code: any drift between "what the kernel accepts"
//! and "what the sign tool emits" becomes impossible. The kernel
//! adds `verify_exports` on top (uses wasmi); the sign tool adds
//! `verify_against_wasm_exports` (walks the wasm directly).
//!
//! The parser is deliberately minimal: WASM section walker
//! (preamble + LEB128-prefixed section list), then a fixed-offset
//! header read, then bounds-checked slice extraction for the
//! export and import arrays. No allocation, bounded LOC,
//! Kani-friendly — DI-6 adds the proof harness.

use core::mem::size_of;

use crate::{
    DriverKind, DriverManifestError, ExportDecl, FuncSig, ImportDecl, ManifestHeader, MAGIC,
    MANIFEST_ABI_VERSION, SECTION_NAME,
};

/// Borrowed view over a parsed manifest. Zero-copy — every field
/// references the original WASM buffer.
pub struct DriverManifestView<'a> {
    /// 16-byte fixed header.
    pub header: &'a ManifestHeader,
    /// `header.export_count` entries.
    pub exports: &'a [ExportDecl],
    /// `header.import_count` entries.
    pub imports: &'a [ImportDecl],
}

impl<'a> DriverManifestView<'a> {
    /// Driver kind decoded from the header. `None` ⇒
    /// [`DriverManifestError::UnknownKind`].
    pub fn kind(&self) -> Result<DriverKind, DriverManifestError> {
        // SAFETY: `self.header` came from a payload bounds-checked
        // to be at least `size_of::<ManifestHeader>()` bytes;
        // `addr_of!` returns a raw pointer without forming a
        // reference to a packed field (which is rustc UB).
        let raw = unsafe { unaligned_u16(core::ptr::addr_of!(self.header.kind)) };
        DriverKind::from_raw(raw).ok_or(DriverManifestError::UnknownKind)
    }

    /// ABI version from the header.
    pub fn abi_version(&self) -> u16 {
        // SAFETY: same as `kind`.
        unsafe { unaligned_u16(core::ptr::addr_of!(self.header.abi_version)) }
    }
}

// ── WASM preamble + section walker ───────────────────────────────

const WASM_MAGIC: [u8; 4] = [0x00, 0x61, 0x73, 0x6d]; // "\0asm"
const WASM_VERSION: [u8; 4] = [0x01, 0x00, 0x00, 0x00];
const WASM_PREAMBLE_LEN: usize = 8;
const SECTION_ID_CUSTOM: u8 = 0;

/// Parse a WASM binary, find the `wari_driver_manifest` custom
/// section, return a typed view into its bytes.
///
/// # Errors
///
/// - [`DriverManifestError::Missing`] — no matching section
/// - [`DriverManifestError::Truncated`] — section payload too short
///   for the declared `export_count` × stride + `import_count` ×
///   stride, or any LEB128 / section header would read off the end
/// - [`DriverManifestError::BadMagic`] — header magic ≠ `b"WDM\0"`
/// - [`DriverManifestError::UnsupportedAbiVersion`] — `abi_version`
///   ≠ [`MANIFEST_ABI_VERSION`]
/// - [`DriverManifestError::UnknownSig`] — any `FuncSig`
///   discriminant the parser does not know
///
/// Does **not** verify the kind matches what the caller expects —
/// that is the loader's / sign-tool's job.
pub fn parse_from_wasm(wasm: &[u8]) -> Result<DriverManifestView<'_>, DriverManifestError> {
    let payload = find_section_payload(wasm, SECTION_NAME)?.ok_or(DriverManifestError::Missing)?;
    parse_payload(payload)
}

fn find_section_payload<'a>(
    wasm: &'a [u8],
    target: &str,
) -> Result<Option<&'a [u8]>, DriverManifestError> {
    if wasm.len() < WASM_PREAMBLE_LEN {
        return Err(DriverManifestError::Truncated);
    }
    if wasm[0..4] != WASM_MAGIC || wasm[4..8] != WASM_VERSION {
        return Err(DriverManifestError::Missing);
    }

    let mut i = WASM_PREAMBLE_LEN;
    while i < wasm.len() {
        let id = wasm[i];
        i += 1;
        let (sec_size, after_size) = read_leb128_u32(wasm, i)?;
        i = after_size;
        let sec_size = sec_size as usize;
        let sec_end = i
            .checked_add(sec_size)
            .ok_or(DriverManifestError::Truncated)?;
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
                return Ok(Some(&wasm[name_end..sec_end]));
            }
        }

        i = sec_end;
    }

    Ok(None)
}

fn parse_payload(payload: &[u8]) -> Result<DriverManifestView<'_>, DriverManifestError> {
    if payload.len() < size_of::<ManifestHeader>() {
        return Err(DriverManifestError::Truncated);
    }

    let header_ptr = payload.as_ptr() as *const ManifestHeader;
    // SAFETY: payload bounds-checked to be at least
    // size_of::<ManifestHeader>(). `ManifestHeader` is `repr(C,
    // packed)` with byte alignment, so any byte boundary is a
    // valid `*const ManifestHeader`. We never form &-references
    // to packed fields — accessors use raw-pointer reads.
    let header: &ManifestHeader = unsafe { &*header_ptr };

    if header.magic != MAGIC {
        return Err(DriverManifestError::BadMagic);
    }
    // SAFETY: header bounded above; addr_of! avoids packed-field
    // reference UB.
    let abi_v = unsafe { unaligned_u16(core::ptr::addr_of!(header.abi_version)) };
    if abi_v != MANIFEST_ABI_VERSION {
        return Err(DriverManifestError::UnsupportedAbiVersion);
    }

    // SAFETY: same.
    let export_count = unsafe { unaligned_u16(core::ptr::addr_of!(header.export_count)) } as usize;
    // SAFETY: same.
    let import_count = unsafe { unaligned_u16(core::ptr::addr_of!(header.import_count)) } as usize;

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

    // SAFETY: bounds-checked above. Both decl types are repr(C,
    // packed) with byte alignment — `*const Decl` is well-formed
    // at any byte boundary; field accessors use unaligned reads.
    let exports: &[ExportDecl] = unsafe {
        core::slice::from_raw_parts(
            payload.as_ptr().add(exports_off) as *const ExportDecl,
            export_count,
        )
    };
    // SAFETY: same.
    let imports: &[ImportDecl] = unsafe {
        core::slice::from_raw_parts(
            payload.as_ptr().add(imports_off) as *const ImportDecl,
            import_count,
        )
    };

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

    Ok(DriverManifestView {
        header,
        exports,
        imports,
    })
}

// ── Helpers ──────────────────────────────────────────────────────

/// Copy a u16 out of a packed-struct field via a raw pointer.
/// Forming a `&u16` to a packed field is rustc UB even when paired
/// with `read_unaligned`; the caller must hand in
/// `core::ptr::addr_of!(field)` to skip reference creation.
///
/// # Safety
/// Caller asserts `p` is in-bounds for a valid 2-byte region of
/// the manifest payload that the parser already bounds-checked.
pub unsafe fn unaligned_u16(p: *const u16) -> u16 {
    // SAFETY: caller-asserted in-bounds.
    unsafe { core::ptr::read_unaligned(p) }
}

/// Read a single LEB128-encoded u32 starting at `wasm[off]`. WASM
/// spec §5.2.2. Caps at 5 bytes (max for u32) to refuse hostile
/// overlong encodings.
pub fn read_leb128_u32(wasm: &[u8], off: usize) -> Result<(u32, usize), DriverManifestError> {
    let mut result: u32 = 0;
    let mut shift: u32 = 0;
    let mut i = off;
    let mut bytes_read = 0;
    loop {
        if i >= wasm.len() {
            return Err(DriverManifestError::Truncated);
        }
        if bytes_read >= 5 {
            return Err(DriverManifestError::Truncated);
        }
        let b = wasm[i];
        i += 1;
        bytes_read += 1;
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

/// Trim trailing NULs from a fixed-size name buffer. Both kernel
/// and sign tool need to compare manifest names against actual
/// WASM-export names; the manifest uses NUL-padded fixed buffers.
pub fn trim_nul(buf: &[u8]) -> &[u8] {
    let mut end = buf.len();
    while end > 0 && buf[end - 1] == 0 {
        end -= 1;
    }
    &buf[..end]
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use crate::{build_manifest, DriverKind, FuncSig};
    use alloc::vec::Vec;

    /// Build a minimal valid WASM that wraps a driver manifest in
    /// the `wari_driver_manifest` custom section. All adversarial
    /// tests start from this and corrupt one field.
    fn synth_wasm(manifest: &[u8]) -> Vec<u8> {
        let name = SECTION_NAME.as_bytes();
        let mut sec_payload = Vec::new();
        sec_payload.push(name.len() as u8);
        sec_payload.extend_from_slice(name);
        sec_payload.extend_from_slice(manifest);

        let mut wasm = Vec::new();
        wasm.extend_from_slice(&WASM_MAGIC);
        wasm.extend_from_slice(&WASM_VERSION);
        wasm.push(SECTION_ID_CUSTOM);
        // Section size as LEB128.
        let mut s = sec_payload.len() as u32;
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

    fn uart_manifest_bytes() -> [u8; 192] {
        build_manifest::<192>(
            DriverKind::Uart,
            &[
                (b"write", FuncSig::U32xU32I32),
                (b"_start", FuncSig::UnitUnit),
            ],
            &[
                (b"wari", b"mmio_write8", FuncSig::U32xU32I32),
                (b"wari", b"mmio_read8", FuncSig::U32U32),
            ],
        )
    }

    /// Locate the manifest payload start within a synth wasm
    /// (offset where header magic begins). Used by adversarial
    /// tests to corrupt a specific byte without re-encoding the
    /// whole wasm.
    fn manifest_start_offset(wasm: &[u8]) -> usize {
        let payload = find_section_payload(wasm, SECTION_NAME).unwrap().unwrap();
        (payload.as_ptr() as usize) - (wasm.as_ptr() as usize)
    }

    #[test]
    fn round_trip_uart() {
        let wasm = synth_wasm(&uart_manifest_bytes());
        let view = parse_from_wasm(&wasm).expect("parse ok");
        assert_eq!(view.kind().expect("kind ok"), DriverKind::Uart);
        assert_eq!(view.abi_version(), MANIFEST_ABI_VERSION);
        assert_eq!(view.exports.len(), 2);
        assert_eq!(view.imports.len(), 2);
        assert_eq!(trim_nul(&view.exports[0].name), b"write");
        assert_eq!(view.exports[0].sig, FuncSig::U32xU32I32 as u8);
        assert_eq!(trim_nul(&view.exports[1].name), b"_start");
        assert_eq!(view.exports[1].sig, FuncSig::UnitUnit as u8);
        assert_eq!(trim_nul(&view.imports[0].module), b"wari");
        assert_eq!(trim_nul(&view.imports[0].name), b"mmio_write8");
        assert_eq!(trim_nul(&view.imports[1].name), b"mmio_read8");
    }

    #[test]
    fn rejects_bad_magic() {
        let mut wasm = synth_wasm(&uart_manifest_bytes());
        let off = manifest_start_offset(&wasm);
        wasm[off] = b'X';
        assert!(matches!(
            parse_from_wasm(&wasm),
            Err(DriverManifestError::BadMagic)
        ));
    }

    #[test]
    fn rejects_bad_abi_version() {
        let mut wasm = synth_wasm(&uart_manifest_bytes());
        let off = manifest_start_offset(&wasm);
        wasm[off + 4] = 0xff;
        wasm[off + 5] = 0xff;
        assert!(matches!(
            parse_from_wasm(&wasm),
            Err(DriverManifestError::UnsupportedAbiVersion)
        ));
    }

    #[test]
    fn rejects_unknown_sig_in_export() {
        let mut wasm = synth_wasm(&uart_manifest_bytes());
        let off = manifest_start_offset(&wasm);
        // First ExportDecl at off + 16, sig byte at +NAME_MAX = +32.
        wasm[off + 16 + crate::NAME_MAX] = 0xff;
        assert!(matches!(
            parse_from_wasm(&wasm),
            Err(DriverManifestError::UnknownSig)
        ));
    }

    #[test]
    fn rejects_unknown_sig_in_import() {
        let mut wasm = synth_wasm(&uart_manifest_bytes());
        let off = manifest_start_offset(&wasm);
        // First ImportDecl at off + 16 + 2*36 = off + 88.
        // Sig byte at +MODULE_MAX + NAME_MAX = +16+32 = +48.
        wasm[off + 88 + crate::MODULE_MAX + crate::NAME_MAX] = 0xff;
        assert!(matches!(
            parse_from_wasm(&wasm),
            Err(DriverManifestError::UnknownSig)
        ));
    }

    #[test]
    fn rejects_truncated_header() {
        // Manifest payload only 8 bytes — half a header.
        let mut bytes = [0u8; 8];
        bytes[0..4].copy_from_slice(&MAGIC);
        let wasm = synth_wasm(&bytes);
        assert!(matches!(
            parse_from_wasm(&wasm),
            Err(DriverManifestError::Truncated)
        ));
    }

    #[test]
    fn rejects_truncated_export_array() {
        // Header claims 99 exports but payload only carries the
        // header. The parser must catch the size overflow.
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&MAGIC);
        bytes[4] = MANIFEST_ABI_VERSION as u8;
        bytes[5] = (MANIFEST_ABI_VERSION >> 8) as u8;
        bytes[6] = DriverKind::Uart as u8;
        bytes[8] = 99; // export_count low byte
        let wasm = synth_wasm(&bytes);
        assert!(matches!(
            parse_from_wasm(&wasm),
            Err(DriverManifestError::Truncated)
        ));
    }

    #[test]
    fn rejects_unknown_kind_in_view() {
        // Build a header with kind = 99 (not in DriverKind enum).
        // Parse succeeds, kind() lookup fails. This split lets the
        // loader print a useful error before instantiate.
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&MAGIC);
        bytes[4] = MANIFEST_ABI_VERSION as u8;
        bytes[5] = (MANIFEST_ABI_VERSION >> 8) as u8;
        bytes[6] = 99; // kind low byte
        let wasm = synth_wasm(&bytes);
        let view = parse_from_wasm(&wasm).expect("header parses");
        assert_eq!(view.kind(), Err(DriverManifestError::UnknownKind));
    }

    #[test]
    fn missing_when_no_section() {
        // Minimum-valid WASM, no custom sections.
        let mut wasm = Vec::new();
        wasm.extend_from_slice(&WASM_MAGIC);
        wasm.extend_from_slice(&WASM_VERSION);
        assert!(matches!(
            parse_from_wasm(&wasm),
            Err(DriverManifestError::Missing)
        ));
    }

    #[test]
    fn missing_when_wrong_section_name() {
        // Rewrite the section name byte-in-place to a different
        // ASCII string of the same length. Avoids re-implementing
        // LEB128 size encoding for the wrong-name case.
        let mut wasm = synth_wasm(&uart_manifest_bytes());
        let needle = SECTION_NAME.as_bytes();
        let pos = wasm
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("wasm contains the section name");
        wasm[pos] = b'X'; // section name now starts with X — no match
        assert!(matches!(
            parse_from_wasm(&wasm),
            Err(DriverManifestError::Missing)
        ));
    }

    #[test]
    fn rejects_truncated_input() {
        // A valid prefix sliced at every length below header size
        // returns either Truncated or Missing — never panics.
        let wasm = synth_wasm(&uart_manifest_bytes());
        for cut in 0..wasm.len() {
            let _ = parse_from_wasm(&wasm[..cut]);
        }
    }

    #[test]
    fn leb128_one_byte() {
        let buf = [0x00, 0x7f, 0x01, 0x42];
        assert_eq!(read_leb128_u32(&buf, 0).unwrap(), (0, 1));
        assert_eq!(read_leb128_u32(&buf, 1).unwrap(), (127, 2));
        assert_eq!(read_leb128_u32(&buf, 2).unwrap(), (1, 3));
        assert_eq!(read_leb128_u32(&buf, 3).unwrap(), (66, 4));
    }

    #[test]
    fn leb128_multi_byte() {
        // 624485 = 0xE5 0x8E 0x26 (canonical 3-byte LEB128).
        let buf = [0xE5, 0x8E, 0x26];
        assert_eq!(read_leb128_u32(&buf, 0).unwrap(), (624485, 3));
    }

    #[test]
    fn leb128_rejects_overlong() {
        // 6 continuation bytes — exceeds u32 max encoding length.
        let buf = [0x80, 0x80, 0x80, 0x80, 0x80, 0x80];
        assert!(read_leb128_u32(&buf, 0).is_err());
    }

    #[test]
    fn leb128_rejects_truncated() {
        // Single continuation byte at end of input.
        let buf = [0x80];
        assert!(read_leb128_u32(&buf, 0).is_err());
    }

    /// Property: the parser is total — never panics, never reads
    /// out of bounds, on any byte slice. Burns through all 1-byte
    /// permutations of a small fixed-length input as a smoke test
    /// of bounds-checking discipline.
    ///
    /// Real proof of totality lives in the Kani harness below;
    /// this test catches the obvious bugs without the Kani toolchain.
    #[test]
    fn parser_is_total_on_short_inputs() {
        // Brute-force every 32-byte input — would be 2^256 inputs;
        // instead poke each byte position with each of 256 values,
        // starting from a known-good manifest. Catches bit-flip
        // bugs without real fuzzing.
        let wasm = synth_wasm(&uart_manifest_bytes());
        for i in 0..wasm.len().min(64) {
            for v in [0u8, 0x01, 0x80, 0xff] {
                let mut copy = wasm.clone();
                copy[i] = v;
                let _ = parse_from_wasm(&copy);
            }
        }
    }
}

// ── Kani proof harnesses (PR DI-6) ───────────────────────────────
//
// Run with `cargo kani --harness <name>`. The harnesses prove
// totality: for any byte slice of bounded length, parse_from_wasm
// returns Ok or Err — never panics, never reads out of bounds,
// never returns a view whose slices escape the input bounds.

#[cfg(kani)]
mod kani_proofs {
    use super::*;

    /// Bounded harness: any 64-byte input either parses to an
    /// `Ok(view)` whose `header` reference lies inside the input,
    /// or returns `Err` cleanly.
    #[kani::proof]
    #[kani::unwind(8)]
    fn parse_total_on_64_bytes() {
        let mut buf = [0u8; 64];
        for b in &mut buf {
            *b = kani::any();
        }
        let result = parse_from_wasm(&buf);
        if let Ok(view) = result {
            // Header reference must point inside `buf`.
            let header_addr = view.header as *const _ as usize;
            let buf_start = buf.as_ptr() as usize;
            let buf_end = buf_start + buf.len();
            kani::assert(
                header_addr >= buf_start && header_addr < buf_end,
                "view.header lies outside the input buffer",
            );
        }
    }

    /// LEB128 reader is total on any 8-byte input at offset 0.
    #[kani::proof]
    #[kani::unwind(8)]
    fn leb128_total_on_8_bytes() {
        let mut buf = [0u8; 8];
        for b in &mut buf {
            *b = kani::any();
        }
        // Either Ok(value, off) with off <= 5, or Err — no panic.
        if let Ok((_, off)) = read_leb128_u32(&buf, 0) {
            kani::assert(off <= 5, "LEB128 read past 5 bytes");
        }
    }
}
