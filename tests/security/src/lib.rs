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
    boot_capture(wallclock, &[])
}

/// QEMU device arguments that give the guest a VirtIO-net NIC on the
/// same subnet `make run` uses (`Makefile` `QEMU_ARGS`), minus the
/// host port forward so parallel test binaries never contend for a
/// host port. Without a NIC the Tier-2 net driver's VirtIO init fails
/// at the magic-value read and nothing past it runs.
const NET_DEVICE_ARGS: &[&str] = &[
    "-global",
    "virtio-mmio.force-legacy=false",
    "-netdev",
    "user,id=net0,net=192.168.50.0/24,host=192.168.50.2",
    "-device",
    "virtio-net-device,netdev=net0",
];

/// [`boot_kernel_capture`] with a VirtIO-net device attached, for
/// tests whose property lives in the net driver's init path.
pub fn boot_kernel_capture_with_net(wallclock: Duration) -> String {
    boot_capture(wallclock, NET_DEVICE_ARGS)
}

fn boot_capture(wallclock: Duration, extra_args: &[&str]) -> String {
    let root = workspace_root();
    let kernel = root.join(KERNEL_ELF_REL);
    assert!(
        kernel.exists(),
        "kernel ELF not found at {:?}. Run `make build` first.",
        kernel,
    );

    let mut child = Command::new("qemu-system-riscv64")
        .args(["-machine", "virt", "-nographic", "-bios", "default"])
        .args(extra_args)
        .arg("-kernel")
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

    /// Prefix of every per-instance line the Tier-1 runner prints
    /// (`kernel/src/runtime/tier1_pool.rs::settle`): `[t1:<proc_id>]`
    /// followed by one of the `T1_*` events below. The proc id is
    /// handed out at boot and shifts whenever the daemon set changes,
    /// so tests match on the event with [`tier1_event`] rather than
    /// on a literal line.
    pub const TIER1_PREFIX: &str = "[t1:";

    /// Tier-1 instance called `proc_exit(0)`.
    pub const T1_EXIT_0: &str = "exit(0)";

    /// wasmi trapped inside the instance (OOB load, unreachable, …);
    /// the kernel logged the trap kind and faulted only that tenant.
    pub const T1_TRAP: &str = "runtime trap";

    /// `_start` returned without `proc_exit`: protocol violation the
    /// kernel treats as `exit(0)`.
    pub const T1_RETURNED: &str = "returned cleanly without proc_exit";

    /// The instance burned its whole per-step fuel budget without
    /// yielding (`wari_sched::TIER1_FUEL_PER_STEP`) and was faulted.
    pub const T1_OUT_OF_FUEL: &str = "out of fuel";

    /// Does any line of `text` say the scheduler faulted a Tier-1
    /// instance (`kernel/src/sched/mod.rs`: `[sched] Tier-1 instance
    /// proc_id=<n> faulted`)? This is the typed-rejection terminal
    /// state: a load failure or a trap ends here, never in a panic.
    ///
    /// ```
    /// use wari_security_tests::markers::sched_tier1_faulted;
    /// assert!(sched_tier1_faulted("[sched] Tier-1 instance proc_id=7 faulted\n"));
    /// assert!(!sched_tier1_faulted("[sched] Tier-1 instance proc_id=7 exited (code=0)"));
    /// ```
    pub fn sched_tier1_faulted(text: &str) -> bool {
        text.lines().any(|line| {
            line.starts_with("[sched] Tier-1 instance proc_id=") && line.ends_with(" faulted")
        })
    }

    /// Does any line of `text` read `[t1:<digits>] <event>…`?
    ///
    /// Pure string scan, no regex dependency. `event` is matched as a
    /// prefix of what follows the proc id, so `T1_TRAP` matches
    /// `[t1:3] runtime trap: MemoryOutOfBounds` as well.
    ///
    /// ```
    /// use wari_security_tests::markers::{tier1_event, T1_EXIT_0};
    /// assert!(tier1_event("boot\n[t1:12] exit(0)\n", T1_EXIT_0));
    /// assert!(!tier1_event("[hello] exit(0)", T1_EXIT_0)); // old format
    /// assert!(!tier1_event("[t1:] exit(0)", T1_EXIT_0));   // no proc id
    /// ```
    pub fn tier1_event(text: &str, event: &str) -> bool {
        text.lines().any(|line| {
            let Some(rest) = line.strip_prefix(TIER1_PREFIX) else {
                return false;
            };
            let id_len = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            if id_len == 0 {
                return false;
            }
            rest[id_len..]
                .strip_prefix("] ")
                .is_some_and(|tail| tail.starts_with(event))
        })
    }

    /// Printed by the kernel boot banner. Always expected — its
    /// absence indicates the kernel did not even boot.
    pub const BOOT_BANNER: &str = "Wari v0 build";

    /// Printed by the kernel once the Tier-2 net driver's smoltcp
    /// interface is up (`runtime::run_tier2_net`). Absent if VirtIO
    /// init failed — including when the driver's INV-25 self-check
    /// refused to proceed (`dma_escape.rs`).
    pub const NET_UP: &str = "[net] smoltcp interface up";
}
