// SPDX-License-Identifier: AGPL-3.0-only
//! Pure checks and encodings for kernel-owned DMA descriptor rings
//! (ADR-002 step 1, INV-25).
//!
//! One concern: decide whether a Tier-2 driver's request to aim a DMA
//! descriptor at a span of its own linear memory is well-formed, and
//! produce the descriptor bytes the device will read. The kernel glue
//! (`cap::dma`, `mmio::dma_mem`) is thin over this file; everything
//! that can be wrong with the *arguments*, and every bit of the
//! descriptor, is decided here, on the host, with tests.
//!
//! ## Why the kernel owns the descriptors, not just the addresses
//!
//! A WASM sandbox bounds what the driver loads and stores. It does
//! not bound what the driver tells a bus master to load and store,
//! and neither the JH7110 GMAC nor QEMU's VirtIO sits behind an
//! IOMMU. INV-25 therefore requires that every span a DMA engine is
//! told to touch lies inside the driver's own linear memory.
//!
//! The first cut of this had the kernel validate an offset and write
//! the resulting physical address into a descriptor that still lived
//! in the driver's memory. That is not enough: the driver could
//! overwrite the address after validation with an ordinary WASM
//! store, enlarge the length next to it, or read the address back to
//! learn its own physical base. So the descriptor tables themselves
//! live in kernel memory, the driver never sees them, and it asks for
//! operations ("aim descriptor `idx` at this buffer") rather than
//! writing descriptor words. A driver cannot express an unsafe
//! descriptor because it cannot express a descriptor at all.
//!
//! Prior art: Xen grant tables (the guest names memory by reference,
//! the hypervisor validates and installs the mapping); the seL4
//! Device Driver Framework's kernel-mediated shared regions.
//!
//! ## Descriptor layout encoded here
//!
//! VirtIO 1.2 §2.7.5 `struct virtq_desc`, 16 bytes, little-endian:
//! `addr: u64`, `len: u32`, `flags: u16`, `next: u16`.
//! [`virtq_desc`] is the only place that layout is written down.

/// Size in bytes of one DMA descriptor, on both engines Wari drives
/// (VirtIO `virtq_desc`; DWMAC4 `TDES0..3` / `RDES0..3`).
pub const DESC_SIZE: usize = 16;

/// Descriptors in one kernel-owned ring. The VirtIO driver uses 8,
/// the GMAC driver 16; the kernel reserves this many per ring and
/// refuses a larger request.
pub const KERNEL_RING_ENTRIES: u32 = 16;

/// VirtIO 1.2 §2.7.5: the device writes into this buffer (set on
/// receive descriptors, clear on transmit descriptors).
pub const VIRTQ_DESC_F_WRITE: u16 = 0x2;

/// Which way data moves through a ring's buffers. The kernel derives
/// it from the ring a driver names, never from an argument, so a
/// driver cannot turn a transmit buffer into one the device writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingDir {
    /// Receive: the device writes frames into the buffers.
    DeviceWrites,
    /// Transmit: the device reads frames from the buffers.
    DeviceReads,
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

/// May a driver claim a kernel-owned ring of `entries` descriptors?
///
/// ```
/// use wari_validate::dma::{ring_entries_ok, KERNEL_RING_ENTRIES};
/// assert!(ring_entries_ok(8));
/// assert!(ring_entries_ok(KERNEL_RING_ENTRIES));
/// assert!(!ring_entries_ok(0));                        // no ring
/// assert!(!ring_entries_ok(KERNEL_RING_ENTRIES + 1));  // more than reserved
/// ```
pub const fn ring_entries_ok(entries: u32) -> bool {
    entries != 0 && entries <= KERNEL_RING_ENTRIES
}

/// Is `idx` a descriptor of a ring claimed with `entries`
/// descriptors? An unclaimed ring (`entries == 0`) has none.
///
/// ```
/// use wari_validate::dma::desc_index_ok;
/// assert!(desc_index_ok(8, 0));
/// assert!(desc_index_ok(8, 7));
/// assert!(!desc_index_ok(8, 8));   // one past the end
/// assert!(!desc_index_ok(0, 0));   // ring not claimed
/// ```
pub const fn desc_index_ok(entries: u32, idx: u32) -> bool {
    ring_entries_ok(entries) && idx < entries
}

/// The 16 bytes of a VirtIO descriptor aiming the device at `len`
/// bytes of physical memory starting at `pa`, as a single buffer
/// (no chaining: `next` is zero and `VIRTQ_DESC_F_NEXT` is clear).
///
/// `pa` and `len` must already have been validated against the
/// driver's linear memory with [`span_in_linmem`]; this function only
/// encodes.
///
/// ```
/// use wari_validate::dma::{virtq_desc, RingDir};
/// let d = virtq_desc(0x8000_1000, 1536, RingDir::DeviceWrites);
/// assert_eq!(&d[0..8], &0x8000_1000u64.to_le_bytes());  // addr
/// assert_eq!(&d[8..12], &1536u32.to_le_bytes());        // len
/// assert_eq!(&d[12..14], &2u16.to_le_bytes());          // flags: WRITE
/// assert_eq!(&d[14..16], &[0, 0]);                      // next
/// // A transmit descriptor is never device-writable.
/// let t = virtq_desc(0x8000_1000, 64, RingDir::DeviceReads);
/// assert_eq!(&t[12..14], &[0, 0]);
/// ```
pub const fn virtq_desc(pa: u64, len: u32, dir: RingDir) -> [u8; DESC_SIZE] {
    let a = pa.to_le_bytes();
    let l = len.to_le_bytes();
    let flags = match dir {
        RingDir::DeviceWrites => VIRTQ_DESC_F_WRITE,
        RingDir::DeviceReads => 0,
    };
    let f = flags.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], l[0], l[1], l[2], l[3], f[0], f[1], 0, 0,
    ]
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
        assert!(span_in_linmem(4095, 1, 4096));
        assert!(span_in_linmem(0, 4096, 4096));
        assert!(span_in_linmem(4000, 96, 4096));
    }

    #[test]
    fn ring_entries_bounds() {
        assert!(!ring_entries_ok(0));
        assert!(ring_entries_ok(1));
        assert!(ring_entries_ok(KERNEL_RING_ENTRIES));
        assert!(!ring_entries_ok(KERNEL_RING_ENTRIES + 1));
        assert!(!ring_entries_ok(u32::MAX));
    }

    #[test]
    fn desc_index_stops_at_the_ring_end() {
        for entries in 1..=KERNEL_RING_ENTRIES {
            assert!(desc_index_ok(entries, entries - 1));
            assert!(!desc_index_ok(entries, entries));
            assert!(!desc_index_ok(entries, u32::MAX));
        }
        // An over-sized or unclaimed ring has no valid index at all.
        assert!(!desc_index_ok(KERNEL_RING_ENTRIES + 1, 0));
        assert!(!desc_index_ok(0, 0));
    }

    #[test]
    fn virtq_desc_fields_land_where_the_spec_puts_them() {
        let d = virtq_desc(0x0123_4567_89AB_CDEF, 0xA1B2_C3D4, RingDir::DeviceWrites);
        assert_eq!(
            u64::from_le_bytes([d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]),
            0x0123_4567_89AB_CDEF
        );
        assert_eq!(u32::from_le_bytes([d[8], d[9], d[10], d[11]]), 0xA1B2_C3D4);
        assert_eq!(u16::from_le_bytes([d[12], d[13]]), VIRTQ_DESC_F_WRITE);
        assert_eq!(u16::from_le_bytes([d[14], d[15]]), 0);
    }

    #[test]
    fn transmit_descriptor_is_never_device_writable_or_chained() {
        let d = virtq_desc(u64::MAX, u32::MAX, RingDir::DeviceReads);
        // flags == 0: neither WRITE (0x2) nor NEXT (0x1) nor INDIRECT (0x4).
        assert_eq!(u16::from_le_bytes([d[12], d[13]]), 0);
        assert_eq!(u16::from_le_bytes([d[14], d[15]]), 0);
    }
}
