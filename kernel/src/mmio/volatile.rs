// SPDX-License-Identifier: AGPL-3.0-only
//! `VolatilePtr<T>` — typed volatile MMIO access.
//!
//! The entire raison d'être of this wrapper is to be the *single*
//! place in the tree that calls `core::ptr::{read,write}_volatile`.
//! Every MMIO access above this file goes through `VolatilePtr`, so
//! R3 can be verified mechanically: grep for the volatile intrinsics
//! outside `kernel/src/mmio/` and expect zero hits.
//!
//! Prior art: the `volatile` crate (BSD-licensed). We do not take the
//! dependency — the useful surface is ~15 lines, and in-tree code is
//! easier to audit against R3.
//!
//! # Invariant
//!
//! Every construction is `unsafe` because the caller is asserting
//! that the pointer targets a valid MMIO register per INV-3 (MMIO
//! address validity). Once constructed, `read`/`write` are safe.
//!
//! # Example
//!
//! ```ignore
//! // SAFETY: INV-3. NS16550 LSR at QEMU virt UART base + 5.
//! let lsr: VolatilePtr<u8> =
//!     unsafe { VolatilePtr::new(0x1000_0005 as *mut u8) };
//! if lsr.read() & 0x20 != 0 { /* THR empty */ }
//! ```
#![allow(missing_docs)]

use core::marker::PhantomData;
use core::ptr;

/// Typed pointer to a single volatile MMIO register.
///
/// `Copy` so call sites can pass it around cheaply; it is logically
/// just a `*mut T` with volatile semantics bolted on.
#[derive(Clone, Copy)]
pub struct VolatilePtr<T: Copy> {
    ptr: *mut T,
    _marker: PhantomData<T>,
}

impl<T: Copy> VolatilePtr<T> {
    /// Construct from a raw pointer.
    ///
    /// # Safety
    ///
    /// Caller asserts `ptr` targets a valid MMIO register (INV-3) or,
    /// in tests, a live backing store. `ptr` must be non-null and
    /// properly aligned for `T` for the lifetime of this wrapper.
    #[inline]
    pub const unsafe fn new(ptr: *mut T) -> Self {
        Self {
            ptr,
            _marker: PhantomData,
        }
    }

    /// Read the register.
    #[inline]
    pub fn read(&self) -> T {
        // SAFETY: INV-3. `self.ptr` was asserted valid at construction.
        unsafe { ptr::read_volatile(self.ptr) }
    }

    /// Write the register.
    #[inline]
    pub fn write(&self, val: T) {
        // SAFETY: INV-3. `self.ptr` was asserted valid at construction.
        unsafe { ptr::write_volatile(self.ptr, val) }
    }
}

// `VolatilePtr<T>` is just a pointer; Send/Sync are caller policy.
// We leave them unimplemented — upstream callers wrap in statics or
// pass by value on a single hart (INV-1) in Phase 0.
//
// Host unit tests for this wrapper are deferred to a future pure-logic
// split: the kernel crate is `no_std` / `no_main` and cannot host a
// `#[cfg(test)]` module without a structural refactor (pure logic
// moved to a sibling lib crate). PR 2+ introduces that split for
// `mem::page_alloc` etc.; the MMIO wrapper can follow when the
// pattern is established. Surgical-changes principle: not scope for
// PR 1.
