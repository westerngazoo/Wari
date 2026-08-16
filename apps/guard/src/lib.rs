// SPDX-License-Identifier: AGPL-3.0-only
//! Wari's first guard agent — a resident Tier-1 daemon that watches
//! the kernel audit event stream.
//!
//! This is the payoff of the capability model made concrete: a
//! security agent that is *just a workload*. It holds exactly one
//! authority beyond stdout+exit — an `EventLog` READ cap the
//! Supervisor granted — and with it it observes every module stage,
//! every spawn, every rejection, and every tenant exit, printing a
//! line per record. It cannot alter the stream (there is no
//! `event_write`), cannot touch other tenants, cannot do MMIO. A
//! compromised guard leaks a bounded cap set, not the machine.
//!
//! ## Loop shape
//!
//! `event_read` from a cursor, printing each record and advancing.
//! When caught up, `event_read` BLOCKS — the kernel suspends the guard
//! at zero CPU until the next audit event wakes it — so the daemon
//! loops forever without spinning and never exits under normal
//! operation. Surviving a crash (restart-on-exit) is the module
//! registry's job, a later brick; this proves the blocking read path
//! and the capability gate end to end.
//!
//! ## Records decode with `wari-events`
//!
//! The 16-byte buffer `event_read` fills is a `wari_events::Event`.
//! We decode field-by-field from the little-endian bytes and name
//! kinds via `EventKind::from_raw`, so a kernel that adds a new kind
//! prints `kind=<n>` here rather than misreporting — forward-
//! compatible by construction (INV-24: one layout source).

#![no_std]
#![no_main]

use wari_events::{Event, EventKind};

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // SAFETY: extern host fn; terminates this instance non-zero.
    unsafe { wasi::proc_exit(1) };
}

mod wasi {
    /// WASI Preview 1 imports (stdout + exit), bound by the kernel
    /// under `wasi_snapshot_preview1`.
    #[link(wasm_import_module = "wasi_snapshot_preview1")]
    extern "C" {
        pub fn fd_write(fd: u32, iovs: *const Iovec, iovs_len: u32, nwritten: *mut u32) -> u32;
        pub fn proc_exit(code: u32) -> !;
    }

    /// Wari guard-agent imports.
    #[link(wasm_import_module = "wari")]
    extern "C" {
        /// Read the audit record at `cursor` (a u64 split lo/hi) into
        /// the 16-byte buffer at `out_ptr`. Returns:
        /// `0` record written (advance to seq+1); `1` up-to-date;
        /// `2` lagged (oldest surviving record written; gap =
        /// seq − cursor); negative = errno (`-1` no EventLog cap).
        pub fn event_read(cursor_lo: u32, cursor_hi: u32, out_ptr: u32) -> i32;
    }

    /// WASI Preview 1 `iovec` — `(buf, buf_len)`.
    #[repr(C)]
    pub struct Iovec {
        pub buf: *const u8,
        pub buf_len: u32,
    }
}

fn print(s: &[u8]) {
    let iov = wasi::Iovec {
        buf: s.as_ptr(),
        buf_len: s.len() as u32,
    };
    let mut nw: u32 = 0;
    // SAFETY: host fn does its own validation; worst case is a
    // non-zero errno we ignore.
    let _ = unsafe { wasi::fd_write(1, &iov, 1, &mut nw) };
}

/// Decode a `wari_events::Event` from the 16-byte little-endian
/// buffer the kernel wrote. Layout is `wari-events`' own, not
/// transcribed here (INV-24).
fn decode(buf: &[u8; 16]) -> Event {
    Event {
        seq: u64::from_le_bytes([
            buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
        ]),
        kind: u16::from_le_bytes([buf[8], buf[9]]),
        a: u16::from_le_bytes([buf[10], buf[11]]),
        b: u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
    }
}

fn kind_name(kind: u16) -> &'static [u8] {
    match EventKind::from_raw(kind) {
        Some(EventKind::ModuleStaged) => b"module-staged",
        Some(EventKind::SpawnVerified) => b"spawn-verified",
        Some(EventKind::SpawnRejected) => b"SPAWN-REJECTED",
        Some(EventKind::TenantExited) => b"tenant-exited",
        Some(EventKind::TenantFaulted) => b"TENANT-FAULTED",
        None => b"unknown-kind",
    }
}

/// Write `v` as decimal into `out[at..]`, returning the new end
/// index. Bounded: the caller ensures room for up to 20 digits.
fn format_u64(out: &mut [u8], at: usize, v: u64) -> usize {
    let mut tmp = [0u8; 20];
    let mut i = tmp.len();
    let mut n = v;
    loop {
        i -= 1;
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    let digits = &tmp[i..];
    out[at..at + digits.len()].copy_from_slice(digits);
    at + digits.len()
}

/// Print one decoded record: `[guard] #<seq> <kind> a=<a> b=<b>`.
fn print_record(e: &Event) {
    let mut line = [0u8; 96];
    let mut n = 0;
    let head = b"[guard] #";
    line[..head.len()].copy_from_slice(head);
    n += head.len();
    n = format_u64(&mut line, n, e.seq);
    line[n] = b' ';
    n += 1;
    let name = kind_name(e.kind);
    line[n..n + name.len()].copy_from_slice(name);
    n += name.len();
    let a_tag = b" a=";
    line[n..n + a_tag.len()].copy_from_slice(a_tag);
    n += a_tag.len();
    n = format_u64(&mut line, n, e.a as u64);
    let b_tag = b" b=";
    line[n..n + b_tag.len()].copy_from_slice(b_tag);
    n += b_tag.len();
    n = format_u64(&mut line, n, e.b as u64);
    line[n] = b'\r';
    line[n + 1] = b'\n';
    n += 2;
    print(&line[..n]);
}

/// Guard entry point. Never returns.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    print(b"[guard] watching the audit stream\r\n");

    let mut cursor: u64 = 0;
    let mut buf = [0u8; 16];
    // A real daemon: `event_read` BLOCKS when the stream is caught up —
    // the kernel suspends us at zero CPU until the next `emit` wakes
    // us. So this loop is unbounded and never spins. When every other
    // tenant has exited, we are the only process left, Blocked on the
    // ring; the scheduler returns to the kernel idle loop and the
    // system rests with Ctrl-R live and the guard asleep. That is the
    // whole point of the brick: a watcher that costs nothing while
    // idle, versus the busy-poll it replaced.
    loop {
        let lo = cursor as u32;
        let hi = (cursor >> 32) as u32;
        // SAFETY: host fn; EventLog-cap-gated; writes 16 bytes into
        // our own linmem at buf, validated by the kernel.
        //
        // The kernel writes `buf` through this pointer, which the Rust
        // abstract machine cannot see across the opaque FFI call — so
        // `decode(&buf)` below must reload from memory, not read a
        // cached zero (the stale-read class that cost us the DMA loop,
        // INV-25). What forces the reload is the address *escaping* to
        // an opaque call; `as_mut_ptr` states the intent honestly (the
        // callee mutates), which is why it is used rather than the
        // `mut`-vs-`const` distinction per se.
        let rc = unsafe { wasi::event_read(lo, hi, buf.as_mut_ptr() as u32) };
        match rc {
            0 | 2 => {
                let e = decode(&buf);
                if rc == 2 {
                    let gap = e.seq.saturating_sub(cursor);
                    let mut line = *b"[guard] LOST ..................... records\r\n";
                    let end = format_u64(&mut line, 13, gap);
                    let tail = b" records\r\n";
                    line[end..end + tail.len()].copy_from_slice(tail);
                    print(&line[..end + tail.len()]);
                }
                print_record(&e);
                cursor = e.seq + 1;
            }
            // 1 = woken after a block (or, only if too many guards are
            // blocked at once, the kernel's degraded up-to-date
            // fallback). Either way: loop and re-read — a fresh record
            // is now waiting for us.
            1 => {}
            _ => {
                print(b"[guard] read err, exiting\r\n");
                break;
            }
        }
    }

    // SAFETY: extern host fn; `-> !`, traps the instance. Reached only
    // on a read error — the healthy daemon never leaves the loop.
    unsafe { wasi::proc_exit(0) }
}

// ── Module manifest (Phase 2 dynamic loading) ────────────────────
//
// Complete import/export surface, cross-checked by
// `scripts/sign-module` before signing (symmetric: an undeclared
// import or export refuses to sign). INV-11's gate; INV-24's
// one-source discipline.
#[link_section = "wari_driver_manifest"]
#[used]
static WARI_MANIFEST: [u8; wari_driver_iface::manifest_size(1, 3)] = {
    use wari_driver_iface::FuncSig::{U32x3I32, U32x4I32, U32Unit, UnitUnit};
    wari_driver_iface::build_manifest::<{ wari_driver_iface::manifest_size(1, 3) }>(
        wari_driver_iface::DriverKind::Tier1App,
        &[(b"_start", UnitUnit)],
        &[
            (b"wasi_snapshot_preview1", b"fd_write", U32x4I32),
            (b"wasi_snapshot_preview1", b"proc_exit", U32Unit),
            (b"wari", b"event_read", U32x3I32),
        ],
    )
};
