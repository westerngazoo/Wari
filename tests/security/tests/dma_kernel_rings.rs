// SPDX-License-Identifier: AGPL-3.0-only
//! The NIC's descriptor tables live in kernel memory and the network
//! still works (INV-25).
//!
//! ## What this test verifies
//!
//! A DMA engine reads its descriptors to learn where in physical
//! memory to read and write. While those descriptors sat in the
//! driver's linear memory, the kernel validating an address before
//! writing it there proved nothing: the driver could overwrite the
//! address, or the length next to it, with an ordinary WASM store
//! afterwards, and aim the device at kernel memory. The tables now
//! live in the kernel; the driver has no descriptor to store to.
//!
//! That removal is structural — there is nothing left in the sandbox
//! to attack, so there is no attack to replay. What can break, and
//! what this test catches, is the other side: the device must really
//! be using the kernel's tables. So the test drives one frame through
//! each DMA direction and looks for it on the wire:
//!
//! - **RX**: QEMU's user-mode stack sends an ARP request for the
//!   guest's address. The guest can only see it if the device wrote
//!   it into a buffer named by a kernel-written RX descriptor.
//! - **TX**: the guest's ARP reply appears on the wire only if the
//!   device read a kernel-written TX descriptor.
//!
//! The refusals (span outside memory, index past the ring, length
//! past the end) are asserted in `dma_escape.rs`.

use std::time::Duration;

use wari_security_tests::{boot_kernel_capture_wire, markers, pcap_frames};

/// The guest's MAC: QEMU's default for the first NIC.
const GUEST_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
/// EtherType ARP.
const ETHERTYPE_ARP: [u8; 2] = [0x08, 0x06];
/// ARP opcode 2: reply.
const ARP_REPLY: [u8; 2] = [0x00, 0x02];
/// ARP opcode 1: request.
const ARP_REQUEST: [u8; 2] = [0x00, 0x01];

/// The net stack is up well inside the first second; the poke goes
/// out after it, and the run ends once the exchange has had time.
const WALLCLOCK: Duration = Duration::from_secs(8);
const POKE_AFTER: Duration = Duration::from_secs(4);

/// Is `frame` an ARP frame with opcode `op` sent from `src`
/// (`None` = from anyone)?
fn is_arp(frame: &[u8], op: [u8; 2], src: Option<[u8; 6]>) -> bool {
    // Ethernet: dst(6) src(6) type(2); ARP opcode is bytes 6..8 of
    // the ARP payload.
    frame.get(12..14) == Some(&ETHERTYPE_ARP)
        && frame.get(20..22) == Some(&op)
        && src.is_none_or(|mac| frame.get(6..12) == Some(&mac))
}

#[test]
fn frames_cross_both_dma_directions_through_kernel_owned_descriptors() {
    let (text, wire) = boot_kernel_capture_wire(WALLCLOCK, POKE_AFTER);

    assert!(
        text.contains(markers::NET_UP),
        "net stack did not come up:\n{text}",
    );
    let frames = pcap_frames(&wire);

    // Control: a request not sent by the guest really went out.
    // Without it a missing reply would say nothing about the guest.
    assert!(
        frames
            .iter()
            .any(|f| is_arp(f, ARP_REQUEST, None) && !is_arp(f, ARP_REQUEST, Some(GUEST_MAC))),
        "no ARP request reached the wire ({} frames captured) — harness problem:\n{text}",
        frames.len(),
    );

    // RX then TX: the guest received the request and answered it.
    assert!(
        frames.iter().any(|f| is_arp(f, ARP_REPLY, Some(GUEST_MAC))),
        "the guest never answered ARP: a frame did not cross the RX or the TX \
         DMA path ({} frames captured):\n{text}",
        frames.len(),
    );

    assert!(!text.contains("panicked at"), "rust panic:\n{text}");
}
