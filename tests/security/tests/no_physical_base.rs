// SPDX-License-Identifier: AGPL-3.0-only
//! No shipped Tier-2 net driver can learn its own physical address
//! (ADR-002 step 1, INV-25).
//!
//! The kernel no longer provides `wari::lin_mem_base`: a driver
//! importing it fails to instantiate. That makes the property "no
//! driver forms a DMA address" structural — but only for drivers that
//! actually ship without the import, because a driver that still
//! asked for it would simply stop loading, which the boot tests would
//! catch as an outage, not as a containment regression. This test
//! pins the property on the artefacts themselves: the signed net
//! drivers for both platforms neither import the function nor declare
//! it in their manifest (the sign-tool keeps the two in step).

use wari_security_tests::workspace_root;

/// The retired host function's name, as it would appear in a WASM
/// import entry and in the driver manifest.
const RETIRED_IMPORT: &[u8] = b"lin_mem_base";

/// Signed net drivers the kernel embeds, workspace-relative.
const SIGNED_NET_DRIVERS: &[&str] = &[
    "build/drivers/net-qemu.signed.wasm",
    "build/drivers/net-vf2.signed.wasm",
];

#[test]
fn shipped_net_drivers_do_not_ask_for_their_physical_base() {
    for rel in SIGNED_NET_DRIVERS {
        let path = workspace_root().join(rel);
        let wasm = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{rel} missing ({e}); build through `make` first"));
        assert!(
            !wasm
                .windows(RETIRED_IMPORT.len())
                .any(|w| w == RETIRED_IMPORT),
            "{rel} still names `lin_mem_base` (import or manifest entry)"
        );
    }
}
