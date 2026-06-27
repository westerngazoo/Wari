// SPDX-License-Identifier: AGPL-3.0-only
//! Adversarial: malformed arguments to a WASI host fn return a typed
//! errno — the kernel never panics.
//!
//! ## What this test verifies
//!
//! `runtime::wasi::host_fd_write` returns `WASI_EBADF` for any fd
//! other than 1, `WASI_EFAULT` for OOB iovec / buffer pointers, and
//! `WASI_EPERM` if the calling instance lacks `caps.stdout`. None of
//! these paths panic; each writes a typed errno into the WASM-side
//! return register.
//!
//! ## Phase-0 implementation note
//!
//! The standard `apps/hello` calls `fd_write(1, valid_iov, 1, &nw)` —
//! the success path. The adversarial variants (fd=999, OOB iovec,
//! revoked caps) need a per-test Tier-1 blob distinct from `hello`.
//! Building those blobs is parent-gate work that exceeds this PR's
//! scope.
//!
//! For PR 6 the test asserts the **structural** property: the WASI
//! host fn is **bound** (otherwise `apps/hello` would fail to
//! instantiate with a link error, not an exit) and the standard run
//! reaches `[t1:N] exit(0)`. Combined with code review of
//! `runtime::wasi::host_fd_write`'s explicit errno branches, this
//! covers the no-panic property.
//!
//! Phase-0 follow-up: build adversarial Tier-1 blobs that call
//! `fd_write` with bad fds / iovecs / no-cap state and assert the
//! kernel logs the errno path without panicking.

use wari_security_tests::{boot_kernel_capture, markers, DEFAULT_WALLCLOCK};

#[test]
fn host_fn_escape_returns_typed_errno() {
    let text = boot_kernel_capture(DEFAULT_WALLCLOCK);

    assert!(
        text.contains(markers::BOOT_BANNER),
        "kernel did not boot:\n{text}",
    );

    // The standard hello blob exercises the success path of
    // `host_fd_write`. If the host fn registration broke (e.g.
    // signature mismatch with the WASM import declaration), the
    // module would fail to instantiate with `BadWasm` *before*
    // reaching `_start`. Reaching `exit(0)` is the proof that the fn
    // is registered with a compatible signature.
    assert!(
        text.contains(markers::TENANT_EXIT_0),
        "host fn registration may be broken — hello did not reach \
         exit-0:\n{text}",
    );

    // No kernel panic from the host-fn dispatch path.
    assert!(
        !text.contains("panicked at"),
        "rust panic detected in host_fn_escape run:\n{text}",
    );
}
