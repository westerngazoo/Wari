// SPDX-License-Identifier: AGPL-3.0-only
//! A real TCP client gets the HTTP demo's answer through QEMU's
//! VirtIO-net (the end-to-end test PM-78 asked for).
//!
//! Until this test existed the VirtIO transmit path was only ever
//! exercised by one frame per boot, and it was broken: the driver
//! published its first frame twice and never published another, so a
//! connection's SYN-ACK never left the guest. ARP answers hid it.
//!
//! The client is the host side of a QEMU `hostfwd`; the server is the
//! Tier-1 `apps/hello` demo behind the Tier-2 net driver. Passing
//! needs the handshake, the request, the response and its close to
//! cross both queues, several frames each way. (Whether it would also
//! catch the single transmit buffer being reused before the device has
//! read it is not demonstrated: that depends on QEMU's TX timing.)

use std::time::Duration;

use wari_security_tests::boot_kernel_http_get;
use wari_security_tests::markers::{ACCEPT_WINDOW_OPEN, HTTP_SERVED};

/// Boot plus the first accept window opening takes about a second;
/// the exchange itself is milliseconds. Generous for loaded CI hosts.
const WALLCLOCK: Duration = Duration::from_secs(20);

#[test]
fn http_get_over_virtio_gets_the_demo_reply() {
    let (uart, response) = boot_kernel_http_get(WALLCLOCK);
    assert!(
        uart.contains(ACCEPT_WINDOW_OPEN),
        "the HTTP demo never opened its accept window:\n{uart}"
    );
    assert!(
        response.starts_with("HTTP/1.0 200 OK"),
        "no HTTP reply over VirtIO (got {:?}); UART:\n{uart}",
        response
    );
    assert!(
        response.ends_with("Hello from Wari\n"),
        "reply truncated or corrupted: {response:?}"
    );
    assert!(
        uart.contains(HTTP_SERVED),
        "the tenant did not report serving the request:\n{uart}"
    );
}
