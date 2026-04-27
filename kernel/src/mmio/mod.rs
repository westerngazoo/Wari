// SPDX-License-Identifier: AGPL-3.0-only
//! Typed volatile MMIO wrappers — CLAUDE R3.
//!
//! Raw `ptr::read_volatile`/`write_volatile` is **banned outside this
//! module**. Drivers use `VolatilePtr<T>` or device-specific typed
//! register definitions.
//!
//! Why: compiler optimization + MMIO = silent correctness bugs. A
//! typed wrapper makes the intent explicit at every call site, and
//! a future lint or audit can verify R3 mechanically by checking
//! that no other file imports `core::ptr::{read,write}_volatile`.
//!
//! Submodules:
//!   - `volatile`     — `VolatilePtr<T>` generic wrapper
//!   - `uart_ns16550` — kernel-private NS16550 putc (early printk /
//!     panic path). NOT the customer UART driver; the Tier-2 WASM
//!     driver lands in PR 5.

pub mod plic;
pub mod uart_ns16550;
pub mod volatile;
