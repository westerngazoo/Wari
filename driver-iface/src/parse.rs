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
    DriverKind, DriverManifestError, ExportDecl, FuncSig, ImportDecl,
    ManifestHeader, MAGIC, MANIFEST_ABI_VERSION, SECTION_NAME,
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
pub fn parse_from_wasm(
    wasm: &[u8],
) -> Result<DriverManifestView<'_>, DriverManifestError> {
    let payload =
        find_section_payload(wasm, SECTION_NAME)?.ok_or(DriverManifestError::Missing)?;
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
        let sec_end =
            i.checked_add(sec_size).ok_or(DriverManifestError::Truncated)?;
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

    // SAFETY: payload bounds-checked to be at least
    // size_of::<ManifestHeader>(). `ManifestHeader` is `repr(C,
    // packed)` with byte alignment, so any byte boundary is a
    // valid `*const ManifestHeader`. We never form &-references
    // to packed fields — accessors use raw-pointer reads.
    let header_ptr = payload.as_ptr() as *const ManifestHeader;
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
    let export_count = unsafe {
        unaligned_u16(core::ptr::addr_of!(header.export_count))
    } as usize;
    let import_count = unsafe {
        unaligned_u16(core::ptr::addr_of!(header.import_count))
    } as usize;

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

    Ok(DriverManifestView { header, exports, imports })
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
pub fn read_leb128_u32(
    wasm: &[u8],
    off: usize,
) -> Result<(u32, usize), DriverManifestError> {
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
