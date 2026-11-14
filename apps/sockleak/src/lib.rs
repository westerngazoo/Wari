// SPDX-License-Identifier: AGPL-3.0-only
//! Adversarial Tier-1 module: takes every socket the net driver has
//! left, then dies without closing any of them.
//!
//! This is the attack `tests/security/tests/socket_release_on_fault.rs`
//! needs. The driver has a handful of sockets for the whole board; a
//! tenant that faults while holding some must not keep them, or a few
//! crashes leave the board unable to open a connection until reset.
//! The kernel bakes two instances of this module and runs them one
//! after the other at every boot (same discipline as `apps/hog`: a
//! trust-boundary feature ships with its attack). The first empties
//! the driver and traps. The second can only get a socket if the
//! kernel released the first one's when it died.
//!
//! Skeleton shared with `apps/hog` (panic handler, WASI imports,
//! manifest): duplicated on purpose, each probe is a self-contained
//! cdylib with no code in common worth a crate.

#![no_std]
#![no_main]

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // SAFETY: extern host fn; terminates this instance non-zero.
    unsafe { wasi::proc_exit(1) };
}

mod wasi {
    // WASI Preview 1 imports (stdout + exit), bound by the kernel
    // under `wasi_snapshot_preview1`.
    #[link(wasm_import_module = "wasi_snapshot_preview1")]
    extern "C" {
        pub fn fd_write(fd: u32, iovs: *const Iovec, iovs_len: u32, nwritten: *mut u32) -> u32;
        pub fn proc_exit(code: u32) -> !;
    }

    #[link(wasm_import_module = "wari")]
    extern "C" {
        /// Allocate a socket of `proto` (1 = TCP) and mint its cap
        /// into the caller's CSpace at `slot_for_cap`. 0 on success,
        /// negative errno otherwise.
        pub fn net_socket_create(proto: u32, slot_for_cap: u32) -> i32;
    }

    /// WASI Preview 1 `iovec` — `(buf, buf_len)`.
    #[repr(C)]
    pub struct Iovec {
        pub buf: *const u8,
        pub buf_len: u32,
    }
}

/// First CSpace slot the probe mints a Socket cap into. Above the
/// kernel-installed slots (stdout, exit, Net, IPC), same choice as
/// `apps/hello`.
const FIRST_SOCKET_SLOT: u32 = 8;

/// Most sockets the probe asks for. Above the driver's whole supply
/// (`SOCKET_BACKING_LEN` = 4 in `drivers/net`), so the loop always
/// ends on a refusal rather than on this bound.
const MAX_GRAB: u32 = 8;

/// TCP, as `net_socket_create` numbers protocols.
const PROTO_TCP: u32 = 1;

/// Printed when the probe holds at least one socket, right before it
/// traps. The kernel's fault line for this proc id follows.
const HOLDING: &[u8] = b"[sockleak] holding every free socket, dying without closing\r\n";

/// Printed when the driver had nothing left: an earlier dead tenant
/// still owns the sockets. The security test fails on this line.
const NONE_LEFT: &[u8] = b"[sockleak] no socket available\r\n";

fn print(s: &[u8]) {
    let iov = wasi::Iovec {
        buf: s.as_ptr(),
        buf_len: s.len() as u32,
    };
    let mut nwritten: u32 = 0;
    // SAFETY: extern host fn; validates its own arguments and returns
    // an errno at worst, which this probe ignores.
    let _ = unsafe { wasi::fd_write(1, &iov, 1, &mut nwritten) };
}

/// WASI module entry point. Never returns normally.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    let mut held: u32 = 0;
    while held < MAX_GRAB {
        // SAFETY: extern host fn; validates its own arguments.
        let rc = unsafe { wasi::net_socket_create(PROTO_TCP, FIRST_SOCKET_SLOT + held) };
        if rc != 0 {
            break;
        }
        held += 1;
    }

    if held == 0 {
        print(NONE_LEFT);
        // SAFETY: extern host fn; terminates this instance.
        unsafe { wasi::proc_exit(1) };
    }

    print(HOLDING);
    // Die the way a buggy tenant does: a wasm trap, with the sockets
    // still open.
    core::arch::wasm32::unreachable()
}

// ── Module manifest ──────────────────────────────────────────────
//
// Complete import/export surface, cross-checked by
// `scripts/sign-module` before signing (INV-11's gate; INV-24's
// one-source discipline).
#[link_section = "wari_driver_manifest"]
#[used]
static WARI_MANIFEST: [u8; wari_driver_iface::manifest_size(1, 3)] = {
    use wari_driver_iface::FuncSig::{U32Unit, U32x4I32, U32xU32I32, UnitUnit};
    wari_driver_iface::build_manifest::<{ wari_driver_iface::manifest_size(1, 3) }>(
        wari_driver_iface::DriverKind::Tier1App,
        &[(b"_start", UnitUnit)],
        &[
            (b"wasi_snapshot_preview1", b"fd_write", U32x4I32),
            (b"wasi_snapshot_preview1", b"proc_exit", U32Unit),
            (b"wari", b"net_socket_create", U32xU32I32),
        ],
    )
};
