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
