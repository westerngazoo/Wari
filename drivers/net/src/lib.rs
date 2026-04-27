// SPDX-License-Identifier: AGPL-3.0-only
//! Tier-2 net driver — Phase-1b scaffold.
//!
//! Built as a separately-signed `.wasm` per platform: one for QEMU
//! `virt` (VirtIO-net at `0x10008000`) and one for StarFive
//! VisionFive 2 (JH7110 GMAC eth0 at `0x16030000`). Activated via
//! cargo feature: `--features qemu` or `--features vf2`.
//!
//! ## Phase-1b PR Net-4a scope (this PR)
//!
//! - Crate scaffold + per-platform feature gates.
//! - Host-fn imports declared so wasmi link-time resolution exercises
//!   the kernel-side `register_net_host_fns` from PR Net-3.
//! - `_start` is a stub that returns immediately. The driver loads
//!   into the kernel without actually driving any NIC hardware.
//!
//! ## Phase-1b PR Net-4b (next)
//!
//! - VirtIO-net device discovery + queue setup.
//! - MAC address read from device config.
//! - Link bring-up.
//!
//! ## Phase-1b PR Net-4c
//!
//! - ARP responder (kernel responds to ARP requests for its IP).
//! - ICMP echo responder (kernel responds to host pings).
//!
//! ## Phase-1c PR Net-9
//!
//! - JH7110 GMAC implementation in `mod gmac` (currently a `loop {}`
//!   stub gated on `feature = "vf2"`).
//!
//! ## Lockstep maintenance
//!
//! The platform constants below duplicate
//! `kernel/src/validate.rs::NET_MMIO_BASE`. The duplication is
//! structural — a `wasm32-unknown-unknown` cdylib cannot depend on
//! the kernel crate. If the NIC moves, update both sides in the same
//! PR. The kernel-side validator (`validate::is_net_mmio_addr`)
//! enforces that the driver only ever writes to the agreed NIC
//! window regardless.

#![no_std]
#![no_main]

#[cfg(not(any(feature = "qemu", feature = "vf2")))]
compile_error!("wari-driver-net requires --features qemu or --features vf2.");

#[cfg(all(feature = "qemu", feature = "vf2"))]
compile_error!("wari-driver-net accepts only one of --features qemu / vf2.");

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // Tier-2 panics are bugs in signed code. wasmi's trap will fire
    // before this in practice; the loop is the structural fallback.
    loop {}
}

// ── Platform constants (lockstep with kernel/src/validate.rs) ────

#[cfg(feature = "qemu")]
mod plat {
    /// VirtIO-net MMIO base on QEMU `virt`.
    #[allow(dead_code)]
    pub const NIC_BASE: u32 = 0x1000_8000;
    /// Kind discriminant matching the kernel's `Net.nic_kind`.
    #[allow(dead_code)]
    pub const NIC_KIND: u8 = 0;
}

#[cfg(feature = "vf2")]
mod plat {
    /// JH7110 GMAC eth0 base on VisionFive 2.
    #[allow(dead_code)]
    pub const NIC_BASE: u32 = 0x1603_0000;
    /// Kind discriminant matching the kernel's `Net.nic_kind`.
    #[allow(dead_code)]
    pub const NIC_KIND: u8 = 1;
}

// ── Host-function imports ────────────────────────────────────────
//
// Declared so wasmi's link-time resolution exercises the kernel's
// `register_net_host_fns` from PR Net-3. PR Net-4b actually CALLS
// these from `init_nic`; this PR's stub never invokes them, but the
// import declarations keep the WASM ABI stable as features land.

#[link(wasm_import_module = "wari")]
extern "C" {
    /// Host fn — write a 32-bit value to NIC register at `addr`.
    /// Cap-gated by the driver's `Net` cap with WRITE rights.
    /// Returns 0 on success, negative errno on failure.
    #[allow(dead_code)]
    #[link_name = "net_mmio_write32"]
    fn wari_net_mmio_write32(addr: u32, val: u32) -> i32;

    /// Host fn — read a 32-bit NIC register. Same gating with READ.
    /// Returns the value (0..=u32::MAX-1) on success, `u32::MAX` as
    /// no-permission / out-of-range sentinel.
    #[allow(dead_code)]
    #[link_name = "net_mmio_read32"]
    fn wari_net_mmio_read32(addr: u32) -> u32;

    /// Host fn — block waiting for the IRQ-bound notification at the
    /// driver's CSpace `slot`. Phase-1b polling: returns 0 if
    /// signaled, `-4` (E_AGAIN) if not.
    #[allow(dead_code)]
    #[link_name = "notification_wait"]
    fn wari_notification_wait(slot: u32) -> i32;

    /// Host fn — clear the signal bitmap on the IRQ-bound
    /// notification at `slot` after IRQ work completes.
    #[allow(dead_code)]
    #[link_name = "notification_ack"]
    fn wari_notification_ack(slot: u32) -> i32;
}

// ── Driver entry ─────────────────────────────────────────────────

/// Driver entry. Phase-1b PR Net-4a: stub that returns immediately.
/// PR Net-4b will replace this body with `init_nic()` (VirtIO-net
/// discovery + ring setup + link bring-up).
///
/// The kernel's loader path (added in this PR alongside the driver)
/// instantiates the WASM module, calls `_start` once at boot, then
/// retains the instance as a "library" process the scheduler does
/// not pick to run. Future Tier-1 socket calls re-enter the driver
/// via host-fn dispatch.
#[no_mangle]
pub extern "C" fn _start() {
    // Phase-1b PR Net-4a — no-op. The driver loads, registers itself
    // as a library process, and idles. Future PRs add real
    // initialization (Net-4b), protocol handlers (Net-4c), and the
    // socket API (Net-5).
}
