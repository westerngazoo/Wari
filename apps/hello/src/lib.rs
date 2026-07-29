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

    // ── Agentic PoC brick 1 — policy-mediated Planner → Executor ──
    //
    // This evolves the brick-3b PING/PONG into the first agentic
    // exchange (docs/ai-os-assistant-design.md). Two isolated Tier-1
    // instances share ONE Endpoint (SLOT_IPC), role-split by
    // proc_self():
    //   proc 2 = PLANNER (untrusted, "LLM"): emits action REQUESTS as
    //            data. It ASKS; it holds no dangerous capability.
    //   proc 3 = EXECUTOR (deterministic, non-LLM): recv each request,
    //            run the `wari_policy` gauntlet, and sanction or refuse.
    //
    // The whole security thesis in one demo: a request the Planner
    // "wants" is only performed if the deterministic policy allows it —
    // a prompt-injected Planner asking for an un-sanctioned action is
    // blocked by the Executor, not by trusting the model.
    //
    // Wire format of the 40-byte IPC message (badge + 4 words):
    //   [8..12] action_id (u32 LE)     [12] consequence (0/1/2)
    //   [13] in_allow_list  [14] cap_covers  [15] tainted
    //   reply: [16] decision (0=Allow 1=Deny 2=Confirm)  [17] reason
    const SLOT_IPC: u32 = 3;
    // SAFETY: extern host fn; returns this instance's proc_id, no args.
    let me = unsafe { wasi::proc_self() };

    // The two requests the Planner issues, back to back:
    //   #1 a benign, allow-listed, capable, untainted read → ALLOW
    //   #2 an irreversible egress the Planner is NOT allow-listed for,
    //      with tainted params → the "prompt-injected exfiltration"
    //      the policy must refuse (default-deny on the allow-list).
    // (action_id, consequence, in_allow_list, cap_covers, tainted)
    const REQUESTS: [(u32, u8, u8, u8, u8); 2] = [
        (0x0000_0010, 0, 1, 1, 0), // "read_doc"  → Allow
        (0x0000_00E6, 2, 0, 0, 1), // "egress"    → Deny(NotInAllowList)
    ];

    let mut ipc_msg = [0u8; 40];
    if me == 2 {
        // PLANNER: ask for each action, report the Executor's verdict.
        for (action_id, cons, allow, covers, tainted) in REQUESTS {
            ipc_msg = [0u8; 40];
            ipc_msg[8..12].copy_from_slice(&action_id.to_le_bytes());
            ipc_msg[12] = cons;
            ipc_msg[13] = allow;
            ipc_msg[14] = covers;
            ipc_msg[15] = tainted;
            // SAFETY: extern host fn; kernel cap-checks the Endpoint at
            // SLOT_IPC and validates msg_ptr against our linear memory.
            let rc = unsafe { wasi::ipc_call(SLOT_IPC, ipc_msg.as_ptr() as u32) };
            if rc != 0 {
                print(b"  planner: ipc_call err\r\n");
                break;
            }
            // Reply overwrote the buffer: [16]=decision, [17]=reason.
            let verdict: &[u8] = match ipc_msg[16] {
                0 => b"ALLOW  (executor performed it)",
                1 => b"DENY   (blocked by policy)    ",
                _ => b"CONFIRM(needs human authority)",
            };
            let mut line = *b"  planner: action 0x00000000 -> ??????????????????????????????\r\n";
            hex8(action_id, &mut line[20..28]);
            line[32..32 + verdict.len()].copy_from_slice(verdict);
            print(&line);
        }
    } else {
        // EXECUTOR: mediate each request through the wari_policy
        // gauntlet. This is the trusted, deterministic core — the same
        // pure crate the kernel-side policy uses. A generous budget so
        // the demo turns on allow-list / consequence, not rate.
        let budget = wari_policy::Budget {
            actions_used: 0,
            actions_max: 64,
        };
        for _ in 0..REQUESTS.len() {
            // SAFETY: extern host fn; cap-checked Endpoint + validated ptr.
            let rc = unsafe { wasi::ipc_recv(SLOT_IPC, ipc_msg.as_ptr() as u32) };
            if rc != 0 {
                print(b"  executor: ipc_recv err\r\n");
                break;
            }
            let action_id = u32::from_le_bytes([
                ipc_msg[8], ipc_msg[9], ipc_msg[10], ipc_msg[11],
            ]);
            let req = wari_policy::Request {
                action_id,
                consequence: match ipc_msg[12] {
                    0 => wari_policy::Consequence::Benign,
                    1 => wari_policy::Consequence::Consequential,
                    _ => wari_policy::Consequence::Irreversible,
                },
                in_allow_list: ipc_msg[13] != 0,
                cap_covers_target: ipc_msg[14] != 0,
                tainted_params: ipc_msg[15] != 0,
            };
            let (dcode, rcode): (u8, u8) = match wari_policy::evaluate(&req, &budget) {
                wari_policy::Decision::Allow => (0, 0),
                wari_policy::Decision::Deny(r) => (1, r as u8),
                wari_policy::Decision::Confirm(r) => (2, r as u8),
            };
            let tag: &[u8] = match dcode {
                0 => b"ALLOW -> performing",
                1 => b"DENY  -> refused   ",
                _ => b"CONFIRM-> escalated",
            };
            let mut line = *b"  executor: 0x00000000 policy=???????????????????\r\n";
            hex8(action_id, &mut line[14..22]);
            line[30..30 + tag.len()].copy_from_slice(tag);
            print(&line);
            // Reply the verdict so the Planner learns it (in-place).
            ipc_msg[16] = dcode;
            ipc_msg[17] = rcode;
            // SAFETY: extern host fn; cap-checked Endpoint + validated ptr.
            let _ = unsafe { wasi::ipc_reply(SLOT_IPC, ipc_msg.as_ptr() as u32) };
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

    /// Write `v` as 8 lowercase hex digits into `out[0..8]`.
    /// Used by the agentic demo to print action ids readably.
    fn hex8(v: u32, out: &mut [u8]) {
        const D: &[u8; 16] = b"0123456789abcdef";
        for i in 0..8 {
            out[i] = D[((v >> (28 - i * 4)) & 0xF) as usize];
        }
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
