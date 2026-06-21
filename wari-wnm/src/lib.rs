// SPDX-License-Identifier: AGPL-3.0-only
//! Wari Native Module (WNM) — the AOT-compiled-artifact container format.
//!
//! This crate is the **single source of truth** for the on-disk/in-memory
//! layout of an AOT-compiled Wari module (see
//! `docs/wasm-jit-design.md` — the AOT-over-runtime-JIT execution
//! strategy). A WNM is produced **offline** in the signing pipeline
//! (compile WASM→RISC-V, emit native `.text` + relocations + a
//! sandbox-safety certificate + the original `.wasm`), wrapped in the
//! same signing envelope Tier-2 drivers already use, and consumed by the
//! kernel loader. Two callers by design — the offline toolchain and the
//! kernel — so the format lives in its own pure crate, not in the
//! syscall-ABI crate or the Tier-2-driver crate.
//!
//! This module holds only the **pure, structural** part: the header /
//! section-table layout and a host-testable validator that bounds-checks
//! a candidate container *before* the loader trusts any offset. It does
//! NOT verify the safety certificate (that's the loader's VeriWasm-style
//! check), nor the signature (the envelope's job), nor execute anything.
//! No `unsafe`, no MMIO: pure logic, host-testable.
//!
//! ## Layout
//!
//! ```text
//! 0                     WnmHeader (12 bytes)
//!   [0..4)   magic   = "WNM\0"
//!   [4..6)   abi_version : u16 LE
//!   [6..8)   section_count : u16 LE
//!   [8..12)  total_len : u32 LE      (authoritative container size)
//! 12                    section table : section_count × Entry (12 bytes)
//!   [0]      kind : u8               (see WnmSection)
//!   [1..4)   reserved (must be 0)
//!   [4..8)   offset : u32 LE         (payload start, from container base)
//!   [8..12)  len : u32 LE
//! …                     section payloads at their declared offsets
//! ```
//!
//! Validation order is cheapest-first and short-circuits, so a hostile or
//! truncated buffer is rejected before any section offset is dereferenced.

#![cfg_attr(not(test), no_std)]

/// Magic bytes at offset 0 of a WNM container. Distinguishes an AOT
/// artifact from a raw `.wasm` or a `WDM\0` Tier-2 driver manifest.
pub const WNM_MAGIC: [u8; 4] = *b"WNM\0";

/// WNM container ABI version. Bump on any change to the header layout,
/// the section-entry layout, or the [`WnmSection`] discriminants. The
/// loader refuses a container whose version it does not implement.
pub const WNM_ABI_VERSION: u16 = 1;

/// Size of [`WnmHeader`] on the wire (bytes).
pub const WNM_HEADER_LEN: usize = 12;

/// Size of one section-table entry on the wire (bytes).
pub const WNM_SECTION_ENTRY_LEN: usize = 12;

/// Upper bound on `section_count`. Bounds the table walk and keeps the
/// header region small; far above the handful a real module needs.
pub const WNM_MAX_SECTIONS: usize = 16;

/// Section kinds in a WNM container's section table.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WnmSection {
    /// Native RISC-V `.text`. The loader maps this **RX-only** — never
    /// writable+executable (the W^X invariant the AOT strategy rests on).
    Text = 1,
    /// Relocation entries applied into the per-instance arena at load.
    /// Optional: a fully position-independent module may carry none.
    Relocs = 2,
    /// Sandbox-safety certificate — the VeriWasm-style proof that the
    /// native code provably stays within its linear memory. The loader
    /// checks this to trust the code **without trusting the compiler**.
    SafetyCert = 3,
    /// The original `.wasm`, retained so the loader can re-validate
    /// structural isolation independent of the offline compiler.
    Wasm = 4,
}

impl WnmSection {
    /// Decode a section-kind byte. Returns `None` for an unknown
    /// discriminant (the validator turns that into
    /// [`WnmCheck::UnknownSection`]).
    #[inline]
    pub const fn from_u8(b: u8) -> Option<WnmSection> {
        match b {
            1 => Some(WnmSection::Text),
            2 => Some(WnmSection::Relocs),
            3 => Some(WnmSection::SafetyCert),
            4 => Some(WnmSection::Wasm),
            _ => None,
        }
    }
}

/// Outcome of [`validate_header`]. Distinct rejection reasons (not a bare
/// `bool`) so callers and tests can assert *why* a container was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WnmCheck {
    /// Header + section table valid and every section lies within the
    /// declared container; required sections present.
    Ok,
    /// Buffer smaller than the fixed [`WnmHeader`].
    Truncated,
    /// `magic` != [`WNM_MAGIC`].
    BadMagic,
    /// `abi_version` != [`WNM_ABI_VERSION`].
    BadVersion,
    /// `section_count` > [`WNM_MAX_SECTIONS`].
    TooManySections,
    /// `total_len` exceeds the supplied buffer, or is smaller than the
    /// minimum (header + declared section table).
    BadTotalLen,
    /// Header + section table does not fit within `total_len`.
    SectionTableOverflow,
    /// A section-kind byte is not a known [`WnmSection`].
    UnknownSection,
    /// A section's `[offset, offset+len)` overflows, overlaps the header/
    /// table region, or escapes `total_len`.
    SectionOutOfBounds,
    /// One of the required sections (`Text`, `SafetyCert`, `Wasm`) is
    /// absent.
    MissingRequired,
}

#[inline]
fn rd_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

#[inline]
fn rd_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Validate the structural integrity of a candidate WNM container.
///
/// Pure and allocation-free: checks, cheapest-first, that `bytes` carries
/// a well-formed WNM header + section table whose every section lies
/// wholly inside the declared `total_len`, that no section overlaps the
/// header/table region, and that the required sections are present. It
/// does **not** verify the safety certificate, the signature, or the
/// payload contents — those are later, separate gates.
///
/// Returns [`WnmCheck::Ok`] iff the container is structurally sound, else
/// the first failing check. Safe to call on fully attacker-controlled
/// bytes: every slice access is guarded by a prior length check, and
/// section arithmetic uses checked addition.
///
/// ```
/// use wari_wnm::{validate_header, WnmCheck, WNM_MAGIC};
/// // An empty buffer is truncated, not a panic.
/// assert_eq!(validate_header(&[]), WnmCheck::Truncated);
/// // Wrong magic is rejected before anything else is read.
/// let mut b = [0u8; 12];
/// b[..4].copy_from_slice(b"XXXX");
/// assert_eq!(validate_header(&b), WnmCheck::BadMagic);
/// ```
pub fn validate_header(bytes: &[u8]) -> WnmCheck {
    if bytes.len() < WNM_HEADER_LEN {
        return WnmCheck::Truncated;
    }
    if bytes[0..4] != WNM_MAGIC {
        return WnmCheck::BadMagic;
    }
    if rd_u16(bytes, 4) != WNM_ABI_VERSION {
        return WnmCheck::BadVersion;
    }
    let section_count = rd_u16(bytes, 6) as usize;
    if section_count > WNM_MAX_SECTIONS {
        return WnmCheck::TooManySections;
    }
    let total_len = rd_u32(bytes, 8) as usize;
    // The declared container must fit in the supplied buffer and be at
    // least large enough for the header.
    if total_len > bytes.len() || total_len < WNM_HEADER_LEN {
        return WnmCheck::BadTotalLen;
    }

    // The section table must fit within the declared container. Use
    // checked arithmetic; section_count is already bounded above, so this
    // cannot overflow, but be explicit.
    let table_bytes = match section_count.checked_mul(WNM_SECTION_ENTRY_LEN) {
        Some(n) => n,
        None => return WnmCheck::SectionTableOverflow,
    };
    let table_end = match WNM_HEADER_LEN.checked_add(table_bytes) {
        Some(n) => n,
        None => return WnmCheck::SectionTableOverflow,
    };
    if table_end > total_len {
        return WnmCheck::SectionTableOverflow;
    }

    let mut have_text = false;
    let mut have_cert = false;
    let mut have_wasm = false;

    let mut i = 0;
    while i < section_count {
        let base = WNM_HEADER_LEN + i * WNM_SECTION_ENTRY_LEN;
        let kind_byte = bytes[base];
        let kind = match WnmSection::from_u8(kind_byte) {
            Some(k) => k,
            None => return WnmCheck::UnknownSection,
        };
        let offset = rd_u32(bytes, base + 4) as usize;
        let len = rd_u32(bytes, base + 8) as usize;

        // Payload must start past the header+table and end within the
        // declared container, with no overflow.
        let end = match offset.checked_add(len) {
            Some(e) => e,
            None => return WnmCheck::SectionOutOfBounds,
        };
        if offset < table_end || end > total_len {
            return WnmCheck::SectionOutOfBounds;
        }

        match kind {
            WnmSection::Text => have_text = true,
            WnmSection::SafetyCert => have_cert = true,
            WnmSection::Wasm => have_wasm = true,
            WnmSection::Relocs => {}
        }
        i += 1;
    }

    if !(have_text && have_cert && have_wasm) {
        return WnmCheck::MissingRequired;
    }
    WnmCheck::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a WNM container from `(kind, payload_len)` sections, laying
    /// payloads back-to-back after the section table. Returns the bytes.
    fn build(sections: &[(u8, u32)]) -> Vec<u8> {
        let count = sections.len();
        let table_end = WNM_HEADER_LEN + count * WNM_SECTION_ENTRY_LEN;
        // Lay out payloads.
        let mut offsets = Vec::new();
        let mut cursor = table_end;
        for (_, len) in sections {
            offsets.push(cursor);
            cursor += *len as usize;
        }
        let total = cursor;
        let mut b = vec![0u8; total];
        b[0..4].copy_from_slice(&WNM_MAGIC);
        b[4..6].copy_from_slice(&WNM_ABI_VERSION.to_le_bytes());
        b[6..8].copy_from_slice(&(count as u16).to_le_bytes());
        b[8..12].copy_from_slice(&(total as u32).to_le_bytes());
        for (i, (kind, len)) in sections.iter().enumerate() {
            let base = WNM_HEADER_LEN + i * WNM_SECTION_ENTRY_LEN;
            b[base] = *kind;
            b[base + 4..base + 8].copy_from_slice(&(offsets[i] as u32).to_le_bytes());
            b[base + 8..base + 12].copy_from_slice(&len.to_le_bytes());
        }
        b
    }

    const TEXT: u8 = WnmSection::Text as u8;
    const RELOCS: u8 = WnmSection::Relocs as u8;
    const CERT: u8 = WnmSection::SafetyCert as u8;
    const WASM: u8 = WnmSection::Wasm as u8;

    #[test]
    fn minimal_valid_container() {
        let b = build(&[(TEXT, 16), (CERT, 8), (WASM, 32)]);
        assert_eq!(validate_header(&b), WnmCheck::Ok);
    }

    #[test]
    fn valid_with_optional_relocs() {
        let b = build(&[(TEXT, 16), (RELOCS, 8), (CERT, 8), (WASM, 32)]);
        assert_eq!(validate_header(&b), WnmCheck::Ok);
    }

    #[test]
    fn truncated_buffer() {
        assert_eq!(validate_header(&[]), WnmCheck::Truncated);
        assert_eq!(validate_header(&[0u8; WNM_HEADER_LEN - 1]), WnmCheck::Truncated);
    }

    #[test]
    fn bad_magic() {
        let mut b = build(&[(TEXT, 16), (CERT, 8), (WASM, 32)]);
        b[1] = b'!';
        assert_eq!(validate_header(&b), WnmCheck::BadMagic);
    }

    #[test]
    fn bad_version() {
        let mut b = build(&[(TEXT, 16), (CERT, 8), (WASM, 32)]);
        b[4..6].copy_from_slice(&(WNM_ABI_VERSION + 1).to_le_bytes());
        assert_eq!(validate_header(&b), WnmCheck::BadVersion);
    }

    #[test]
    fn too_many_sections() {
        let mut b = build(&[(TEXT, 16), (CERT, 8), (WASM, 32)]);
        b[6..8].copy_from_slice(&((WNM_MAX_SECTIONS as u16) + 1).to_le_bytes());
        assert_eq!(validate_header(&b), WnmCheck::TooManySections);
    }

    #[test]
    fn total_len_exceeds_buffer() {
        let mut b = build(&[(TEXT, 16), (CERT, 8), (WASM, 32)]);
        let bogus = (b.len() as u32) + 1;
        b[8..12].copy_from_slice(&bogus.to_le_bytes());
        assert_eq!(validate_header(&b), WnmCheck::BadTotalLen);
    }

    #[test]
    fn section_table_overflows_total_len() {
        // Declare 3 sections but shrink total_len to below the table end.
        let mut b = build(&[(TEXT, 16), (CERT, 8), (WASM, 32)]);
        let short = (WNM_HEADER_LEN + WNM_SECTION_ENTRY_LEN) as u32; // room for 1 entry
        b[8..12].copy_from_slice(&short.to_le_bytes());
        // total_len now < table_end (3 entries) but still <= buffer.
        assert_eq!(validate_header(&b), WnmCheck::SectionTableOverflow);
    }

    #[test]
    fn unknown_section_kind() {
        let mut b = build(&[(TEXT, 16), (CERT, 8), (WASM, 32)]);
        b[WNM_HEADER_LEN] = 99; // first entry's kind byte
        assert_eq!(validate_header(&b), WnmCheck::UnknownSection);
    }

    #[test]
    fn section_escapes_container() {
        let mut b = build(&[(TEXT, 16), (CERT, 8), (WASM, 32)]);
        // Bloat the first section's len so offset+len > total_len.
        let base = WNM_HEADER_LEN;
        b[base + 8..base + 12].copy_from_slice(&(u32::MAX - 4).to_le_bytes());
        assert_eq!(validate_header(&b), WnmCheck::SectionOutOfBounds);
    }

    #[test]
    fn section_overlaps_table() {
        let mut b = build(&[(TEXT, 16), (CERT, 8), (WASM, 32)]);
        // Point the first section's offset into the header region.
        let base = WNM_HEADER_LEN;
        b[base + 4..base + 8].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(validate_header(&b), WnmCheck::SectionOutOfBounds);
    }

    #[test]
    fn section_len_overflow_is_rejected_not_panicked() {
        let mut b = build(&[(TEXT, 16), (CERT, 8), (WASM, 32)]);
        let base = WNM_HEADER_LEN;
        // offset near usize/u32 max + nonzero len would wrap on add.
        b[base + 4..base + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        b[base + 8..base + 12].copy_from_slice(&8u32.to_le_bytes());
        assert_eq!(validate_header(&b), WnmCheck::SectionOutOfBounds);
    }

    #[test]
    fn missing_required_section() {
        // Text + Wasm but no SafetyCert.
        let b = build(&[(TEXT, 16), (WASM, 32)]);
        assert_eq!(validate_header(&b), WnmCheck::MissingRequired);
    }

    #[test]
    fn section_from_u8_roundtrip() {
        assert_eq!(WnmSection::from_u8(1), Some(WnmSection::Text));
        assert_eq!(WnmSection::from_u8(4), Some(WnmSection::Wasm));
        assert_eq!(WnmSection::from_u8(0), None);
        assert_eq!(WnmSection::from_u8(5), None);
    }
}
