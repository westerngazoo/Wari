// SPDX-License-Identifier: AGPL-3.0-only
//! WNM container **encoder** — the write side of `wari_wnm`, which is
//! decode-only (`load_plan`/`validate_header`).
//!
//! The layout is defined once, in `wari-wnm/src/lib.rs`; this module
//! reuses that crate's constants (`WNM_MAGIC`, `WNM_ABI_VERSION`, the
//! header/entry lengths, and the [`WnmSection`] discriminants) so the
//! bytes it writes are exactly what the loader parses. The output is
//! **bitwise-reproducible** (R8): sections are emitted in a fixed order,
//! payloads packed tightly, and no timestamps or host paths appear.

use wari_wnm::{
    WnmSection, WNM_ABI_VERSION, WNM_HEADER_LEN, WNM_MAGIC, WNM_MAX_SECTIONS,
    WNM_SECTION_ENTRY_LEN,
};

/// One section to pack into the container.
pub struct Section<'a> {
    /// Section kind (Text, Wasm, SafetyCert, Relocs).
    pub kind: WnmSection,
    /// Raw payload bytes.
    pub data: &'a [u8],
}

/// Encode `sections` into a WNM container that `wari_wnm::load_plan`
/// accepts.
///
/// Sections are sorted by kind discriminant before emission, so the
/// output is independent of the order the caller passes them — the
/// reproducibility property (R8) holds regardless of call site. Payloads
/// are packed tightly immediately after the section table; the WNM format
/// imposes no in-container alignment (execution alignment of `.text` is
/// the loader's concern when it maps the section RX).
///
/// # Contract
/// - Returns a container whose header, section table, and payloads
///   satisfy every `validate_header` invariant (bounds, no duplicates,
///   required kinds present — the caller must supply Text + SafetyCert +
///   Wasm).
/// - Bitwise-reproducible: identical `sections` content → identical bytes.
///
/// # Errors
/// - `> WNM_MAX_SECTIONS` sections.
/// - A duplicate section kind (the loader rejects these; fail early).
/// - The container would exceed `u32` (`total_len`'s field width).
pub fn encode(mut sections: Vec<Section>) -> Result<Vec<u8>, String> {
    // Deterministic, caller-order-independent output.
    sections.sort_by_key(|s| s.kind as u8);

    // Reject duplicate kinds up front (load_plan would reject them too).
    for pair in sections.windows(2) {
        if pair[0].kind as u8 == pair[1].kind as u8 {
            return Err(format!("duplicate WNM section kind {:?}", pair[0].kind));
        }
    }

    let n = sections.len();
    if n > WNM_MAX_SECTIONS {
        return Err(format!(
            "too many WNM sections: {n} > {WNM_MAX_SECTIONS}"
        ));
    }

    let table_len = n * WNM_SECTION_ENTRY_LEN;
    let payload_base = WNM_HEADER_LEN + table_len;
    let payload_total: usize = sections.iter().map(|s| s.data.len()).sum();
    let total_len = payload_base + payload_total;
    let total_u32: u32 = total_len
        .try_into()
        .map_err(|_| format!("WNM container {total_len} bytes exceeds u32 total_len"))?;

    let mut out = vec![0u8; total_len];

    // ── Header (12 bytes) ──
    out[0..4].copy_from_slice(&WNM_MAGIC);
    out[4..6].copy_from_slice(&WNM_ABI_VERSION.to_le_bytes());
    out[6..8].copy_from_slice(&(n as u16).to_le_bytes());
    out[8..12].copy_from_slice(&total_u32.to_le_bytes());

    // ── Section table + payloads ──
    let mut off = payload_base;
    for (i, s) in sections.iter().enumerate() {
        let entry = WNM_HEADER_LEN + i * WNM_SECTION_ENTRY_LEN;
        out[entry] = s.kind as u8;
        // out[entry + 1 ..= entry + 3] reserved, already zero.
        out[entry + 4..entry + 8].copy_from_slice(&(off as u32).to_le_bytes());
        out[entry + 8..entry + 12].copy_from_slice(&(s.data.len() as u32).to_le_bytes());
        out[off..off + s.data.len()].copy_from_slice(s.data);
        off += s.data.len();
    }
    debug_assert_eq!(off, total_len, "payload cursor must reach total_len");

    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_load_plan() {
        let text = [0x13u8, 0x00, 0x00, 0x00]; // one RV64 `nop`
        let cert = b"cert";
        let wasm = b"\0asm\x01\0\0\0";
        let bytes = encode(vec![
            Section { kind: WnmSection::Text, data: &text },
            Section { kind: WnmSection::Wasm, data: wasm },
            Section { kind: WnmSection::SafetyCert, data: cert },
        ])
        .unwrap();

        let plan = wari_wnm::load_plan(&bytes).expect("encoder output must load");
        let (to, tl) = plan.text;
        assert_eq!(&bytes[to as usize..(to + tl) as usize], &text);
        let (wo, wl) = plan.wasm;
        assert_eq!(&bytes[wo as usize..(wo + wl) as usize], wasm);
        let (co, cl) = plan.safety_cert;
        assert_eq!(&bytes[co as usize..(co + cl) as usize], cert);
        assert!(plan.relocs.is_none(), "no Relocs section was emitted");
    }

    #[test]
    fn output_is_order_independent() {
        let t = [0x13u8, 0, 0, 0];
        let c = b"c";
        let w = b"w";
        let a = encode(vec![
            Section { kind: WnmSection::Text, data: &t },
            Section { kind: WnmSection::Wasm, data: w },
            Section { kind: WnmSection::SafetyCert, data: c },
        ])
        .unwrap();
        let b = encode(vec![
            Section { kind: WnmSection::SafetyCert, data: c },
            Section { kind: WnmSection::Text, data: &t },
            Section { kind: WnmSection::Wasm, data: w },
        ])
        .unwrap();
        assert_eq!(a, b, "section order must not change the bytes");
    }

    #[test]
    fn rejects_duplicate_kind() {
        let t = [0x13u8, 0, 0, 0];
        let err = encode(vec![
            Section { kind: WnmSection::Text, data: &t },
            Section { kind: WnmSection::Text, data: &t },
        ])
        .unwrap_err();
        assert!(err.contains("duplicate"), "got: {err}");
    }
}
