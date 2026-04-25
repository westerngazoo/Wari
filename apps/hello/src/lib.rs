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

/// The exact bytes Phase 0 demands on the UART (`docs/testing.md`
/// integration `hello_wasm.rs`). The trailing `\n` matches the marker
/// the integration test grep'es for.
const HELLO: &[u8] = b"Hello from Wari\n";

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

    // SAFETY: extern fn into a host import; `-> !` on the WASM side, the
    // host fn returns `wasmi::Error::i32_exit` which traps the instance
    // and never returns to this frame.
    unsafe { wasi::proc_exit(0) }
}
