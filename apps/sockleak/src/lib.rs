// SPDX-License-Identifier: AGPL-3.0-only
//! Adversarial Tier-1 module: tries every way a tenant can make the
//! net driver lose track of a socket.
//!
//! The driver has a handful of sockets for the whole board, and a
//! socket is reached through a Socket cap. The kernel must close the
//! driver socket exactly when its last cap goes away (INV-26), however
//! that happens. This module walks each way in turn and reports, on
//! the console, whether the sockets came back:
//!
//! 1. **`cap_delete`** every Socket cap, then ask for sockets again.
//! 2. **`cap_copy`** a Socket cap and close the original: the copy
//!    must keep the socket, so one fewer is available. If the full
//!    count is available the driver handed the same socket out twice
//!    and the copy now names someone else's.
//! 3. End of life, by proc id:
//!    - the first instance **traps** holding every free socket;
//!    - the second **`cap_revoke`s its Net cap** (which takes the
//!      Socket caps derived from it), then traps.
//!
//! The kernel bakes two instances and runs them in order at every boot
//! (same discipline as `apps/hog`: a trust-boundary feature ships with
//! its attack). The second finds a socket only if the kernel cleaned
//! up after the first. `tests/security/tests/socket_release_on_fault.rs`
//! and `socket_cap_lifetime.rs` read the lines below.
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
        /// Drop the Socket cap at `slot`; closes the socket if it was
        /// the last cap naming it.
        pub fn net_socket_close(slot: u32) -> i32;
        /// Same-rights duplicate of the cap at `src` into `target`.
        pub fn cap_copy(src: u32, target: u32) -> i32;
        /// Remove the cap at `slot`, no cascade.
        pub fn cap_delete(slot: u32) -> i32;
        /// Remove the cap at `slot` and every cap derived from it.
        pub fn cap_revoke(slot: u32) -> i32;
        /// The caller's proc id.
        pub fn proc_self() -> i32;
    }

    /// WASI Preview 1 `iovec` — `(buf, buf_len)`.
    #[repr(C)]
    pub struct Iovec {
        pub buf: *const u8,
        pub buf_len: u32,
    }
}

/// First CSpace slot the probe mints Socket caps into. Above the
/// kernel-installed slots (stdout, exit, Net, IPC), same choice as
/// `apps/hello`.
const FIRST_SOCKET_SLOT: u32 = 8;

/// Most sockets the probe asks for at once. Above the driver's whole
/// supply (`SOCKET_BACKING_LEN` = 4 in `drivers/net`), so a grab
/// always ends on a refusal rather than on this bound.
const MAX_GRAB: u32 = 8;

/// Slot for the duplicate in the `cap_copy` case. Outside
/// `FIRST_SOCKET_SLOT..FIRST_SOCKET_SLOT + MAX_GRAB`.
const COPY_SLOT: u32 = 32;

/// Slot of the kernel-installed Net cap (`cap::boot::SLOT_NET`).
const SLOT_NET: u32 = 2;

/// Proc id of the instance that ends by revoking its Net cap
/// (`cap::boot::PROC_ID_TIER1_SOCKLEAK_B`). Any other instance ends
/// by trapping with its sockets open.
const REVOKE_ROLE_PROC: i32 = 15;

/// TCP, as `net_socket_create` numbers protocols.
const PROTO_TCP: u32 = 1;

// Console lines. The security tests match on these; keep them in
// step with `tests/security/src/lib.rs::markers`.
const NONE_LEFT: &[u8] = b"[sockleak] no socket available\r\n";
const DELETE_OK: &[u8] = b"[sockleak] cap_delete: sockets came back\r\n";
const DELETE_LEAK: &[u8] = b"[sockleak] cap_delete: LEAK, sockets not returned\r\n";
const COPY_OK: &[u8] = b"[sockleak] cap_copy: the copy keeps its socket\r\n";
const COPY_ALIAS: &[u8] = b"[sockleak] cap_copy: ALIAS, a held socket was handed out again\r\n";
const HOLDING: &[u8] = b"[sockleak] holding every free socket, dying without closing\r\n";
const REVOKING: &[u8] = b"[sockleak] holding every free socket, revoking the Net cap\r\n";

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

/// Create sockets into consecutive slots from `FIRST_SOCKET_SLOT`
/// until the kernel refuses. Returns how many were created.
fn grab_all() -> u32 {
    let mut held: u32 = 0;
    while held < MAX_GRAB {
        // SAFETY: extern host fn; validates its own arguments.
        let rc = unsafe { wasi::net_socket_create(PROTO_TCP, FIRST_SOCKET_SLOT + held) };
        if rc != 0 {
            break;
        }
        held += 1;
    }
    held
}

/// Properly close the `count` sockets a [`grab_all`] returned.
fn close_grabbed(count: u32) {
    for i in 0..count {
        // SAFETY: extern host fn; validates its own arguments.
        let _ = unsafe { wasi::net_socket_close(FIRST_SOCKET_SLOT + i) };
    }
}

/// WASI module entry point. Never returns normally.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    // How many sockets the driver can give this tenant right now.
    let total = grab_all();
    if total == 0 {
        print(NONE_LEFT);
        // SAFETY: extern host fn; terminates this instance.
        unsafe { wasi::proc_exit(1) };
    }

    // 1. Drop every cap with cap_delete. All `total` must come back.
    for i in 0..total {
        // SAFETY: extern host fn; validates its own arguments.
        let _ = unsafe { wasi::cap_delete(FIRST_SOCKET_SLOT + i) };
    }
    let back = grab_all();
    print(if back == total {
        DELETE_OK
    } else {
        DELETE_LEAK
    });
    close_grabbed(back);

    // 2. One socket, two caps; close the original. The copy pins the
    //    socket, so exactly `total - 1` others must be available.
    // SAFETY (whole block): extern host fns; each validates its own
    // arguments.
    if unsafe { wasi::net_socket_create(PROTO_TCP, FIRST_SOCKET_SLOT) } == 0 {
        let _ = unsafe { wasi::cap_copy(FIRST_SOCKET_SLOT, COPY_SLOT) };
        let _ = unsafe { wasi::net_socket_close(FIRST_SOCKET_SLOT) };
        let others = grab_all();
        print(if others + 1 == total {
            COPY_OK
        } else {
            COPY_ALIAS
        });
        close_grabbed(others);
        let _ = unsafe { wasi::net_socket_close(COPY_SLOT) };
    }

    // 3. End of life with every free socket still open.
    if grab_all() == 0 {
        print(NONE_LEFT);
        // SAFETY: extern host fn; terminates this instance.
        unsafe { wasi::proc_exit(1) };
    }
    // SAFETY: extern host fns; no arguments to validate / validated
    // by the kernel.
    if unsafe { wasi::proc_self() } == REVOKE_ROLE_PROC {
        print(REVOKING);
        let _ = unsafe { wasi::cap_revoke(SLOT_NET) };
    } else {
        print(HOLDING);
    }
    // Die the way a buggy tenant does: a wasm trap.
    core::arch::wasm32::unreachable()
}

// ── Module manifest ──────────────────────────────────────────────
//
// Complete import/export surface, cross-checked by
// `scripts/sign-module` before signing (INV-11's gate; INV-24's
// one-source discipline).
#[link_section = "wari_driver_manifest"]
#[used]
static WARI_MANIFEST: [u8; wari_driver_iface::manifest_size(1, 8)] = {
    use wari_driver_iface::FuncSig::{U32Unit, U32x4I32, U32xU32I32, UnitI32, UnitUnit, U32I32};
    wari_driver_iface::build_manifest::<{ wari_driver_iface::manifest_size(1, 8) }>(
        wari_driver_iface::DriverKind::Tier1App,
        &[(b"_start", UnitUnit)],
        &[
            (b"wasi_snapshot_preview1", b"fd_write", U32x4I32),
            (b"wasi_snapshot_preview1", b"proc_exit", U32Unit),
            (b"wari", b"net_socket_create", U32xU32I32),
            (b"wari", b"net_socket_close", U32I32),
            (b"wari", b"cap_copy", U32xU32I32),
            (b"wari", b"cap_delete", U32I32),
            (b"wari", b"cap_revoke", U32I32),
            (b"wari", b"proc_self", UnitI32),
        ],
    )
};
