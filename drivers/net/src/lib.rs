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

#[cfg(feature = "net-diag")]
mod diag;

/// Embedded build-number tag — the kernel's `build.rs` greps the
/// signed wasm for this string and refuses to compile if it doesn't
/// match the kernel's own `WARI_BUILD`. Catches the stale-driver
/// class of bug (builds 107..114 silently shipped the 106 driver
/// because a RISC-V `fence` instruction broke the wasm build but
/// cargo kept using the last-known-good artifact).
///
/// `#[used]` keeps the linker from stripping the symbol even though
/// nothing in the wasm references it; the string still lands in
/// rodata and `strings(1)` can find it.
#[used]
#[no_mangle]
pub static WARI_DRV_BUILD_TAG: &[u8] = concat!(
    "WARI-DRV-BUILD-TAG-",
    env!("WARI_BUILD"),
).as_bytes();

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
    /// JH7110 GMAC base on VisionFive 2. The `gmac1` cargo feature
    /// switches between GMAC0 (eth0/end0, AON power domain) and
    /// GMAC1 (eth1/end1, SYS power domain) — see Phase-1c-11 plan.
    #[allow(dead_code)]
    #[cfg(not(feature = "gmac1"))]
    pub const NIC_BASE: u32 = 0x1603_0000;
    #[allow(dead_code)]
    #[cfg(feature = "gmac1")]
    pub const NIC_BASE: u32 = 0x1604_0000;

    /// MDIO address of the on-board YT8531C PHY. From the StarFive
    /// BSP DT (`jh7110-starfive-visionfive-2.dtsi`):
    ///   &gmac0 { phy0: ethernet-phy@0; }
    ///   &gmac1 { phy1: ethernet-phy@1; }
    /// The `@N` unit-address IS the MDIO reg per DT convention.
    /// Build 130 discovery: builds 125..129 hard-coded literal `0`
    /// at all 13 mdio_*_phy call sites, so GMAC1 builds were writing
    /// 0xA003/etc to a PHY address that doesn't exist on GMAC1's bus
    /// (or — worse — that some BSP wires to an aliased response). The
    /// 4F51E91B reads we saw at addr 0 on GMAC1 builds were almost
    /// certainly GMAC0's PHY answering through a NIC_BASE mismatch in
    /// the trace window; with NIC_BASE confirmed at 0x16040000, addr 1
    /// is the only PHY on GMAC1's bus.
    #[allow(dead_code)]
    #[cfg(not(feature = "gmac1"))]
    pub const PHY_ADDR: u32 = 0;
    #[allow(dead_code)]
    #[cfg(feature = "gmac1")]
    pub const PHY_ADDR: u32 = 1;
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

// PR Phase-1c-6e — VF2 GMAC0 DMA descriptor rings.
// 16 descriptors × 16 bytes = 256 B each. repr(C, align(16))
// satisfies the DWMAC4 16-byte alignment requirement; the
// physical address handed to the DMA engine is `lin_mem_base()
// + (&VF2_TX_RING.descs[0] as u32)`.
#[cfg(feature = "vf2")]
#[repr(C, align(16))]
pub struct VF2DmaRing {
    pub descs: [[u32; 4]; 16],
}

#[cfg(feature = "vf2")]
#[no_mangle]
pub static mut VF2_TX_RING: VF2DmaRing = VF2DmaRing {
    descs: [[0u32; 4]; 16],
};

#[cfg(feature = "vf2")]
#[no_mangle]
pub static mut VF2_RX_RING: VF2DmaRing = VF2DmaRing {
    descs: [[0u32; 4]; 16],
};

/// PR Phase-1c-7 — TX buffer pool for the vf2 GMAC0 path. Mirrors
/// the RX pool: 16 × 1536 byte buffers, each bound to one entry
/// in VF2_TX_RING. The smoltcp Device::transmit token writes into
/// the next slot, hands the buffer to smoltcp's `consume(len, f)`
/// closure, then publishes the descriptor to the DMA engine and
/// bumps the TX tail pointer.
#[cfg(feature = "vf2")]
#[repr(C, align(64))]
pub struct VF2TxBuffers {
    pub bufs: [[u8; 1536]; 16],
}

#[cfg(feature = "vf2")]
#[no_mangle]
pub static mut VF2_TX_BUFS: VF2TxBuffers = VF2TxBuffers {
    bufs: [[0u8; 1536]; 16],
};

/// VF2 driver-side state owned by the smoltcp Device impl. Set
/// once at boot from `driver_start`'s vf2 branch; read by every
/// poll/transmit call.
#[cfg(feature = "vf2")]
pub mod vf2_state {
    /// Linear-memory physical-base reported by the kernel via
    /// `wari_lin_mem_base()`. Added to wasm32 linmem offsets to
    /// produce kernel-visible PAs for descriptor pointers.
    pub static mut LIN_BASE: u64 = 0;
    /// Round-robin TX descriptor index. Wraps at 16.
    pub static mut TX_NEXT: usize = 0;
    /// Round-robin RX descriptor index for the receive walk.
    pub static mut RX_NEXT: usize = 0;
    /// Build-110 wasmi-tolerant fix: index of the RX slot we
    /// yielded to smoltcp on the previous `receive()` call. By the
    /// time `receive()` is called again, smoltcp has finished with
    /// the slot (consume returned + token dropped), so it's safe
    /// to re-arm. `usize::MAX` = no slot pending re-arm. This
    /// bypasses the Drop-impl path entirely in case wasmi/the
    /// Rust→wasm pipeline isn't synthesizing Drop calls correctly.
    pub static mut PREV_YIELDED: usize = usize::MAX;
    /// Build-112 diagnostic: counter of receive() entries. Used to
    /// throttle a high-frequency log line so we don't drown the
    /// UART but still get visibility into the hot path.
    pub static mut RX_CALL_COUNT: u64 = 0;

    // ── Build-118 cumulative counters (see docs/diagnostic-tags.md) ──
    //
    // Bumped on each event; periodically dumped via the `St**` tag
    // family. If `StCc` stays at zero after 30s of ping you know
    // smoltcp isn't consuming — without the per-packet log spam that
    // builds 107..114 needed to chase the same question.
    pub static mut C_RECEIVE_CALLS: u32 = 0;
    pub static mut C_FRAMES_FOUND: u32 = 0;
    pub static mut C_CONSUME_CALLS: u32 = 0;
    pub static mut C_DROP_CALLS: u32 = 0;
    pub static mut C_REARM_CALLS: u32 = 0;
    pub static mut C_TX_SENT: u32 = 0;

    /// Change-detection memory for the `dPrb` probe. Initial sentinel
    /// (`0xDEAD_BEEF`) forces the first sample to log so we get a
    /// baseline. After that we only log when the value flips, which
    /// turns a stuck-at-MAX bug from 100k log lines/sec into one.
    pub static mut LAST_PREV_YIELDED_LOGGED: u32 = 0xDEAD_BEEF;
}

/// PR Phase-1c-6g — RX buffers, one per descriptor.
///
/// 16 buffers × 1536 bytes = 24 KiB. Each RX descriptor in
/// VF2_RX_RING points to one of these slots; the GMAC writes
/// incoming frames into them and clears the OWN bit when done.
///
/// 1536 B = standard Ethernet MTU (1500) + headroom for VLAN
/// tag + alignment. DMA_CH0_RX_CONTROL.RBSZ tells the engine
/// this is the per-descriptor buffer size.
#[cfg(feature = "vf2")]
#[repr(C, align(64))]
pub struct VF2RxBuffers {
    pub bufs: [[u8; 1536]; 16],
}

#[cfg(feature = "vf2")]
#[no_mangle]
pub static mut VF2_RX_BUFS: VF2RxBuffers = VF2RxBuffers {
    bufs: [[0u8; 1536]; 16],
};

/// PR Phase-1c-6f — first-packet TX buffer.
///
/// Holds a 64-byte broadcast ARP request so the first frame
/// the GMAC ever transmits is meaningful traffic (a switch
/// will respond, a Wireshark on a mirror port will recognise
/// the protocol). Pre-filled at module scope so the descriptor
/// just points at it.
///
/// Wire format (broadcast ARP "who-has 192.168.122.1?"):
///   00..05  dst MAC = ff:ff:ff:ff:ff:ff (broadcast)
///   06..0B  src MAC = 6c:cf:39:00:40:84 (VF2 MAC0 from EEPROM)
///   0C..0D  ethertype = 0x0806 (ARP)
///   0E..0F  HTYPE = 0x0001 (Ethernet)
///   10..11  PTYPE = 0x0800 (IPv4)
///   12      HLEN  = 6
///   13      PLEN  = 4
///   14..15  OPER  = 0x0001 (request)
///   16..1B  SHA = 6c:cf:39:00:40:84
///   1C..1F  SPA = 192.168.122.10
///   20..25  THA = 00 00 00 00 00 00
///   26..29  TPA = 192.168.122.1
///   2A..3F  zero pad to 64 bytes
#[cfg(feature = "vf2")]
#[no_mangle]
pub static VF2_FIRST_PKT: [u8; 64] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,             // dst broadcast
    0x6C, 0xCF, 0x39, 0x00, 0x40, 0x84,             // src VF2 MAC0
    0x08, 0x06,                                     // ethertype ARP
    0x00, 0x01,                                     // HTYPE Ethernet
    0x08, 0x00,                                     // PTYPE IPv4
    0x06, 0x04,                                     // HLEN/PLEN
    0x00, 0x01,                                     // OPER request
    0x6C, 0xCF, 0x39, 0x00, 0x40, 0x84,             // SHA
    0xC0, 0xA8, 0x7A, 0x0A,                         // SPA 192.168.122.10
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00,             // THA
    0xC0, 0xA8, 0x7A, 0x01,                         // TPA 192.168.122.1
    // pad to 64
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,
];

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

// Platform-neutral as of PR Phase-1c-7: smoltcp `Interface`,
// `SocketSet`, the socket buffer pool, and the public surface
// (init / poll / socket_create_tcp / socket_bind / socket_listen
// / socket_close) are identical across qemu and vf2. The ONLY
// platform-specific piece is which `NicDevice` we attach — the
// qemu module reads/writes virtio-net rings, the vf2 module
// reads/writes JH7110 GMAC0 DMA descriptor rings.
mod nic_iface {
    use core::ptr::addr_of_mut;
    use smoltcp::iface::{Config, Interface, SocketSet, SocketStorage};
    use smoltcp::time::Instant;
    use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr};

    #[cfg(feature = "qemu")]
    use super::phy::NicDevice;
    #[cfg(feature = "vf2")]
    use super::vf2_phy::NicDevice;

    /// Phase-1b QEMU demo IP (per net design doc §10 Q1). QEMU
    /// slirp's default subnet is 192.168.122.0/24 with gateway
    /// 192.168.122.1; we take 192.168.122.10.
    // Phase-1c-7 / build 106: back to operator's home Wi-Fi subnet
    // (192.168.100.0/24). Direct-cable test (build 105 / 10.42.0.10)
    // didn't work — USB-Ethernet adapter wasn't delivering frames.
    // Router-based path proven to deliver frames to RX ring.
    // Build 120: isolated test subnet on the spare OpenWrt router.
    // Avoids collision with the operator's home Wi-Fi (192.168.100/24)
    // so Wi-Fi can stay up for client work while the USB-Ethernet
    // adapter handles the test subnet.
    const IP_OCTETS: [u8; 4] = [192, 168, 50, 10];
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
    /// Per-slot smoltcp handle. `None` slot = free buffer pair.
    /// Build 119: replaced an `Option<u32>` + mem::transmute round-
    /// trip with the typed `SocketHandle` directly. The userspace-
    /// visible handle is now the slot index (0..SOCKET_BACKING_LEN),
    /// which is just as opaque and doesn't depend on smoltcp's
    /// internal layout staying repr(transparent) across patch
    /// releases.
    static mut SOCKET_HANDLE_FOR_BUF: [Option<smoltcp::iface::SocketHandle>;
        SOCKET_BACKING_LEN] = [None; SOCKET_BACKING_LEN];
    /// Per-buffer-slot bound port (set by socket_bind, consumed
    /// by socket_listen). 0 = unbound.
    static mut SOCKET_BOUND_PORT: [u16; SOCKET_BACKING_LEN] =
        [0u16; SOCKET_BACKING_LEN];

    /// Monotonic counter used as smoltcp timestamp. Incremented on
    /// every poll() call so TCP retry timers advance even without a
    /// real wall-clock source. Not wall-clock accurate — sufficient
    /// for the Phase-1c demo where ARP/SYN latency << retry window.
    static mut POLL_TICK: u64 = 0;

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
            SOCKET_HANDLE_FOR_BUF[slot] = Some(handle);
            // Userspace handle = slot index. Opaque enough; doesn't
            // leak smoltcp's internal layout. Reverse lookup is
            // just SOCKET_HANDLE_FOR_BUF[slot].
            Ok(slot as i32)
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
            let slot = raw_handle as usize;
            if slot >= SOCKET_BACKING_LEN || SOCKET_HANDLE_FOR_BUF[slot].is_none() {
                return -2;
            }
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
            let slot = raw_handle as usize;
            if slot >= SOCKET_BACKING_LEN {
                return -2;
            }
            let handle = match SOCKET_HANDLE_FOR_BUF[slot] {
                Some(h) => h,
                None => return -2,
            };
            let port = SOCKET_BOUND_PORT[slot];
            if port == 0 {
                return -2; // not bound
            }
            let socket = sockets.get_mut::<tcp::Socket>(handle);
            match socket.listen(port) {
                Ok(()) => 0,
                Err(_) => -3,
            }
        }
    }

    /// Phase-1c HTTP demo — report whether the listening socket has
    /// accepted a connection. Pure state inspection — the kernel
    /// drives `nic_iface::poll` on either side of this call so the
    /// smoltcp state machine advances before/after.
    ///
    /// smoltcp's accept model is not POSIX-shaped: the SAME socket
    /// transitions from `Listen` to `Established` when a remote SYN
    /// arrives. There is no "accept-creates-new-socket" primitive
    /// without an explicit re-listen. For the Phase-1c demo (one
    /// connection per boot, served then closed) the simpler model
    /// suffices.
    ///
    /// Returns:
    /// - `1` if the socket is in `Established` and can accept a
    ///   write (ready for `socket_send_canned` + `socket_close`),
    /// - `0` if still listening / waiting,
    /// - `-2` if the handle is unknown or the socket is in an
    ///   unexpected state.
    pub fn socket_poll_accept(raw_handle: u32) -> i32 {
        use smoltcp::socket::tcp;
        // SAFETY: single-thread driver (INV-1 generalized).
        unsafe {
            // Drive smoltcp before inspecting socket state. Without this
            // poll, the stack never processes incoming ARP or TCP SYNs —
            // the socket stays in Listen forever even when the peer has
            // already sent a SYN. POLL_TICK is a monotonic counter that
            // serves as the smoltcp timestamp; TCP retry timers advance
            // proportionally to poll rate rather than wall clock, which is
            // sufficient for the Phase-1c demo.
            POLL_TICK = POLL_TICK.wrapping_add(1);
            poll(POLL_TICK);

            let sockets = match &mut *addr_of_mut!(SOCKETS) {
                Some(s) => s,
                None => return -2,
            };
            let slot = raw_handle as usize;
            if slot >= SOCKET_BACKING_LEN {
                return -2;
            }
            let handle = match SOCKET_HANDLE_FOR_BUF[slot] {
                Some(h) => h,
                None => return -2,
            };
            let socket = sockets.get_mut::<tcp::Socket>(handle);
            if socket.is_active() && socket.may_send() {
                1
            } else {
                0
            }
        }
    }

    /// Phase-1c HTTP demo — send a hardcoded HTTP/1.0 200 OK reply on
    /// a connected socket. Pure write — the kernel drives
    /// `nic_iface::poll` after this call so the queued reply hits
    /// the wire.
    ///
    /// Body is `b"Hello from Wari\n"`. Header declares
    /// `Content-Length: 16` and `Connection: close`. Total 96 bytes
    /// — fits comfortably in one TCP segment on any MTU.
    ///
    /// Returns:
    /// - bytes queued for transmit on success (the full reply if the
    ///   socket's tx buffer accepted it),
    /// - `-2` if the handle is unknown or the socket is not in a
    ///   sendable state,
    /// - `-3` if smoltcp's `send_slice` returns Err.
    ///
    /// After this call the caller should `socket_close` to release
    /// the smoltcp slot. smoltcp will already have queued FIN
    /// (via `socket.close()`) so the remote sees a clean teardown.
    ///
    /// Phase-1c demo only: a real HTTP server uses generic
    /// `socket_send` taking a Tier-1 lin-mem buffer. That ABI is
    /// queued for after the demo confirms the path works end-to-end.
    pub fn socket_send_canned(raw_handle: u32) -> i32 {
        use smoltcp::socket::tcp;

        const REPLY: &[u8] = b"HTTP/1.0 200 OK\r\n\
            Content-Type: text/plain\r\n\
            Content-Length: 16\r\n\
            Connection: close\r\n\
            \r\n\
            Hello from Wari\n";

        // SAFETY: single-thread driver (INV-1 generalized).
        unsafe {
            let sockets = match &mut *addr_of_mut!(SOCKETS) {
                Some(s) => s,
                None => return -2,
            };
            let slot = raw_handle as usize;
            if slot >= SOCKET_BACKING_LEN {
                return -2;
            }
            let handle = match SOCKET_HANDLE_FOR_BUF[slot] {
                Some(h) => h,
                None => return -2,
            };
            let socket = sockets.get_mut::<tcp::Socket>(handle);
            if !socket.may_send() {
                return -2;
            }
            let queued = match socket.send_slice(REPLY) {
                Ok(n) => n as i32,
                Err(_) => return -3,
            };
            // Signal end-of-response so smoltcp emits FIN after the
            // reply drains. The remote sees a clean close.
            socket.close();
            // Poll immediately so the queued reply + FIN are handed to
            // the NIC DMA in this same call, rather than waiting for the
            // next accept-loop iteration.
            POLL_TICK = POLL_TICK.wrapping_add(1);
            poll(POLL_TICK);
            queued
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
            let slot = raw_handle as usize;
            if slot >= SOCKET_BACKING_LEN {
                return -2;
            }
            let handle = match SOCKET_HANDLE_FOR_BUF[slot] {
                Some(h) => h,
                None => return -2, // E_INVAL: unknown handle
            };
            sockets.remove(handle);
            SOCKET_HANDLE_FOR_BUF[slot] = None;
            SOCKET_BOUND_PORT[slot] = 0;
            0
        }
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
    // PR Phase-1c-7 — both platforms share the same `nic_iface`
    // entrypoint now that vf2 has its own NicDevice impl.
    if nic_iface::poll(timestamp_ms) {
        1
    } else {
        0
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

// ── VF2 GMAC0 smoltcp Device impl (PR Phase-1c-7) ────────────────
//
// Mirror of the qemu `phy` module above, but reads from the JH7110
// GMAC0 DMA descriptor rings instead of VirtIO virtqueues.
// Smoltcp's `Interface::poll` calls into this through the `Device`
// trait the same way it does for qemu — the upper-layer code in
// `nic_iface` is platform-neutral.

#[cfg(feature = "vf2")]
pub mod vf2_phy {
    use super::{
        vf2_state, wari_net_mmio_write32, VF2_RX_BUFS, VF2_RX_RING,
        VF2_TX_BUFS, VF2_TX_RING,
    };
    use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
    use smoltcp::time::Instant;

    /// Zero-sized Device handle. All state lives in module-level
    /// statics (`VF2_*_RING`, `VF2_*_BUFS`, `vf2_state::*`); the
    /// struct is just the type smoltcp can name.
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

    /// Standard Ethernet MTU. The GMAC accepts up to 1518 (1500
    /// + Ethernet header); smoltcp passes us payload up to MTU.
    const SMOLTCP_MTU: usize = 1500;

    /// JH7110 GMAC base address (mirror of `super::plat::NIC_BASE`
    /// for vf2 builds — re-declared locally to keep this module
    /// self-contained). Gated by the `gmac1` feature.
    #[cfg(not(feature = "gmac1"))]
    const GMAC_BASE: u32 = 0x1603_0000;
    #[cfg(feature = "gmac1")]
    const GMAC_BASE: u32 = 0x1604_0000;
    const DMA_CH0_TX_TAIL: u32 = 0x1120;

    /// DWMAC4 RDES3 status bits.
    const RDES3_OWN: u32 = 0x8000_0000;

    /// DWMAC4 TDES3 — OWN | LD | FD set with packet length in low 15.
    const TDES3_OWN: u32 = 0x8000_0000;
    const TDES3_LD:  u32 = 0x2000_0000;
    const TDES3_FD:  u32 = 0x1000_0000;

    impl Device for NicDevice {
        type RxToken<'a> = Vf2NicRxToken;
        type TxToken<'a> = Vf2NicTxToken;

        fn capabilities(&self) -> DeviceCapabilities {
            let mut caps = DeviceCapabilities::default();
            caps.medium = Medium::Ethernet;
            caps.max_transmission_unit = SMOLTCP_MTU;
            // GMAC offloads are off; smoltcp computes/verifies all
            // checksums.
            caps
        }

        fn receive(
            &mut self,
            _ts: Instant,
        ) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
            // Build-118: change-detection dPrb + cumulative counter
            // dump (see docs/diagnostic-tags.md). dPrb fires only
            // when PREV_YIELDED's value flips — stuck-at-MAX bugs
            // log once, then go silent until the value moves.
            // The 6-line stat burst fires every ~65k receive() calls
            // so we always have current event counters in the trace.
            unsafe {
                vf2_state::C_RECEIVE_CALLS = vf2_state::C_RECEIVE_CALLS.wrapping_add(1);
                vf2_state::RX_CALL_COUNT = vf2_state::RX_CALL_COUNT.wrapping_add(1);
                let py = vf2_state::PREV_YIELDED as u32;
                if py != vf2_state::LAST_PREV_YIELDED_LOGGED {
                    vf2_state::LAST_PREV_YIELDED_LOGGED = py;
                    let _ = super::wari_drv_log_u32(0x6450_7262, py);
                }
                if vf2_state::RX_CALL_COUNT & 0xFFFF == 0 {
                    let _ = super::wari_drv_log_u32(0x5374_5263, vf2_state::C_RECEIVE_CALLS);
                    let _ = super::wari_drv_log_u32(0x5374_5266, vf2_state::C_FRAMES_FOUND);
                    let _ = super::wari_drv_log_u32(0x5374_4363, vf2_state::C_CONSUME_CALLS);
                    let _ = super::wari_drv_log_u32(0x5374_4463, vf2_state::C_DROP_CALLS);
                    let _ = super::wari_drv_log_u32(0x5374_5261, vf2_state::C_REARM_CALLS);
                    let _ = super::wari_drv_log_u32(0x5374_5478, vf2_state::C_TX_SENT);
                }
            }
            // Build-110 wasmi-tolerant fix, retained: re-arm the
            // slot we yielded to smoltcp last time. By the time
            // receive() is called again, smoltcp has finished with
            // the slot (consume returned + token dropped). The
            // dPyR diagnostic tag is gone (build 119) — rXCn on
            // the next line of vf2_rx_rearm covers the same signal,
            // and StRa in the periodic dump counts these calls.
            unsafe {
                if vf2_state::PREV_YIELDED != usize::MAX {
                    let prev = vf2_state::PREV_YIELDED;
                    vf2_state::PREV_YIELDED = usize::MAX;
                    vf2_rx_rearm(prev);
                }
            }
            // Build-129 net-diag: 17-register RX-path snapshot every
            // ~32K calls. Branch-predictable: the early-return inside
            // covers 99.997% of calls. See drivers/net/src/diag.rs.
            #[cfg(feature = "net-diag")]
            super::diag::maybe_snapshot(GMAC_BASE);
            // Round-robin walk over the 16 RX descriptors looking
            // for one whose OWN bit was cleared by the DMA engine.
            // SAFETY: single-threaded driver; static muts are the
            // only data path. Module-static read of OWN bit is
            // atomic at the 32-bit-aligned word level on RISC-V.
            unsafe {
                let start = vf2_state::RX_NEXT;
                for n in 0..16usize {
                    let i = (start + n) % 16;
                    let rdes3 = VF2_RX_RING.descs[i][3];
                    if rdes3 & RDES3_OWN == 0 {
                        // Frame received in slot i. Length is in
                        // RDES3 bits 14:0.
                        let len = (rdes3 & 0x7FFF) as u16;
                        vf2_state::RX_NEXT = (i + 1) % 16;
                        // Build-118: log rXFr with idx in val.b3 (top
                        // byte) and rdes3 in val.b2..b0. Old scheme
                        // OR'd idx into the tag's low nibble, which
                        // aliased pairs of slots (e.g. idx 0 & 2 both
                        // logged as 0x72 because 0x72 already had bit
                        // 1 set). See docs/diagnostic-tags.md.
                        let val = ((i as u32) << 24) | (rdes3 & 0x00FF_FFFF);
                        let _ = super::wari_drv_log_u32(0x7258_4672, val);
                        vf2_state::C_FRAMES_FOUND =
                            vf2_state::C_FRAMES_FOUND.wrapping_add(1);
                        // Build-129 net-diag: one-shot deep dump at the
                        // critical 0→1 frame-found transition. Internal
                        // guard makes subsequent calls no-ops.
                        #[cfg(feature = "net-diag")]
                        super::diag::note_first_frame(GMAC_BASE, i as u32);
                        // Stash the slot so the NEXT receive()
                        // call re-arms it (see top of fn).
                        vf2_state::PREV_YIELDED = i;
                        return Some((
                            Vf2NicRxToken { idx: i, len },
                            Vf2NicTxToken { idx: vf2_state::TX_NEXT },
                        ));
                    }
                }
            }
            None
        }

        fn transmit(&mut self, _ts: Instant) -> Option<Self::TxToken<'_>> {
            // SAFETY: same as above.
            unsafe {
                let i = vf2_state::TX_NEXT;
                let tdes3 = VF2_TX_RING.descs[i][3];
                if tdes3 & TDES3_OWN != 0 {
                    // DMA hasn't released this slot yet; back-pressure.
                    return None;
                }
                Some(Vf2NicTxToken { idx: i })
            }
        }
    }

    pub struct Vf2NicRxToken {
        idx: usize,
        len: u16,
    }

    /// Re-arm a single RX descriptor so DMA can write the next frame
    /// into its buffer. Idempotent — safe to call from both
    /// `consume` (the smoltcp-processed path) and `Drop` (the
    /// dropped-without-consume path). The `fence ow,ow` after the
    /// store ensures the U74 store buffer has flushed before the
    /// next poll re-reads RDES3 — JH7110 GMAC is IO-coherent for
    /// DDR, but a CPU-local store buffer can hide the update.
    /// Also logs `rXCn`+idx so future traces can distinguish the
    /// consumed-vs-leaked descriptor cases.
    fn vf2_rx_rearm(idx: usize) {
        unsafe {
            // Bump counter for the periodic StRa stat dump. The
            // earlier per-step rRaE/rRaB/rRaW/rRaX saturation tags
            // are gone (build 119) — they were diagnostic crutches
            // from the stale-driver hunt and the counters subsume
            // them now. rXCn + rXTl below still emit per-event so
            // operators can see EACH rearm in the trace if they
            // need to.
            vf2_state::C_REARM_CALLS = vf2_state::C_REARM_CALLS.wrapping_add(1);
            let bp: u64 = vf2_state::LIN_BASE
                + (core::ptr::addr_of!(VF2_RX_BUFS.bufs[idx]) as u32) as u64;
            let d = &mut VF2_RX_RING.descs[idx];
            d[0] = bp as u32;
            d[1] = (bp >> 32) as u32;
            d[2] = 0;
            d[3] = 0xC100_0000; // OWN | IOC | BUF1V
            // Re-kick the RX_TAIL doorbell so DWMAC4 walks the
            // descriptor we just rearmed. The host-fn call crosses
            // the wasm→native boundary and naturally serializes,
            // so no explicit fence is needed (and inline asm wouldn't
            // compile to wasm anyway — that's how builds 107..114
            // silently shipped a stale driver).
            let rx_ring_off = core::ptr::addr_of!(VF2_RX_RING.descs) as u32;
            let rx_tail_pa: u32 =
                (vf2_state::LIN_BASE + rx_ring_off as u64 + 16 * 16) as u32;
            let _ = wari_net_mmio_write32(GMAC_BASE + 0x1128, rx_tail_pa);
            // tag = 'rXTl' — RX tail doorbell write.
            let _ = super::wari_drv_log_u32(0x7258_546C, rx_tail_pa);
            // tag = 'rXCn' — descriptor re-armed. val = slot idx.
            let _ = super::wari_drv_log_u32(0x7258_434E, idx as u32);
        }
    }

    impl RxToken for Vf2NicRxToken {
        fn consume<R, F>(mut self, f: F) -> R
        where
            F: FnOnce(&mut [u8]) -> R,
        {
            // Build-109 bracket: 'rXCe' (consume entry) — fires
            // before the closure runs, so we can tell consume from
            // Drop and prove smoltcp is actually processing the
            // frame rather than silently leaking the token.
            unsafe {
                vf2_state::C_CONSUME_CALLS = vf2_state::C_CONSUME_CALLS.wrapping_add(1);
                let _ = super::wari_drv_log_u32(0x7258_4365, self.idx as u32);
            }
            // SAFETY: single-threaded driver; this slot's buffer is
            // exclusively ours until we re-arm the descriptor below.
            let result = unsafe {
                let buf = &mut VF2_RX_BUFS.bufs[self.idx][..self.len as usize];
                f(buf)
            };
            vf2_rx_rearm(self.idx);
            // Mark as already-recycled so Drop is a no-op.
            self.idx = usize::MAX;
            result
        }
    }

    /// If smoltcp drops the RxToken without calling `consume` (it
    /// can do this when no socket is interested in the frame, or
    /// on certain error paths), the descriptor would leak with
    /// OWN=0 forever, eventually freezing the RX ring after the
    /// 16th leak. Drop guarantees re-arm in those cases too.
    impl Drop for Vf2NicRxToken {
        fn drop(&mut self) {
            // Build-109 bracket: 'rXDr' (drop entry) — fires
            // every time Rust drops the token, including the
            // already-consumed case where idx == usize::MAX.
            unsafe {
                vf2_state::C_DROP_CALLS = vf2_state::C_DROP_CALLS.wrapping_add(1);
                let _ = super::wari_drv_log_u32(0x7258_4472, self.idx as u32);
            }
            if self.idx != usize::MAX {
                vf2_rx_rearm(self.idx);
            }
        }
    }

    pub struct Vf2NicTxToken {
        idx: usize,
    }

    impl TxToken for Vf2NicTxToken {
        fn consume<R, F>(self, len: usize, f: F) -> R
        where
            F: FnOnce(&mut [u8]) -> R,
        {
            let i = self.idx;

            // SAFETY: TX_BUFS[i] is exclusively ours until we
            // publish via the descriptor write below.
            let result = unsafe {
                let buf = &mut VF2_TX_BUFS.bufs[i][..len];
                f(buf)
            };

            // Build-118: clean tag (idx in val.b3), bump TX counter
            // for the periodic StTx stat dump.
            unsafe {
                vf2_state::C_TX_SENT = vf2_state::C_TX_SENT.wrapping_add(1);
                let val = ((i as u32) << 24) | ((len as u32) & 0x00FF_FFFF);
                let _ = super::wari_drv_log_u32(0x7458_5472, val);
            }

            // Publish: descriptor + bump tail.
            // SAFETY: same scoping invariants.
            unsafe {
                let bp: u64 = vf2_state::LIN_BASE
                    + (core::ptr::addr_of!(VF2_TX_BUFS.bufs[i]) as u32) as u64;
                let d = &mut VF2_TX_RING.descs[i];
                d[0] = bp as u32;
                d[1] = (bp >> 32) as u32;
                d[2] = len as u32; // TDES2 buffer-1 length
                d[3] = TDES3_OWN | TDES3_LD | TDES3_FD | (len as u32 & 0x7FFF);

                // Round-robin advance.
                let next = (i + 1) % 16;
                vf2_state::TX_NEXT = next;

                // Tail = address of next descriptor (DWMAC processes
                // descriptors while CURR < TAIL). Store fence first
                // so DMA sees our descriptor write before the kick.
                core::sync::atomic::compiler_fence(
                    core::sync::atomic::Ordering::SeqCst,
                );
                let tail_pa: u32 = (vf2_state::LIN_BASE
                    + (core::ptr::addr_of!(VF2_TX_RING.descs[next]) as u32) as u64)
                    as u32;
                let _ = wari_net_mmio_write32(GMAC_BASE + DMA_CH0_TX_TAIL, tail_pa);
            }
            result
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
/// PR Phase-1c-4 — DWMAC MDIO Clause-22 read.
///
/// Encodes a (PHY addr, register) tuple into MAC_MDIO_ADDRESS
/// (GMAC0 + 0x200), kicks the busy bit, polls for completion, and
/// returns the low 16 bits of MAC_MDIO_DATA (GMAC0 + 0x204) as a
/// `u32`. Times out after ~100k spin iterations and returns
/// `0xFFFF_FFFE` so the trace makes a timeout obvious vs a
/// floating-bus `0xFFFF_FFFF`.
#[cfg(feature = "vf2")]
fn mdio_read_phy(gmac_base: u32, phy_addr: u32, reg: u32) -> u32 {
    const MAC_MDIO_ADDRESS_OFFSET: u32 = 0x200;
    const MAC_MDIO_DATA_OFFSET:    u32 = 0x204;
    const GB: u32                      = 1 << 0;
    const GOC_READ: u32                = 0b11 << 2; // Clause-22 read
    const CR_CSR_DIV_26: u32           = 4 << 8;    // CSR/26 — safe default

    let cmd = GB
        | GOC_READ
        | CR_CSR_DIV_26
        | ((reg & 0x1F) << 16)
        | ((phy_addr & 0x1F) << 21);

    // SAFETY: extern host fn into the kernel's net_mmio surface.
    let _ = unsafe {
        wari_net_mmio_write32(gmac_base + MAC_MDIO_ADDRESS_OFFSET, cmd)
    };

    // Poll busy bit. PHY responses settle in ~µs; cap iterations
    // generously so a stuck-busy doesn't hang the boot.
    let mut tries = 0u32;
    loop {
        // SAFETY: same.
        let s = unsafe {
            wari_net_mmio_read32(gmac_base + MAC_MDIO_ADDRESS_OFFSET)
        };
        if s & GB == 0 {
            break;
        }
        tries += 1;
        if tries > 100_000 {
            return 0xFFFF_FFFE;
        }
    }
    // SAFETY: same.
    let data = unsafe {
        wari_net_mmio_read32(gmac_base + MAC_MDIO_DATA_OFFSET)
    };
    data & 0xFFFF
}

/// PR Phase-1c-5 — DWMAC MDIO Clause-22 write.
///
/// Mirror of `mdio_read_phy`. Writes `value` (low 16 bits) into
/// the PHY register at `(phy_addr, reg)`. Same busy-poll + timeout
/// behaviour. Returns 0 on success, `-1` on timeout.
#[cfg(feature = "vf2")]
fn mdio_write_phy(gmac_base: u32, phy_addr: u32, reg: u32, value: u16) -> i32 {
    const MAC_MDIO_ADDRESS_OFFSET: u32 = 0x200;
    const MAC_MDIO_DATA_OFFSET:    u32 = 0x204;
    const GB: u32                      = 1 << 0;
    const GOC_WRITE: u32               = 0b01 << 2; // Clause-22 write
    const CR_CSR_DIV_26: u32           = 4 << 8;

    // SAFETY: extern host fn.
    let _ = unsafe {
        wari_net_mmio_write32(gmac_base + MAC_MDIO_DATA_OFFSET, value as u32)
    };

    let cmd = GB
        | GOC_WRITE
        | CR_CSR_DIV_26
        | ((reg & 0x1F) << 16)
        | ((phy_addr & 0x1F) << 21);
    // SAFETY: same.
    let _ = unsafe {
        wari_net_mmio_write32(gmac_base + MAC_MDIO_ADDRESS_OFFSET, cmd)
    };

    let mut tries = 0u32;
    loop {
        // SAFETY: same.
        let s = unsafe {
            wari_net_mmio_read32(gmac_base + MAC_MDIO_ADDRESS_OFFSET)
        };
        if s & GB == 0 {
            return 0;
        }
        tries += 1;
        if tries > 100_000 {
            return -1;
        }
    }
}

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

    // PR Phase-1c-3d — correct GMAC0 bring-up via AON CRG.
    //
    // Build 75/76 result + Linux-mainline cross-check (clk-starfive-
    // jh7110-aon.c) revealed the misdiagnosis: GMAC0's AHB/AXI/TX/RX
    // clock gates live in AON CRG (0x17000000), NOT SYSCRG. The
    // SYSCRG offsets 0x190/0x194/0x198 we poked earlier are GMAC1's
    // gates (id 100/101/102) — that's why bit 31 was being silently
    // dropped: their parent path was off, and GMAC1 is irrelevant
    // anyway. Reverting those misdirected writes — they had no effect
    // on GMAC0.
    //
    // Correct sequence (Linux mainline, AON CRG offsets):
    //   AONCRG +0x08 = gmac0_ahb gate (id 2)  -> set bit 31
    //   AONCRG +0x0C = gmac0_axi gate (id 3)  -> set bit 31
    //   AONCRG +0x38 = AON reset assert word  -> clear bits 0+1
    //                  (bit 0 = GMAC0_AXI rst, bit 1 = GMAC0_AHB rst).
    //                  AONCRG +0x3C is the reset status; deassert
    //                  is bootloader-default but write 0s anyway
    //                  to be definite.
    //
    // After: read GMAC0+0x110. If non-zero, we have first contact
    // with the DWMAC IP block on real silicon. JH7110 ships DWMAC
    // v5.20 → expected version byte 0x52 (or v5.10 → 0x51).
    #[cfg(feature = "vf2")]
    {
        const ENABLE_BIT: u32 = 0x8000_0000;

        // ── GMAC0 path: AON CRG gates + reset ───────────────────
        #[cfg(not(feature = "gmac1"))]
        {
            const AONCRG_BASE: u32 = 0x1700_0000;
            const AONRST_OFF:  u32 = 0x38;
            const GMAC0_AXI_AHB_RST_MASK: u32 = 0x3; // bits 0 and 1

            // Step 1: enable AHB clock gate.
            let _ = unsafe {
                wari_net_mmio_write32(AONCRG_BASE + 0x08, ENABLE_BIT)
            };
            // Step 2: enable AXI clock gate.
            let _ = unsafe {
                wari_net_mmio_write32(AONCRG_BASE + 0x0C, ENABLE_BIT)
            };
            // Step 3: deassert GMAC0 reset (clear bits 0+1).
            let rst_cur = unsafe { wari_net_mmio_read32(AONCRG_BASE + AONRST_OFF) };
            let _ = unsafe {
                wari_net_mmio_write32(AONCRG_BASE + AONRST_OFF,
                    rst_cur & !GMAC0_AXI_AHB_RST_MASK)
            };

            // Verify each write landed. Tags spell 'Aon0' / 'Aon1' /
            // 'Aon8' / 'AonR' so the trace shows what's at each offset.
            let v08 = unsafe { wari_net_mmio_read32(AONCRG_BASE + 0x08) };
            let _ = unsafe { wari_drv_log_u32(0x416F_6E08, v08) };
            let v0c = unsafe { wari_net_mmio_read32(AONCRG_BASE + 0x0C) };
            let _ = unsafe { wari_drv_log_u32(0x416F_6E0C, v0c) };
            let v38 = unsafe { wari_net_mmio_read32(AONCRG_BASE + AONRST_OFF) };
            let _ = unsafe { wari_drv_log_u32(0x416F_6E38, v38) };
            let v3c = unsafe { wari_net_mmio_read32(AONCRG_BASE + 0x3C) };
            let _ = unsafe { wari_drv_log_u32(0x416F_6E3C, v3c) };
        }

        // ── GMAC1 path: SYS CRG gates + reset (Phase-1c-11) ─────
        #[cfg(feature = "gmac1")]
        {
            const SYSCRG_BASE: u32 = 0x1302_0000;
            const SYSRST_OFF:  u32 = 0x300;
            const GMAC1_RST_MASK: u32 = 0xC; // bits 2+3 = axi_rst, ahb_rst

            // Step 1: enable GMAC1 AHB clock gate.
            let _ = unsafe {
                wari_net_mmio_write32(SYSCRG_BASE + 0x184, ENABLE_BIT)
            };
            // Step 2: enable GMAC1 AXI clock gate.
            let _ = unsafe {
                wari_net_mmio_write32(SYSCRG_BASE + 0x188, ENABLE_BIT)
            };
            // Step 3: deassert GMAC1 resets — RMW only. A blind write
            // here would reset every device whose enum is in [64..96]
            // (DMA / security / USB / PCIe).
            let rst_cur = unsafe { wari_net_mmio_read32(SYSCRG_BASE + SYSRST_OFF) };
            let _ = unsafe {
                wari_net_mmio_write32(SYSCRG_BASE + SYSRST_OFF,
                    rst_cur & !GMAC1_RST_MASK)
            };

            // Verify each write landed. Tags 'G1RG' / 'G1RA' / 'G1Rs'
            // / 'G1Rt' (gmac1 reset assert + status).
            let v184 = unsafe { wari_net_mmio_read32(SYSCRG_BASE + 0x184) };
            let _ = unsafe { wari_drv_log_u32(0x4731_5247, v184) }; // 'G1RG'
            let v188 = unsafe { wari_net_mmio_read32(SYSCRG_BASE + 0x188) };
            let _ = unsafe { wari_drv_log_u32(0x4731_5241, v188) }; // 'G1RA'
            let v300 = unsafe { wari_net_mmio_read32(SYSCRG_BASE + SYSRST_OFF) };
            let _ = unsafe { wari_drv_log_u32(0x4731_5273, v300) }; // 'G1Rs'
            let v310 = unsafe { wari_net_mmio_read32(SYSCRG_BASE + 0x310) };
            let _ = unsafe { wari_drv_log_u32(0x4731_5274, v310) }; // 'G1Rt'
        }

        // Build 132 — phy_intf_sel write moved EARLIER (was fired
        // after PHY init at line ~2492). Hypothesis: if SYS SYSCON
        // +0x90 isn't programmed to RGMII before the MAC version
        // read, the GMAC IP block latches into the wrong RX clock
        // sourcing mode and doesn't recover when we set it later.
        // Builds 125-131 all wrote it AFTER PHY init; if that was
        // the bug, NmGB should climb starting with this build.
        // Original (later) block kept as a redundant idempotent
        // write — harmless if the value is already correct.
        #[cfg(feature = "gmac1")]
        {
            const SYS_SYSCON_BASE: u32 = 0x1303_0000;
            const PHY_INTF_OFFSET: u32 = 0x90;
            let pi_pre = unsafe { wari_net_mmio_read32(SYS_SYSCON_BASE + PHY_INTF_OFFSET) };
            // tag 'pIeP' (PI early Pre)
            let _ = unsafe { wari_drv_log_u32(0x7049_4570, pi_pre) };
            let pi_new = (pi_pre & !0x1C) | 0x04;
            let _ = unsafe {
                wari_net_mmio_write32(SYS_SYSCON_BASE + PHY_INTF_OFFSET, pi_new)
            };
            let pi_post = unsafe { wari_net_mmio_read32(SYS_SYSCON_BASE + PHY_INTF_OFFSET) };
            // tag 'pIeN' (PI early New)
            let _ = unsafe { wari_drv_log_u32(0x7049_454E, pi_post) };
        }

        // Re-read GMAC version — this is the line that matters.
        const GMAC_VERSION_OFFSET: u32 = 0x110;
        let v_after = unsafe {
            wari_net_mmio_read32(plat::NIC_BASE + GMAC_VERSION_OFFSET)
        };
        let _ = unsafe { wari_drv_log_u32(0x476D_6143, v_after) }; // 'GmaC'

        // Phase-1c-4 — read PHY ID via the GMAC's MDIO subblock.
        //
        // VF2 wires a Motorcomm YT8531C at PHY MDIO address 0 to
        // GMAC0 (RGMII). Reading IEEE-802.3 standard PHY registers
        // 2 and 3 yields the OUI / model / revision: for the
        // YT8531C we expect PHYID1 ≈ 0x4F51 and PHYID2 with
        // OUI bits + model 0xE91B.
        //
        // DWMAC4 MAC_MDIO_ADDRESS (offset 0x200) format:
        //   bit  0     GB     busy / start
        //   bits 3:2   GOC    operation (00=write, 11=read C22)
        //   bits 11:8  CR     CSR clock range (4 = CSR/26)
        //   bits 20:16 RDA    register address (5b)
        //   bits 25:21 PA     PHY address (5b)
        // Data lands in the low 16 bits of MAC_MDIO_DATA (0x204).
        // Build 130: log which PHY MDIO address we're using before
        // doing anything else, so any subsequent PHYID = 0xFFFF /
        // 0x0000 result is unambiguously diagnosable.
        let _ = unsafe { wari_drv_log_u32(0x5061_4472, plat::PHY_ADDR) }; // 'PaDr'
        let phyid1 = mdio_read_phy(plat::NIC_BASE, plat::PHY_ADDR, 2);
        let phyid2 = mdio_read_phy(plat::NIC_BASE, plat::PHY_ADDR, 3);
        let _ = unsafe { wari_drv_log_u32(0x5048_5901, phyid1) }; // 'PHY\1'
        let _ = unsafe { wari_drv_log_u32(0x5048_5902, phyid2) }; // 'PHY\2'

        // PR Phase-1c-9 — YT8531C extended-register RGMII delay config.
        //
        // VF2 rev 1.3+ mainline DT (jh7110-starfive-visionfive-2-v1.3b
        // .dts): rx-internal-delay-ps = 1500, tx-internal-delay-ps =
        // 1500, tx-clk-1000-inverted.
        //
        // Without these the PHY samples RXD on the wrong RXC edge —
        // ~99% CRC fail at the MAC, frames silently dropped. The
        // spare-router L2 test on 2026-05-13 showed 1/118 ping
        // success with Debian seeing the same frames perfectly on
        // the same cable — classic RGMII timing margin signature.
        //
        // Extended-register protocol (motorcomm.c ytphy_write_ext):
        //   write PHY reg 0x1E (PAGE_SELECT) = extended-reg address
        //   write/read PHY reg 0x1F (PAGE_DATA)
        //
        // YT8521_RGMII_CONFIG1_REG = 0xA003:
        //   bit  14    TX_CLK_SEL_INVERTED  (set: tx-clk-1000-inverted)
        //   bits 13:10 RX_DELAY             (4-bit, 150 ps/step)
        //   bits  7:4  FE_TX_DELAY (100M)   (4-bit, 150 ps/step)
        //   bits  3:0  GE_TX_DELAY (1G)     (4-bit, 150 ps/step)
        //
        // GMAC0 (phy0 on VF2 v1.3b dts):
        //   rx-internal-delay-ps = 1500 → 0x0A (10 × 150ps)
        //   tx-internal-delay-ps = 1500 → 0x0A
        //   tx-clk-1000-inverted (bit 14 set)
        //   Final value: (1<<14) | (0x0A<<10) | (0x0A<<0) = 0x680A.
        //
        // GMAC1 (phy1 on this board — `starfive,visionfive-v2`, the
        // base v1.0/1.2 path, NOT mainline's -v1.3b override). The
        // operator's /proc/device-tree dump confirmed:
        //   - `ls /proc/device-tree/soc/ethernet@16040000/` shows
        //     ethernet-phy@1 (NOT @0) → MDIO addr 1
        //   - No `rx-internal-delay-ps` / `tx-internal-delay-ps`
        //     anywhere in DT → BSP defaults apply
        // BSP defaults per ytphy_of_config() in StarFive's motorcomm.c:
        //   rx_delay_sel        = 0x2  → bits 13:10
        //   tx_delay_sel_fe     = 0x5  → bits  7:4   (100M only, but
        //                                              BSP always writes)
        //   tx_delay_sel        = 0x0  → bits  3:0
        //   tx_clk_1000_inverted = 0   → bit 14 clear
        //   Final: (0x2<<10) | (0x5<<4) | (0x0<<0) = 0x0850.
        // Build 130 swaps from 0x0800 (the mainline v1.3b value our
        // builds 124-129 were guessing with) to 0x0850 (BSP-confirmed,
        // for THIS board revision).
        const YTPHY_PAGE_SELECT:        u32 = 0x1E;
        const YTPHY_PAGE_DATA:          u32 = 0x1F;
        const YT8521_RGMII_CONFIG1_REG: u16 = 0xA003;
        #[cfg(not(feature = "gmac1"))]
        const YT8531_RC1R_VF2_VALUE:    u16 = 0x680A;
        #[cfg(feature = "gmac1")]
        const YT8531_RC1R_VF2_VALUE:    u16 = 0x0850;

        // Pre-read so we know U-Boot's starting value.
        let _ = mdio_write_phy(plat::NIC_BASE, plat::PHY_ADDR, YTPHY_PAGE_SELECT,
                               YT8521_RGMII_CONFIG1_REG);
        let rc1r_pre = mdio_read_phy(plat::NIC_BASE, plat::PHY_ADDR, YTPHY_PAGE_DATA);
        let _ = unsafe { wari_drv_log_u32(0x5243_3152, rc1r_pre) }; // 'RC1R'

        // Build 137 — full BSP PHY init restored. The three-fault
        // post-mortem of builds 124-136:
        //
        //   fault A: PHY MDIO address was 0, real PHY at 1  (fixed 130)
        //   fault B: SYSCRG clock cluster wrong, RX clock 0 (fixed 136)
        //   fault C: PHY left at U-Boot residue 0xA003=0x00F1
        //            (rx-delay 0) instead of Linux's working config
        //
        // Each individual fix was "disproven" on silicon because the
        // other two faults still zeroed RX: build 127 wrote the right
        // PHY values to the wrong address through dead clocks; build
        // 130 fixed the address but clocks were still dead; build 131
        // then REMOVED the PHY writes (the "inheritance test") on the
        // grounds they "didn't help." Build 136's golden clocks made
        // NmGB=0 finally attributable to the PHY alone: U-Boot's
        // 0x00F1 has RX delay 0, Linux's ytphy_of_config runs 0x0850.
        //
        // This restores the exact 3-step RMW sequence StarFive's
        // motorcomm.c ytphy_of_config() applies to phy1, now on the
        // correct address (1) with correct clocks:
        //   1. 0xA001 Chip Config:      clear bit 8 (RXC_DLY_EN=0)
        //   2. 0xA010 Pad Drive:        rxc_ds=6, rxd_ds=3
        //   3. 0xA003 RGMII Config 1:   rx=0x2, fe=0x5, ge=0x0 → 0x0850
        // GMAC0 path unchanged (single 0x680A write, proven working).
        #[cfg(not(feature = "gmac1"))]
        {
            let _ = mdio_write_phy(plat::NIC_BASE, plat::PHY_ADDR, YTPHY_PAGE_SELECT,
                                   YT8521_RGMII_CONFIG1_REG);
            let _ = mdio_write_phy(plat::NIC_BASE, plat::PHY_ADDR, YTPHY_PAGE_DATA,
                                   YT8531_RC1R_VF2_VALUE);
        }
        #[cfg(feature = "gmac1")]
        {
            const YT8521_CHIP_CONFIG_REG: u16 = 0xA001;
            const YT8531_PAD_DRIVE_REG:   u16 = 0xA010;

            // ext_rmw: page-select, read, modify, page-select, write.
            // Returns the pre-modify value for logging.
            let ext_rmw = |reg: u16, clr: u32, set: u32| -> u32 {
                let _ = mdio_write_phy(plat::NIC_BASE, plat::PHY_ADDR,
                                       YTPHY_PAGE_SELECT, reg);
                let old = mdio_read_phy(plat::NIC_BASE, plat::PHY_ADDR,
                                        YTPHY_PAGE_DATA);
                let new = (old & !clr) | set;
                let _ = mdio_write_phy(plat::NIC_BASE, plat::PHY_ADDR,
                                       YTPHY_PAGE_SELECT, reg);
                let _ = mdio_write_phy(plat::NIC_BASE, plat::PHY_ADDR,
                                       YTPHY_PAGE_DATA, new as u16);
                old
            };

            // Step 1 — 0xA001: clear RXC_DLY_EN (bit 8). The 300 ps
            // rx delay lives in the low half of the delay LUT; bit 8
            // selects the +1.9 ns high half, which must be OFF.
            let cc_pre = ext_rmw(YT8521_CHIP_CONFIG_REG, 0x0000_0100, 0);
            let _ = unsafe { wari_drv_log_u32(0x4343_3072, cc_pre) };  // 'CC0r'

            // Step 2 — 0xA010: pad drive strength. rxc_ds = 6 (bits
            // 15:13), rxd_ds_hi = 0 (bit 12), rxd_ds_low = 3 (bits
            // 5:4). Mask 0xF030, value 0xC030. Per yt8531_set_ds
            // with the BSP LDO table (3970 uA -> 6, 2910 uA -> 3).
            let pd_pre = ext_rmw(YT8531_PAD_DRIVE_REG,
                                 0x0000_F030, 0x0000_C030);
            let _ = unsafe { wari_drv_log_u32(0x5044_5372, pd_pre) };  // 'PDSr'

            // Step 3 — 0xA003: the RGMII delays. RMW only the delay
            // nibbles (13:10, 7:4, 3:0), preserving bits 15:14 and
            // 9:8 whatever the strap set them to.
            let _ = ext_rmw(YT8521_RGMII_CONFIG1_REG,
                            0x0000_3CFF,
                            YT8531_RC1R_VF2_VALUE as u32);
        }

        // Verify-read.
        let _ = mdio_write_phy(plat::NIC_BASE, plat::PHY_ADDR, YTPHY_PAGE_SELECT,
                               YT8521_RGMII_CONFIG1_REG);
        let rc1r_post = mdio_read_phy(plat::NIC_BASE, plat::PHY_ADDR, YTPHY_PAGE_DATA);
        let _ = unsafe { wari_drv_log_u32(0x5243_3170, rc1r_post) }; // 'RC1p'

        // Force re-AN whenever the PHY config changed: the YT8531C
        // latches RGMII timing at link-up, so the new delays only
        // take effect on a fresh link cycle.
        let needs_relink = rc1r_pre != rc1r_post;

        // PR Phase-1c-5 — IEEE 802.3 auto-negotiation.
        //
        // PHY register map (Clause 22 standard):
        //   0x00 = Basic Control
        //          bit 12 = AN enable
        //          bit  9 = AN restart
        //          bit  6 = speed[1] (with bit 13 = speed[0])
        //          bit 13 = speed[0] (00=10, 01=100, 10=1000)
        //          bit  8 = duplex (1=full)
        //   0x01 = Basic Status
        //          bit  5 = AN complete
        //          bit  2 = link up
        //   0x04 = AN advertisement (10/100 capability)
        //   0x09 = 1000BASE-T control (1000Mb advertisement)
        //   0x0A = 1000BASE-T status (1000Mb negotiation result)
        //
        // Sequence:
        //   1. log current Basic Control + Basic Status
        //   2. enable + restart AN by writing reg 0 = 0x1200
        //      (bit 12 AN enable + bit 9 AN restart)
        //   3. poll Basic Status bit 5 (AN done) ~100 ms budget
        //   4. log final Basic Status + 1000BASE-T status

        // Step 1 — pre-AN snapshot.
        let bc_pre = mdio_read_phy(plat::NIC_BASE, plat::PHY_ADDR, 0);
        let bs_pre = mdio_read_phy(plat::NIC_BASE, plat::PHY_ADDR, 1);
        let _ = unsafe { wari_drv_log_u32(0x5048_5910, bc_pre) }; // 'PHY\x10'
        let _ = unsafe { wari_drv_log_u32(0x5048_5911, bs_pre) }; // 'PHY\x11'

        // Step 2 — DON'T restart AN if it already converged.
        // Build 80 trace showed bs_pre = 0x796D (bits 2 + 5 set
        // = link up + AN complete) — U-Boot already brought up
        // the link. Restarting AN drops the link for ~100 ms.
        // Only kick AN if either link is down or AN hasn't
        // completed yet.
        const BS_LINK_UP:    u32 = 1 << 2;
        const BS_AN_COMPLETE:u32 = 1 << 5;
        // Build 121: if we changed 0xA003 (RGMII delays), force a
        // fresh AN cycle — the new delays only latch at link-up.
        let already_linked = !needs_relink
                          && (bs_pre & BS_LINK_UP) != 0
                          && (bs_pre & BS_AN_COMPLETE) != 0;
        if already_linked {
            // Tag 0x12 retains its position in the trace so the
            // boot-line layout doesn't shift; value 0xA17EAD11 =
            // 'already' marker.
            let _ = unsafe { wari_drv_log_u32(0x5048_5912, 0xA17E_AD11) };
        } else {
            let _ = mdio_write_phy(plat::NIC_BASE, plat::PHY_ADDR, 0, 0x1200);
            let _ = unsafe { wari_drv_log_u32(0x5048_5912, 0x0000_0000) };
        }

        // Step 3 — poll AN-complete bit. Budget 500k MDIO reads
        // ≈ ~500 ms wall-clock; covers YT8531C's worst-case
        // ~250 ms convergence. Skipped if already_linked.
        let bs_final;
        let an_tries;
        if already_linked {
            bs_final = bs_pre;
            an_tries = 0;
        } else {
            let mut tries = 0u32;
            let s_final = loop {
                let s = mdio_read_phy(plat::NIC_BASE, plat::PHY_ADDR, 1);
                if s & BS_AN_COMPLETE != 0 && s & BS_LINK_UP != 0 {
                    break s;
                }
                tries += 1;
                if tries > 500_000 {
                    break s;
                }
            };
            bs_final = s_final;
            an_tries = tries;
        }
        let _ = unsafe { wari_drv_log_u32(0x5048_5913, bs_final) };
        let _ = unsafe { wari_drv_log_u32(0x5048_5914, an_tries) };

        // Step 4 — 1000BASE-T status (reg 0x0A): bit 11 = "1000Mb
        // full duplex resolved", bit 10 = "1000Mb half".
        let gig_status = mdio_read_phy(plat::NIC_BASE, plat::PHY_ADDR, 0x0A);
        let _ = unsafe { wari_drv_log_u32(0x5048_5915, gig_status) };

        // PR Phase-1c-5b — GMAC HW capability + DMA bus-mode dump.
        //
        // Now that the IP block is alive (clocks on, version
        // 0x52 readable), read its self-reported feature
        // registers. This tells us:
        //   - how many TX/RX channels the silicon implements
        //   - hash filter size, ARP offload, EEE, IEEE-1588
        //   - DMA bus mode = current DMA engine state
        // We need these numbers to size descriptor rings in
        // Phase-1c-6 without guessing.
        const MAC_CONFIG_OFFSET:      u32 = 0x000;
        const MAC_HW_FEATURE0_OFFSET: u32 = 0x11C;
        const MAC_HW_FEATURE1_OFFSET: u32 = 0x120;
        const MAC_HW_FEATURE2_OFFSET: u32 = 0x124;
        const MAC_HW_FEATURE3_OFFSET: u32 = 0x128;
        const DMA_BUS_MODE_OFFSET:    u32 = 0x1000;
        const DMA_SYS_BUS_MODE_OFF:   u32 = 0x1004;
        for (off, tag_low) in [
            (MAC_CONFIG_OFFSET,      0x00u32),
            (MAC_HW_FEATURE0_OFFSET, 0xF0),
            (MAC_HW_FEATURE1_OFFSET, 0xF1),
            (MAC_HW_FEATURE2_OFFSET, 0xF2),
            (MAC_HW_FEATURE3_OFFSET, 0xF3),
            (DMA_BUS_MODE_OFFSET,    0xD0),
            (DMA_SYS_BUS_MODE_OFF,   0xD1),
        ] {
            // SAFETY: extern host fn; addr is inside GMAC0 window
            // mapped + cap-validated.
            let v = unsafe { wari_net_mmio_read32(plat::NIC_BASE + off) };
            let tag = 0x4857_0000 | tag_low; // 'HW\0\0' + tag_low
            let _ = unsafe { wari_drv_log_u32(tag, v) };
        }

        // PR Phase-1c-6a — DMA channel-0 register dump. Reads
        // every register the Phase-1c-6 ring-setup will eventually
        // write, so we know:
        //   - whether U-Boot left any descriptor pointers loaded
        //   - the current DMA state (idle / suspended / running)
        //   - which interrupts are already enabled
        //   - the current head/tail pointers
        // This is the last read-only diagnostic before Phase-1c-6
        // starts allocating + writing.
        for (off, tag_low) in [
            (0x1100u32, 0x00u32), // DMA_CH0_CONTROL
            (0x1104,    0x04),    // DMA_CH0_TX_CONTROL
            (0x1108,    0x08),    // DMA_CH0_RX_CONTROL
            (0x1110,    0x10),    // DMA_CH0_TXDESC_LIST_ADDR_HI
            (0x1114,    0x14),    // DMA_CH0_TXDESC_LIST_ADDR
            (0x1118,    0x18),    // DMA_CH0_RXDESC_LIST_ADDR_HI
            (0x111C,    0x1C),    // DMA_CH0_RXDESC_LIST_ADDR
            (0x1120,    0x20),    // DMA_CH0_TXDESC_TAIL_POINTER
            (0x1128,    0x28),    // DMA_CH0_RXDESC_TAIL_POINTER
            (0x112C,    0x2C),    // DMA_CH0_TXDESC_RING_LENGTH
            (0x1130,    0x30),    // DMA_CH0_RXDESC_RING_LENGTH
            (0x1134,    0x34),    // DMA_CH0_INTERRUPT_ENABLE
            (0x1144,    0x44),    // DMA_CH0_CURRENT_APP_TXDESC
            (0x114C,    0x4C),    // DMA_CH0_CURRENT_APP_RXDESC
            (0x1160,    0x60),    // DMA_CH0_STATUS
        ] {
            // SAFETY: same.
            let v = unsafe { wari_net_mmio_read32(plat::NIC_BASE + off) };
            let tag = 0x444D_0000 | tag_low; // 'DM\0\0' + low
            let _ = unsafe { wari_drv_log_u32(tag, v) };
        }

        // PR Phase-1c-6b — clear the DMA soft reset.
        //
        // Build 83 dump showed DMA_BUS_MODE = 0x00000001 — bit 0
        // (SWR / software reset) is asserted, which is why every
        // DMA channel-0 register reads 0: the engine is held in
        // reset.
        //
        // Per DWMAC databook (DMA_BUS_MODE §6.10): SWR is
        // write-1-to-trigger; the bit auto-clears when the engine
        // finishes its reset cycle. Reading 1 means either we're
        // mid-reset OR the bit needs to be re-poked.
        //
        // Strategy:
        //   1. Read SWR. If 0, skip ahead.
        //   2. If 1, poll up to 100k iterations for it to auto-
        //      clear.
        //   3. If still 1, explicitly write 0 (some integrations
        //      require the clear).
        //   4. Re-dump DMA_BUS_MODE + DMA channel-0 registers
        //      (4 representative slots) to verify they're now
        //      writable / reflecting their power-on defaults.
        const DMA_BUS_MODE_OFFSET_FULL: u32 = 0x1000;
        const SWR_BIT: u32 = 1 << 0;

        let mut wait_iters = 0u32;
        let mut bm = unsafe { wari_net_mmio_read32(plat::NIC_BASE + DMA_BUS_MODE_OFFSET_FULL) };
        while bm & SWR_BIT != 0 && wait_iters < 100_000 {
            bm = unsafe { wari_net_mmio_read32(plat::NIC_BASE + DMA_BUS_MODE_OFFSET_FULL) };
            wait_iters += 1;
        }
        let _ = unsafe { wari_drv_log_u32(0x4253_5701, wait_iters) }; // 'BSW\1' = poll iters
        let _ = unsafe { wari_drv_log_u32(0x4253_5702, bm) };          // 'BSW\2' = bus_mode after poll

        if bm & SWR_BIT != 0 {
            // Force-clear by writing without bit 0.
            let _ = unsafe {
                wari_net_mmio_write32(plat::NIC_BASE + DMA_BUS_MODE_OFFSET_FULL, bm & !SWR_BIT)
            };
            let bm2 = unsafe { wari_net_mmio_read32(plat::NIC_BASE + DMA_BUS_MODE_OFFSET_FULL) };
            let _ = unsafe { wari_drv_log_u32(0x4253_5703, bm2) };     // 'BSW\3' = post-force-clear
        }

        // PR Phase-1c-6c — enable SYSCRG upstream clocks.
        //
        // Build 84 trace showed DMA SWR stuck at 1 even after
        // explicit force-clear. The DMA engine can't complete
        // its reset because upstream parent clocks aren't
        // running:
        //   SYSCRG +0x024  AHB0 (id 9)            — parent of GMAC0_AHB
        //   SYSCRG +0x180  NOC_BUS_STG_AXI (id 96) — parent of GMAC0_AXI
        // Linux marks these CLK_IS_CRITICAL and assumes U-Boot
        // keeps them on; on VF2 that assumption may not hold
        // after U-Boot's Ethernet probe finishes.
        //
        // Read both, log, and OR-in 0x80000000 if not already
        // set. Then retry the DMA SWR clear.
        // (ENABLE_BIT already declared earlier in this scope — reuse it.)
        const SYSCRG_BASE: u32 = 0x1302_0000;

        for (off, tag_low) in [
            (0x024u32, 0x24u32), // AHB0
            (0x180,    0x80),    // NOC_BUS_STG_AXI
        ] {
            let pre = unsafe { wari_net_mmio_read32(SYSCRG_BASE + off) };
            let tag_pre = 0x5550_0000 | tag_low; // 'UP\0\0' + low
            let _ = unsafe { wari_drv_log_u32(tag_pre, pre) };

            if pre & ENABLE_BIT == 0 {
                let _ = unsafe {
                    wari_net_mmio_write32(SYSCRG_BASE + off, pre | ENABLE_BIT)
                };
                let post = unsafe { wari_net_mmio_read32(SYSCRG_BASE + off) };
                let tag_post = 0x5570_0000 | tag_low; // 'Up\0\0' + low
                let _ = unsafe { wari_drv_log_u32(tag_post, post) };
            }
        }

        // PR Phase-1c-6L / Phase-1c-11 — PHY interface select.
        //
        // GMAC0: AON SYSCON @ 0x17010000 +0x0C, bits 20:18 = mode.
        // GMAC1: SYS SYSCON @ 0x13030000 +0x90, bits  4:2 = mode.
        // Value 0b001 = RGMII for both.
        //
        // Without this, the rx_inv gate silently rejects its enable
        // bit because its parent isn't toggling.
        #[cfg(not(feature = "gmac1"))]
        {
            const AON_SYSCON_BASE: u32 = 0x1701_0000;
            const PHY_INTF_OFFSET: u32 = 0x0C;
            let pi_pre = unsafe { wari_net_mmio_read32(AON_SYSCON_BASE + PHY_INTF_OFFSET) };
            let _ = unsafe { wari_drv_log_u32(0x5049_5F50, pi_pre) }; // 'PI_P' pre
            let pi_new = (pi_pre & !(0x7 << 18)) | (0x1 << 18);
            let _ = unsafe {
                wari_net_mmio_write32(AON_SYSCON_BASE + PHY_INTF_OFFSET, pi_new)
            };
            let pi_post = unsafe { wari_net_mmio_read32(AON_SYSCON_BASE + PHY_INTF_OFFSET) };
            let _ = unsafe { wari_drv_log_u32(0x5049_5F4E, pi_post) }; // 'PI_N' new
        }
        #[cfg(feature = "gmac1")]
        {
            const SYS_SYSCON_BASE: u32 = 0x1303_0000;
            const PHY_INTF_OFFSET: u32 = 0x90;
            let pi_pre = unsafe { wari_net_mmio_read32(SYS_SYSCON_BASE + PHY_INTF_OFFSET) };
            let _ = unsafe { wari_drv_log_u32(0x5049_5F50, pi_pre) }; // 'PI_P' pre
            // Bits[4:2] = mode; mask 0x1C, RGMII = 0b001 << 2 = 0x04.
            let pi_new = (pi_pre & !0x1C) | 0x04;
            let _ = unsafe {
                wari_net_mmio_write32(SYS_SYSCON_BASE + PHY_INTF_OFFSET, pi_new)
            };
            let pi_post = unsafe { wari_net_mmio_read32(SYS_SYSCON_BASE + PHY_INTF_OFFSET) };
            let _ = unsafe { wari_drv_log_u32(0x4731_5070, pi_post) }; // 'G1Pp'
        }

        // PR Phase-1c-6d — enable the rest of GMAC0's datapath
        // clocks. Build 86 trace showed AHB/AXI + upstream are
        // on but DMA SWR still won't clear; the engine can't
        // flush its state machines without TX/RX clocks running.
        //
        // Per Linux mainline clk-starfive-jh7110-aon.c +
        // clk-starfive-jh7110-sys.c, GMAC0's complete clock
        // tree:
        //   AON (already done): +0x08 ahb, +0x0C axi
        //   AON (NEW):           +0x14 tx (GMUX, mux=0=gtxclk)
        //                         +0x20 rx_inv (bit 30 = invert for RGMII)
        //   SYS (NEW):           +0x1B0 gtxclk (GDIV, en + div=5 for 1Gbps)
        //                         +0x1B4 ptp    (GDIV, en + div=10)
        //                         +0x1B8 phy    (GDIV, en + div=30 for MDC)
        //                         +0x1BC gtxc   (gate, en)
        //
        // Note: +0x10 rmii_rtx + +0x18 tx_inv + +0x1C rx are
        // intentionally untouched — defaults are correct for
        // RGMII (VF2 phy mode).
        #[cfg(not(feature = "gmac1"))]
        {
            const AONCRG_BASE_2: u32 = 0x1700_0000;

            // AON datapath: tx GMUX with mux=0 (parent gmac0_gtxclk)
            let _ = unsafe { wari_net_mmio_write32(AONCRG_BASE_2 + 0x14, ENABLE_BIT) };
            // AON: gmac0_rx MUX (id 7) — bit 31 enable + mux=0 (rgmii_rxin)
            let _ = unsafe { wari_net_mmio_write32(AONCRG_BASE_2 + 0x1C, ENABLE_BIT) };
            // AON: rx_inv bit 30 set for RGMII
            let _ = unsafe { wari_net_mmio_write32(AONCRG_BASE_2 + 0x20, 0x4000_0000) };

            // SYS: gtxclk = enable + divider 5 (PLL0 1000MHz / 5 = 200MHz)
            let _ = unsafe { wari_net_mmio_write32(SYSCRG_BASE + 0x1B0, ENABLE_BIT | 0x5) };
            // SYS: ptp = enable + divider 10
            let _ = unsafe { wari_net_mmio_write32(SYSCRG_BASE + 0x1B4, ENABLE_BIT | 0xA) };
            // SYS: phy MDC = enable + divider 30 (~16MHz)
            let _ = unsafe { wari_net_mmio_write32(SYSCRG_BASE + 0x1B8, ENABLE_BIT | 0x1E) };
            // SYS: gtxc gate (parent gmac0_gtxclk)
            let _ = unsafe { wari_net_mmio_write32(SYSCRG_BASE + 0x1BC, ENABLE_BIT) };

            // Verify-read each. Tags 'AOn' / 'SyS' + low byte.
            for off in [0x08u32, 0x0C, 0x10, 0x14, 0x18, 0x1C, 0x20] {
                let v = unsafe { wari_net_mmio_read32(AONCRG_BASE_2 + off) };
                let tag = 0x414F_6E00 | (off & 0xFF); // 'AOn\0' + low
                let _ = unsafe { wari_drv_log_u32(tag, v) };
            }
            for off in [0x1B0u32, 0x1B4, 0x1B8, 0x1BC] {
                let v = unsafe { wari_net_mmio_read32(SYSCRG_BASE + off) };
                let tag = 0x5379_5300 | (off & 0xFF); // 'SyS\0' + low
                let _ = unsafe { wari_drv_log_u32(tag, v) };
            }
        }

        // ── GMAC1 datapath clocks (all in SYS CRG) ──────────────
        //
        // Build 136 — GOLDEN-REFERENCE EXACT MATCH. The 2026-07-06
        // register dump from working Linux (5.15.0-starfive, end1
        // receiving 23K+ packets, scripts/dump-gmac1-regs.sh) showed
        // builds 125-135 had the clock cluster WRONG:
        //
        //   reg     Linux(works)  Wari(0 frames)  meaning
        //   +0x190  0x0000000C    0x00000005      gtxclk div 12 not 5
        //   +0x194  0x00000001    (never set)     rmii_rtx div 1
        //   +0x19C  0x00000020    0x00000000      gmac1_rx — RX CLOCK;
        //                                          our bit31 write never
        //                                          stuck, Linux uses 0x20
        //   +0x1A4  0x81000000    0x80000000      tx mux parent 1 (the
        //                                          'tx-use-rgmii-clk'
        //                                          quirk), not parent 0
        //   +0x1A8  0x40000000    (never set)     tx_inv bit30
        //   +0x1AC  0x80000020    0x80000000      gtxc en + 0x20 low
        //
        // With +0x19C = 0 the MAC RX domain has no clock — the exact
        // all-zeros signature (MMC=0, MTL=0, DMA idle, link up) that
        // survived every PHY-register theory from builds 124-135.
        // RXQ_CTRL0 (builds 133/134) also reads 0 on working Linux —
        // that theory is dead; the register is a no-op here.
        //
        // Philosophy: copy the working system EXACTLY, no theorizing
        // about what bit 5 means. Linux receives frames with these
        // values; Wari now writes the identical cluster.
        #[cfg(feature = "gmac1")]
        {
            // gmac1_gtxclk @ +0x190 — div 12 (golden).
            let _ = unsafe { wari_net_mmio_write32(SYSCRG_BASE + 0x190, 0x0000_000C) };
            // gmac1_rmii_rtx @ +0x194 — div 1 (golden; feeds tx mux parent 1).
            let _ = unsafe { wari_net_mmio_write32(SYSCRG_BASE + 0x194, 0x0000_0001) };
            // gmac1_ptp @ +0x198 — en + div 10 (already matched golden).
            let _ = unsafe { wari_net_mmio_write32(SYSCRG_BASE + 0x198, ENABLE_BIT | 0xA) };
            // gmac1_rx @ +0x19C — 0x20 (golden). NOT bit31: builds
            // 125-135 wrote ENABLE_BIT here and it silently read back
            // as 0. Linux's working value has no bit31 at all.
            let _ = unsafe { wari_net_mmio_write32(SYSCRG_BASE + 0x19C, 0x0000_0020) };
            // gmac1_rx_inv @ +0x1A0 — bit30 (already matched golden).
            let _ = unsafe { wari_net_mmio_write32(SYSCRG_BASE + 0x1A0, 0x4000_0000) };
            // gmac1_tx GMUX @ +0x1A4 — en + mux parent 1 (golden).
            let _ = unsafe { wari_net_mmio_write32(SYSCRG_BASE + 0x1A4, 0x8100_0000) };
            // gmac1_tx_inv @ +0x1A8 — bit30 (golden).
            let _ = unsafe { wari_net_mmio_write32(SYSCRG_BASE + 0x1A8, 0x4000_0000) };
            // gmac1_gtxc @ +0x1AC — en + 0x20 (golden).
            let _ = unsafe { wari_net_mmio_write32(SYSCRG_BASE + 0x1AC, ENABLE_BIT | 0x20) };
            // Shared MDC root @ +0x1B8 — Wari's en+div30 kept: MDIO
            // is proven working under Wari (PHYID reads OK). Linux
            // runs 0x0A here; both work, don't touch what works.
            let _ = unsafe { wari_net_mmio_write32(SYSCRG_BASE + 0x1B8, ENABLE_BIT | 0x1E) };

            // Verify-read: full GMAC1 cluster incl. the previously
            // unlogged 0x18C (gmac_src, U-Boot-owned, golden = 2)
            // and the newly written 0x194 / 0x1A8.
            // Tags 'Sy1\0' + low byte.
            for off in [0x184u32, 0x188, 0x18C, 0x190, 0x194, 0x198,
                        0x19C, 0x1A0, 0x1A4, 0x1A8, 0x1AC, 0x1B8] {
                let v = unsafe { wari_net_mmio_read32(SYSCRG_BASE + off) };
                let tag = 0x5379_3100 | (off & 0xFF); // 'Sy1\0' + low
                let _ = unsafe { wari_drv_log_u32(tag, v) };
            }
        }

        // Retry the DMA soft-reset clear now that upstream is on.
        let mut wait_iters_2 = 0u32;
        let mut bm_2 = unsafe { wari_net_mmio_read32(plat::NIC_BASE + DMA_BUS_MODE_OFFSET_FULL) };
        while bm_2 & SWR_BIT != 0 && wait_iters_2 < 100_000 {
            bm_2 = unsafe { wari_net_mmio_read32(plat::NIC_BASE + DMA_BUS_MODE_OFFSET_FULL) };
            wait_iters_2 += 1;
        }
        let _ = unsafe { wari_drv_log_u32(0x5257_5201, wait_iters_2) }; // 'RWR\1' = retry iters
        let _ = unsafe { wari_drv_log_u32(0x5257_5202, bm_2) };          // 'RWR\2' = bus_mode after retry

        // If still set even after upstream clocks: trigger a
        // fresh SWR cycle (write 1) so the engine restarts the
        // reset with clocks now running.
        if bm_2 & SWR_BIT != 0 {
            let _ = unsafe {
                wari_net_mmio_write32(plat::NIC_BASE + DMA_BUS_MODE_OFFSET_FULL, SWR_BIT)
            };
            let mut tries_3 = 0u32;
            let mut bm_3 = unsafe { wari_net_mmio_read32(plat::NIC_BASE + DMA_BUS_MODE_OFFSET_FULL) };
            while bm_3 & SWR_BIT != 0 && tries_3 < 100_000 {
                bm_3 = unsafe { wari_net_mmio_read32(plat::NIC_BASE + DMA_BUS_MODE_OFFSET_FULL) };
                tries_3 += 1;
            }
            let _ = unsafe { wari_drv_log_u32(0x5257_5203, tries_3) }; // 'RWR\3' = re-trigger iters
            let _ = unsafe { wari_drv_log_u32(0x5257_5204, bm_3) };
        }

        // Final dump of 4 representative DMA channel-0 regs to
        // confirm the engine is alive.
        for (off, tag_low) in [
            (0x1100u32, 0x80u32), // CONTROL
            (0x1104,    0x84),    // TX_CONTROL
            (0x1108,    0x88),    // RX_CONTROL
            (0x1160,    0xE0),    // STATUS
        ] {
            let v = unsafe { wari_net_mmio_read32(plat::NIC_BASE + off) };
            let tag = 0x4332_0000 | tag_low; // 'C2\0' + low
            let _ = unsafe { wari_drv_log_u32(tag, v) };
        }

        // PR Phase-1c-6e — program TX/RX descriptor ring base
        // addresses + ring lengths. DMA is alive; write the
        // pointers and verify-read.
        //
        // Ring sizing: 16 descriptors × 16 bytes = 256 B per ring.
        // VF2_DMA_RINGS holds both rings + 16 RX buffers (1536 B
        // each = 24 KiB). Total static = ~24.5 KiB in driver
        // linmem.
        //
        // Physical address = lin_mem_base() + linmem-offset of
        // the ring static. The kernel side translates via
        // page-table mappings — wasm32 pointers ARE the linmem
        // offset (driver is wasm32, 32-bit ptrs).
        let lin_base = unsafe { wari_lin_mem_base() };

        let tx_ring_off = unsafe {
            core::ptr::addr_of!(VF2_TX_RING.descs) as u32
        };
        let rx_ring_off = unsafe {
            core::ptr::addr_of!(VF2_RX_RING.descs) as u32
        };
        let tx_pa: u64 = lin_base + tx_ring_off as u64;
        let rx_pa: u64 = lin_base + rx_ring_off as u64;

        // Tags: 'TXp' / 'RXp' carry the ring physical address
        // for visibility before we hand them to the DMA engine.
        let _ = unsafe { wari_drv_log_u32(0x5458_7048, (tx_pa >> 32) as u32) }; // TXpH
        let _ = unsafe { wari_drv_log_u32(0x5458_704C, tx_pa as u32) };          // TXpL
        let _ = unsafe { wari_drv_log_u32(0x5258_7048, (rx_pa >> 32) as u32) }; // RXpH
        let _ = unsafe { wari_drv_log_u32(0x5258_704C, rx_pa as u32) };          // RXpL

        // Write TX ring base (high then low; per DWMAC databook
        // the engine latches LOW writes once HIGH is set).
        let _ = unsafe {
            wari_net_mmio_write32(plat::NIC_BASE + 0x1110, (tx_pa >> 32) as u32)
        };
        let _ = unsafe {
            wari_net_mmio_write32(plat::NIC_BASE + 0x1114, tx_pa as u32)
        };
        // Write RX ring base.
        let _ = unsafe {
            wari_net_mmio_write32(plat::NIC_BASE + 0x1118, (rx_pa >> 32) as u32)
        };
        let _ = unsafe {
            wari_net_mmio_write32(plat::NIC_BASE + 0x111C, rx_pa as u32)
        };
        // Ring lengths — 16 entries each (DMA expects N-1).
        let _ = unsafe { wari_net_mmio_write32(plat::NIC_BASE + 0x112C, 15) };
        let _ = unsafe { wari_net_mmio_write32(plat::NIC_BASE + 0x1130, 15) };

        // Verify-read all 6 slots.
        for (off, tag_low) in [
            (0x1110u32, 0xA0u32), // TX_BASE_HI
            (0x1114,    0xA4),    // TX_BASE_LO
            (0x1118,    0xA8),    // RX_BASE_HI
            (0x111C,    0xAC),    // RX_BASE_LO
            (0x112C,    0xBC),    // TX_RING_LEN
            (0x1130,    0xC0),    // RX_RING_LEN
        ] {
            let v = unsafe { wari_net_mmio_read32(plat::NIC_BASE + off) };
            let tag = 0x4456_0000 | tag_low; // 'DV\0\0' + low
            let _ = unsafe { wari_drv_log_u32(tag, v) };
        }

        // PR Phase-1c-6f — populate one TX descriptor with a
        // 64-byte broadcast ARP, enable MAC TX/RX, start DMA TX,
        // kick the tail pointer. First Wari frame on real wire.
        //
        // DWMAC4 normal TX descriptor layout:
        //   TDES0: buffer-1 address bits 31:0
        //   TDES1: buffer-1 address bits 63:32
        //   TDES2: bits 13:0  = buffer-1 length
        //          bit  31    = IOC (interrupt on completion)
        //   TDES3: bits 14:0  = total frame length
        //          bit  28    = FD (first descriptor)
        //          bit  29    = LD (last descriptor)
        //          bit  31    = OWN (1 = DMA owns; 0 = SW)
        //
        // Pkt PA = lin_mem_base + linmem-offset of VF2_FIRST_PKT.
        let pkt_off = core::ptr::addr_of!(VF2_FIRST_PKT) as u32;
        let pkt_pa: u64 = lin_base + pkt_off as u64;
        let _ = unsafe { wari_drv_log_u32(0x504B_5448, (pkt_pa >> 32) as u32) }; // 'PKTH'
        let _ = unsafe { wari_drv_log_u32(0x504B_544C, pkt_pa as u32) };          // 'PKTL'

        // SAFETY: VF2_TX_RING is module-static; this is the only
        // writer and runs once at boot before DMA reads it.
        unsafe {
            let d = &mut VF2_TX_RING.descs[0];
            d[0] = pkt_pa as u32;             // TDES0 PA low
            d[1] = (pkt_pa >> 32) as u32;     // TDES1 PA high
            d[2] = 64;                        // TDES2 buf len = 64
            d[3] = 0x8000_0000                // OWN
                 | 0x2000_0000                // LD
                 | 0x1000_0000                // FD
                 | 64;                        // total length 64
        }

        // Build 134 — MAC_RXQ_CTRL0 (0x00A0) bits[1:0] = RXQ0EN.
        // Linux dwmac4_rx_queue_enable() writes 0b10 (DCB mode = 2)
        // here BEFORE the MAC's RE bit is set. Build 133 wrote it
        // AFTER and the read-back was 0 — the MAC silently rejected
        // the change because RX queue assignment is locked once RE=1.
        // Moving the write to right before MAC_CONFIG fixes the
        // ordering. Verify-read tag 'RXQ0' (0x52585130) immediately
        // confirms whether the value sticks this time.
        let _ = unsafe { wari_net_mmio_write32(plat::NIC_BASE + 0x0A0, 0x0000_0002) };
        let rxq_early = unsafe { wari_net_mmio_read32(plat::NIC_BASE + 0x0A0) };
        let _ = unsafe { wari_drv_log_u32(0x5258_5130, rxq_early) }; // 'RXQ0' early

        // MAC_CONFIG = 0x0000_2003 (DM | TE | RE) — full-duplex +
        // enable transmit + enable receive. Build 121: added DM
        // bit (13) per Linux mainline stmmac_mac_link_up for
        // 1Gbps full-duplex links. Default DM=0 is half-duplex
        // which causes the MAC to misinterpret incoming frames
        // as collisions on a switched link.
        // SAFETY: extern host fn into kernel net_mmio surface.
        let _ = unsafe { wari_net_mmio_write32(plat::NIC_BASE + 0x000, 0x2003) };

        // DMA_CH0_TX_CONTROL: set ST (bit 0) — start transmit.
        // Default TXPBL = 1 (lower bits 21:16). Use 0x0010_0001
        // (TXPBL=1, ST=1).
        let _ = unsafe {
            wari_net_mmio_write32(plat::NIC_BASE + 0x1104, 0x0010_0001)
        };
        // DMA_CH0_RX_CONTROL: set SR (bit 0). RXPBL=1, RBSZ field
        // bits 14:1 left at 0 for now (Phase-1c-6g sets 1536).
        let _ = unsafe {
            wari_net_mmio_write32(plat::NIC_BASE + 0x1108, 0x0010_0001)
        };

        // Tail pointer: write address of descriptor[1] (one past
        // the last ready descriptor). DWMAC starts processing
        // when current head < tail.
        let tx_tail_pa: u32 = (lin_base + tx_ring_off as u64 + 16) as u32;
        let _ = unsafe {
            wari_net_mmio_write32(plat::NIC_BASE + 0x1120, tx_tail_pa)
        };

        // Verify-read post-write state.
        for (off, tag_low) in [
            (0x000u32,  0xC0u32), // MAC_CONFIG
            (0x1104,    0xC4),    // TX_CONTROL
            (0x1108,    0xC8),    // RX_CONTROL
            (0x1120,    0xCC),    // TX_TAIL
            (0x1160,    0xD0),    // DMA_CH0_STATUS
        ] {
            let v = unsafe { wari_net_mmio_read32(plat::NIC_BASE + off) };
            let tag = 0x4754_0000 | tag_low; // 'GT\0\0' + low
            let _ = unsafe { wari_drv_log_u32(tag, v) };
        }

        // Read TDES3 of descriptor[0] back from linmem to see if
        // DMA cleared the OWN bit (= packet sent).
        // SAFETY: same — module static, single accessor.
        let tdes3 = unsafe { VF2_TX_RING.descs[0][3] };
        let _ = unsafe { wari_drv_log_u32(0x5444_4533, tdes3) }; // 'TDE3'

        // PR Phase-1c-6g — populate RX ring + set RBSZ.
        //
        // Build 89 trace showed DMA_CH0_STATUS bit 7 (RBU =
        // RX Buffer Unavailable). The DMA engine wants to write
        // received bytes somewhere but no descriptors are armed.
        //
        // Each RX descriptor:
        //   RDES0   buffer-1 PA low
        //   RDES1   buffer-1 PA high
        //   RDES2   buffer-2 PA (unused, 0)
        //   RDES3   bit 24 = BUF1V (buffer 1 valid)
        //           bit 30 = IOC (interrupt on completion)
        //           bit 31 = OWN (1 = DMA owns)
        //
        // Then DMA_CH0_RX_CONTROL.RBSZ_x_0 (bits 14:1) carries
        // the buffer size. For 1536 we write (1536 << 1) = 0xC00
        // into bits 14:1 alongside SR + RXPBL.
        // SAFETY: addr-of read on a module-static; no deref here.
        let bufs_off = unsafe { core::ptr::addr_of!(VF2_RX_BUFS.bufs) as u32 };
        let bufs_pa: u64 = lin_base + bufs_off as u64;
        let _ = unsafe { wari_drv_log_u32(0x5258_4248, (bufs_pa >> 32) as u32) }; // 'RXBH'
        let _ = unsafe { wari_drv_log_u32(0x5258_424C, bufs_pa as u32) };          // 'RXBL'

        // Fill all 16 RX descriptors. Each buffer is 1536 B
        // apart. SAFETY: module static, single writer at boot.
        unsafe {
            for i in 0..16 {
                let bp: u64 = bufs_pa + (i as u64) * 1536;
                let d = &mut VF2_RX_RING.descs[i];
                d[0] = bp as u32;             // RDES0 PA low
                d[1] = (bp >> 32) as u32;     // RDES1 PA high
                d[2] = 0;                     // RDES2 unused
                d[3] = 0x8000_0000            // OWN
                     | 0x4000_0000            // IOC
                     | 0x0100_0000;           // BUF1V
            }
        }

        // Re-write DMA_CH0_RX_CONTROL with the buffer size.
        // (1 << 0)=SR | (1536 << 1)=RBSZ | (1 << 16)=RXPBL
        let rx_ctrl = 0x0001_0000u32 | (1536u32 << 1) | 0x1;
        let _ = unsafe {
            wari_net_mmio_write32(plat::NIC_BASE + 0x1108, rx_ctrl)
        };

        // RX tail pointer = ring base + 16 descriptors × 16 B
        // = "all 16 descs are armed".
        let rx_tail_pa: u32 = (lin_base + rx_ring_off as u64 + 16 * 16) as u32;
        let _ = unsafe {
            wari_net_mmio_write32(plat::NIC_BASE + 0x1128, rx_tail_pa)
        };

        // Verify reads.
        let rx_ctrl_rb = unsafe { wari_net_mmio_read32(plat::NIC_BASE + 0x1108) };
        let _ = unsafe { wari_drv_log_u32(0x5258_43E0, rx_ctrl_rb) }; // 'RXC\xE0'

        let rx_tail_rb = unsafe { wari_net_mmio_read32(plat::NIC_BASE + 0x1128) };
        let _ = unsafe { wari_drv_log_u32(0x5258_43E1, rx_tail_rb) }; // 'RXC\xE1'

        // Read DMA_CH0_STATUS again — bit 7 (RBU) should clear.
        let status_2 = unsafe { wari_net_mmio_read32(plat::NIC_BASE + 0x1160) };
        let _ = unsafe { wari_drv_log_u32(0x5374_4132, status_2) }; // 'StA2'

        // Read RDES3 of descriptor[0] — OWN bit should still be
        // set (no frame received yet, DMA owns the buffer).
        let rdes3_0 = unsafe { VF2_RX_RING.descs[0][3] };
        let _ = unsafe { wari_drv_log_u32(0x5244_4533, rdes3_0) }; // 'RDE3'

        // PR Phase-1c-6h — clear sticky DMA_CH0_STATUS bits and
        // wait briefly to see if any frame arrives (broadcast
        // ARP responses, switch chatter, neighbour discovery
        // from other LAN hosts).
        //
        // DMA_CH0_STATUS interrupt bits are write-1-to-clear.
        // Writing 0x484 clears TBU+RBU+ETI from prior states.
        // Then a busy-wait loop (no sleep in WASM driver), then
        // re-read STATUS, the RX descriptor, and the
        // DMA_CH0_CURRENT_APP_RXDESC pointer to see what the
        // engine is doing.
        let _ = unsafe {
            wari_net_mmio_write32(plat::NIC_BASE + 0x1160, 0x0000_FFFF)
        };

        // Crude busy-wait — ~10 million NIC-base reads ≈ tens of
        // ms wall-clock. Plenty of time for a few RX frames in
        // typical LAN traffic.
        let mut spin = 0u32;
        while spin < 5_000_000 {
            spin += 1;
        }

        // Post-wait diagnostics. Tags 'PoSt' family.
        let post_status = unsafe { wari_net_mmio_read32(plat::NIC_BASE + 0x1160) };
        let _ = unsafe { wari_drv_log_u32(0x506F_5301, post_status) }; // PoS\1

        let post_rx_curr = unsafe { wari_net_mmio_read32(plat::NIC_BASE + 0x114C) };
        let _ = unsafe { wari_drv_log_u32(0x506F_5302, post_rx_curr) }; // PoS\2 (current RX descriptor pointer)

        let post_rdes3_0 = unsafe { VF2_RX_RING.descs[0][3] };
        let _ = unsafe { wari_drv_log_u32(0x506F_5303, post_rdes3_0) }; // PoS\3

        // If a frame arrived, the buffer's first 4 bytes are the
        // start of the Ethernet header (dst MAC bytes 0..3). Log
        // them so we can recognise the sender at a glance.
        let frame_word0 = unsafe { (VF2_RX_BUFS.bufs[0].as_ptr() as *const u32).read_unaligned() };
        let _ = unsafe { wari_drv_log_u32(0x506F_5304, frame_word0) }; // PoS\4 first 4B of buf

        // PR Phase-1c-6i — bypass the MAC's address filter.
        //
        // Build 92 trace showed clean DMA status post-clear but
        // no RX activity. The MAC's default packet filter only
        // accepts frames whose dst MAC matches MAC_ADDR0 (which
        // we never programmed — reads as zeros, AE=0 on JH7110
        // post-reset → MAC accepts NOTHING).
        //
        // Two writes to fix:
        //   MAC_ADDRESS0_HIGH (0x300) = 0x80004084  (AE=1 + bytes 5-4 of MAC = 84:40)
        //   MAC_ADDRESS0_LOW  (0x304) = 0x390000C7? (bytes 3-0 = 39:00:CF:6C reversed)
        //   actually MAC = 6c:cf:39:00:40:84
        //   LO = bytes [3:0] = 39 00 cf 6c LE → 0x390000? hmm
        //   The DWMAC stores MAC bytes [0..3] in MAC_ADDRESS0_LO
        //   little-endian, bytes [4..5] in MAC_ADDRESS0_HIGH low 16
        //   bits. So:
        //     LO = (b[3]<<24)|(b[2]<<16)|(b[1]<<8)|b[0]
        //        = 0x003900CF | wait
        //   For MAC = 6c:cf:39:00:40:84:
        //     b0=0x6c, b1=0xcf, b2=0x39, b3=0x00, b4=0x40, b5=0x84
        //     LO = (b3<<24)|(b2<<16)|(b1<<8)|b0
        //        = 0x00000000 | 0x00390000 | 0x0000CF00 | 0x0000006C
        //        = 0x0039CF6C
        //     HI = (AE<<31) | (b5<<8) | b4
        //        = 0x80000000 | 0x00008400 | 0x00000040
        //        = 0x80008440
        //
        //   Belt-and-braces: also enable promiscuous mode in
        //   MAC_PACKET_FILTER (0x008) bit 0. That makes the MAC
        //   accept every frame regardless of dst MAC, so even if
        //   the addr programming is wrong we still see traffic.
        // VF2 EEPROM: MAC0 = 6c:cf:39:00:40:84 (eth0/end0)
        //             MAC1 = 6c:cf:39:00:40:85 (eth1/end1)
        // Low 4 bytes [0..3] are identical (= 0x0039CF6C); the high
        // word's byte 5 flips :84 vs :85 → 0x80008440 vs 0x80008540.
        let mac_lo: u32 = 0x0039_CF6C;
        #[cfg(not(feature = "gmac1"))]
        let mac_hi: u32 = 0x8000_8440;
        #[cfg(feature = "gmac1")]
        let mac_hi: u32 = 0x8000_8540;
        let _ = unsafe { wari_net_mmio_write32(plat::NIC_BASE + 0x300, mac_hi) };
        let _ = unsafe { wari_net_mmio_write32(plat::NIC_BASE + 0x304, mac_lo) };

        // MAC_PACKET_FILTER: PR bit 0 = promiscuous (accept all).
        let _ = unsafe { wari_net_mmio_write32(plat::NIC_BASE + 0x008, 0x0000_0001) };

        // Build 133 — MAC_RXQ_CTRL0 (0x00A0) bits[1:0] = RXQ0EN.
        // After reset this register is 0x00000000, meaning RXQ0 is
        // DISABLED. With RXQ0 disabled the MAC's RX FSM silently
        // drops every frame BEFORE it counts in MMC, BEFORE it
        // reaches MTL, BEFORE it reaches DMA — which exactly
        // matches the NmGB=0 + NT_M=0 + ND_C-not-advancing + link-up
        // signature we observed across builds 124-132.
        //
        // Linux's dwmac4_rx_queue_enable() writes 0b10 = DCB mode
        // for normal Ethernet (vs 0b01 AVB). Wari never wrote this
        // register; that's why no frames ever made it to the
        // descriptor ring despite everything else looking correct.
        //
        // Value 0x00000002 = RXQ0EN[1:0] = 10 (DCB), all other
        // queues left disabled (we only use RXQ0).
        let _ = unsafe { wari_net_mmio_write32(plat::NIC_BASE + 0x0A0, 0x0000_0002) };

        // Verify-read.
        let mac_hi_rb = unsafe { wari_net_mmio_read32(plat::NIC_BASE + 0x300) };
        let mac_lo_rb = unsafe { wari_net_mmio_read32(plat::NIC_BASE + 0x304) };
        let pf_rb     = unsafe { wari_net_mmio_read32(plat::NIC_BASE + 0x008) };
        let rxq_rb    = unsafe { wari_net_mmio_read32(plat::NIC_BASE + 0x0A0) };
        let _ = unsafe { wari_drv_log_u32(0x4D41_4348, mac_hi_rb) }; // 'MACH'
        let _ = unsafe { wari_drv_log_u32(0x4D41_434C, mac_lo_rb) }; // 'MACL'
        let _ = unsafe { wari_drv_log_u32(0x4D41_4346, pf_rb) };     // 'MACF'
        let _ = unsafe { wari_drv_log_u32(0x5258_5130, rxq_rb) };    // 'RXQ0'

        // Clear status sticky bits again, longer wait.
        let _ = unsafe { wari_net_mmio_write32(plat::NIC_BASE + 0x1160, 0x0000_FFFF) };
        let mut spin2 = 0u32;
        while spin2 < 20_000_000 {
            spin2 += 1;
        }

        // Second-look diagnostics. Tags 'Wt2_' family.
        let st2 = unsafe { wari_net_mmio_read32(plat::NIC_BASE + 0x1160) };
        let _ = unsafe { wari_drv_log_u32(0x5774_3201, st2) };

        // Walk the first 4 RX descriptors looking for OWN cleared.
        for i in 0..4u32 {
            let r = unsafe { VF2_RX_RING.descs[i as usize][3] };
            let tag = 0x5774_3300 | i;
            let _ = unsafe { wari_drv_log_u32(tag, r) };
        }

        // Also dump first 4 bytes of buffer 0 again.
        let w2 = unsafe {
            (VF2_RX_BUFS.bufs[0].as_ptr() as *const u32).read_unaligned()
        };
        let _ = unsafe { wari_drv_log_u32(0x5774_3204, w2) };

        // PR Phase-1c-6j — configure MTL RX queue 0.
        //
        // DWMAC4 has two layers between the GMAC IP and the
        // wire: MTL (Media Transmission Layer, internal FIFO
        // scheduling) and the DMA engine. We configured DMA
        // already; MTL RXQ0 still defaults to disabled, which
        // is why frames are silently dropped before they reach
        // the descriptor.
        //
        // MTL_RXQ0_OPERATION_MODE @ 0xD30:
        //   bit  5     RSF        Receive Store-and-Forward
        //   bits 19:8  RQS        Receive Queue Size = (FIFO/256)-1
        //                         JH7110 RX FIFO = 2 KiB → RQS = 7
        //
        // Write 0x00000720 = (7 << 8) | (1 << 5).
        //
        // Also re-write MTL_TXQ0_OPERATION_MODE @ 0xD00 to be
        // explicit even though TX worked with defaults:
        //   bit  1     TSF        Transmit Store-and-Forward
        //   bits  3:2  TXQEN      Transmit Queue Enable
        //                         (10 = enable as default queue)
        //   bits 24:16 TQS        Transmit Queue Size — 32 KiB / 256
        //                         - 1 = 0x7F
        const MTL_TXQ0_OP_MODE: u32 = 0xD00;
        const MTL_RXQ0_OP_MODE: u32 = 0xD30;
        // TX: TSF | TXQEN=10b (enabled) | TQS=0x7F
        let tx_q_op = (1u32 << 1) | (0b10 << 2) | (0x7F << 16);
        let _ = unsafe { wari_net_mmio_write32(plat::NIC_BASE + MTL_TXQ0_OP_MODE, tx_q_op) };
        // RX: RSF | RQS=7
        let rx_q_op = (1u32 << 5) | (7 << 8);
        let _ = unsafe { wari_net_mmio_write32(plat::NIC_BASE + MTL_RXQ0_OP_MODE, rx_q_op) };

        // Verify-read.
        let txq_rb = unsafe { wari_net_mmio_read32(plat::NIC_BASE + MTL_TXQ0_OP_MODE) };
        let rxq_rb = unsafe { wari_net_mmio_read32(plat::NIC_BASE + MTL_RXQ0_OP_MODE) };
        let _ = unsafe { wari_drv_log_u32(0x4D54_4C54, txq_rb) }; // 'MTLT'
        let _ = unsafe { wari_drv_log_u32(0x4D54_4C52, rxq_rb) }; // 'MTLR'

        // Long wait, re-check.
        let _ = unsafe { wari_net_mmio_write32(plat::NIC_BASE + 0x1160, 0x0000_FFFF) };
        let mut spin3 = 0u32;
        while spin3 < 30_000_000 {
            spin3 += 1;
        }

        let st3 = unsafe { wari_net_mmio_read32(plat::NIC_BASE + 0x1160) };
        let _ = unsafe { wari_drv_log_u32(0x5774_3301, st3) }; // Wt3\1

        // Walk all 16 RX descs looking for OWN cleared.
        for i in 0..16u32 {
            let r = unsafe { VF2_RX_RING.descs[i as usize][3] };
            // Only log if changed (not still 0xC1000000).
            if r != 0xC100_0000 {
                let tag = 0x5230_0000 | (i & 0xFF);
                let _ = unsafe { wari_drv_log_u32(tag, r) };
            }
        }

        // Final: dump first 4 bytes of buf 0 + buf 1.
        let f0 = unsafe { (VF2_RX_BUFS.bufs[0].as_ptr() as *const u32).read_unaligned() };
        let f1 = unsafe { (VF2_RX_BUFS.bufs[1].as_ptr() as *const u32).read_unaligned() };
        let _ = unsafe { wari_drv_log_u32(0x4275_4630, f0) }; // 'BuF0'
        let _ = unsafe { wari_drv_log_u32(0x4275_4631, f1) }; // 'BuF1'

        // PR Phase-1c-6k — DMA RX IRQ enable + RPF + send a 2nd ARP
        // to generate traffic, then re-check.
        //
        // DMA_CH0_INTERRUPT_ENABLE @ 0x1134:
        //   bit 0  = TIE  Transmit Interrupt Enable
        //   bit 6  = RIE  Receive Interrupt Enable
        //   bit 14 = AIE  Abnormal Interrupt Enable
        //   bit 15 = NIE  Normal Interrupt Enable
        // Without this set, status bits like RI may not assert
        // even when the engine IS receiving frames.
        let ie = (1u32 << 0) | (1 << 6) | (1 << 14) | (1 << 15);
        let _ = unsafe { wari_net_mmio_write32(plat::NIC_BASE + 0x1134, ie) };

        // Re-write DMA_CH0_RX_CONTROL with RPF (bit 31) set —
        // forces the RX descriptor processor to poll continuously
        // rather than wait for a tail-pointer update. Belt-and-
        // braces in case our tail-pointer write didn't kick the
        // engine out of idle.
        let rx_ctrl_rpf = 0x8000_0000u32 | 0x0001_0000 | (1536u32 << 1) | 0x1;
        let _ = unsafe { wari_net_mmio_write32(plat::NIC_BASE + 0x1108, rx_ctrl_rpf) };

        // Re-fire a TX broadcast ARP so any switch/AP/router on
        // the LAN responds and we get RX traffic in our wait
        // window. Reuse VF2_FIRST_PKT — descriptor 1.
        let pkt_pa_2: u64 = lin_base + (core::ptr::addr_of!(VF2_FIRST_PKT) as u32) as u64;
        unsafe {
            let d = &mut VF2_TX_RING.descs[1];
            d[0] = pkt_pa_2 as u32;
            d[1] = (pkt_pa_2 >> 32) as u32;
            d[2] = 64;
            d[3] = 0xB000_0040; // OWN | LD | FD | length 64
        }
        // Update TX tail pointer to descriptor[2].
        let new_tx_tail: u32 = (lin_base + tx_ring_off as u64 + 32) as u32;
        let _ = unsafe { wari_net_mmio_write32(plat::NIC_BASE + 0x1120, new_tx_tail) };

        // Verify-reads.
        let ie_rb = unsafe { wari_net_mmio_read32(plat::NIC_BASE + 0x1134) };
        let rxc_rb = unsafe { wari_net_mmio_read32(plat::NIC_BASE + 0x1108) };
        let _ = unsafe { wari_drv_log_u32(0x4945_4E54, ie_rb) }; // 'IENT'
        let _ = unsafe { wari_drv_log_u32(0x5258_4332, rxc_rb) }; // 'RXC2'

        // Long wait, last check.
        let _ = unsafe { wari_net_mmio_write32(plat::NIC_BASE + 0x1160, 0x0000_FFFF) };
        let mut spin4 = 0u32;
        while spin4 < 50_000_000 {
            spin4 += 1;
        }

        let st4 = unsafe { wari_net_mmio_read32(plat::NIC_BASE + 0x1160) };
        let _ = unsafe { wari_drv_log_u32(0x5374_3401, st4) }; // 'St4\1'

        for i in 0..16u32 {
            let r = unsafe { VF2_RX_RING.descs[i as usize][3] };
            if r != 0xC100_0000 {
                let tag = 0x5234_0000 | (i & 0xFF);
                let _ = unsafe { wari_drv_log_u32(tag, r) };
            }
        }
        let f0_2 = unsafe { (VF2_RX_BUFS.bufs[0].as_ptr() as *const u32).read_unaligned() };
        let _ = unsafe { wari_drv_log_u32(0x4275_4632, f0_2) }; // 'BuF2'

        // PR Phase-1c-7 — wire smoltcp on top of the GMAC bring-up.
        //
        // Steps:
        //   1. stash lin_base for the smoltcp Device impl
        //   2. derive MAC from MAC_ADDR0 readback (which we already
        //      programmed in 1c-6i to 6c:cf:39:00:40:84)
        //   3. nic_iface::init(mac) — builds smoltcp Interface +
        //      empty SocketSet
        //   4. wari_nic_set_mac(...) — kernel-side: sets
        //      Net.initialized = true so run_tier2_net runs
        //      tier2_net::install, after which the kernel idle
        //      loop calls driver_poll(tick) every iteration =
        //      continuous RX drain.

        unsafe {
            vf2_state::LIN_BASE = lin_base;
        }

        // MAC bytes from EEPROM, mirroring the MAC_ADDR0 write
        // at lines 2849-2851. eth0 = :84 (GMAC0), eth1 = :85
        // (GMAC1). Without the cfg gate here, builds 125-128 fed
        // :84 to wari_nic_set_mac even when the rest of the driver
        // was correctly targeting GMAC1 — making the kernel's
        // boot trace lie about which MAC was running.
        #[cfg(not(feature = "gmac1"))]
        let mac: [u8; 6] = [0x6c, 0xCF, 0x39, 0x00, 0x40, 0x84];
        #[cfg(feature = "gmac1")]
        let mac: [u8; 6] = [0x6c, 0xCF, 0x39, 0x00, 0x40, 0x85];

        // Kick smoltcp Interface up. nic_iface owns the static
        // INTERFACE / SOCKETS slots; init populates them.
        if nic_iface::init(mac).is_err() {
            // Init failure leaves Net.initialized = false; the
            // kernel logs '[net] virtio init failed' as before.
            return;
        }

        // Tell the kernel we're ready. mac_low = bytes 3..0,
        // mac_high = bytes 5..4 (matches qemu path).
        let mac_low = (mac[0] as u32)
            | ((mac[1] as u32) << 8)
            | ((mac[2] as u32) << 16)
            | ((mac[3] as u32) << 24);
        let mac_high = (mac[4] as u32) | ((mac[5] as u32) << 8);
        // SAFETY: extern host fn. Kernel cap-checks Net+WRITE.
        let _ = unsafe { wari_nic_set_mac(mac_low, mac_high) };

        // Build-129 net-diag: boot-time register snapshot. Fires once
        // at end of init so the operator sees the full RX-path state
        // BEFORE any traffic arrives — a "this is what the MAC looks
        // like with my init complete" baseline.
        #[cfg(feature = "net-diag")]
        diag::boot_dump(plat::NIC_BASE);
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
    fn socket_accept(handle: u32) -> i32 {
        nic_iface::socket_poll_accept(handle)
    }
    fn socket_send_canned(handle: u32) -> i32 {
        nic_iface::socket_send_canned(handle)
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

/// `socket_create` — platform-neutral as of Phase-1c-7. Allocates
/// a TCP smoltcp socket via `nic_iface::socket_create_tcp`, returns
/// a packed handle (smoltcp::iface::SocketHandle as u32).
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

pub fn driver_socket_close(handle: u32) -> i32 {
    nic_iface::socket_close(handle)
}

pub fn driver_socket_bind(handle: u32, ip_be: u32, port: u32) -> i32 {
    nic_iface::socket_bind(handle, ip_be, port)
}

pub fn driver_socket_listen(handle: u32, backlog: u32) -> i32 {
    let _ = backlog; // smoltcp single-pending only — backlog ignored
    nic_iface::socket_listen(handle)
}

// Phase-1c HTTP demo — accept + canned-send are exported by the
// `wari_net_driver!` macro from the `NetDriver` trait impl below
// (which delegates to `nic_iface::socket_poll_accept` /
// `socket_send_canned`). The macro writes manifest entries the
// sign tool requires.
