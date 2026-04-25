// SPDX-License-Identifier: AGPL-3.0-only
//! Runtime smoke test — Phase 0 PR 4.
//!
//! Boots the kernel under QEMU virt RV64 and asserts the post-runtime
//! marker `"wasmi OK"` shows up on the console within the wall-clock
//! budget. If the marker appears, the bump allocator initialized, the
//! wasmi engine linked, and the noop module instantiated cleanly.
//!
//! Mirrors the pattern in `mmu_smoke.rs`; only the marker and the
//! deadline differ.
//!
//! Failure modes the test catches:
//!   - wasmi failed to validate the noop blob → `wasmi runtime failed`
//!     line appears instead of `wasmi OK`
//!   - bump allocator OOM mid-instantiate → same
//!   - kernel hangs in trap → no marker, timeout
//!   - kernel panics → no marker, possible panic banner

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const KERNEL_ELF_REL: &str =
    "target/riscv64gc-unknown-none-elf/release/wari";

/// Deadline for the marker to appear. Wasmi instantiation is allowed
/// more headroom than MMU init because module parse/validate runs
/// against the bump allocator.
const MARKER_DEADLINE: Duration = Duration::from_secs(5);

/// Hard cap on QEMU lifetime.
const QEMU_WALLCLOCK: Duration = Duration::from_secs(7);

fn workspace_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

#[test]
fn kernel_runs_wasmi_noop() {
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
        text.contains("wasmi OK"),
        "wasmi runtime marker not found in QEMU stdout:\n--- begin ---\n{}\n--- end ---",
        text,
    );
}
