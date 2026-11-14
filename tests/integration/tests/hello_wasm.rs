// SPDX-License-Identifier: AGPL-3.0-only
//! Tier-1 hello smoke test (`docs/testing.md`, Phase-0 list).
//!
//! Boots the kernel under QEMU virt RV64 and asserts that a Tier-1
//! instance ran under wasmi: `Hello from Wari` reached the UART via
//! WASI `fd_write`, and the scheduler logged that instance's
//! `[t1:<proc_id>] exit(0)` (`kernel/src/runtime/tier1_pool.rs`).
//! The boot-time noop instantiation this file used to watch for
//! (`wasmi OK`) no longer exists: the scheduler runs the real Tier-1
//! pool instead, so the proof that the engine links and instantiates
//! is the hello instance itself.
//!
//! Mirrors the pattern in `mmu_smoke.rs`; only the markers and the
//! deadline differ.
//!
//! Failure modes the test catches:
//!   - wasmi failed to validate or link the hello blob → the
//!     scheduler reports the instance faulted, no `Hello from Wari`
//!   - `fd_write` or `proc_exit` unbound or mis-typed → no exit line
//!   - kernel hangs in trap → no marker, timeout
//!   - kernel panics → no marker, possible panic banner

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const KERNEL_ELF_REL: &str = "target/riscv64gc-unknown-none-elf/release/wari";

/// Deadline for the markers to appear. The hello instance only starts
/// after both Tier-2 drivers load and the module registry stages its
/// daemons, so it gets more headroom than the MMU marker; 8 s matches
/// `tests/security`'s `DEFAULT_WALLCLOCK` for the same boot.
const MARKER_DEADLINE: Duration = Duration::from_secs(8);

/// Hard cap on QEMU lifetime.
const QEMU_WALLCLOCK: Duration = Duration::from_secs(10);

fn workspace_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

#[test]
fn tier1_hello_runs_under_wasmi() {
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
        text.contains("Hello from Wari"),
        "Tier-1 hello output not found in QEMU stdout:\n--- begin ---\n{}\n--- end ---",
        text,
    );

    // The proc id is assigned at boot, so match the shape of the
    // line rather than a literal.
    let exited_zero = text
        .lines()
        .any(|line| line.starts_with("[t1:") && line.ends_with("] exit(0)"));
    assert!(
        exited_zero,
        "no `[t1:<n>] exit(0)` line in QEMU stdout:\n--- begin ---\n{}\n--- end ---",
        text,
    );
}
