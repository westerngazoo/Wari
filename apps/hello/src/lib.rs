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
        pub fn fd_write(fd: u32, iovs: *const Iovec, iovs_len: u32, nwritten: *mut u32) -> u32;
        pub fn proc_exit(code: u32) -> !;
    }

    /// Wari socket API (PR Net-6b / Net-6c / Phase-1c HTTP demo).
    /// Tier-1 calls these to allocate and tear down sockets, bind /
    /// listen, and serve a canned HTTP reply via the Tier-2 net
    /// driver.
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
        /// Bind the Socket cap at `slot` to a local port (PR Net-6c).
        pub fn net_socket_bind(slot: u32, ip_be: u32, port: u32) -> i32;
        /// Mark the Socket cap at `slot` as listening (PR Net-6c).
        pub fn net_socket_listen(slot: u32, backlog: u32) -> i32;
        /// Phase-1c HTTP demo — returns 1 if the listening socket
        /// has accepted a connection, 0 if still waiting, negative
        /// errno on driver error. Each call drives one smoltcp
        /// poll cycle inside the kernel.
        pub fn net_socket_accept(slot: u32) -> i32;
        /// Phase-1c HTTP demo — queue the hardcoded HTTP/1.0 200 OK
        /// reply on a connected socket. Returns bytes queued or
        /// negative errno. Kernel drives one smoltcp poll after the
        /// queue so the segment leaves the device on the same hop.
        pub fn net_socket_send_canned(slot: u32) -> i32;

        // ── Synchronous IPC (Option B brick 3b) ──────────────────
        // `slot` = Endpoint cap slot; `msg_ptr` = linmem offset of a
        // 40-byte message buffer (badge u64 | 4×u64 words, LE —
        // wari_abi::net::IPC_MSG_BYTES). Return 0 or negative errno.
        // recv/call suspend this instance until a peer rendezvouses;
        // call's reply overwrites the same buffer (seL4 MR in/out).
        /// Send + await reply (suspends until replied).
        pub fn ipc_call(slot: u32, msg_ptr: u32) -> i32;
        /// Receive into `msg_ptr` (suspends if no sender waiting).
        pub fn ipc_recv(slot: u32, msg_ptr: u32) -> i32;
        /// Reply to the caller awaiting on this endpoint.
        pub fn ipc_reply(slot: u32, msg_ptr: u32) -> i32;
        /// This instance's proc_id (role-splitting for the demo).
        pub fn proc_self() -> i32;
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

    // ── Option B brick 3b — cross-tenant synchronous IPC demo ────
    //
    // Both hello instances hold a READ+WRITE cap to ONE shared
    // Endpoint at SLOT_IPC (cap::boot). Role split by proc_self():
    //   proc 2 (instance A): ipc_call with "PING" in word 0, then
    //     print the reply that overwrote the same buffer.
    //   proc 3 (instance B): ipc_recv (rendezvouses with A's queued
    //     call), print what arrived, ipc_reply "PONG".
    // Whichever instance runs first blocks (suspends via the kernel
    // yield protocol) until the other rendezvouses — this exchange
    // is the first cross-tenant synchronous IPC on Wari.
    const SLOT_IPC: u32 = 3;
    let me = unsafe { wasi::proc_self() };
    let mut ipc_msg = [0u8; 40];
    if me == 2 {
        ipc_msg[8..12].copy_from_slice(b"PING");
        let rc = unsafe { wasi::ipc_call(SLOT_IPC, ipc_msg.as_ptr() as u32) };
        if rc == 0 {
            let mut line = *b"  ipc: reply=????\r\n";
            line[13..17].copy_from_slice(&ipc_msg[8..12]);
            print(&line);
        } else {
            print(b"  ipc: call err\r\n");
        }
    } else {
        let rc = unsafe { wasi::ipc_recv(SLOT_IPC, ipc_msg.as_ptr() as u32) };
        if rc == 0 {
            let mut line = *b"  ipc: got=???? -> replying PONG\r\n";
            line[11..15].copy_from_slice(&ipc_msg[8..12]);
            print(&line);
            ipc_msg[8..12].copy_from_slice(b"PONG");
            let _ = unsafe { wasi::ipc_reply(SLOT_IPC, ipc_msg.as_ptr() as u32) };
        } else {
            print(b"  ipc: recv err\r\n");
        }
    }

    // Phase-1c HTTP demo — full end-to-end:
    //   create → bind 7000 → listen → busy-poll accept → send_canned → close
    //
    // CSpace slot 8 holds the Socket cap (well above slots 0/1/2
    // used for stdout/exit/Net). If any step fails we print the
    // step name + rc and exit cleanly; the demo never panics. The
    // accept loop is bounded so a second tenant (Tier-1 instance
    // B, which also runs hello) doesn't busy-spin forever when no
    // client connects on its turn.
    // accept is a busy-poll: each iteration host-calls into the
    // kernel which drives one smoltcp poll cycle then inspects the
    // socket state. The interpreter loops fast (~µs/iter on QEMU);
    // a generous cap gives a human-paced `curl` a few seconds to
    // hit the port. Real apps will get a blocking wait once the
    // IPC story can suspend a Tier-1 cleanly (Phase-1c follow-up).
    const ACCEPT_MAX_ITERS: u32 = 50_000_000;

    fn print(s: &[u8]) {
        let mut nw: u32 = 0;
        let iov = wasi::Iovec {
            buf: s.as_ptr(),
            buf_len: s.len() as u32,
        };
        // SAFETY: host fn does its own validation.
        let _ = unsafe { wasi::fd_write(1, &iov, 1, &mut nw) };
    }

    let create_rc = unsafe { wasi::net_socket_create(1, 8) };
    if create_rc != 0 {
        print(b"  http: create err\r\n");
        unsafe { wasi::proc_exit(0) }
    }
    let bind_rc = unsafe { wasi::net_socket_bind(8, 0, 7000) };
    if bind_rc != 0 {
        print(b"  http: bind err (port busy?)\r\n");
        let _ = unsafe { wasi::net_socket_close(8) };
        unsafe { wasi::proc_exit(0) }
    }
    let listen_rc = unsafe { wasi::net_socket_listen(8, 1) };
    if listen_rc != 0 {
        print(b"  http: listen err\r\n");
        let _ = unsafe { wasi::net_socket_close(8) };
        unsafe { wasi::proc_exit(0) }
    }
    print(b"  http: listening on :7000\r\n");

    // Kernel-side wall-clock deadline (Phase-1c Ctrl-R fix B):
    // `net_socket_accept` returns E_TIMEDOUT (-7) once the 60 s
    // accept window expires, so the busy-poll below is bounded by
    // time even if ACCEPT_MAX_ITERS would take hours to count down
    // on silicon. Matches `kernel/src/cap/syscall.rs::E_TIMEDOUT`.
    const E_TIMEDOUT: i32 = -7;

    let mut i: u32 = 0;
    let mut accepted = false;
    while i < ACCEPT_MAX_ITERS {
        let rc = unsafe { wasi::net_socket_accept(8) };
        if rc == 1 {
            accepted = true;
            break;
        }
        if rc == E_TIMEDOUT {
            // Expected when no client connects inside the window;
            // the shared "accept timeout" epilogue below reports it.
            break;
        }
        if rc < 0 {
            print(b"  http: accept err\r\n");
            break;
        }
        i = i.wrapping_add(1);
    }

    if accepted {
        let sent = unsafe { wasi::net_socket_send_canned(8) };
        if sent > 0 {
            print(b"  http: served 200 OK\r\n");
        } else {
            print(b"  http: send err\r\n");
        }
    } else {
        print(b"  http: accept timeout, no client\r\n");
    }

    let _ = unsafe { wasi::net_socket_close(8) };

    // SAFETY: extern fn into a host import; `-> !` on the WASM side, the
    // host fn returns `wasmi::Error::i32_exit` which traps the instance
    // and never returns to this frame.
    unsafe { wasi::proc_exit(0) }
}
