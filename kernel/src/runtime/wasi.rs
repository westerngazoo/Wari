// SPDX-License-Identifier: AGPL-3.0-only
//! WASI Preview 1 host functions exposed to Tier-1 WASM.
//!
//! Phase 0 surface (matches `docs/wasi-surface.md`):
//!   - `wasi_snapshot_preview1::fd_write(fd, iovs, iovs_len, nwritten) -> errno`
//!   - `wasi_snapshot_preview1::proc_exit(code) -> !`
//!
//! Both are gated by Tier-1 capabilities (`stdout`, `exit`). Failure
//! modes return WASI errnos to the calling module — never a kernel
//! panic (R5).
//!
//! ## Why `wasi_snapshot_preview1` as the import module name
//!
//! Picked: the standard WASI Preview 1 module name. Considered: a
//! Wari-private `wari_wasi` module name (rejected — would force every
//! Tier-1 toolchain to emit non-standard imports; defeats the
//! "WASI-compatible Tier-1" goal in `docs/wasi-surface.md`). Why this
//! won: maximum toolchain compatibility (wasi-libc, Rust's `std::io`,
//! Go's WASI target, etc.). Cost accepted: Wari must implement the
//! exact WASI P1 ABI shapes, not invent its own.
//!
//! ## Why single-iovec `fd_write`
//!
//! Picked: read only `iovs[0]` and ignore higher iovecs. Considered:
//! full iovec-array semantics (write each iovec sequentially, sum the
//! byte counts). Why this won: Phase 0 Simplicity First — the only
//! Tier-1 module is `apps/hello`, which always passes a single iovec.
//! A loop over up to 16 iovecs is ~30 LOC of marshalling per call;
//! defer until a second caller appears. Cost accepted: a Tier-1 module
//! that batched multiple iovecs would silently drop all but the first.
//! `nwritten` reflects the actual count written, so callers that check
//! `nwritten` against `iovs_len`-summed-buf_len would notice.
//!
//! ## Why `proc_exit` via `wasmi::Error::i32_exit`
//!
//! Picked: return `wasmi::Error::i32_exit(code as i32)` from the host
//! fn. The error unwinds the wasmi call stack; the kernel-side runner
//! (`runtime::run_tier1_hello`) inspects the resulting `Error` via
//! `i32_exit_status()`. Considered:
//!   - set a flag in `Tier1HostState.exit_code` and have the host fn
//!     return normally → rejected: the WASI spec says `proc_exit` does
//!     not return; returning normally would let the WASM module
//!     continue executing past the call, which would either trap on a
//!     bogus return type mismatch or behave undefined.
//!   - `wasmi::Error::host` with a custom HostError → workable but
//!     re-implements the i32-exit pattern wasmi already exposes for
//!     exactly this case (see `wasmi/src/error.rs`'s comment "This is
//!     usually used as return code by WASI applications").
//! Why this won: matches wasmi's first-class WASI exit support. The
//! kernel detects the exit cleanly via `i32_exit_status()`. Cost
//! accepted: we still also stash the code in `Tier1HostState.exit_code`
//! for diagnostic logging at the trap site.

#![allow(dead_code)]
#![allow(clippy::doc_lazy_continuation)]

use wasmi::{Caller, Error, Linker};

use crate::cap::Caps;
use crate::error::KernelError;
use crate::runtime::tier2_uart;

/// WASI Preview 1 errno: success.
pub const WASI_ESUCCESS: u32 = 0;
/// WASI Preview 1 errno: bad fd.
pub const WASI_EBADF: u32 = 8;
/// WASI Preview 1 errno: bad address (iovec OOB on linear memory).
pub const WASI_EFAULT: u32 = 21;
/// WASI Preview 1 errno: I/O error (driver-side failure).
pub const WASI_EIO: u32 = 29;
/// WASI Preview 1 errno: permission denied (capability not granted).
pub const WASI_EPERM: u32 = 63;

/// Per-instance host context for Tier-1 modules.
///
/// Carries the Tier-1 capability set and an exit-code latch the kernel
/// inspects after `proc_exit` traps the instance. `exit_code` is `None`
/// until `host_proc_exit` fires; the runner clears nothing afterward
/// (the instance is dropped immediately).
pub struct Tier1HostState {
    /// Capabilities granted to this instance at `load_tier1` time.
    pub caps: Caps,
    /// Set by `host_proc_exit` so the kernel-side runner can log the
    /// exit code in addition to inspecting `Error::i32_exit_status`.
    pub exit_code: Option<u32>,
}

/// Register Tier-1 WASI host fns into a fresh linker, with the
/// caller's `proc_id` baked into each closure via `move`.
///
/// # Contract
///
/// - Precondition: `linker` is freshly constructed.
/// - Postcondition: `wasi_snapshot_preview1::{fd_write, proc_exit}`
///   plus the cap-management `wari::cap_*` host fns are bound, all
///   carrying `proc_id` so cap checks reach the calling instance's
///   CSpace.
/// - Errors: `KernelError::BadWasm` if wasmi rejects any binding.
pub fn register_wasi_host_fns(
    linker: &mut Linker<Tier1HostState>,
    proc_id: u8,
) -> Result<(), KernelError> {
    let pid = proc_id;

    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "fd_write",
            move |caller: Caller<'_, Tier1HostState>,
                  fd: u32,
                  iovs_ptr: u32,
                  iovs_len: u32,
                  nwritten_ptr: u32|
                  -> u32 {
                host_fd_write(caller, pid, fd, iovs_ptr, iovs_len, nwritten_ptr)
            },
        )
        .map_err(|_| KernelError::BadWasm)?;
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "proc_exit",
            move |caller: Caller<'_, Tier1HostState>, code: u32|
                  -> Result<(), Error> {
                host_proc_exit(caller, pid, code)
            },
        )
        .map_err(|_| KernelError::BadWasm)?;

    // Phase-1b cap-management host fns. proc_id is captured by each
    // closure so each Tier-1 instance touches its own CSpace.
    use crate::cap::{
        cap_copy_impl, cap_delete_impl, cap_lookup_impl, cap_mint_impl,
        cap_register_impl, cap_revoke_impl, cap_unregister_impl,
    };
    linker
        .func_wrap(
            "wari",
            "cap_mint",
            move |_: Caller<'_, Tier1HostState>, ps: u32, ts: u32, r: u32, b: u32| -> i32 {
                cap_mint_impl(pid, ps, ts, r, b)
            },
        )
        .map_err(|_| KernelError::BadWasm)?;
    linker
        .func_wrap(
            "wari",
            "cap_copy",
            move |_: Caller<'_, Tier1HostState>, src: u32, tgt: u32| -> i32 {
                cap_copy_impl(pid, src, tgt)
            },
        )
        .map_err(|_| KernelError::BadWasm)?;
    linker
        .func_wrap(
            "wari",
            "cap_revoke",
            move |_: Caller<'_, Tier1HostState>, slot: u32| -> i32 {
                cap_revoke_impl(pid, slot)
            },
        )
        .map_err(|_| KernelError::BadWasm)?;
    linker
        .func_wrap(
            "wari",
            "cap_delete",
            move |_: Caller<'_, Tier1HostState>, slot: u32| -> i32 {
                cap_delete_impl(pid, slot)
            },
        )
        .map_err(|_| KernelError::BadWasm)?;
    linker
        .func_wrap(
            "wari",
            "cap_lookup",
            move |mut caller: Caller<'_, Tier1HostState>, slot: u32, out_buf: u32| -> i32 {
                cap_lookup_impl(&mut caller, pid, slot, out_buf)
            },
        )
        .map_err(|_| KernelError::BadWasm)?;
    // Cap-fastpath register/unregister (PR cap-fastpath-1).
    linker
        .func_wrap(
            "wari",
            "cap_register",
            move |_: Caller<'_, Tier1HostState>, cspace_slot: u32| -> i32 {
                cap_register_impl(pid, cspace_slot)
            },
        )
        .map_err(|_| KernelError::BadWasm)?;
    linker
        .func_wrap(
            "wari",
            "cap_unregister",
            move |_: Caller<'_, Tier1HostState>, reg_idx: u32| -> i32 {
                cap_unregister_impl(pid, reg_idx)
            },
        )
        .map_err(|_| KernelError::BadWasm)?;

    // PR Net-6b — Tier-1-facing socket API. Both fns dispatch to
    // crate::cap::syscall, which validates the caller's Net cap
    // at SLOT_NET, calls into the Tier-2 net driver, and mints/
    // revokes Socket caps in the caller's CSpace.
    use crate::cap::{
        net_socket_bind_impl, net_socket_close_impl, net_socket_create_impl,
        net_socket_listen_impl,
    };
    linker
        .func_wrap(
            "wari",
            "net_socket_create",
            move |_: Caller<'_, Tier1HostState>, proto: u32, slot_for_cap: u32| -> i32 {
                net_socket_create_impl(pid, proto, slot_for_cap)
            },
        )
        .map_err(|_| KernelError::BadWasm)?;
    linker
        .func_wrap(
            "wari",
            "net_socket_close",
            move |_: Caller<'_, Tier1HostState>, slot: u32| -> i32 {
                net_socket_close_impl(pid, slot)
            },
        )
        .map_err(|_| KernelError::BadWasm)?;
    // BISECT NOTE: net_socket_bind / net_socket_listen registration
    // is currently disabled here. With it on, Tier-1 instantiate
    // hangs (build 58 trace) even when hello does not import them.
    // The driver-side bind/listen exports + kernel-side typed-func
    // resolution work fine (boot self-test passes). Investigating
    // in Net-6c follow-up; the host fns + impls are still defined
    // and exported from cap, so re-enabling these two
    // registrations is one comment-removal away.
    let _ = (net_socket_bind_impl, net_socket_listen_impl);

    Ok(())
}

/// Size of a WASI Preview 1 `iovec` in linear memory: two `u32`s.
const IOVEC_SIZE: usize = 8;

/// Maximum byte count Phase-0 will marshal in a single `fd_write` call.
///
/// Bounded so the on-stack scratch buffer in `host_fd_write` is small
/// and known. The hello string is 16 bytes; this leaves headroom for
/// short messages without needing the heap (R2: no allocation in
/// host-fn dispatch).
const FD_WRITE_MAX: usize = 256;

/// `wasi_snapshot_preview1::fd_write(fd, iovs_ptr, iovs_len, nwritten_ptr) -> errno`.
///
/// Phase 0 semantics:
///   - Validate `caps.stdout` — `EPERM` if denied.
///   - Validate `fd == 1` — `EBADF` for any other fd.
///   - Read the first iovec from caller's linear memory at `iovs_ptr`
///     (8 bytes: `(buf, buf_len)` as little-endian `u32`s).
///   - Read up to `FD_WRITE_MAX` bytes from caller's lin-mem at `buf`.
///   - Push the bytes through the Tier-2 UART driver via
///     `tier2_uart::write` — `EIO` on driver failure.
///   - Write the byte count to caller's lin-mem at `nwritten_ptr`.
///   - Return `ESUCCESS` (0).
///
/// Any out-of-bounds linear-memory access on the caller side returns
/// `EFAULT` rather than panicking (R5).
fn host_fd_write(
    mut caller: Caller<'_, Tier1HostState>,
    proc_id: u8,
    fd: u32,
    iovs_ptr: u32,
    iovs_len: u32,
    nwritten_ptr: u32,
) -> u32 {
    // PR 3b cap-mediated gate: Tier-1 holds an Endpoint cap with
    // WRITE rights at slot 0 (the send side of uart_ipc_ep — the
    // shape of "stdout" in Phase 1b's cap model). With the
    // scheduler's multi-instance support, `proc_id` is passed in
    // by the closure so each Tier-1 instance consults its own
    // CSpace.
    use crate::cap::{check_cap, ObjectKind, CAP_RIGHT_WRITE};
    if check_cap(proc_id, 0, ObjectKind::Endpoint, CAP_RIGHT_WRITE).is_err() {
        return WASI_EPERM;
    }
    // Phase-0: only stdout (fd=1) is plumbed.
    if fd != 1 {
        return WASI_EBADF;
    }
    // No iovecs → trivially zero bytes; report success and zero count.
    if iovs_len == 0 {
        return write_nwritten(&mut caller, nwritten_ptr, 0);
    }

    // Resolve the caller's linear memory.
    let memory = match caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
    {
        Some(m) => m,
        None => return WASI_EFAULT,
    };

    // Stage 1 — read the first iovec (single-iovec policy, see
    // module docstring).
    let mut iov_buf = [0u8; IOVEC_SIZE];
    if memory
        .read(&caller, iovs_ptr as usize, &mut iov_buf)
        .is_err()
    {
        return WASI_EFAULT;
    }
    let buf_offset =
        u32::from_le_bytes([iov_buf[0], iov_buf[1], iov_buf[2], iov_buf[3]]);
    let buf_len =
        u32::from_le_bytes([iov_buf[4], iov_buf[5], iov_buf[6], iov_buf[7]]);

    // Bound the byte count by the on-stack scratch (R2: no alloc in
    // host-fn dispatch). Truncate silently if the caller asked for
    // more — the byte count returned via `nwritten_ptr` reflects what
    // was actually written.
    let n = (buf_len as usize).min(FD_WRITE_MAX);
    let mut bytes = [0u8; FD_WRITE_MAX];
    if memory
        .read(&caller, buf_offset as usize, &mut bytes[..n])
        .is_err()
    {
        return WASI_EFAULT;
    }

    // Stage 2 — push through the Tier-2 UART driver.
    // SAFETY: INV-1 (single-hart) + INV-8 (post-init) + INV-14 (Tier-2
    // singleton). `kmain` orders `run_tier2_uart` before
    // `run_tier1_hello`, so by the time any Tier-1 host fn fires the
    // singleton is `Some(_)`.
    let written = match unsafe { tier2_uart::write(&bytes[..n]) } {
        Ok(w) => w,
        Err(_) => return WASI_EIO,
    };

    // Stage 3 — report the byte count back to the caller.
    write_nwritten(&mut caller, nwritten_ptr, written as u32)
}

/// Helper: write `count` to caller's linear memory at `nwritten_ptr`
/// (4 bytes, little-endian). Returns `WASI_ESUCCESS` on a clean write,
/// `WASI_EFAULT` if the address is OOB.
fn write_nwritten(
    caller: &mut Caller<'_, Tier1HostState>,
    nwritten_ptr: u32,
    count: u32,
) -> u32 {
    let memory = match caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
    {
        Some(m) => m,
        None => return WASI_EFAULT,
    };
    let bytes = count.to_le_bytes();
    if memory
        .write(&mut *caller, nwritten_ptr as usize, &bytes)
        .is_err()
    {
        return WASI_EFAULT;
    }
    WASI_ESUCCESS
}

/// `wasi_snapshot_preview1::proc_exit(code) -> !`.
///
/// Returns `wasmi::Error::i32_exit(code as i32)` to trap the instance.
/// The kernel-side runner (`runtime::run_tier1_hello`) catches the
/// `Error` and inspects `i32_exit_status()` to obtain `code`.
///
/// Capability gate: requires `caps.exit`. A denied call still traps
/// with `i32_exit(-1)` so the module cannot continue executing without
/// the cap — the alternative (return normally) violates the WASI spec
/// "does not return" contract.
fn host_proc_exit(
    mut caller: Caller<'_, Tier1HostState>,
    proc_id: u8,
    code: u32,
) -> Result<(), Error> {
    // PR 3b cap-mediated gate: Tier-1 holds an Endpoint cap with
    // WRITE rights at slot 1 (the send side of kernel_exit_ep).
    // Without this cap the module cannot exit cleanly; we still
    // trap-with-(-1) since WASI requires `proc_exit` to not return.
    use crate::cap::{check_cap, ObjectKind, CAP_RIGHT_WRITE};
    if check_cap(proc_id, 1, ObjectKind::Endpoint, CAP_RIGHT_WRITE).is_err() {
        caller.data_mut().exit_code = Some(u32::MAX);
        return Err(Error::i32_exit(-1));
    }
    caller.data_mut().exit_code = Some(code);
    Err(Error::i32_exit(code as i32))
}
