// SPDX-License-Identifier: AGPL-3.0-only
//! Pure builders for the raw frames Wari puts on the wire.
//!
//! One concern: turning typed inputs (a MAC, an IPv4 address) into the
//! exact bytes a NIC transmits. No MMIO, no statics, no `unsafe` — so
//! this compiles for `wasm32` (where the drivers run) *and* for the
//! host (where the tests run). Same discipline as `wari-policy` and
//! `wari-validate`.
//!
//! # Why this crate exists
//!
//! The VF2 bring-up frame used to be 64 hand-transcribed hex bytes with
//! a prose comment describing the layout. Nothing tied the bytes to the
//! comment, and nothing tied either to the platform's actual MAC and IP.
//! Both drifted: the frame still carried the GMAC0 MAC after the driver
//! moved to GMAC1, and an IP from a subnet the test network had not used
//! in months. It was a broadcast question no host could answer, and no
//! test could have caught it — because a byte array has no contract.
//!
//! The fix is not "validate the bytes." It is to stop writing bytes by
//! hand: derive every frame from the same named values the rest of the
//! driver uses, so a platform change updates the frame automatically.
//! See INV-24 in `docs/invariants.md`.
//!
//! # Prior art
//!
//! Frame layouts follow RFC 826 (ARP) and IEEE 802.3. Building frames
//! from typed inputs rather than literals is the approach smoltcp takes
//! with its `wire` module; we need a dependency-free subset because the
//! bring-up frame is transmitted *before* smoltcp is initialized.

#![no_std]

/// Bytes in the frame produced by [`arp_announce`].
///
/// An ARP-over-Ethernet frame is 42 bytes (14 Ethernet + 28 ARP). We
/// emit 64 so the buffer handed to the DMA engine is a round,
/// cache-line-friendly size and comfortably above the 60-byte Ethernet
/// minimum — the MAC pads and appends the FCS itself, but starting at
/// the minimum leaves no room for a descriptor length off-by-one to go
/// unnoticed on a scope.
pub const ARP_FRAME_LEN: usize = 64;

/// Octets in an Ethernet MAC address.
pub const MAC_LEN: usize = 6;

/// Octets in an IPv4 address.
pub const IPV4_LEN: usize = 4;

// ── Field offsets (RFC 826 over IEEE 802.3) ─────────────────────────
// Named so the builder never indexes with a bare integer. The tests
// deliberately do NOT use these constants — they assert against
// independently written literals, so a wrong constant fails the suite
// instead of silently redefining "correct".

const OFF_DST_MAC: usize = 0;
const OFF_SRC_MAC: usize = 6;
const OFF_ETHERTYPE: usize = 12;
const OFF_HTYPE: usize = 14;
const OFF_PTYPE: usize = 16;
const OFF_HLEN: usize = 18;
const OFF_PLEN: usize = 19;
const OFF_OPER: usize = 20;
const OFF_SHA: usize = 22;
const OFF_SPA: usize = 28;
const OFF_THA: usize = 32;
const OFF_TPA: usize = 38;
/// First byte of the zero pad; also the true end of ARP content.
const OFF_PAD: usize = 42;

const ETHERTYPE_ARP: [u8; 2] = [0x08, 0x06];
const HTYPE_ETHERNET: [u8; 2] = [0x00, 0x01];
const PTYPE_IPV4: [u8; 2] = [0x08, 0x00];
const OPER_REQUEST: [u8; 2] = [0x00, 0x01];
const BROADCAST: [u8; MAC_LEN] = [0xFF; MAC_LEN];

// The ARP fields must tile exactly, with no gap or overlap, and the
// whole frame must fit its buffer. Checked at compile time so a bad
// offset edit cannot ship — including for THA, which is left zero at
// runtime and would otherwise have no reader to catch a wrong value.
const _: () = assert!(OFF_SHA + MAC_LEN == OFF_SPA);
const _: () = assert!(OFF_SPA + IPV4_LEN == OFF_THA);
const _: () = assert!(OFF_THA + MAC_LEN == OFF_TPA);
const _: () = assert!(OFF_TPA + IPV4_LEN == OFF_PAD);
const _: () = assert!(OFF_PAD <= ARP_FRAME_LEN);

/// Build a **gratuitous ARP announcement** for `mac` / `ip`.
///
/// A gratuitous ARP sets both the sender and target protocol address to
/// the host's own IP. It says "this MAC now owns this IP" rather than
/// asking a question, which is exactly what a NIC's first transmission
/// should do:
///
/// - It is meaningful on **any** network. A who-has request needs a
///   peer address to ask about, which would mean the driver knowing
///   something about the deployment's subnet — the assumption that
///   rotted last time. An announcement needs only what the interface
///   already knows about itself.
/// - It populates switch MAC-address tables immediately, so the first
///   real reply is not delayed by the switch flooding to find us.
/// - It is legible on a mirror port: a protocol analyzer decodes it as
///   ARP rather than as an unclassified runt.
///
/// # Parameters
///
/// - `mac`: the interface's hardware address, as read from the device.
///   Never a literal — see the module docs.
/// - `ip`: the interface's configured IPv4 address.
///
/// # Postconditions
///
/// Returns exactly [`ARP_FRAME_LEN`] bytes: a well-formed ARP request
/// whose sender and target protocol addresses both equal `ip`, whose
/// source and sender hardware addresses both equal `mac`, destined for
/// the broadcast address. Bytes from [`OFF_PAD`] to the end are zero.
///
/// # Panics
///
/// Never. All indices are compile-time constants checked against
/// [`ARP_FRAME_LEN`] by the `const` assertions above.
pub fn arp_announce(mac: [u8; MAC_LEN], ip: [u8; IPV4_LEN]) -> [u8; ARP_FRAME_LEN] {
    let mut f = [0u8; ARP_FRAME_LEN];

    f[OFF_DST_MAC..OFF_DST_MAC + MAC_LEN].copy_from_slice(&BROADCAST);
    f[OFF_SRC_MAC..OFF_SRC_MAC + MAC_LEN].copy_from_slice(&mac);
    f[OFF_ETHERTYPE..OFF_ETHERTYPE + 2].copy_from_slice(&ETHERTYPE_ARP);

    f[OFF_HTYPE..OFF_HTYPE + 2].copy_from_slice(&HTYPE_ETHERNET);
    f[OFF_PTYPE..OFF_PTYPE + 2].copy_from_slice(&PTYPE_IPV4);
    f[OFF_HLEN] = MAC_LEN as u8;
    f[OFF_PLEN] = IPV4_LEN as u8;
    f[OFF_OPER..OFF_OPER + 2].copy_from_slice(&OPER_REQUEST);

    f[OFF_SHA..OFF_SHA + MAC_LEN].copy_from_slice(&mac);
    f[OFF_SPA..OFF_SPA + IPV4_LEN].copy_from_slice(&ip);
    // THA stays zero: in a request the target hardware address is what
    // we are asking for, and in an announcement nobody needs to answer.
    f[OFF_TPA..OFF_TPA + IPV4_LEN].copy_from_slice(&ip);

    f
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Distinctive values so a misplaced field is obvious in a failure
    /// message, and so no two fields share a byte pattern.
    const MAC: [u8; 6] = [0x6C, 0xCF, 0x39, 0x11, 0x22, 0x33];
    const IP: [u8; 4] = [10, 1, 2, 3];

    // NOTE: these tests intentionally use literal offsets and literal
    // wire values taken from RFC 826, NOT the module's constants. If a
    // constant is edited to something wrong, the builder and the
    // constants would still agree with each other — only an
    // independently written expectation catches it.

    #[test]
    fn frame_is_exactly_64_bytes() {
        assert_eq!(arp_announce(MAC, IP).len(), 64);
    }

    #[test]
    fn ethernet_header_is_broadcast_from_our_mac_typed_arp() {
        let f = arp_announce(MAC, IP);
        assert_eq!(&f[0..6], &[0xFF; 6], "dst must be broadcast");
        assert_eq!(&f[6..12], &MAC, "src must be our MAC");
        assert_eq!(&f[12..14], &[0x08, 0x06], "ethertype must be ARP");
    }

    #[test]
    fn arp_header_declares_ethernet_over_ipv4_request() {
        let f = arp_announce(MAC, IP);
        assert_eq!(&f[14..16], &[0x00, 0x01], "HTYPE must be Ethernet");
        assert_eq!(&f[16..18], &[0x08, 0x00], "PTYPE must be IPv4");
        assert_eq!(f[18], 6, "HLEN must be 6");
        assert_eq!(f[19], 4, "PLEN must be 4");
        assert_eq!(&f[20..22], &[0x00, 0x01], "OPER must be request");
    }

    #[test]
    fn announcement_claims_our_own_address_on_both_sides() {
        let f = arp_announce(MAC, IP);
        assert_eq!(&f[22..28], &MAC, "SHA must be our MAC");
        assert_eq!(&f[28..32], &IP, "SPA must be our IP");
        assert_eq!(&f[32..38], &[0u8; 6], "THA must be zero");
        // The gratuitous property: we announce, we do not interrogate.
        assert_eq!(&f[38..42], &IP, "TPA must equal SPA");
    }

    #[test]
    fn tail_is_zero_padded() {
        assert_eq!(&arp_announce(MAC, IP)[42..64], &[0u8; 22]);
    }

    /// The regression this crate exists to prevent: the frame must
    /// track its inputs. A hand-transcribed frame passes every test
    /// above and still fails this one once the platform changes.
    #[test]
    fn frame_tracks_its_inputs_rather_than_a_literal() {
        let a = arp_announce(MAC, IP);
        let b = arp_announce([0x00, 0x11, 0x22, 0x33, 0x44, 0x55], [192, 168, 1, 1]);
        assert_ne!(a, b, "frame must depend on mac/ip, not on constants");

        // And a MAC change must move exactly the two MAC fields.
        let c = arp_announce([0x6C, 0xCF, 0x39, 0x11, 0x22, 0x34], IP);
        assert_eq!(&c[28..32], &IP, "changing MAC must not disturb SPA");
        assert_ne!(&c[6..12], &a[6..12], "src MAC must follow the input");
        assert_ne!(&c[22..28], &a[22..28], "SHA must follow the input");
    }

    /// A zero MAC means the driver never learned its address. The frame
    /// is still well-formed — catching that is the caller's job — but
    /// this pins the behavior so it cannot change silently.
    #[test]
    fn zero_mac_still_produces_a_well_formed_frame() {
        let f = arp_announce([0u8; 6], IP);
        assert_eq!(&f[12..14], &[0x08, 0x06]);
        assert_eq!(&f[6..12], &[0u8; 6]);
    }
}
