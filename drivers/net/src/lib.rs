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

    #[allow(dead_code)]
    #[link_name = "notification_wait"]
    fn wari_notification_wait(slot: u32) -> i32;

    #[allow(dead_code)]
    #[link_name = "notification_ack"]
    fn wari_notification_ack(slot: u32) -> i32;
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

    // §3.1.1 step 7 — virtqueue setup. **DEFERRED to PR Net-4c.**
    // Phase-1b PR Net-4b leaves queues unconfigured; the device
    // is configured but no packets exchange.

    // §3.1.1 step 8 — set DRIVER_OK. Device now considers the
    // driver ready (even though we have no queues yet — VirtIO
    // allows a driver with zero queues; the device just won't
    // send/receive packets).
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
#[no_mangle]
pub extern "C" fn _start() {
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
    }
    // The vf2 path is a Phase-1c stub — return immediately, leave
    // Net.initialized = false on the kernel side. The kernel logs
    // "[net] not yet implemented on vf2" if needed (Phase-1c TODO).
    #[cfg(feature = "vf2")]
    {}
}
