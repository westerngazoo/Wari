// SPDX-License-Identifier: AGPL-3.0-only
//! Kernel printk — `kputc`, `kputs`, `kprintln!`.
//!
//! Every caller is kernel-only. This is *not* a Tier-1 path; user-mode
//! WASM uses WASI `fd_write` which routes through the Tier-2 UART
//! driver (PR 5) under a capability gate.
//!
//! The write path is a single-hart (INV-1) sequential loop over
//! `mmio::uart_ns16550::putc`, with `core::fmt::Write` plumbing so
//! that `kprintln!` uses the standard formatter.

use core::fmt::{self, Write};

use crate::mmio::uart_ns16550;

/// Write one byte to the kernel console.
pub fn kputc(byte: u8) {
    uart_ns16550::putc(byte);
}

/// Write a `&str` to the kernel console.
pub fn kputs(s: &str) {
    for b in s.as_bytes() {
        kputc(*b);
    }
}

/// `core::fmt::Write` sink backing `kprintln!`. Zero-size.
pub struct KConsole;

impl Write for KConsole {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        kputs(s);
        Ok(())
    }
}

/// Format + write to the kernel console, followed by `\r\n`.
///
/// `\r\n` matches what QEMU's `-nographic` console expects for clean
/// line breaks on a serial-style terminal.
#[macro_export]
macro_rules! kprintln {
    () => { $crate::kputc::kputs("\r\n") };
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        // Errors from KConsole are impossible (write_str is infallible).
        let _ = core::write!($crate::kputc::KConsole, $($arg)*);
        $crate::kputc::kputs("\r\n");
    }};
}

/// Subsystem-tagged debug print. Compiled out unless the
/// `debug-kernel` feature is on. Use for diagnostic information
/// that the operator wants in a debug-mode boot trace but not in
/// production. The `subsys` token is stringified into the line
/// so the trace can be filtered by subsystem at the receive side.
///
/// ```ignore
/// kdebug!(net, "socket_create proto={} slot={}", proto, slot);
/// ```
///
/// becomes (debug-kernel on):
/// ```text
///   [debug:net] socket_create proto=1 slot=8
/// ```
///
/// and a no-op (zero bytes, zero cost) when debug-kernel is off.
#[macro_export]
macro_rules! kdebug {
    ($subsys:ident, $($arg:tt)*) => {{
        #[cfg(feature = "debug-kernel")]
        {
            $crate::kprintln!(
                "  [debug:{}] {}",
                ::core::stringify!($subsys),
                ::core::format_args!($($arg)*)
            );
        }
    }};
}

/// Subsystem-tagged trace print. Compiled out unless the
/// `trace-kernel` feature is on. Trace is **noisier** than
/// debug — for hot-path observability (every host fn dispatch,
/// every cap lookup, every scheduler decision). Splitting trace
/// from debug means a debug-build can capture the high-level
/// flow without drowning the operator in per-instruction noise.
#[macro_export]
macro_rules! ktrace {
    ($subsys:ident, $($arg:tt)*) => {{
        #[cfg(feature = "trace-kernel")]
        {
            $crate::kprintln!(
                "  [trace:{}] {}",
                ::core::stringify!($subsys),
                ::core::format_args!($($arg)*)
            );
        }
    }};
}
