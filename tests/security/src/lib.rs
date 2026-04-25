// SPDX-License-Identifier: AGPL-3.0-only
//! Shared QEMU runner for the Phase-0 adversarial security tests.
//!
//! Each test boots the same kernel ELF under `qemu-system-riscv64`,
//! captures UART output for a deterministic wall-clock budget, and
//! returns the captured text. Per-test assertions live in the
//! `tests/<testname>.rs` files.
//!
//! ## Why a shared lib (not duplicate per test)
//!
//! Each adversarial test runs the same QEMU command shape; only the
//! per-run kernel build / blob varies. Pulling the spawn loop into one
//! `boot_kernel_capture` function keeps every per-test file under 80
//! lines and focused on its assertion.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Path (workspace-relative) of the kernel ELF built by `make build`.
pub const KERNEL_ELF_REL: &str = "target/riscv64gc-unknown-none-elf/release/wari";

/// Default wall-clock cap on a QEMU run. Adversarial tests deliberately
/// short-circuit kernel boot (panic absence, exit detection) so a full
/// driver-load + Tier-1 run easily fits in 8 seconds.
pub const DEFAULT_WALLCLOCK: Duration = Duration::from_secs(8);

/// Locate the workspace root from the test crate's `CARGO_MANIFEST_DIR`.
///
/// `tests/security/Cargo.toml` sits at `<root>/tests/security/`, so
/// pop two levels.
pub fn workspace_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

/// Boot the kernel under QEMU and capture UART output until the
/// wall-clock budget elapses.
///
/// # Contract
///
/// - Precondition: `make build` has produced the kernel ELF at
///   `KERNEL_ELF_REL`. The test asserts existence with a clear hint.
/// - Returns the captured stdout as a `String`. Non-UTF-8 bytes are
///   replaced (`String::from_utf8_lossy`).
/// - The QEMU child is killed at the end of the budget — no
///   indefinite hangs.
pub fn boot_kernel_capture(wallclock: Duration) -> String {
    let root = workspace_root();
    let kernel = root.join(KERNEL_ELF_REL);
    assert!(
        kernel.exists(),
        "kernel ELF not found at {:?}. Run `make build` first.",
        kernel,
    );

    let mut child = Command::new("qemu-system-riscv64")
        .args([
            "-machine",
            "virt",
            "-nographic",
            "-bios",
            "default",
            "-kernel",
        ])
        .arg(&kernel)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn qemu-system-riscv64 — is QEMU installed?");

    let mut stdout = child.stdout.take().expect("qemu stdout");
    let start = Instant::now();

    let handle = thread::spawn(move || {
        let mut buf = Vec::with_capacity(8192);
        let mut chunk = [0u8; 256];
        while start.elapsed() < wallclock {
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
        }
        buf
    });

    thread::sleep(wallclock + Duration::from_millis(500));
    let _ = child.kill();
    let _ = child.wait();

    let captured = handle.join().expect("reader thread panicked");
    String::from_utf8_lossy(&captured).into_owned()
}

/// Markers the kernel emits during a Phase-0 boot. Tests assert their
/// presence/absence to verify the kernel survived an adversarial input.
pub mod markers {
    /// Printed by the kernel after `runtime::run_tier2_uart` succeeds.
    /// If absent, signature verification or driver instantiation
    /// failed — the kernel halted in `kmain`'s wfi loop.
    pub const TIER2_LOADED: &str = "tier-2 uart driver loaded";

    /// Printed by Tier-1 hello after `proc_exit(0)`: see
    /// `kernel/src/runtime/mod.rs::run_tier1_hello`.
    pub const HELLO_EXIT_0: &str = "[hello] exit(0)";

    /// Printed by the kernel boot banner. Always expected — its
    /// absence indicates the kernel did not even boot.
    pub const BOOT_BANNER: &str = "Wari v0 build";
}
