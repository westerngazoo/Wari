// SPDX-License-Identifier: AGPL-3.0-only
//! Tier-1 "hello world" WASM module — Phase 0 exit demo.
//!
//! Calls WASI Preview 1 `fd_write(1, ...)` to print "Hello from Wari\n",
//! then `proc_exit(0)`. The kernel routes `fd_write` through the loaded
//! Tier-2 UART driver (kernel-side wiring lives in
//! `kernel/src/runtime/wasi.rs` + `kernel/src/runtime/tier2_uart.rs`).
//!
//! ## Phase 0 scope
//!
//! - One iovec, one fd (stdout = 1).
//! - No richer WASI surface (no `args_get`, no `clock_time_get`, etc.).
//!
//! TODO(Phase 1): expand WASI surface; add `args_get`/`environ_get` once
//! the manifest registry lands.
//!
//! ## Why a hand-rolled WASI binding
//!
//! Picked: a 4-line `extern "C"` block with `#[link(wasm_import_module
//! = "wasi_snapshot_preview1")]`. Considered: the `wasi` crate (rejected
//! — it pulls a generated bindings tree larger than the entire app), the
//! `wasi-sys` crate (same objection). Why this won: smallest sound
//! primitive; one import block matches one host-fn registration in
//! `kernel/src/runtime/wasi.rs::register_wasi_host_fns`. Cost accepted:
//! we re-declare two function signatures, but they are stable WASI P1
//! shapes and a mismatch fails at WASM link time.

#![no_std]
#![no_main]

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // A Tier-1 panic is best-effort: we ask the kernel to terminate this
    // module with a non-zero exit code. `proc_exit` is `-> !`, so the
    // trailing loop is structural only.
    unsafe { wasi::proc_exit(1) };
}

mod wasi {
    /// WASI Preview 1 host-fn imports. The kernel binds these under the
    /// module name `wasi_snapshot_preview1` via wasmi's `Linker::func_wrap`.
    ///
    /// # Linker contract
    ///
    /// - `fd_write(fd, iovs, iovs_len, nwritten)` returns a WASI errno
    ///   (0 on success). On success, `*nwritten` is the byte count
    ///   actually written.
    /// - `proc_exit(code)` does not return — wasmi traps the instance
    ///   and the kernel observes the exit code.
    #[link(wasm_import_module = "wasi_snapshot_preview1")]
    extern "C" {
        pub fn fd_write(
            fd: u32,
            iovs: *const Iovec,
            iovs_len: u32,
            nwritten: *mut u32,
        ) -> u32;
        pub fn proc_exit(code: u32) -> !;
    }

    /// Wari socket API (PR Net-6b). Tier-1 calls these to allocate
    /// and tear down sockets via the Tier-2 net driver.
    #[link(wasm_import_module = "wari")]
    extern "C" {
        /// Allocate a smoltcp socket of `proto` (1=TCP, 2=UDP) and
        /// mint a Socket cap into the caller's CSpace at
        /// `slot_for_cap`. Returns 0 on success, negative errno
        /// otherwise.
        pub fn net_socket_create(proto: u32, slot_for_cap: u32) -> i32;
        /// Tear down the Socket cap at `slot`. Returns 0 on
        /// success, negative errno otherwise.
        pub fn net_socket_close(slot: u32) -> i32;
    }

    /// WASI Preview 1 `iovec` — `(buf, buf_len)` pair.
    ///
    /// Layout matches the WASI P1 ABI: two `u32`s contiguous in linear
    /// memory. `buf` is a linear-memory offset; `buf_len` is the byte
    /// length of the slice at that offset.
    #[repr(C)]
    pub struct Iovec {
        pub buf: *const u8,
        pub buf_len: u32,
    }
}

/// The exact bytes the UART expects. NS16550 (and the JH7110
/// DesignWare 8250 on VF2) ship raw bytes — no LF→CRLF
/// translation. Without the `\r`, the next print after this one
/// stays at column 15 on the terminal, indenting the kernel's
/// `[t1:N] exit(0)` line. Sending `\r\n` returns the cursor to
/// column 0 like a real terminal expects.
const HELLO: &[u8] = b"Hello from Wari\r\n";

/// WASI module entry point. Wasmi calls this as the `_start` export
/// after instantiation. Never returns.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    let iov = wasi::Iovec {
        buf: HELLO.as_ptr(),
        buf_len: HELLO.len() as u32,
    };
    let mut nwritten: u32 = 0;

    // SAFETY: extern fn into a host import. The host fn does its own
    // capability + argument validation; the worst case is a non-zero
    // errno return (which we ignore — Phase 0's `_start` always exits
    // success).
    let _ = unsafe { wasi::fd_write(1, &iov, 1, &mut nwritten) };

    // PR Net-6b — exercise the Tier-1 socket API. Allocates a TCP
    // socket (proto=1) into CSpace slot 8 (well above the boot-
    // installed slots 0/1/2 for stdout/exit/Net), then closes it.
    // On success prints "  socket ok"; on any errno prints
    // "  socket err N". Either way we proc_exit(0) — Phase-0 demo
    // contract keeps the test green even if net is unavailable.
    let create_rc = unsafe { wasi::net_socket_create(1, 8) };
    if create_rc == 0 {
        let close_rc = unsafe { wasi::net_socket_close(8) };
        if close_rc == 0 {
            let msg: &[u8] = b"  socket ok\r\n";
            let iov2 = wasi::Iovec { buf: msg.as_ptr(), buf_len: msg.len() as u32 };
            let _ = unsafe { wasi::fd_write(1, &iov2, 1, &mut nwritten) };
        } else {
            let msg: &[u8] = b"  socket close err\r\n";
            let iov2 = wasi::Iovec { buf: msg.as_ptr(), buf_len: msg.len() as u32 };
            let _ = unsafe { wasi::fd_write(1, &iov2, 1, &mut nwritten) };
        }
    } else {
        let msg: &[u8] = b"  socket create err\r\n";
        let iov2 = wasi::Iovec { buf: msg.as_ptr(), buf_len: msg.len() as u32 };
        let _ = unsafe { wasi::fd_write(1, &iov2, 1, &mut nwritten) };
    }

    // SAFETY: extern fn into a host import; `-> !` on the WASM side, the
    // host fn returns `wasmi::Error::i32_exit` which traps the instance
    // and never returns to this frame.
    unsafe { wasi::proc_exit(0) }
}
