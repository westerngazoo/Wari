// SPDX-License-Identifier: AGPL-3.0-only
//! Boot-initialized singleton for the Tier-2 UART driver instance.
//!
//! The Tier-2 UART driver loaded at boot (PR 5) holds the only path to
//! the NS16550 register window: its `wari::mmio_write8` host fn is the
//! one place gated by `CAP_MMIO_UART`. Tier-1 hello (PR 6) needs to
//! print via `fd_write`, which means the kernel must reach into the
//! driver's `write` export from the WASI host fn dispatch.
//!
//! ## Why a singleton (Why/How depth)
//!
//! Picked: `static mut Option<Tier2UartHandle>`, written once during
//! boot via `install`, read via `&mut TIER2_UART` in `write` calls.
//! Considered:
//!   - thread the handle through every host-fn dispatch path → rejected:
//!     wasmi's `Caller<'_, T>` is parameterised by Tier-1's HostState,
//!     not Tier-2's; threading would force a unified HostState across
//!     tiers, which conflates capability sets.
//!   - put the driver's Store inside Tier-1's HostState → rejected:
//!     would force Tier-1's Store to outlive its own instance and
//!     reverse the natural ownership.
//!   - put the driver in a `RefCell`/`Mutex` → rejected: borrow-check
//!     dynamism without need (INV-1 makes locks redundant in Phase 0).
//!
//! Why this won: matches the `page_alloc::get()` and `runtime::heap`
//! pattern already established (INV-8). Smallest unsafe surface, single
//! call site, exclusivity is structural under INV-1.
//!
//! Cost accepted: the handle is global, so any future code path can
//! reach into UART. Phase 1 replaces this with a per-process driver
//! handle table once the capability system lands.
//!
//! ## SCRATCH_OFFSET (Why/How depth)
//!
//! Picked: `0x80000` (512 KiB) into the driver's linear memory.
//! Considered:
//!   - `0x100000` (1 MiB) → rejected: at the boundary of the default
//!     17-page initial linmem produced by `wasm32-unknown-unknown` for
//!     a tiny `cdylib`; `Memory::write` would OOB and we'd need to
//!     grow the memory first.
//!   - `0x10000` (64 KiB) → rejected: too close to the LLVM-default
//!     stack base (top-of-memory, growing down toward 0).
//!   - export `__heap_base` from the driver → considered for Phase 1;
//!     adds a build-time symbol contract that's overkill while the
//!     driver is < 4 KiB of code with no `.data`.
//! Why this won: 512 KiB is well above any plausible stack depth in a
//! `no_std` driver (driver code is < 100 LOC; recursion depth zero) and
//! well below the 17-page default initial linmem. Cost accepted: a
//! magic constant; the driver must keep its data section small enough
//! that `[SCRATCH_OFFSET, SCRATCH_OFFSET + N)` is not used by the
//! driver's own globals. Documented; if this ever changes, the test
//! harness's `Memory::write` will fail loudly.
//!
//! ## Invariants
//!
//! - INV-1 (single-hart): `&mut TIER2_UART` access has no contention.
//! - INV-8 (post-init singleton): `install` runs exactly once during
//!   boot before any `write` call.
//! - INV-14 (this PR): `TIER2_UART` is set exactly once via `install`
//!   and only mutated through `write` (which mutates the driver's Store
//!   transitively, not the `Option` itself).

#![allow(dead_code)]
#![allow(clippy::doc_lazy_continuation)]

use core::ptr::addr_of_mut;

use wasmi::{Instance, Store, TypedFunc};

use crate::error::KernelError;
use crate::runtime::host_fns::Tier2HostState;

/// Scratch offset into the Tier-2 driver's linear memory where the
/// kernel marshals Tier-1 bytes before invoking `write`. See module
/// docstring for the choice rationale.
pub const SCRATCH_OFFSET: u32 = 0x80000;

/// Live handle to the Tier-2 UART driver instance.
///
/// Carries the per-instance `Store` (so `Memory::write` can be issued
/// against the driver's linear memory) plus the typed `write` function
/// handle resolved once at install time.
pub struct Tier2UartHandle {
    /// The driver's wasmi instance.
    pub instance: Instance,
    /// Store owning the driver's `Tier2HostState`.
    pub store: Store<Tier2HostState>,
    /// Typed handle to the driver's `write(buf_ptr, len) -> i32` export.
    pub write_fn: TypedFunc<(u32, u32), i32>,
}

/// The singleton — set by `install`, read by `write`.
///
/// `None` until boot reaches `runtime::run_tier2_uart`. Subsequent
/// reads through `write` always observe `Some(_)` thanks to the kernel
/// `kmain` ordering (`run_tier2_uart` precedes `run_tier1_hello`).
static mut TIER2_UART: Option<Tier2UartHandle> = None;

/// Install the Tier-2 UART singleton at boot.
///
/// # Preconditions
///
/// - Called exactly once, during `runtime::run_tier2_uart`.
/// - Single-hart boot context (INV-1); interrupts disabled.
/// - The handle's `instance` and `write_fn` resolve against `store`.
///
/// # Postconditions
///
/// - `TIER2_UART` is `Some(handle)`. INV-14 established.
///
/// # Safety
///
/// The caller must guarantee one-time invocation pre-runtime use. A
/// second call would overwrite the previous handle, leaking the
/// driver's previous Store and breaking INV-14.
pub unsafe fn install(handle: Tier2UartHandle) {
    // SAFETY: INV-1 (single-hart) + INV-8 (boot-time install) +
    // INV-14 (one-time set). Caller asserts the preconditions above.
    unsafe {
        *addr_of_mut!(TIER2_UART) = Some(handle);
    }
}

/// Push `bytes` to the UART through the Tier-2 driver.
///
/// Marshalling sequence:
///   1. Acquire `&mut TIER2_UART`. Bail with `DriverError` if `None`
///      (caller broke ordering).
///   2. Resolve the driver's `memory` export.
///   3. Write `bytes` into the driver's linear memory at
///      `[SCRATCH_OFFSET, SCRATCH_OFFSET + bytes.len())`.
///   4. Invoke the typed `write_fn(SCRATCH_OFFSET, bytes.len() as u32)`.
///   5. Inspect the return: a non-negative i32 is the byte count
///      pushed; a negative i32 is the host-side errno from
///      `wari::mmio_write8` (capability or address denied).
///
/// # Errors
///
/// `KernelError::DriverError` on:
///   - singleton not installed (programmer error in boot ordering),
///   - driver does not export `memory` (broken signed blob),
///   - linear-memory write OOB (scratch offset miscalibrated),
///   - typed-call failure (driver trapped),
///   - driver returned a negative errno (capability misconfigured).
///
/// Returns `Ok(n)` where `n` is the byte count the driver reports
/// having written (always `bytes.len()` on success in Phase 0; the
/// driver loops one byte at a time and returns `len` on success).
///
/// # Safety
///
/// Single-hart, post-init, exclusive `&mut` to the singleton (INV-1 +
/// INV-8 + INV-14). No other path holds an alias to `TIER2_UART`.
pub unsafe fn write(bytes: &[u8]) -> Result<usize, KernelError> {
    // SAFETY: INV-1 + INV-8 + INV-14. Single-hart kernel; the singleton
    // is post-`install` (caller is `runtime::wasi::host_fd_write`, which
    // only fires after `kmain` has called `run_tier2_uart`).
    let handle: &mut Tier2UartHandle = unsafe {
        match &mut *addr_of_mut!(TIER2_UART) {
            Some(h) => h,
            None => return Err(KernelError::DriverError),
        }
    };

    // Resolve `memory`. Wasmi exposes WASM linear memory under the
    // export name "memory" by default — the driver's `cdylib` build
    // emits exactly that.
    let memory = handle
        .instance
        .get_export(&handle.store, "memory")
        .and_then(|e| e.into_memory())
        .ok_or(KernelError::DriverError)?;

    // Stage 1: write the Tier-1 bytes into the driver's linear memory.
    memory
        .write(&mut handle.store, SCRATCH_OFFSET as usize, bytes)
        .map_err(|_| KernelError::DriverError)?;

    // Stage 2: invoke the driver's `write(buf_ptr, len)`. Bounds the
    // call by `bytes.len()` cast to u32 — caller (fd_write) has already
    // checked the iovec length fits a u32.
    let r = handle
        .write_fn
        .call(&mut handle.store, (SCRATCH_OFFSET, bytes.len() as u32))
        .map_err(|_| KernelError::DriverError)?;

    if r < 0 {
        return Err(KernelError::DriverError);
    }

    Ok(r as usize)
}
