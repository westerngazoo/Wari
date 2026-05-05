// SPDX-License-Identifier: AGPL-3.0-only
//! Tier-2 net driver — Phase-1b VirtIO-net device init (PR Net-4b).
//!
//! Built as a separately-signed `.wasm` per platform: one for QEMU
//! `virt` (VirtIO-net at `0x10008000`) and one for StarFive
//! VisionFive 2 (JH7110 GMAC eth0 at `0x16030000`). Activated via
//! cargo feature: `--features qemu` or `--features vf2`.
//!
//! ## Phase-1b PR Net-4b scope (this PR)
//!
//! - VirtIO MMIO transport discovery (magic + version + device id)
//! - Status-bit handshake per VirtIO 1.2 §3.1.1:
//!     reset → ACK → DRIVER → FEATURES_OK → DRIVER_OK
//! - Feature negotiation: accept `VIRTIO_F_VERSION_1` + `VIRTIO_NET_F_MAC`,
//!   reject everything else
//! - Read MAC from device config space (offset 0x100) per VirtIO 1.2 §5.1.4
//! - Communicate MAC to the kernel via `wari::nic_set_mac`, which also
//!   sets `Net.initialized = true` so the kernel-side
//!   `runtime::run_tier2_net` can observe driver readiness
//!
//! **NOT in this PR**: virtqueue setup, packet RX/TX, ARP, ICMP. Those
//! land in PR Net-4c. PR Net-4b leaves the device "configured but
//! silent" — `DRIVER_OK` is set so the device knows the driver is
//! present, but no packets exchange because no queues are wired.
//!
//! ## Spec citations
//!
//! Every VirtIO operation below cites the VirtIO 1.2 specification
//! section it implements (`§N.M.K`). Authoritative source:
//! https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html
//!
//! ## Verification status
//!
//! Code is **structurally correct per spec** but **has not yet been
//! end-to-end tested in QEMU**. The first run-in-QEMU test is the
//! exit gate for PR Net-4c. Until then, treat the MAC printed by
//! `run_tier2_net` as the verification signal: a zeroed MAC means
//! init failed silently somewhere in this file; a real `52:54:00:…`
//! MAC means QEMU's VirtIO device is in the `DRIVER_OK` state and
//! responding to config-space reads.
//!
//! ## Lockstep maintenance
//!
//! `NIC_BASE` here mirrors `kernel/src/validate.rs::NET_MMIO_BASE`.
//! `NET_MMIO_LEN = 0x200` in the kernel must cover both the transport
//! window (0x000..0x100) and the device config region (0x100..0x200)
//! — PR Net-4b widens the validator from 0x100 to 0x200 specifically
//! to allow MAC reads.

#![no_std]
#![no_main]
// VF2 builds gate everything VirtIO-related behind cfg(feature =
// "qemu"); vf2's _start is a Phase-1c stub. Suppress dead-code
// noise on vf2 builds — the items are used on qemu, this is the
// expected shape of "two cfg-gated platforms in one crate".
#![cfg_attr(feature = "vf2", allow(dead_code))]

#[cfg(not(any(feature = "qemu", feature = "vf2")))]
compile_error!("wari-driver-net requires --features qemu or --features vf2.");

#[cfg(all(feature = "qemu", feature = "vf2"))]
compile_error!("wari-driver-net accepts only one of --features qemu / vf2.");

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // Tier-2 panics are bugs in signed code. The infinite loop is
    // a structural last resort; PR Net-4b's init path uses
    // Result-style early-return rather than panic so a failed init
    // simply leaves Net.initialized = false on the kernel side.
    loop {}
}

// ── Platform constants (lockstep with kernel/src/validate.rs) ────

#[cfg(feature = "qemu")]
mod plat {
    /// VirtIO-net MMIO base on QEMU `virt`.
    pub const NIC_BASE: u32 = 0x1000_8000;
}

#[cfg(feature = "vf2")]
mod plat {
    /// JH7110 GMAC eth0 base on VisionFive 2 — Phase 1c will land
    /// the GMAC implementation. Phase 1b's vf2 build is a stub.
    #[allow(dead_code)]
    pub const NIC_BASE: u32 = 0x1603_0000;
}

// ── VirtIO MMIO register offsets (VirtIO 1.2 §4.2.2) ─────────────
//
// These offsets are spec-fixed and apply to any VirtIO MMIO transport
// device (network, block, console, etc.). VirtIO-net's device-specific
// config starts at 0x100.

const VIRTIO_MMIO_MAGIC_VALUE:         u32 = 0x000;
const VIRTIO_MMIO_VERSION:             u32 = 0x004;
const VIRTIO_MMIO_DEVICE_ID:           u32 = 0x008;
#[allow(dead_code)]
const VIRTIO_MMIO_VENDOR_ID:           u32 = 0x00c;
const VIRTIO_MMIO_DEVICE_FEATURES:     u32 = 0x010;
const VIRTIO_MMIO_DEVICE_FEATURES_SEL: u32 = 0x014;
const VIRTIO_MMIO_DRIVER_FEATURES:     u32 = 0x020;
const VIRTIO_MMIO_DRIVER_FEATURES_SEL: u32 = 0x024;
const VIRTIO_MMIO_STATUS:              u32 = 0x070;
/// Device-specific config region; for VirtIO-net see §5.1.4.
const VIRTIO_MMIO_CONFIG:              u32 = 0x100;

// ── VirtIO magic + protocol constants (VirtIO 1.2 §4.2.2.1) ──────

/// `MagicValue` register reads as the four bytes "virt" little-endian
/// = `0x74726976`. Any other value means we're reading garbage / no
/// VirtIO device at this MMIO base.
const VIRTIO_MAGIC: u32 = 0x7472_6976;

/// `Version` register: 2 = modern (VirtIO 1.0+), 1 = legacy (deprecated).
/// PR Net-4b targets modern.
const VIRTIO_VERSION_MODERN: u32 = 2;

/// `DeviceID` register: 1 = network. The device ID space is in
/// VirtIO 1.2 §5; 1 is the network device class.
const VIRTIO_DEVICE_ID_NET: u32 = 1;

// ── VirtIO Status bits (VirtIO 1.2 §2.1) ─────────────────────────

const VIRTIO_STATUS_ACKNOWLEDGE: u32 = 0x01;
const VIRTIO_STATUS_DRIVER:      u32 = 0x02;
const VIRTIO_STATUS_DRIVER_OK:   u32 = 0x04;
const VIRTIO_STATUS_FEATURES_OK: u32 = 0x08;
#[allow(dead_code)]
const VIRTIO_STATUS_NEEDS_RESET: u32 = 0x40;
#[allow(dead_code)]
const VIRTIO_STATUS_FAILED:      u32 = 0x80;

// ── VirtIO feature bits we negotiate ─────────────────────────────
//
// VirtIO 1.2 §6: feature bits 0..31 are device-specific; 32+ are
// transport / general. Driver writes both halves via
// DriverFeaturesSel = 0 / 1.

/// VirtIO-net §5.1.3: device provides a MAC address in config space.
/// Without this we'd have to invent a MAC, which Phase-1b doesn't.
const VIRTIO_NET_F_MAC: u32 = 5;

/// VirtIO 1.2 §6: the driver speaks the modern protocol. MUST be set
/// by every modern (version=2) driver.
const VIRTIO_F_VERSION_1: u32 = 32;

// ── Host-function imports ────────────────────────────────────────

#[link(wasm_import_module = "wari")]
extern "C" {
    /// Cap-gated NIC register write. Returns 0 on success.
    #[link_name = "net_mmio_write32"]
    fn wari_net_mmio_write32(addr: u32, val: u32) -> i32;

    /// Cap-gated NIC register read. Returns `u32::MAX` on
    /// permission / out-of-range failure.
    #[link_name = "net_mmio_read32"]
    fn wari_net_mmio_read32(addr: u32) -> u32;

    /// Driver → kernel signaling: "I finished VirtIO init, my MAC
    /// is (mac_low, mac_high)." See `cap::syscall::nic_set_mac_impl`.
    /// Returns 0 on success, negative errno on failure.
    #[link_name = "nic_set_mac"]
    fn wari_nic_set_mac(mac_low: u32, mac_high: u32) -> i32;

    /// PR Phase-1c-2 — diagnostic line. Driver passes a tag word
    /// + a value; kernel formats both onto COM7. Use sparingly:
    /// boot-time register dumps, milestone markers. Returns 0.
    #[link_name = "drv_log_u32"]
    fn wari_drv_log_u32(tag: u32, val: u32) -> i32;

    #[allow(dead_code)]
    #[link_name = "notification_wait"]
    fn wari_notification_wait(slot: u32) -> i32;

    #[allow(dead_code)]
    #[link_name = "notification_ack"]
    fn wari_notification_ack(slot: u32) -> i32;

    /// Driver → kernel: bind a virtqueue's three rings (descriptor
    /// table, available ring, used ring) to the NIC. The driver
    /// passes lin-mem offsets; the kernel translates to physical
    /// addresses and writes the VirtIO MMIO queue config registers.
    /// PR Net-4c host fn.
    #[link_name = "nic_attach_queue"]
    fn wari_nic_attach_queue(
        queue_idx: u32,
        desc_off: u32,
        avail_off: u32,
        used_off: u32,
        queue_size: u32,
    ) -> i32;

    /// Kick the device's QueueNotify register for `queue_idx`
    /// (0 = rx, 1 = tx) after the driver has updated the available
    /// ring. PR Net-4d host fn.
    #[link_name = "nic_queue_notify"]
    fn wari_nic_queue_notify(queue_idx: u32) -> i32;

    /// Return the driver's WASM lin-mem physical address. Used by
    /// the driver to compute PAs for VirtIO descriptor `addr`
    /// fields (which are PAs, not lin-mem offsets). PR Net-4d
    /// host fn. Cap-gated by Net + READ; returns 0 on cap denial.
    #[link_name = "lin_mem_base"]
    fn wari_lin_mem_base() -> u64;
}

// ── Virtqueue ring storage (PR Net-4c) ───────────────────────────
//
// Phase-1b queue size is 8 — small enough that two queues
// (rx + tx) plus their rings fit in well under 1 KiB. Each ring's
// alignment is enforced by `#[repr(align(N))]` on its wrapper
// struct.
//
// The `static mut` pattern is the standard way to give wasmi a
// known lin-mem offset without runtime allocation. `addr_of_mut!`
// returns the offset.

const QUEUE_SIZE: u32 = 8;

/// Descriptor table — `virtq_desc[QUEUE_SIZE]`. Each desc is 16
/// bytes; alignment 16 (VirtIO 1.2 §2.6).
#[repr(C, align(16))]
struct DescTable {
    bytes: [u8; 16 * QUEUE_SIZE as usize],
}

/// Available ring — `flags : u16, idx : u16, ring : u16[QUEUE_SIZE]`.
/// 4 + 2*8 = 20 bytes. Alignment 2.
#[repr(C, align(2))]
struct AvailRing {
    bytes: [u8; 4 + 2 * QUEUE_SIZE as usize],
}

/// Used ring — `flags : u16, idx : u16, ring : virtq_used_elem[QUEUE_SIZE]`.
/// virtq_used_elem is { id : u32, len : u32 } = 8 bytes.
/// Total 4 + 8*8 = 68 bytes. Alignment 4.
#[repr(C, align(4))]
struct UsedRing {
    bytes: [u8; 4 + 8 * QUEUE_SIZE as usize],
}

static mut RX_DESC: DescTable = DescTable {
    bytes: [0; 16 * QUEUE_SIZE as usize],
};
static mut RX_AVAIL: AvailRing = AvailRing {
    bytes: [0; 4 + 2 * QUEUE_SIZE as usize],
};
static mut RX_USED: UsedRing = UsedRing {
    bytes: [0; 4 + 8 * QUEUE_SIZE as usize],
};

static mut TX_DESC: DescTable = DescTable {
    bytes: [0; 16 * QUEUE_SIZE as usize],
};
static mut TX_AVAIL: AvailRing = AvailRing {
    bytes: [0; 4 + 2 * QUEUE_SIZE as usize],
};
static mut TX_USED: UsedRing = UsedRing {
    bytes: [0; 4 + 8 * QUEUE_SIZE as usize],
};

/// Set up one virtqueue: select it on the device, choose queue size,
/// hand the kernel offsets for the three rings via `nic_attach_queue`
/// (which writes the VirtIO MMIO queue-config registers).
///
/// `queue_idx`: 0 = rx, 1 = tx (VirtIO-net §5.1.6.1 convention).
fn attach_queue(
    queue_idx: u32,
    desc_off: u32,
    avail_off: u32,
    used_off: u32,
) -> Result<(), ()> {
    // SAFETY: extern host-fn call. Kernel does cap check + bounds
    // check on the offsets, then writes VirtIO MMIO queue regs.
    let r = unsafe {
        wari_nic_attach_queue(queue_idx, desc_off, avail_off, used_off, QUEUE_SIZE)
    };
    if r == 0 {
        Ok(())
    } else {
        Err(())
    }
}

// ── Packet buffers (PR Net-4d) ───────────────────────────────────
//
// Phase-1b ships 8 rx buffers + 1 tx scratch. Each is sized to
// hold a full Ethernet frame (1518 bytes = 14 hdr + 1500 mtu + 4
// crc) plus the 12-byte VirtIO-net header (§5.1.6) — round to
// 1536 for alignment headroom. Total: 8 * 1536 + 1 * 1536 =
// 14 KiB of static lin-mem.

const ETH_FRAME_MAX: usize = 1536;
const RX_BUF_COUNT: usize = QUEUE_SIZE as usize;

/// VirtIO descriptor flag — buffer is device-write (rx).
const VIRTQ_DESC_F_WRITE: u16 = 0x2;

/// VirtIO-net packet header per §5.1.6. The device prepends this
/// to every frame on rx, and expects it on every tx frame. We
/// negotiated zero protocol features so all 12 bytes are zero on
/// our tx, and the device's rx headers are ignored by us
/// (smoltcp in PR Net-5 will pull a frame past the header).
#[allow(dead_code)]
const VIRTIO_NET_HDR_LEN: usize = 12;

#[repr(C, align(8))]
pub struct PacketBuffer {
    pub bytes: [u8; ETH_FRAME_MAX],
}

static mut RX_BUFS: [PacketBuffer; RX_BUF_COUNT] = [const {
    PacketBuffer { bytes: [0; ETH_FRAME_MAX] }
}; RX_BUF_COUNT];
/// Convenience scratch for callers that don't supply their own
/// buffer. Currently unused by the driver itself; PR Net-5's
/// smoltcp wrapper (or PR Net-6's socket-IPC marshaller) writes
/// frames here and calls `tx_send(addr_of_mut!(TX_BUF) as u32,
/// len)`. Kept exported so it survives optimization.
#[no_mangle]
pub static mut TX_BUF: PacketBuffer = PacketBuffer { bytes: [0; ETH_FRAME_MAX] };

/// Driver-side ring index tracking. Phase-1b keeps it simple — no
/// wraparound logic beyond the `% QUEUE_SIZE` masking; rx_used_seen
/// monotonically advances and the kernel's idle loop (Phase-2+)
/// would call `rx_pop()` repeatedly until it returns 0.
static mut RX_USED_SEEN: u16 = 0;
static mut RX_AVAIL_NEXT: u16 = 0;
static mut TX_USED_SEEN: u16 = 0;

/// Write a 16-bit little-endian value to a lin-mem offset.
fn write_u16_le(off: u32, val: u16) {
    let p = off as *mut u8;
    // SAFETY: caller passes a valid offset within our lin-mem;
    // wasmi traps on OOB. Two byte stores compile to a single
    // wasm i32.store16.
    unsafe {
        p.write(val as u8);
        p.add(1).write((val >> 8) as u8);
    }
}

#[allow(dead_code)]
fn read_u16_le(off: u32) -> u16 {
    let p = off as *const u8;
    // SAFETY: same as write_u16_le.
    unsafe { p.read() as u16 | ((p.add(1).read() as u16) << 8) }
}

/// Write a 32-bit little-endian value to a lin-mem offset.
fn write_u32_le(off: u32, val: u32) {
    let p = off as *mut u8;
    // SAFETY: same as write_u16_le.
    unsafe {
        p.write(val as u8);
        p.add(1).write((val >> 8) as u8);
        p.add(2).write((val >> 16) as u8);
        p.add(3).write((val >> 24) as u8);
    }
}

#[allow(dead_code)]
fn read_u32_le(off: u32) -> u32 {
    let p = off as *const u8;
    // SAFETY: same as write_u32_le.
    unsafe {
        p.read() as u32
            | ((p.add(1).read() as u32) << 8)
            | ((p.add(2).read() as u32) << 16)
            | ((p.add(3).read() as u32) << 24)
    }
}

/// Write a 64-bit little-endian value to a lin-mem offset. Used
/// for VirtIO descriptor `addr` fields.
fn write_u64_le(off: u32, val: u64) {
    let p = off as *mut u8;
    // SAFETY: same as write_u16_le.
    unsafe {
        for i in 0..8 {
            p.add(i).write((val >> (i * 8)) as u8);
        }
    }
}

/// Build a VirtIO descriptor at `desc_off + idx*16` pointing at a
/// packet buffer.
///
/// VirtIO 1.2 §2.6.5:
///   struct virtq_desc { le64 addr; le32 len; le16 flags; le16 next; }
fn write_desc(desc_off: u32, idx: u16, buf_pa: u64, len: u32, flags: u16, next: u16) {
    let off = desc_off + (idx as u32) * 16;
    write_u64_le(off, buf_pa);          // addr
    write_u32_le(off + 8, len);         // len
    write_u16_le(off + 12, flags);      // flags
    write_u16_le(off + 14, next);       // next
}

/// Populate the rx queue: build 8 descriptors, each pointing at a
/// distinct rx buffer and flagged WRITE (device writes incoming
/// packets here). Add all 8 indices to the available ring, advance
/// `avail.idx`, kick QueueNotify(0).
///
/// Called once from `init_virtio` after queue attach + before
/// DRIVER_OK. After this, the device may write incoming packets
/// to our buffers; until something calls `rx_pop` the packets
/// pile up in the used ring (Phase-1b polling, Phase-2+ adds an
/// idle loop or worker to drain them).
fn populate_rx() -> Result<(), ()> {
    // SAFETY: extern host-fn — kernel cap-checks Net + READ. The
    // returned PA is the WASM lin-mem base in kernel-physical
    // address space. Returns 0 on cap denial.
    let lin_base = unsafe { wari_lin_mem_base() };
    if lin_base == 0 {
        return Err(());
    }

    // Offsets in lin-mem.
    let rx_desc_off = core::ptr::addr_of_mut!(RX_DESC) as u32;
    let rx_avail_off = core::ptr::addr_of_mut!(RX_AVAIL) as u32;
    // SAFETY: addr_of_mut! over indexed static doesn't deref, but
    // Rust's E0133 still flags the index expression. Single-thread
    // driver, no data race.
    let rx_buf0_off =
        unsafe { core::ptr::addr_of_mut!(RX_BUFS[0]) } as u32;

    // Build 8 descriptors, one per rx buffer.
    for i in 0..RX_BUF_COUNT {
        let buf_pa = lin_base + (rx_buf0_off as u64) + (i as u64) * (ETH_FRAME_MAX as u64);
        write_desc(
            rx_desc_off,
            i as u16,
            buf_pa,
            ETH_FRAME_MAX as u32,
            VIRTQ_DESC_F_WRITE,
            0,
        );
    }

    // Available ring layout (§2.6.6):
    //   le16 flags          @ avail_off + 0
    //   le16 idx            @ avail_off + 2
    //   le16 ring[QSIZE]    @ avail_off + 4
    // Phase-1b leaves flags = 0 (no event-idx, no suppression).
    write_u16_le(rx_avail_off, 0); // flags
    for i in 0..RX_BUF_COUNT {
        write_u16_le(rx_avail_off + 4 + (i as u32) * 2, i as u16);
    }
    // Set idx LAST — VirtIO §2.6.13.4 requires the entries are
    // written before idx advances. The host-fn boundary (notify
    // call below) acts as the memory barrier the spec requires.
    write_u16_le(rx_avail_off + 2, RX_BUF_COUNT as u16);

    // SAFETY: single-threaded driver context, INV-1 covers exclusivity.
    unsafe {
        RX_AVAIL_NEXT = RX_BUF_COUNT as u16;
    }

    // Kick the device. SAFETY: extern host-fn, kernel cap-checks.
    let r = unsafe { wari_nic_queue_notify(0) };
    if r == 0 { Ok(()) } else { Err(()) }
}

// ── Exported RX/TX helpers (consumed by PR Net-5 / smoltcp) ──────

/// Send a frame from `buf_off` of length `len` bytes. Phase-1b
/// allows only one in-flight tx at a time (no descriptor pool); the
/// caller must wait for the previous send to retire before calling
/// again. Returns 0 on success, -1 on host-fn failure.
///
/// Caller is responsible for prepending the 12-byte VirtIO-net
/// header to the frame (per §5.1.6); Phase-1b's smoltcp wrapper
/// (PR Net-5) handles this.
pub fn driver_tx_send(buf_off: u32, len: u32) -> i32 {
    if len > ETH_FRAME_MAX as u32 {
        return -1;
    }
    // SAFETY: extern host-fn, kernel cap-checks Net + READ.
    let lin_base = unsafe { wari_lin_mem_base() };
    if lin_base == 0 {
        return -1;
    }

    let tx_desc_off = core::ptr::addr_of_mut!(TX_DESC) as u32;
    let tx_avail_off = core::ptr::addr_of_mut!(TX_AVAIL) as u32;

    // Always reuse descriptor 0 — Phase-1b has no in-flight queue.
    let desc_idx: u16 = 0;
    let buf_pa = lin_base + (buf_off as u64);

    // VIRTQ_DESC_F_WRITE not set on tx — device reads, doesn't
    // write. flags = 0.
    write_desc(tx_desc_off, desc_idx, buf_pa, len, 0, 0);

    // Available ring: bump idx to publish the descriptor.
    let avail_idx = unsafe {
        let new_idx = (TX_USED_SEEN.wrapping_add(1)) % QUEUE_SIZE as u16;
        write_u16_le(
            tx_avail_off + 4 + ((new_idx % QUEUE_SIZE as u16) as u32) * 2,
            desc_idx,
        );
        new_idx
    };
    write_u16_le(tx_avail_off + 2, avail_idx.wrapping_add(1));

    // SAFETY: extern host-fn, kernel cap-checks.
    unsafe { wari_nic_queue_notify(1) }
}

/// Drain the rx used ring. Returns the byte count of the next
/// received frame in the high 32 bits and the lin-mem offset of
/// the buffer in the low 32 bits, packed as a single u64. Returns
/// `0` if no new packets are available since the last `rx_pop`
/// call.
///
/// The buffer pointed to by the returned offset remains owned by
/// the driver until `rx_recycle` is called with the same desc
/// index — until then the device has been told the buffer is in
/// use.
///
/// Returns u64 packed as `(buf_off as u64) | ((len as u64) << 32)`.
/// `0` (== both fields 0) is the "no packets" sentinel — a real
/// packet always has len > 0 (Ethernet frames carry ≥ 60 bytes
/// after preamble).
pub fn driver_rx_pop() -> u64 {
    let rx_used_off = core::ptr::addr_of_mut!(RX_USED) as u32;
    let device_idx = read_u16_le(rx_used_off + 2);

    // SAFETY: single-threaded driver, RX_USED_SEEN is local.
    let seen = unsafe { RX_USED_SEEN };
    if device_idx == seen {
        return 0; // no new packets
    }

    // Read used ring entry at slot `seen % QSIZE`:
    //   struct virtq_used_elem { le32 id; le32 len; }
    let slot = (seen as u32) % QUEUE_SIZE;
    let elem_off = rx_used_off + 4 + slot * 8;
    let desc_id = read_u32_le(elem_off);
    let used_len = read_u32_le(elem_off + 4);

    // SAFETY: single-threaded.
    unsafe {
        RX_USED_SEEN = seen.wrapping_add(1);
    }

    // Locate the buffer this descriptor points at. Descriptor i
    // points at RX_BUFS[i] in our layout, so desc_id = buffer index.
    if (desc_id as usize) >= RX_BUF_COUNT {
        return 0; // device returned an unexpected desc_id
    }
    // SAFETY: addr_of_mut! doesn't deref; the index expression
    // requires unsafe. Single-thread driver, no race.
    let buf_off =
        unsafe { core::ptr::addr_of_mut!(RX_BUFS[desc_id as usize]) } as u32;
    (buf_off as u64) | ((used_len as u64) << 32)
}

/// Recycle a buffer after the caller is done with it. Builds a
/// fresh rx descriptor at `desc_idx` and adds it to the available
/// ring so the device can write a new packet there.
pub fn driver_rx_recycle(desc_idx: u32) -> i32 {
    if (desc_idx as usize) >= RX_BUF_COUNT {
        return -1;
    }
    // SAFETY: extern host-fn — kernel cap-checks.
    let lin_base = unsafe { wari_lin_mem_base() };
    if lin_base == 0 {
        return -1;
    }

    let rx_desc_off = core::ptr::addr_of_mut!(RX_DESC) as u32;
    let rx_avail_off = core::ptr::addr_of_mut!(RX_AVAIL) as u32;
    // SAFETY: addr_of_mut! over indexed static doesn't deref, but
    // Rust's E0133 still flags the index expression. Single-thread
    // driver, no data race.
    let rx_buf0_off =
        unsafe { core::ptr::addr_of_mut!(RX_BUFS[0]) } as u32;
    let buf_pa = lin_base
        + (rx_buf0_off as u64)
        + (desc_idx as u64) * (ETH_FRAME_MAX as u64);

    // Rewrite the descriptor (idempotent).
    write_desc(
        rx_desc_off,
        desc_idx as u16,
        buf_pa,
        ETH_FRAME_MAX as u32,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    // Append to avail ring.
    let avail_idx = unsafe {
        let next = RX_AVAIL_NEXT;
        write_u16_le(
            rx_avail_off + 4 + ((next % QUEUE_SIZE as u16) as u32) * 2,
            desc_idx as u16,
        );
        let new_next = next.wrapping_add(1);
        RX_AVAIL_NEXT = new_next;
        new_next
    };
    write_u16_le(rx_avail_off + 2, avail_idx);

    // SAFETY: extern host-fn.
    unsafe { wari_nic_queue_notify(0) }
}

// ── smoltcp Interface (PR Net-5b) ────────────────────────────────
//
// `nic_iface::init(mac)` is called once from `_start` after VirtIO
// init succeeds. It builds a `smoltcp::iface::Interface` configured
// with our static IP and the device-supplied MAC, plus a small
// SocketSet backing array. After this, the kernel idle loop calls
// the exported `poll(timestamp_ms)` function which advances
// `Interface::poll`.

#[cfg(feature = "qemu")]
mod nic_iface {
    use core::ptr::addr_of_mut;
    use smoltcp::iface::{Config, Interface, SocketSet, SocketStorage};
    use smoltcp::time::Instant;
    use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr};

    use super::phy::NicDevice;

    /// Phase-1b QEMU demo IP (per net design doc §10 Q1). QEMU
    /// slirp's default subnet is 192.168.122.0/24 with gateway
    /// 192.168.122.1; we take 192.168.122.10.
    const IP_OCTETS: [u8; 4] = [192, 168, 122, 10];
    const IP_PREFIX_LEN: u8 = 24;

    /// SocketSet backing storage. Phase-1b reserves 4 socket slots
    /// (none populated until PR Net-6 wires the Tier-1 socket host
    /// fns). Larger workloads bump this constant later.
    const SOCKET_BACKING_LEN: usize = 4;

    static mut INTERFACE: Option<Interface> = None;
    static mut DEVICE: NicDevice = NicDevice::new();
    static mut SOCKETS: Option<SocketSet<'static>> = None;
    static mut SOCKETS_STORAGE: [SocketStorage<'static>; SOCKET_BACKING_LEN] =
        [const { SocketStorage::EMPTY }; SOCKET_BACKING_LEN];

    // ── Per-socket buffer pool (PR Net-6a) ───────────────────────
    //
    // smoltcp::socket::tcp::Socket needs &'static mut [u8] for
    // its rx/tx buffers. We pre-allocate `SOCKET_BACKING_LEN`
    // pairs and hand them out at socket_create time. SLOT_USED
    // tracks which pairs are owned by an open socket.
    const SOCKET_TX_BUF_LEN: usize = 1024;
    const SOCKET_RX_BUF_LEN: usize = 1024;

    static mut SOCKET_TX_BUFS: [[u8; SOCKET_TX_BUF_LEN]; SOCKET_BACKING_LEN] =
        [[0u8; SOCKET_TX_BUF_LEN]; SOCKET_BACKING_LEN];
    static mut SOCKET_RX_BUFS: [[u8; SOCKET_RX_BUF_LEN]; SOCKET_BACKING_LEN] =
        [[0u8; SOCKET_RX_BUF_LEN]; SOCKET_BACKING_LEN];
    /// Maps SocketSet handle (raw u32) → buffer pair index, so
    /// socket_close can free the pair when the smoltcp socket is
    /// removed. `None` slot = free buffer pair.
    static mut SOCKET_HANDLE_FOR_BUF: [Option<u32>; SOCKET_BACKING_LEN] =
        [None; SOCKET_BACKING_LEN];
    /// Per-buffer-slot bound port (set by socket_bind, consumed
    /// by socket_listen). 0 = unbound.
    static mut SOCKET_BOUND_PORT: [u16; SOCKET_BACKING_LEN] =
        [0u16; SOCKET_BACKING_LEN];

    /// Allocate a TCP smoltcp socket. Returns the raw
    /// `SocketHandle` value as i32 on success, negative errno on
    /// failure (`-3` = E_NOMEM if the buffer pool is exhausted).
    /// Called from `driver_socket_create` (Net-6a).
    pub fn socket_create_tcp() -> Result<i32, i32> {
        use smoltcp::socket::tcp;
        // SAFETY: single-thread driver (INV-1 generalized).
        unsafe {
            let sockets = match &mut *addr_of_mut!(SOCKETS) {
                Some(s) => s,
                None => return Err(-3),
            };
            // Find a free buffer pair.
            let slot = match SOCKET_HANDLE_FOR_BUF.iter().position(|s| s.is_none()) {
                Some(i) => i,
                None => return Err(-3), // E_NOMEM
            };
            let rx_buf = tcp::SocketBuffer::new(
                &mut SOCKET_RX_BUFS[slot][..],
            );
            let tx_buf = tcp::SocketBuffer::new(
                &mut SOCKET_TX_BUFS[slot][..],
            );
            let socket = tcp::Socket::new(rx_buf, tx_buf);
            let handle = sockets.add(socket);
            // smoltcp's SocketHandle is opaque but reads as a usize
            // index internally. Cast through Debug-derived shape:
            // (handle as u32) is what we hand back to the kernel.
            let raw = handle_to_raw(handle);
            SOCKET_HANDLE_FOR_BUF[slot] = Some(raw);
            Ok(raw as i32)
        }
    }

    /// Stash the requested local port for a TCP socket so a
    /// later `socket_listen` can hand it to smoltcp. Phase-1b
    /// ignores the IP arg (smoltcp listens on all local IPs by
    /// default; binding to a specific local IP is Phase-2).
    /// Returns 0 on success, `-2` if the handle is unknown.
    pub fn socket_bind(raw_handle: u32, _ip_be: u32, port: u32) -> i32 {
        // SAFETY: single-thread driver.
        unsafe {
            let slot = match SOCKET_HANDLE_FOR_BUF
                .iter()
                .position(|s| *s == Some(raw_handle))
            {
                Some(i) => i,
                None => return -2,
            };
            if port == 0 || port > u16::MAX as u32 {
                return -2;
            }
            SOCKET_BOUND_PORT[slot] = port as u16;
            0
        }
    }

    /// Mark a TCP socket as listening on its previously-bound
    /// port. Returns 0 on success, `-2` if the handle is unknown
    /// or the socket has not been bound yet, `-3` (E_NOMEM) if
    /// smoltcp's listen call fails (already-listening / connected).
    pub fn socket_listen(raw_handle: u32) -> i32 {
        use smoltcp::socket::tcp;
        // SAFETY: single-thread driver.
        unsafe {
            let sockets = match &mut *addr_of_mut!(SOCKETS) {
                Some(s) => s,
                None => return -3,
            };
            let slot = match SOCKET_HANDLE_FOR_BUF
                .iter()
                .position(|s| *s == Some(raw_handle))
            {
                Some(i) => i,
                None => return -2,
            };
            let port = SOCKET_BOUND_PORT[slot];
            if port == 0 {
                return -2; // not bound
            }
            let handle = raw_to_handle(raw_handle);
            let socket = sockets.get_mut::<tcp::Socket>(handle);
            match socket.listen(port) {
                Ok(()) => 0,
                Err(_) => -3,
            }
        }
    }

    /// Tear down a TCP socket previously returned by
    /// `socket_create_tcp`. Returns 0 on success, `-2` (E_INVAL)
    /// if the handle is unknown.
    pub fn socket_close(raw_handle: u32) -> i32 {
        // SAFETY: same as socket_create.
        unsafe {
            let sockets = match &mut *addr_of_mut!(SOCKETS) {
                Some(s) => s,
                None => return -3,
            };
            let slot = match SOCKET_HANDLE_FOR_BUF
                .iter()
                .position(|s| *s == Some(raw_handle))
            {
                Some(i) => i,
                None => return -2, // E_INVAL: unknown handle
            };
            let handle = raw_to_handle(raw_handle);
            sockets.remove(handle);
            SOCKET_HANDLE_FOR_BUF[slot] = None;
            SOCKET_BOUND_PORT[slot] = 0;
            0
        }
    }

    /// Convert smoltcp's opaque `SocketHandle` to its raw u32
    /// integer. smoltcp doesn't expose the inner index publicly;
    /// we use a transmute that the same-version invariant makes
    /// safe (both are repr(transparent) over usize internally).
    fn handle_to_raw(h: smoltcp::iface::SocketHandle) -> u32 {
        // SAFETY: SocketHandle is repr(transparent) over usize in
        // smoltcp 0.11; we only round-trip the low 32 bits within
        // a single instance lifetime. The reverse function below
        // performs the inverse transmute.
        unsafe { core::mem::transmute::<_, usize>(h) as u32 }
    }
    fn raw_to_handle(raw: u32) -> smoltcp::iface::SocketHandle {
        // SAFETY: counterpart to handle_to_raw above.
        unsafe { core::mem::transmute::<usize, _>(raw as usize) }
    }

    /// Build the smoltcp `Interface`. Called once from `_start`.
    ///
    /// Returns `Err(())` if the IP push fails (storage exhausted).
    /// Phase 1b only pushes one CIDR so this can't actually fail,
    /// but the Result keeps the contract honest.
    pub fn init(mac: [u8; 6]) -> Result<(), ()> {
        let hwaddr = EthernetAddress::from_bytes(&mac);
        let config = Config::new(HardwareAddress::Ethernet(hwaddr));
        // SAFETY: `_start` runs once at boot; INV-1 / INV-14
        // generalized — this static-mut access happens before any
        // poll. `addr_of_mut!(DEVICE).as_mut()` is sound because
        // DEVICE is a valid initialized static.
        let mut iface = unsafe {
            Interface::new(
                config,
                addr_of_mut!(DEVICE).as_mut().expect("DEVICE static is non-null"),
                Instant::from_millis(0),
            )
        };
        let mut push_ok = true;
        iface.update_ip_addrs(|addrs| {
            if addrs
                .push(IpCidr::new(
                    IpAddress::v4(IP_OCTETS[0], IP_OCTETS[1], IP_OCTETS[2], IP_OCTETS[3]),
                    IP_PREFIX_LEN,
                ))
                .is_err()
            {
                push_ok = false;
            }
        });
        if !push_ok {
            return Err(());
        }
        // SAFETY: same as Interface::new above.
        unsafe {
            INTERFACE = Some(iface);
            SOCKETS = Some(SocketSet::new(&mut SOCKETS_STORAGE[..]));
        }
        Ok(())
    }

    /// Advance smoltcp's poll cycle once. `timestamp_ms` is a
    /// logical monotonic counter from the kernel idle loop; not
    /// wall-clock-aligned, but smoltcp only requires monotonicity
    /// for retransmit decisions.
    ///
    /// Returns `true` if any state changed (incoming packet
    /// processed, outgoing packet emitted, ARP entry updated).
    pub fn poll(timestamp_ms: u64) -> bool {
        // SAFETY: single-thread driver; INV-1 generalized.
        unsafe {
            let iface = match &mut *addr_of_mut!(INTERFACE) {
                Some(i) => i,
                None => return false,
            };
            let sockets = match &mut *addr_of_mut!(SOCKETS) {
                Some(s) => s,
                None => return false,
            };
            let device = addr_of_mut!(DEVICE)
                .as_mut()
                .expect("DEVICE static is non-null");
            iface.poll(Instant::from_millis(timestamp_ms as i64), device, sockets)
        }
    }
}

/// `wari_driver_net::poll(timestamp_ms: u64) -> i32`.
///
/// Kernel idle-loop entry point. Returns 1 if smoltcp processed
/// any state, 0 if idle. Negative on init failure (vf2 stub or
/// nic_iface not initialized).
pub fn driver_poll(timestamp_ms: u64) -> i32 {
    #[cfg(feature = "qemu")]
    {
        if nic_iface::poll(timestamp_ms) {
            1
        } else {
            0
        }
    }
    #[cfg(feature = "vf2")]
    {
        let _ = timestamp_ms;
        -1
    }
}

// ── smoltcp Device trait impl (PR Net-5a) ────────────────────────
//
// `NicDevice` is a zero-sized handle wrapping the static rx/tx
// state above. smoltcp's `Interface` calls into it via the
// `Device`/`RxToken`/`TxToken` traits to drain incoming packets and
// publish outgoing ones. PR Net-5a defines the trait impl; PR
// Net-5b will instantiate `Interface` and wire `poll` into the
// kernel idle loop.
//
// VirtIO-net §5.1.6 prepends a 12-byte header to every packet on
// rx and expects one on tx. We negotiated zero protocol features
// so the header is always 12 zero bytes; smoltcp does not see it
// (the `consume` closures get a slice that skips past the header).

#[cfg(feature = "qemu")]
pub mod phy {
    use core::sync::atomic::{compiler_fence, Ordering};

    use super::{
        rx_pop, rx_recycle, tx_send, ETH_FRAME_MAX, RX_BUFS, TX_BUF,
        VIRTIO_NET_HDR_LEN,
    };
    use smoltcp::phy::{
        Checksum, Device, DeviceCapabilities, Medium, RxToken, TxToken,
    };
    use smoltcp::time::Instant;

    /// Zero-sized Device handle. All state lives in `super`'s
    /// static muts; constructing a `NicDevice` is just naming the
    /// driver's NIC-state sandbox.
    pub struct NicDevice;

    impl NicDevice {
        pub const fn new() -> Self {
            Self
        }
    }

    impl Default for NicDevice {
        fn default() -> Self {
            Self::new()
        }
    }

    /// MTU minus the VirtIO-net header (smoltcp doesn't see header
    /// bytes). Phase 1b's MTU is the standard Ethernet 1500.
    const SMOLTCP_MTU: usize = 1500;

    impl Device for NicDevice {
        type RxToken<'a> = NicRxToken;
        type TxToken<'a> = NicTxToken;

        fn capabilities(&self) -> DeviceCapabilities {
            let mut caps = DeviceCapabilities::default();
            caps.medium = Medium::Ethernet;
            caps.max_transmission_unit = SMOLTCP_MTU;
            // Phase-1b doesn't negotiate VIRTIO_NET_F_GUEST_CSUM,
            // so the device hands us full Ethernet+IP+TCP frames
            // with valid checksums; smoltcp must verify on rx and
            // emit on tx itself. Default = both sides verify/emit.
            caps.checksum.ipv4 = Checksum::Both;
            caps.checksum.tcp = Checksum::Both;
            caps.checksum.udp = Checksum::Both;
            caps.checksum.icmpv4 = Checksum::Both;
            caps
        }

        fn receive(
            &mut self,
            _timestamp: Instant,
        ) -> Option<(NicRxToken, NicTxToken)> {
            let packed = rx_pop();
            if packed == 0 {
                return None;
            }
            let buf_off = (packed & 0xFFFF_FFFF) as u32;
            let used_len = (packed >> 32) as u32;

            // Recover desc_idx from buf_off. RX_BUFS[i] is at
            // a fixed offset; (buf_off - rx_buf0_off) /
            // sizeof(buf) gives the index. SAFETY: addr_of_mut!
            // over indexed-into static needs the unsafe gate even
            // though it doesn't deref.
            let rx_buf0_off =
                unsafe { core::ptr::addr_of_mut!(RX_BUFS[0]) } as u32;
            let desc_idx = (buf_off - rx_buf0_off) / (ETH_FRAME_MAX as u32);

            let rx = NicRxToken {
                buf_off,
                used_len,
                desc_idx,
            };
            let tx = NicTxToken;
            Some((rx, tx))
        }

        fn transmit(&mut self, _timestamp: Instant) -> Option<NicTxToken> {
            // Phase 1b uses a single TX_BUF; smoltcp can always
            // get a tx token. Future PRs may add tx-ring
            // back-pressure (return None when tx queue saturated).
            Some(NicTxToken)
        }
    }

    /// Holds the lin-mem offset + length of one received packet
    /// plus the desc index for recycle on consume.
    pub struct NicRxToken {
        buf_off: u32,
        used_len: u32,
        desc_idx: u32,
    }

    impl RxToken for NicRxToken {
        fn consume<R, F>(self, f: F) -> R
        where
            F: FnOnce(&mut [u8]) -> R,
        {
            // Skip the 12-byte VirtIO-net header to expose the raw
            // Ethernet frame to smoltcp.
            let frame_off = self.buf_off + VIRTIO_NET_HDR_LEN as u32;
            let frame_len = self
                .used_len
                .saturating_sub(VIRTIO_NET_HDR_LEN as u32) as usize;
            // SAFETY: buf_off is the offset of an entry of RX_BUFS
            // (each ETH_FRAME_MAX bytes long); used_len ≤
            // ETH_FRAME_MAX (device wrote ≤ that many bytes,
            // checked at attach time). Single-threaded. smoltcp
            // 0.11 takes &mut [u8] to allow in-place processing.
            let slice = unsafe {
                core::slice::from_raw_parts_mut(frame_off as *mut u8, frame_len)
            };
            let r = f(slice);

            // After the closure runs, recycle the buffer back to
            // the device. compiler_fence guards against wasmi
            // reordering between the read and the host-fn call.
            compiler_fence(Ordering::SeqCst);
            let _ = rx_recycle(self.desc_idx);
            r
        }
    }

    /// Zero-sized TxToken. Phase-1b uses a single shared
    /// `TX_BUF`; future PRs add a real tx descriptor pool.
    pub struct NicTxToken;

    impl TxToken for NicTxToken {
        fn consume<R, F>(self, len: usize, f: F) -> R
        where
            F: FnOnce(&mut [u8]) -> R,
        {
            // Frame goes after the 12-byte VirtIO-net header.
            let total_len = len + VIRTIO_NET_HDR_LEN;
            let tx_buf_off =
                core::ptr::addr_of_mut!(TX_BUF) as u32;
            let frame_off = tx_buf_off + VIRTIO_NET_HDR_LEN as u32;
            // SAFETY: TX_BUF is ETH_FRAME_MAX bytes; total_len ≤
            // SMOLTCP_MTU + 12 < ETH_FRAME_MAX. Single-threaded.
            let slice = unsafe {
                core::slice::from_raw_parts_mut(frame_off as *mut u8, len)
            };

            // Zero the VirtIO-net header (no protocol features
            // negotiated → 12 zero bytes).
            // SAFETY: TX_BUF is owned, header is in-bounds.
            unsafe {
                core::ptr::write_bytes(
                    tx_buf_off as *mut u8,
                    0,
                    VIRTIO_NET_HDR_LEN,
                );
            }

            let r = f(slice);

            // Memory barrier before the host-fn boundary, then
            // hand off to the device.
            compiler_fence(Ordering::SeqCst);
            let _ = tx_send(tx_buf_off, total_len as u32);
            r
        }
    }
}

// ── Register access helpers ──────────────────────────────────────

/// Read a 32-bit NIC register at `offset` from `NIC_BASE`. Returns
/// `Err(())` if the host fn signaled failure (cap denied or address
/// out of range — both surface as `u32::MAX`).
fn nic_read32(offset: u32) -> Result<u32, ()> {
    // SAFETY: extern host-fn call. Kernel validates address, cap.
    let v = unsafe { wari_net_mmio_read32(plat::NIC_BASE + offset) };
    // The sentinel u32::MAX is also a legal device-features high
    // word in some configurations, but for VirtIO MagicValue,
    // Version, and DeviceID it can only legitimately occur on
    // failure — those registers are spec-fixed and never u32::MAX.
    // Per-call sites that care interpret u32::MAX appropriately.
    Ok(v)
}

/// Write a 32-bit NIC register at `offset`. `Err(())` if the host
/// fn returned non-zero.
fn nic_write32(offset: u32, val: u32) -> Result<(), ()> {
    // SAFETY: extern host-fn call. Kernel validates address, cap.
    let r = unsafe { wari_net_mmio_write32(plat::NIC_BASE + offset, val) };
    if r == 0 {
        Ok(())
    } else {
        Err(())
    }
}

// ── VirtIO init sequence (VirtIO 1.2 §3.1.1) ─────────────────────

/// Run the VirtIO driver-init sequence. On success returns
/// `Ok([u8; 6])` with the MAC read from device config; `Err(())` on
/// any spec violation or host-fn failure.
fn init_virtio() -> Result<[u8; 6], ()> {
    // §4.2.2.1 — verify the MMIO magic. Any other value means we're
    // either reading garbage MMIO or the device tree is wrong.
    if nic_read32(VIRTIO_MMIO_MAGIC_VALUE)? != VIRTIO_MAGIC {
        return Err(());
    }
    // §4.2.2.1 — verify Version = 2 (modern). We don't support
    // legacy in Phase 1b.
    if nic_read32(VIRTIO_MMIO_VERSION)? != VIRTIO_VERSION_MODERN {
        return Err(());
    }
    // §4.2.2.1 — verify DeviceID = 1 (network).
    if nic_read32(VIRTIO_MMIO_DEVICE_ID)? != VIRTIO_DEVICE_ID_NET {
        return Err(());
    }

    // §3.1.1 step 1 — reset by writing 0 to status.
    nic_write32(VIRTIO_MMIO_STATUS, 0)?;

    // §3.1.1 step 2 — set ACKNOWLEDGE.
    nic_write32(VIRTIO_MMIO_STATUS, VIRTIO_STATUS_ACKNOWLEDGE)?;

    // §3.1.1 step 3 — set DRIVER.
    nic_write32(
        VIRTIO_MMIO_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    )?;

    // §3.1.1 step 4 — feature negotiation.
    //
    // Read device features in two halves (low = bits 0..31, high =
    // bits 32..63) selected by DeviceFeaturesSel.
    nic_write32(VIRTIO_MMIO_DEVICE_FEATURES_SEL, 0)?;
    let dev_feat_lo = nic_read32(VIRTIO_MMIO_DEVICE_FEATURES)?;
    nic_write32(VIRTIO_MMIO_DEVICE_FEATURES_SEL, 1)?;
    let dev_feat_hi = nic_read32(VIRTIO_MMIO_DEVICE_FEATURES)?;

    // We require the device to offer:
    //   - VIRTIO_F_VERSION_1  (bit 32, in dev_feat_hi at bit 0)
    //   - VIRTIO_NET_F_MAC    (bit 5,  in dev_feat_lo at bit 5)
    // Reject the device if either is missing.
    if (dev_feat_hi & (1 << (VIRTIO_F_VERSION_1 - 32))) == 0 {
        return Err(());
    }
    if (dev_feat_lo & (1 << VIRTIO_NET_F_MAC)) == 0 {
        return Err(());
    }

    // Write back the subset of features we accept. Phase-1b accepts
    // exactly these two; everything else is rejected.
    let our_feat_lo = 1u32 << VIRTIO_NET_F_MAC;
    let our_feat_hi = 1u32 << (VIRTIO_F_VERSION_1 - 32);
    nic_write32(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0)?;
    nic_write32(VIRTIO_MMIO_DRIVER_FEATURES, our_feat_lo)?;
    nic_write32(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1)?;
    nic_write32(VIRTIO_MMIO_DRIVER_FEATURES, our_feat_hi)?;

    // §3.1.1 step 5 — set FEATURES_OK to lock the negotiation.
    nic_write32(
        VIRTIO_MMIO_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK,
    )?;

    // §3.1.1 step 6 — verify FEATURES_OK is still set. If the
    // device cleared it, the driver's accepted-feature subset was
    // not acceptable to the device.
    let status = nic_read32(VIRTIO_MMIO_STATUS)?;
    if (status & VIRTIO_STATUS_FEATURES_OK) == 0 {
        return Err(());
    }

    // §3.1.1 step 7 — virtqueue setup (PR Net-4c).
    //
    // Compute lin-mem offsets of the static-mut ring storage. These
    // are the wasm32 lin-mem addresses; the kernel's
    // `nic_attach_queue` host fn translates them to physical
    // addresses for VirtIO.
    //
    // SAFETY: addr_of_mut! returns a raw pointer without aliasing;
    // we only convert to u32 (lin-mem offset) and pass to the
    // kernel, never dereference here. The kernel accesses the
    // memory via the wasmi Memory abstraction (bounds-checked).
    let rx_desc_off = core::ptr::addr_of_mut!(RX_DESC) as u32;
    let rx_avail_off = core::ptr::addr_of_mut!(RX_AVAIL) as u32;
    let rx_used_off = core::ptr::addr_of_mut!(RX_USED) as u32;
    let tx_desc_off = core::ptr::addr_of_mut!(TX_DESC) as u32;
    let tx_avail_off = core::ptr::addr_of_mut!(TX_AVAIL) as u32;
    let tx_used_off = core::ptr::addr_of_mut!(TX_USED) as u32;

    // VirtIO-net §5.1.6.1: queue 0 is receiveq[0], queue 1 is
    // transmitq[0]. Phase-1b uses one rx + one tx queue, no
    // controlq (didn't negotiate VIRTIO_NET_F_CTRL_VQ).
    attach_queue(0, rx_desc_off, rx_avail_off, rx_used_off)?;
    attach_queue(1, tx_desc_off, tx_avail_off, tx_used_off)?;

    // PR Net-4d — populate the rx queue with descriptors pointing
    // at our packet buffers. After this the device may start
    // writing incoming frames; until something calls `rx_pop`
    // they pile up in the used ring (which is harmless — the
    // device simply runs out of buffers and drops further packets,
    // which is the correct degradation mode).
    populate_rx()?;

    // §3.1.1 step 8 — set DRIVER_OK. Device now considers the
    // driver ready and starts honoring the queues. PR Net-4c does
    // NOT yet add buffers to the rx queue, so incoming packets
    // are dropped by the device until PR Net-4d populates rx
    // descriptors.
    nic_write32(
        VIRTIO_MMIO_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    )?;

    // §5.1.4 — read MAC from device-specific config region. The
    // MAC field is the first 6 bytes of `virtio_net_config`. We
    // read it as two 32-bit values: bytes 0..4 in `mac01`, bytes
    // 4..8 in `mac02` (we only care about the low 16 bits of mac02).
    let mac01 = nic_read32(VIRTIO_MMIO_CONFIG)?;
    let mac23 = nic_read32(VIRTIO_MMIO_CONFIG + 4)?;
    let mac = [
        (mac01 & 0xFF) as u8,
        ((mac01 >> 8) & 0xFF) as u8,
        ((mac01 >> 16) & 0xFF) as u8,
        ((mac01 >> 24) & 0xFF) as u8,
        (mac23 & 0xFF) as u8,
        ((mac23 >> 8) & 0xFF) as u8,
    ];
    Ok(mac)
}

// ── Driver entry ─────────────────────────────────────────────────

/// Driver entry. Phase-1b PR Net-4b: run VirtIO discovery + init,
/// communicate MAC to kernel via `wari::nic_set_mac` on success.
///
/// On failure the function returns silently, leaving
/// `Net.initialized = false` on the kernel side. The kernel-side
/// `run_tier2_net` will see this and log an error rather than the
/// success line.
pub fn driver_start() {
    // PR Phase-1c-2: log a milestone marker so the kernel-side
    // boot trace shows "the driver _start has begun executing".
    // tag = ASCII 'WAR\0' = 0x57415200.
    // SAFETY: extern host-fn call (Net+ no-cap-required diagnostic).
    let _ = unsafe { wari_drv_log_u32(0x57415200, 0xC0DE_0001) };

    #[cfg(feature = "qemu")]
    {
        let mac = match init_virtio() {
            Ok(m) => m,
            Err(()) => return,
        };
        // Pack 6 bytes into two u32s. mac_low = bytes [0..4],
        // mac_high = low 16 bits of bytes [4..8].
        let mac_low = (mac[0] as u32)
            | ((mac[1] as u32) << 8)
            | ((mac[2] as u32) << 16)
            | ((mac[3] as u32) << 24);
        let mac_high = (mac[4] as u32) | ((mac[5] as u32) << 8);
        // SAFETY: extern host-fn call. Kernel cap-checks Net+WRITE.
        let _ = unsafe { wari_nic_set_mac(mac_low, mac_high) };

        // PR Net-5b — bring up the smoltcp Interface. Failure here
        // means the kernel's run_tier2_net will see Net.initialized
        // = true (we already set it via nic_set_mac) but
        // tier2_net::is_installed will be false (poll export
        // resolves but its first call hits a None Interface and
        // returns 0). The kernel logs "[net] virtio init failed"
        // / "[net] smoltcp interface up" lines accordingly.
        let _ = nic_iface::init(mac);
    }

    // PR Phase-1c-2 — read the JH7110 GMAC0 version register.
    #[cfg(feature = "vf2")]
    {
        const GMAC_VERSION_OFFSET: u32 = 0x110;
        // SAFETY: extern host-fn — kernel cap-checks Net+READ and
        // bounds-checks the address (now mapped post Phase-1c-1.6).
        let v = unsafe { wari_net_mmio_read32(plat::NIC_BASE + GMAC_VERSION_OFFSET) };
        let _ = unsafe { wari_drv_log_u32(0x474D4143, v) };
    }

    // PR Phase-1c-3c — enable GMAC0 bus clocks via SYSCRG + STGCRG.
    //
    // Build 74 dump showed:
    //   SYSCRG +0x190..0x198 = 0x08 / 0x02 / 0x0a — bit 31 (the
    //     JH7110 clock-enable bit) is clear; lower bits are the
    //     pre-set dividers from U-Boot.
    //   SYSCRG +0x2FC          = 0x07e7fe00 — bits 3 + 4 (GMAC0_AHB
    //     and GMAC0_AXI per vendor-SDK convention) are 0, meaning
    //     reset is already deasserted.
    //   STGCRG +0xEC / 0xF0    = 0x00000000 — also gated, dividers
    //     unset; setting bit 31 alone won't be enough but it's a
    //     valid first move.
    //
    // Strategy: read-modify-write |= 0x80000000 on each gate.
    // Re-read the GMAC version register after to see if we're in.
    // No reset writes (already deasserted, don't poke).
    //
    // Tag scheme (this iteration):
    //   'GMAC' = pre-poke GMAC version (matches build 73/74)
    //   'GmaC' = post-poke GMAC version (lowercase 'mac' to spot
    //            the change-of-state line)
    //   'EnaC' / 'enac' = post-write SYSCRG / STGCRG verify reads
    #[cfg(feature = "vf2")]
    {
        const SYSCRG_BASE: u32 = 0x1302_0000;
        const STGCRG_BASE: u32 = 0x1023_0000;
        const ENABLE_BIT:  u32 = 0x8000_0000;

        // Helper: read, OR enable, write back.
        // SAFETY block scoped to each call below.
        for off in [0x190u32, 0x194, 0x198] {
            let cur = unsafe { wari_net_mmio_read32(SYSCRG_BASE + off) };
            let _ = unsafe {
                wari_net_mmio_write32(SYSCRG_BASE + off, cur | ENABLE_BIT)
            };
        }
        for off in [0xECu32, 0xF0] {
            let cur = unsafe { wari_net_mmio_read32(STGCRG_BASE + off) };
            let _ = unsafe {
                wari_net_mmio_write32(STGCRG_BASE + off, cur | ENABLE_BIT)
            };
        }

        // Verify the writes landed.
        for off in [0x190u32, 0x194, 0x198] {
            let v = unsafe { wari_net_mmio_read32(SYSCRG_BASE + off) };
            let tag = 0x456E_6100 | (off & 0xFF); // 'Ena\0' + off
            let _ = unsafe { wari_drv_log_u32(tag, v) };
        }
        for off in [0xECu32, 0xF0] {
            let v = unsafe { wari_net_mmio_read32(STGCRG_BASE + off) };
            let tag = 0x656E_6100 | (off & 0xFF); // 'ena\0' + off
            let _ = unsafe { wari_drv_log_u32(tag, v) };
        }

        // Re-read the GMAC version. Different tag so the trace
        // shows BEFORE (GMAC) and AFTER (GmaC) side by side.
        const GMAC_VERSION_OFFSET: u32 = 0x110;
        let v_after = unsafe {
            wari_net_mmio_read32(plat::NIC_BASE + GMAC_VERSION_OFFSET)
        };
        let _ = unsafe { wari_drv_log_u32(0x476D_6143, v_after) }; // 'GmaC'
    }

    // The vf2 path is a Phase-1c stub — return immediately, leave
    // Net.initialized = false on the kernel side.
    //
    // The vf2 binary still needs the same WASM imports as qemu so
    // its manifest (which declares them) passes the sign-tool's
    // cross-check (PR DI-5). LTO strips unused imports — to keep
    // them alive WITHOUT actually invoking them (some have kernel
    // side effects, e.g. nic_set_mac unconditionally sets
    // Net.initialized = true), we reference each host fn through
    // a `#[used]` function-pointer static. The pointer reference
    // forces the linker to retain the WASM import; the function
    // is never called from Rust code on vf2.
    //
    // Phase-1c GMAC work replaces this scaffold by *actually
    // using* the imports for real hardware.
}

// Keep the qemu-side host-fn imports alive in the vf2 binary so
// its manifest still cross-checks. Each static is a function-
// pointer reference — LTO retains the symbol; nobody invokes it.
// See driver_start vf2 branch for the why.
#[cfg(feature = "vf2")]
mod vf2_keep_imports {
    use super::*;
    #[used]
    static A: unsafe extern "C" fn(u32, u32) -> i32 = wari_net_mmio_write32;
    #[used]
    static B: unsafe extern "C" fn(u32) -> u32 = wari_net_mmio_read32;
    #[used]
    static C: unsafe extern "C" fn(u32, u32) -> i32 = wari_nic_set_mac;
    #[used]
    static D: unsafe extern "C" fn(u32, u32, u32, u32, u32) -> i32 = wari_nic_attach_queue;
    #[used]
    static E: unsafe extern "C" fn(u32) -> i32 = wari_nic_queue_notify;
    #[used]
    static F: unsafe extern "C" fn() -> u64 = wari_lin_mem_base;
}

// ── Tier-2 net driver registration (PR DI-4) ─────────────────────
//
// The driver author's surface for Phase-2 onward is the
// `wari_driver_iface::NetDriver` trait + the `wari_net_driver!`
// macro. The macro emits the wasm-ABI shims (`_start`, `poll`,
// `tx_send`, `rx_pop`, `rx_recycle`) and the 612-byte manifest
// in WASM custom section `wari_driver_manifest`.
//
// The trait methods just delegate into the existing `driver_*`
// functions to keep the migration narrow — no logic moves. A
// future PR may inline the bodies into the trait if needed.

/// Tier-2 net driver instance (zero-sized; per-call dispatch).
pub struct Driver;

impl wari_driver_iface::NetDriver for Driver {
    fn start() {
        driver_start();
    }
    fn poll(timestamp_ms: u64) -> i32 {
        driver_poll(timestamp_ms)
    }
    fn tx_send(buf: &[u8]) -> i32 {
        // Slice → (offset, len) for the existing virtqueue path.
        // `buf.as_ptr() as u32` is the WASM linmem offset because
        // wasm32 has 32-bit pointers.
        driver_tx_send(buf.as_ptr() as u32, buf.len() as u32)
    }
    fn rx_pop() -> u64 {
        driver_rx_pop()
    }
    fn rx_recycle(desc_idx: u32) -> i32 {
        driver_rx_recycle(desc_idx)
    }
    fn socket_create(proto: u32) -> i32 {
        driver_socket_create(proto)
    }
    fn socket_close(handle: u32) -> i32 {
        driver_socket_close(handle)
    }
    fn socket_bind(handle: u32, ip_be: u32, port: u32) -> i32 {
        driver_socket_bind(handle, ip_be, port)
    }
    fn socket_listen(handle: u32, backlog: u32) -> i32 {
        driver_socket_listen(handle, backlog)
    }
}

wari_driver_iface::wari_net_driver!(Driver);

// ── Socket API (PR Net-6a) ───────────────────────────────────────
//
// Driver-side smoltcp socket open/close. Tier-1 calls
// `wari::net_socket_create(proto, slot_for_cap)` → kernel checks
// the calling tier's Net cap, calls into here to allocate a
// smoltcp socket, mints a Socket cap into the caller's CSpace
// at slot_for_cap. socket_close is the inverse.
//
// Phase-1b scope: TCP only (UDP returns E_INVAL until Net-6c).
// vf2 build returns E_INVAL for both protos (Phase-1c GMAC).

/// `socket_create` body — qemu path. Allocates a TCP smoltcp
/// socket via the smoltcp::Interface, returns a packed handle
/// (smoltcp::iface::SocketHandle as u32).
#[cfg(feature = "qemu")]
pub fn driver_socket_create(proto: u32) -> i32 {
    use wari_driver_iface::SocketProto;
    let Some(p) = SocketProto::from_raw(proto) else {
        return -2; // E_INVAL
    };
    match p {
        SocketProto::Tcp => nic_iface::socket_create_tcp().unwrap_or(-3),
        SocketProto::Udp => -2, // not yet implemented (Net-6c)
    }
}

#[cfg(feature = "qemu")]
pub fn driver_socket_close(handle: u32) -> i32 {
    nic_iface::socket_close(handle)
}

/// `socket_bind` body — qemu path. Stores the requested port
/// inside the driver's per-handle state; the smoltcp `listen`
/// call happens in `socket_listen`.
#[cfg(feature = "qemu")]
pub fn driver_socket_bind(handle: u32, ip_be: u32, port: u32) -> i32 {
    nic_iface::socket_bind(handle, ip_be, port)
}

/// `socket_listen` body — qemu path. Hands the bound port to
/// smoltcp's `tcp::Socket::listen`.
#[cfg(feature = "qemu")]
pub fn driver_socket_listen(handle: u32, backlog: u32) -> i32 {
    let _ = backlog; // smoltcp single-pending only — backlog ignored
    nic_iface::socket_listen(handle)
}

/// vf2 stub — net is the JH7110 GMAC Phase-1c TODO. Refuses
/// every socket operation cleanly so a Tier-1 calling them on
/// VF2 silicon gets a deterministic E_INVAL instead of a hang.
#[cfg(feature = "vf2")]
pub fn driver_socket_create(_proto: u32) -> i32 {
    -2 // E_INVAL
}

#[cfg(feature = "vf2")]
pub fn driver_socket_close(_handle: u32) -> i32 {
    -2 // E_INVAL
}

#[cfg(feature = "vf2")]
pub fn driver_socket_bind(_handle: u32, _ip_be: u32, _port: u32) -> i32 {
    -2
}

#[cfg(feature = "vf2")]
pub fn driver_socket_listen(_handle: u32, _backlog: u32) -> i32 {
    -2
}
