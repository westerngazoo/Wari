// SPDX-License-Identifier: AGPL-3.0-only
//! Adversarial Tier-1 module: announces itself, then spins forever
//! without ever calling a host fn.
//!
//! This is the attack `tests/security/tests/fuel_exhaustion.rs`
//! needs: a tenant that never yields. Without fuel metering it would
//! hold the hart until reset and no other tenant, no network poll and
//! no Ctrl-R handler would run again. The module registry spawns it
//! at every boot (`modreg::self_test_spawn`, same discipline as the
//! tamper check and the Tier-2 net driver's INV-25 self-check: a
//! trust-boundary feature ships with its attack), and the scheduler
//! must report it faulted on fuel.
//!
//! The loop body touches a `black_box`ed counter so the optimizer
//! cannot reduce it to something wasmi would not meter; every
//! iteration is a loop-header `ConsumeFuel` in wasmi's bytecode.

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

    /// WASI Preview 1 `iovec` — `(buf, buf_len)`.
    #[repr(C)]
    pub struct Iovec {
        pub buf: *const u8,
        pub buf_len: u32,
    }
}

/// Printed once before the spin so the UART shows the probe ran and
/// which proc id it got (the `[t1:<n>] out of fuel` line follows).
const BANNER: &[u8] = b"[hog] spinning without yielding\r\n";

/// Pin `proc_exit` into the import section. Only the panic handler
/// references it and `panic = "abort"` lets LTO drop that path, which
/// would leave the manifest declaring an import the binary lacks
/// (`sign-module` refuses the mismatch). Same device as the net
/// driver's `keep_imports` pins.
#[used]
static KEEP_PROC_EXIT: unsafe extern "C" fn(u32) -> ! = wasi::proc_exit;

/// WASI module entry point. Never returns and never yields.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    let iov = wasi::Iovec {
        buf: BANNER.as_ptr(),
        buf_len: BANNER.len() as u32,
    };
    let mut nwritten: u32 = 0;
    // SAFETY: extern host fn; validates its own arguments and returns
    // an errno at worst, which this probe ignores.
    let _ = unsafe { wasi::fd_write(1, &iov, 1, &mut nwritten) };

    let mut n: u32 = 0;
    loop {
        n = core::hint::black_box(n.wrapping_add(1));
    }
}

// ── Module manifest (Phase 2 dynamic loading) ────────────────────
//
// Complete import/export surface, cross-checked by
// `scripts/sign-module` before signing (INV-11's gate; INV-24's
// one-source discipline).
#[link_section = "wari_driver_manifest"]
#[used]
static WARI_MANIFEST: [u8; wari_driver_iface::manifest_size(1, 2)] = {
    use wari_driver_iface::FuncSig::{U32Unit, U32x4I32, UnitUnit};
    wari_driver_iface::build_manifest::<{ wari_driver_iface::manifest_size(1, 2) }>(
        wari_driver_iface::DriverKind::Tier1App,
        &[(b"_start", UnitUnit)],
        &[
            (b"wasi_snapshot_preview1", b"fd_write", U32x4I32),
            (b"wasi_snapshot_preview1", b"proc_exit", U32Unit),
        ],
    )
};
