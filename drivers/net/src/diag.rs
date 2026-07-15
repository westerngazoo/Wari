// SPDX-License-Identifier: AGPL-3.0-only
//! RX-path register snapshot for the DWMAC v5.20 driver on VF2.
//!
//! Problem this solves: builds 124-128 oscillated between 0% and 4%
//! ARP success on real silicon, and we could not tell which layer
//! (PHY / MAC filter / MTL FIFO / DMA / descriptor ring) was dropping
//! frames. Three reasons:
//!
//! 1. The `St**` cumulative counters tell us `StRf=0` (no frames found
//!    in any descriptor) but cannot distinguish "PHY never delivered"
//!    from "MTL ate them" from "DMA wrote to a ring we're not reading."
//! 2. The driver only reads MMC counters at boot, never again. The
//!    MMC_RX_FRAMECOUNT_GB / MMC_RX_CRC_ERROR pair is the single most
//!    diagnostic register on a DWMAC.
//! 3. `MTL_RXQ0_DEBUG` and `MTL_RXQ0_MISSED_PKT_OVF_CNT` were never
//!    read at all. Non-zero missed = MAC dropped frames at MTL.
//!
//! This module fires a 25-line register snapshot periodically (every
//! `SNAP_EVERY` calls to `receive()`) plus a one-shot deep-dump the
//! first time a frame is found. With wasmi running ~100k polls/sec,
//! `SNAP_EVERY = 32768` gives ~3 dumps/sec — comfortable for a
//! 115200-baud UART (~12000 chars/sec).
//!
//! All output goes through `wari_drv_log_u32`, the always-on log host
//! fn — same one the `St**` counters use. No new kernel-side host
//! function is required.
//!
//! The kernel MMU and validator already cover the full GMAC0+GMAC1
//! window (`[0x16030000, 0x16050000)`, 128 KiB, built into
//! `kernel/src/mem/kvm.rs::GMAC0_MMIO_LEN = 0x20000` and the matching
//! `validate.rs::NET_MMIO_LEN`). All registers below are at
//! `GMAC_BASE + offset` with `offset < 0x10000`, so no permission
//! widening is needed.
//!
//! Gated behind the `net-diag` cargo feature — opt-in for diagnostic
//! builds, default off for production.

#![cfg(feature = "net-diag")]

// Same import-module + link-name shape as the main lib.rs extern
// block at lines 175-200. The signed-wasm tool cross-checks each
// import against the manifest; default `extern "C"` gives an
// `env::` module which would fail signing.
#[link(wasm_import_module = "wari")]
extern "C" {
    #[link_name = "net_mmio_read32"]
    fn wari_net_mmio_read32(addr: u32) -> u32;
    #[link_name = "drv_log_u32"]
    fn wari_drv_log_u32(tag: u32, val: u32) -> i32;
}

// ── DWMAC v5.20 register offsets (databook §10) ─────────────────
//
// Per-MAC-instance offsets — work the same against GMAC0_BASE
// (0x16030000) or GMAC1_BASE (0x16040000); the caller passes the
// right base in `gmac_base`.

const MAC_CONFIGURATION: u32 = 0x0000;
const MAC_PACKET_FILTER: u32 = 0x0008;
const MAC_VERSION: u32 = 0x0110;
const MAC_DEBUG: u32 = 0x0114;
const MAC_PHYIF_CTRL_STATUS: u32 = 0x00F8;

// MMC RX counters (DWMAC v5.20 databook table 11-3, MMC block).
// Confirmed against Linux mainline drivers/net/ethernet/stmicro/stmmac/
// dwmac4_descs.h + dwmac4_lib.c readback paths.
const MMC_RX_FRAMECOUNT_GB: u32 = 0x0780; // good + bad
const MMC_RX_OCTETCOUNT_GB: u32 = 0x0784;
const MMC_RX_FRAMECOUNT_G: u32 = 0x0788; // good only
const MMC_RX_OCTETCOUNT_G: u32 = 0x078C;
const MMC_RX_CRC_ERROR: u32 = 0x0794;
const MMC_RX_ALIGN_ERROR: u32 = 0x0798;
const MMC_RX_LENGTH_ERROR: u32 = 0x07A0;
const MMC_RX_FIFO_OVERFLOW: u32 = 0x07D4;

// MTL RXQ0
const MTL_RXQ0_OP_MODE: u32 = 0x0D30;
const MTL_RXQ0_MISSED: u32 = 0x0D34;
const MTL_RXQ0_DEBUG: u32 = 0x0D38;

// DMA channel 0
const DMA_CH0_RX_CONTROL: u32 = 0x1108;
const DMA_CH0_CUR_RXDESC: u32 = 0x114C;
const DMA_CH0_CUR_RXBUF: u32 = 0x1154;
const DMA_CH0_STATUS: u32 = 0x1160;

// ── Tag namespace (ASCII 4-char mnemonics) ─────────────────────
//
// All tags lead with 'N' so a `grep '0x4e'` over the trace pulls
// every diag line. None of these collide with the existing `r`,
// `d`, `S`, `G`, `A`, `P`, `C`, `t` prefix families in
// docs/diagnostic-tags.md.

const fn tag(a: u8, b: u8, c: u8, d: u8) -> u32 {
    u32::from_be_bytes([a, b, c, d])
}

// Lifecycle markers
const T_BOOT: u32 = tag(b'N', b'd', b'g', b'B'); // boot-time deep dump
const T_SNAP: u32 = tag(b'N', b'd', b'g', b'S'); // periodic snapshot header (val = snap#)
const T_FRST: u32 = tag(b'N', b'd', b'g', b'F'); // first frame ever found (val = slot)

// MAC layer (NM**)
const T_NM_C: u32 = tag(b'N', b'M', b'_', b'C'); // MAC_CONFIGURATION
const T_NM_F: u32 = tag(b'N', b'M', b'_', b'F'); // MAC_PACKET_FILTER
const T_NM_D: u32 = tag(b'N', b'M', b'_', b'D'); // MAC_DEBUG
const T_NM_P: u32 = tag(b'N', b'M', b'_', b'P'); // MAC_PHYIF_CTRL_STATUS
const T_NM_V: u32 = tag(b'N', b'M', b'_', b'V'); // MAC_VERSION

// MMC RX counters (Nm**)
const T_NM_RGB: u32 = tag(b'N', b'm', b'G', b'B'); // FRAMECOUNT_GB total
const T_NM_RG_: u32 = tag(b'N', b'm', b'G', b'_'); // FRAMECOUNT_G good
const T_NM_CRC: u32 = tag(b'N', b'm', b'C', b'r'); // CRC_ERROR
const T_NM_ALG: u32 = tag(b'N', b'm', b'A', b'l'); // ALIGN_ERROR
const T_NM_LEN: u32 = tag(b'N', b'm', b'L', b'e'); // LENGTH_ERROR
const T_NM_FOV: u32 = tag(b'N', b'm', b'F', b'o'); // FIFO_OVERFLOW

// MTL layer (NT**)
const T_NT_OP: u32 = tag(b'N', b'T', b'_', b'O'); // MTL_RXQ0_OP_MODE
const T_NT_MS: u32 = tag(b'N', b'T', b'_', b'M'); // MTL_RXQ0_MISSED (key diag)
const T_NT_DB: u32 = tag(b'N', b'T', b'_', b'D'); // MTL_RXQ0_DEBUG

// DMA layer (ND**)
const T_ND_RC: u32 = tag(b'N', b'D', b'_', b'R'); // DMA_CH0_RX_CONTROL
const T_ND_CD: u32 = tag(b'N', b'D', b'_', b'C'); // DMA_CH0_CUR_RXDESC
const T_ND_CB: u32 = tag(b'N', b'D', b'_', b'B'); // DMA_CH0_CUR_RXBUF
const T_ND_ST: u32 = tag(b'N', b'D', b'_', b'S'); // DMA_CH0_STATUS

// ── State ───────────────────────────────────────────────────────

/// Snapshot cadence. wasmi runs ~100k polls/sec on JH7110, so 32768
/// fires ~3 dumps/sec. 25 lines × 3 = 75 lines/sec ≈ 3000 chars/sec
/// on a 115200-baud UART (12000 chars/sec capacity). Comfortable.
const SNAP_EVERY: u32 = 32768;

/// Per-call counter. Wraps at 2^32 (~12 hours at 100k polls/sec).
/// SAFETY: INV-1 single-hart — only `maybe_snapshot` reads/writes
/// this from the driver-side. Bumped under an inlined wrapping
/// add; no aliasing.
static mut POLL_COUNTER: u32 = 0;

/// One-shot guard for the deep-dump fired on the very first frame.
/// SAFETY: same — INV-1 single-hart.
static mut FIRST_FRAME_SEEN: bool = false;

// ── Public API ──────────────────────────────────────────────────

/// Read an MMIO register at `gmac_base + offset` via the cap-gated
/// host-fn, then log it with `tag`. Inlined so a `let _ =` discard
/// pattern doesn't show up at 25 call sites.
#[inline]
fn read_log(gmac_base: u32, offset: u32, tag: u32) {
    // SAFETY: extern host fn. Both addresses are cap-checked
    // kernel-side via `is_net_mmio_addr`; the validator window
    // covers the full `[0x16030000, 0x16050000)` range, so any
    // `offset < 0x10000` against either GMAC base is allowed.
    let v = unsafe { wari_net_mmio_read32(gmac_base + offset) };
    let _ = unsafe { wari_drv_log_u32(tag, v) };
}

/// Deep-dump fired once at end of `driver_start` after the MAC is up
/// and DMA is running. Emits every register in the snapshot set plus
/// MAC_VERSION (only meaningful at boot — confirms the right IP block
/// is responding).
pub fn boot_dump(gmac_base: u32) {
    // SAFETY: extern host fn, no preconditions.
    let _ = unsafe { wari_drv_log_u32(T_BOOT, 0xB007_0000) };
    read_log(gmac_base, MAC_VERSION, T_NM_V);
    read_log(gmac_base, MAC_CONFIGURATION, T_NM_C);
    read_log(gmac_base, MAC_PACKET_FILTER, T_NM_F);
    read_log(gmac_base, MAC_DEBUG, T_NM_D);
    read_log(gmac_base, MAC_PHYIF_CTRL_STATUS, T_NM_P);
    read_log(gmac_base, MMC_RX_FRAMECOUNT_GB, T_NM_RGB);
    read_log(gmac_base, MMC_RX_FRAMECOUNT_G, T_NM_RG_);
    read_log(gmac_base, MMC_RX_CRC_ERROR, T_NM_CRC);
    read_log(gmac_base, MMC_RX_ALIGN_ERROR, T_NM_ALG);
    read_log(gmac_base, MMC_RX_LENGTH_ERROR, T_NM_LEN);
    read_log(gmac_base, MMC_RX_FIFO_OVERFLOW, T_NM_FOV);
    read_log(gmac_base, MTL_RXQ0_OP_MODE, T_NT_OP);
    read_log(gmac_base, MTL_RXQ0_MISSED, T_NT_MS);
    read_log(gmac_base, MTL_RXQ0_DEBUG, T_NT_DB);
    read_log(gmac_base, DMA_CH0_RX_CONTROL, T_ND_RC);
    read_log(gmac_base, DMA_CH0_CUR_RXDESC, T_ND_CD);
    read_log(gmac_base, DMA_CH0_CUR_RXBUF, T_ND_CB);
    read_log(gmac_base, DMA_CH0_STATUS, T_ND_ST);
    // Sentinel matching the header — easier to grep "Ndg.B" pair.
    let _ = unsafe { wari_drv_log_u32(T_BOOT, 0xB007_FFFF) };
}

/// Called once at the top of `receive()`. Returns immediately on
/// 32767 of every 32768 calls; on the 32768th, dumps the full
/// register set tagged with the snapshot number.
///
/// Total cost on hot path: one `static mut` increment + a single
/// modulo check. The MMIO reads only happen on the dump tick.
pub fn maybe_snapshot(gmac_base: u32) {
    // SAFETY: INV-1 single-hart. POLL_COUNTER has only one accessor
    // (this function); no aliasing possible.
    let n = unsafe {
        POLL_COUNTER = POLL_COUNTER.wrapping_add(1);
        POLL_COUNTER
    };
    if n & (SNAP_EVERY - 1) != 0 {
        return;
    }
    let snap_no = n / SNAP_EVERY;
    // SAFETY: extern host fn.
    let _ = unsafe { wari_drv_log_u32(T_SNAP, snap_no) };

    read_log(gmac_base, MAC_CONFIGURATION, T_NM_C);
    read_log(gmac_base, MAC_PACKET_FILTER, T_NM_F);
    read_log(gmac_base, MAC_DEBUG, T_NM_D);
    read_log(gmac_base, MAC_PHYIF_CTRL_STATUS, T_NM_P);

    read_log(gmac_base, MMC_RX_FRAMECOUNT_GB, T_NM_RGB);
    read_log(gmac_base, MMC_RX_FRAMECOUNT_G, T_NM_RG_);
    read_log(gmac_base, MMC_RX_CRC_ERROR, T_NM_CRC);
    read_log(gmac_base, MMC_RX_ALIGN_ERROR, T_NM_ALG);
    read_log(gmac_base, MMC_RX_LENGTH_ERROR, T_NM_LEN);
    read_log(gmac_base, MMC_RX_FIFO_OVERFLOW, T_NM_FOV);

    read_log(gmac_base, MTL_RXQ0_OP_MODE, T_NT_OP);
    read_log(gmac_base, MTL_RXQ0_MISSED, T_NT_MS);
    read_log(gmac_base, MTL_RXQ0_DEBUG, T_NT_DB);

    read_log(gmac_base, DMA_CH0_RX_CONTROL, T_ND_RC);
    read_log(gmac_base, DMA_CH0_CUR_RXDESC, T_ND_CD);
    read_log(gmac_base, DMA_CH0_CUR_RXBUF, T_ND_CB);
    read_log(gmac_base, DMA_CH0_STATUS, T_ND_ST);
}

/// Called from `receive()` the moment we first find OWN=0 on any
/// descriptor. Fires the boot-style deep-dump exactly once — so the
/// operator gets a full register state-snapshot at the critical
/// transition from "0 frames" to "1 frame seen." Subsequent frames
/// rely on `maybe_snapshot` cadence.
pub fn note_first_frame(gmac_base: u32, slot: u32) {
    // SAFETY: INV-1 single-hart.
    let already = unsafe { FIRST_FRAME_SEEN };
    if already {
        return;
    }
    unsafe {
        FIRST_FRAME_SEEN = true;
    }
    let _ = unsafe { wari_drv_log_u32(T_FRST, slot) };
    boot_dump(gmac_base);
}
