// SPDX-License-Identifier: AGPL-3.0-only
//! Tier-2 UART driver smoke test — Phase 0 PR 5.
//!
//! Boots the kernel under QEMU virt RV64 and asserts the post-loader
//! marker `"tier-2 uart driver loaded"` shows up on the console within
//! the wall-clock budget. If the marker appears, the signed-envelope
//! verifier accepted the embedded blob, wasmi parsed + instantiated it,
//! and `wari::mmio_write8` was registered without error.
//!
//! Mirrors the pattern in `runtime_noop.rs`; only the marker differs.
//!
//! Failure modes the test catches:
//!   - signature verification failed (placeholder pubkey, key mismatch)
//!     → `wari runtime: tier-2 load failed` instead of marker
//!   - wasmi parse / instantiate failed → same
//!   - host-fn registration failed → same
//!   - kernel panics → no marker, panic loop
//!
//! Adversarial counterpart (wrong-key rejection) is deferred per PR 5
//! body's "Out of scope" section.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const KERNEL_ELF_REL: &str = "target/riscv64gc-unknown-none-elf/release/wari";

const MARKER: &str = "tier-2 uart driver loaded";

/// Deadline for the marker. Tier-2 instantiation runs after MMU init
/// and wasmi-engine bring-up, plus the ed25519 verify pass.
const MARKER_DEADLINE: Duration = Duration::from_secs(6);

/// Hard cap on QEMU lifetime.
const QEMU_WALLCLOCK: Duration = Duration::from_secs(8);

fn workspace_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

#[test]
fn kernel_loads_tier2_uart_driver() {
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

    thread::sleep(MARKER_DEADLINE + Duration::from_millis(500));
    let _ = child.kill();
    let _ = child.wait();

    let captured = handle.join().expect("reader thread panicked");
    let text = String::from_utf8_lossy(&captured);

    assert!(
        text.contains(MARKER),
        "tier-2 marker not found in QEMU stdout:\n--- begin ---\n{}\n--- end ---",
        text,
    );
}
