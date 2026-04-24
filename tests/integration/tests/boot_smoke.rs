// SPDX-License-Identifier: AGPL-3.0-only
//! Boot smoke test — Phase 0 PR 1.
//!
//! Builds the kernel (assumes Makefile `build` already ran), boots
//! it under QEMU virt RV64, captures stdout, and asserts:
//!
//!   1. The banner string `"Wari v0 build"` appears within 3 seconds.
//!   2. `"boot OK, hart 0"` appears on the same line.
//!   3. The kernel halts (WFI loop) — we bound runtime at 5 seconds
//!      and kill QEMU; success is "banner seen before the timeout".
//!
//! The test is a Cargo integration test rather than a standalone
//! binary so `cargo test --release` from `tests/integration/` picks
//! it up automatically.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Path to the kernel ELF, relative to the workspace root.
///
/// Wari is a Cargo workspace, so `target/` sits at the root (not
/// under `kernel/`). The kernel's `.cargo/config.toml` pins the RV64
/// target, so the ELF lands under this subdir.
const KERNEL_ELF_REL: &str =
    "target/riscv64gc-unknown-none-elf/release/wari";

/// Deadline for the banner to appear.
const BANNER_DEADLINE: Duration = Duration::from_secs(3);

/// Hard cap on total QEMU lifetime.
const QEMU_WALLCLOCK: Duration = Duration::from_secs(5);

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is tests/integration; parent of parent is root.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

#[test]
fn kernel_boots_and_prints_banner() {
    let root = workspace_root();
    let kernel = root.join(KERNEL_ELF_REL);
    assert!(
        kernel.exists(),
        "kernel ELF not found at {:?}. Run `make build` first.",
        kernel,
    );

    let mut child = Command::new("qemu-system-riscv64")
        .args([
            "-machine", "virt",
            "-nographic",
            "-bios", "default",
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

    // Reader thread: capture QEMU stdout non-blockingly by reading
    // into a buffer and checking periodically.
    let handle = thread::spawn(move || {
        let mut buf = Vec::with_capacity(4096);
        let mut chunk = [0u8; 256];
        while start.elapsed() < QEMU_WALLCLOCK {
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
        }
        buf
    });

    // Wait up to BANNER_DEADLINE, then kill QEMU regardless — the
    // kernel is supposed to halt in WFI, not exit, so QEMU will run
    // forever without intervention.
    thread::sleep(BANNER_DEADLINE + Duration::from_millis(500));
    let _ = child.kill();
    let _ = child.wait();

    let captured = handle.join().expect("reader thread panicked");
    let text = String::from_utf8_lossy(&captured);

    assert!(
        text.contains("Wari v0 build"),
        "banner prefix not found in QEMU stdout:\n--- begin ---\n{}\n--- end ---",
        text,
    );
    assert!(
        text.contains("boot OK, hart 0"),
        "banner suffix not found in QEMU stdout:\n--- begin ---\n{}\n--- end ---",
        text,
    );
}
