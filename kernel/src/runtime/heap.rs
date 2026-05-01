// SPDX-License-Identifier: AGPL-3.0-only
//! Bump-allocator backing the kernel's `#[global_allocator]`.
//!
//! ## Why bump (Simplicity First)
//!
//! Phase 0b has exactly one consumer of the heap: wasmi's instantiation
//! machinery during `runtime::run_noop` at boot. There is no second
//! caller, no fragmentation pressure, no concurrent allocator user
//! (INV-1), and no need to free anything (the arena dies with the
//! kernel image at reset). A bump allocator is the smallest sound
//! implementation that satisfies `core::alloc::GlobalAlloc` for this
//! one shot.
//!
//! ## Why no-free
//!
//! `dealloc` is a no-op. Phase 0 is arena-per-boot — we never reset
//! and re-init. When Phase 1 introduces a real allocator (free-list
//! or buddy) for repeated WASM instance creation, this module retires
//! along with INV-12.
//!
//! ## Why hand-rolled vs `linked_list_allocator`
//!
//! The hand-rolled version is ~80 LOC with one `unsafe impl`, all
//! reviewable in one screen. Pulling `linked_list_allocator` (or
//! `talc`, `buddy_system_allocator`) imports an audited dep we don't
//! need yet — the Tier-0 trust base stays smaller. When Phase 1's
//! requirements force a real allocator, we revisit the dep choice
//! against the then-current needs.
//!
//! ## Invariants
//!
//! - INV-1 (single-hart): `HEAP_CURSOR` is mutated without a lock.
//! - INV-12 (this PR): the arena `[HEAP_CURSOR, HEAP_END)` is set up
//!   once during `kvm::init` and never re-initialized; after init,
//!   only `alloc()` advances the cursor; `HEAP_CURSOR <= HEAP_END`
//!   always holds.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

/// Cursor — next free byte in the arena. Monotonic post-init.
static mut HEAP_CURSOR: usize = 0;

/// Exclusive upper bound of the arena.
static mut HEAP_END: usize = 0;

/// Diagnostic accessor — returns (cursor, end). Used for debug
/// kprintlns; safe under INV-1.
pub fn diagnostic_state() -> (usize, usize) {
    // SAFETY: INV-1 single-hart, reads only.
    unsafe {
        let c = core::ptr::read_volatile(core::ptr::addr_of!(HEAP_CURSOR));
        let e = core::ptr::read_volatile(core::ptr::addr_of!(HEAP_END));
        (c, e)
    }
}

/// Bump allocator type — zero-sized; all state lives in the static
/// pair above. One instance, registered as `#[global_allocator]`.
pub struct BumpAllocator;

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator;

/// Initialize the global heap arena.
///
/// # Preconditions
/// - Called exactly once during boot (`mem::kvm::init`).
/// - `start <= end`, both word-aligned.
/// - The range `[start, end)` is identity-mapped kernel-writable RAM.
/// - No prior `alloc` has executed (no allocator users before init).
///
/// # Postconditions
/// - `HEAP_CURSOR == start`, `HEAP_END == end`. INV-12 established.
///
/// # Safety
///
/// Caller asserts the preconditions above. Violating them (calling
/// twice, racing with an allocation, passing an invalid range) breaks
/// INV-12 and any subsequent `alloc` is undefined behavior.
pub unsafe fn init(start: usize, end: usize) {
    debug_assert!(start <= end);
    // SAFETY: INV-1 (single-hart), INV-12 (one-time init pre-allocator-
    // use). Caller guarantees no concurrent or prior allocator activity.
    unsafe {
        HEAP_CURSOR = start;
        HEAP_END = end;
    }
}

// SAFETY: INV-1 + INV-12. Single-hart kernel; the cursor is mutated only
// by `alloc()` calls (and never by the no-op `dealloc`), so concurrent
// access is structurally absent. The arena bounds are established once
// at boot via `init()` and never re-initialized (INV-12).
unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: INV-1 + INV-12. Single-hart, post-init access to the
        // monotonic cursor. We compute a candidate aligned start, check
        // bounds, and either advance the cursor or return null.
        unsafe {
            let cursor = HEAP_CURSOR;
            let end = HEAP_END;

            let align = layout.align();
            let size = layout.size();

            // Round cursor up to the requested alignment. `align` is a
            // power of two by `Layout` invariant.
            let aligned = (cursor + align - 1) & !(align - 1);

            // Overflow / OOM check: if `aligned + size` would wrap, or
            // exceed `end`, we cannot satisfy this request.
            let new_cursor = match aligned.checked_add(size) {
                Some(v) => v,
                None => return ptr::null_mut(),
            };
            if new_cursor > end {
                return ptr::null_mut();
            }

            HEAP_CURSOR = new_cursor;
            aligned as *mut u8
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Phase 0: arena-per-boot. No free. See module docstring.
    }
}
