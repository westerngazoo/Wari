// SPDX-License-Identifier: AGPL-3.0-only
//! Pure checks for kernel-owned DMA addressing (ADR-002 step 1).
//!
//! One concern: decide whether a Tier-2 driver's request to point a
//! DMA descriptor at a span of its own linear memory is well-formed.
//! The kernel host fn (`cap::dma::nic_dma_set_buf_impl`) is thin glue
//! over these two predicates; everything that can be wrong with the
//! *arguments* is decided here, on the host, with tests.
//!
//! ## Why the kernel writes every physical address
//!
//! A WASM sandbox bounds what the driver loads and stores. It does
//! not bound what the driver tells a bus master to load and store,
//! and neither the JH7110 GMAC nor QEMU's VirtIO sits behind an
//! IOMMU. INV-25 therefore requires that every physical address a
//! DMA engine sees lies inside the driver's own linear memory, and
//! the only place that can be checked is the kernel, at the moment
//! the address is formed. The driver hands the kernel an *offset*;
//! the kernel validates it with [`span_in_linmem`], adds the linear
//! memory's base, and writes the result into the descriptor itself.
//! Prior art: Xen grant tables (the guest names memory by reference,
//! the hypervisor validates and installs the mapping).
//!
//! ## Descriptor layout this relies on
//!
//! Both DMA engines Wari drives put the 64-bit buffer address in the
//! first eight bytes of a 16-byte descriptor, little-endian: VirtIO
//! 1.2 §2.6.5 `virtq_desc.addr`, and DWMAC4 `TDES0/TDES1`,
//! `RDES0/RDES1`. [`desc_addr_offset`] is where that fact is encoded.

/// Size in bytes of one DMA descriptor, on both engines.
pub const DESC_SIZE: u32 = 16;

/// Upper bound on descriptors per ring the kernel will track. Both
/// drivers use 8 (VirtIO) or 16 (GMAC); the bound keeps the
/// per-ring arithmetic in `u32` without overflow checks at every
/// step and refuses absurd ring sizes up front.
pub const MAX_RING_ENTRIES: u32 = 1024;

/// A DMA descriptor ring the kernel knows about: where the
/// descriptor table starts in the owning module's linear memory and
/// how many descriptors it holds. Pure data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaRing {
    /// Offset of descriptor 0 in the module's linear memory.
    pub desc_off: u32,
    /// Number of descriptors in the ring (`0` = not configured).
    pub entries: u32,
}

impl DmaRing {
    /// A ring no module has configured.
    pub const fn empty() -> Self {
        DmaRing {
            desc_off: 0,
            entries: 0,
        }
    }

    /// Has a driver configured this ring?
    pub const fn is_active(&self) -> bool {
        self.entries != 0
    }
}

/// Does `[off, off + len)` lie entirely inside a linear memory of
/// `mem_len` bytes, with `len > 0`?
///
/// The arithmetic is checked: an `off`/`len` pair that wraps `u32`
/// is rejected, not wrapped into range. `mem_len` is `usize` because
/// that is what wasmi reports; a 32-bit linear memory never exceeds
/// 4 GiB, so the comparison is exact on 64-bit hosts and kernels.
///
/// ```
/// use wari_validate::dma::span_in_linmem;
/// assert!(span_in_linmem(0, 1, 16));           // first byte
/// assert!(span_in_linmem(15, 1, 16));          // last byte
/// assert!(!span_in_linmem(16, 1, 16));         // one past the end
/// assert!(!span_in_linmem(8, 9, 16));          // spills past the end
/// assert!(!span_in_linmem(0, 0, 16));          // empty span is not a buffer
/// assert!(!span_in_linmem(u32::MAX, 2, usize::MAX)); // wraps u32
/// ```
pub const fn span_in_linmem(off: u32, len: u32, mem_len: usize) -> bool {
    if len == 0 {
        return false;
    }
    match off.checked_add(len) {
        Some(end) => (end as usize) <= mem_len,
        None => false,
    }
}

/// Offset in linear memory of the 8-byte address field of descriptor
/// `idx` in `ring`, or `None` if the ring is not configured, `idx` is
/// past its end, or the arithmetic would wrap.
///
/// The caller still has to check the returned 8-byte span against
/// the linear memory's length with [`span_in_linmem`]: a ring that
/// was valid when configured may sit in memory the module has since
/// shrunk (wasmi memories only grow today, but the check is cheap
/// and keeps the two predicates independent).
///
/// ```
/// use wari_validate::dma::{desc_addr_offset, DmaRing, DESC_SIZE};
/// let ring = DmaRing { desc_off: 0x100, entries: 8 };
/// assert_eq!(desc_addr_offset(&ring, 0), Some(0x100));
/// assert_eq!(desc_addr_offset(&ring, 7), Some(0x100 + 7 * DESC_SIZE));
/// assert_eq!(desc_addr_offset(&ring, 8), None);            // past the end
/// assert_eq!(desc_addr_offset(&DmaRing::empty(), 0), None); // not configured
/// ```
pub const fn desc_addr_offset(ring: &DmaRing, idx: u32) -> Option<u32> {
    if !ring.is_active() || idx >= ring.entries || ring.entries > MAX_RING_ENTRIES {
        return None;
    }
    // idx < entries <= MAX_RING_ENTRIES, so idx * DESC_SIZE cannot
    // overflow; the add still can if desc_off is near the top.
    ring.desc_off.checked_add(idx * DESC_SIZE)
}

/// Is a ring description acceptable to record: a non-zero entry
/// count within [`MAX_RING_ENTRIES`], a 16-byte-aligned descriptor
/// table (VirtIO 1.2 §2.6; DWMAC4 databook), and the whole table
/// inside a linear memory of `mem_len` bytes?
///
/// ```
/// use wari_validate::dma::ring_fits;
/// assert!(ring_fits(0x100, 8, 0x1000));
/// assert!(!ring_fits(0x104, 8, 0x1000));   // misaligned table
/// assert!(!ring_fits(0x100, 0, 0x1000));   // no entries
/// assert!(!ring_fits(0xF90, 8, 0x1000));   // table spills past the end
/// assert!(!ring_fits(0, 2048, 1 << 20));   // over MAX_RING_ENTRIES
/// ```
pub const fn ring_fits(desc_off: u32, entries: u32, mem_len: usize) -> bool {
    if entries == 0 || entries > MAX_RING_ENTRIES || desc_off & 0xF != 0 {
        return false;
    }
    span_in_linmem(desc_off, entries * DESC_SIZE, mem_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_rejects_wrap_and_zero_and_spill() {
        assert!(!span_in_linmem(u32::MAX, 1, usize::MAX));
        assert!(!span_in_linmem(u32::MAX - 1, 2, usize::MAX));
        assert!(!span_in_linmem(0, 0, 1));
        assert!(!span_in_linmem(4096, 1, 4096));
        assert!(!span_in_linmem(4000, 97, 4096));
    }

    #[test]
    fn span_accepts_exact_fit() {
        assert!(span_in_linmem(0, 4096, 4096));
        assert!(span_in_linmem(4095, 1, 4096));
        assert!(span_in_linmem(4000, 96, 4096));
    }

    #[test]
    fn desc_offset_walks_the_ring_and_stops_at_the_end() {
        let ring = DmaRing {
            desc_off: 0x2000,
            entries: 16,
        };
        for i in 0..16 {
            assert_eq!(desc_addr_offset(&ring, i), Some(0x2000 + i * DESC_SIZE));
        }
        assert_eq!(desc_addr_offset(&ring, 16), None);
        assert_eq!(desc_addr_offset(&ring, u32::MAX), None);
    }

    #[test]
    fn desc_offset_refuses_wrapping_table() {
        let ring = DmaRing {
            desc_off: u32::MAX - 8,
            entries: 2,
        };
        assert_eq!(desc_addr_offset(&ring, 0), Some(u32::MAX - 8));
        assert_eq!(desc_addr_offset(&ring, 1), None);
    }

    #[test]
    fn desc_offset_refuses_oversized_ring() {
        let ring = DmaRing {
            desc_off: 0,
            entries: MAX_RING_ENTRIES + 1,
        };
        assert_eq!(desc_addr_offset(&ring, 0), None);
    }

    #[test]
    fn ring_fits_boundaries() {
        // 8 × 16 = 128 bytes ending exactly at the memory end.
        assert!(ring_fits(0x1000 - 128, 8, 0x1000));
        assert!(!ring_fits(0x1000 - 112, 8, 0x1000));
        let max_table = (MAX_RING_ENTRIES * DESC_SIZE) as usize;
        assert!(ring_fits(0, MAX_RING_ENTRIES, max_table));
        assert!(!ring_fits(0, MAX_RING_ENTRIES + 1, usize::MAX));
        // Aligned table whose 16 bytes wrap past u32::MAX.
        assert!(!ring_fits(0xFFFF_FFF0, 1, usize::MAX));
    }

    #[test]
    fn empty_ring_is_inactive() {
        assert!(!DmaRing::empty().is_active());
        assert!(DmaRing {
            desc_off: 0,
            entries: 1
        }
        .is_active());
    }
}
