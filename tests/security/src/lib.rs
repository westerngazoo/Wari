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
use std::path::{Path, PathBuf};
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
    boot_capture(&default_kernel(), wallclock, &[])
}

/// [`boot_kernel_capture`] for a kernel ELF other than the one
/// `make build` produces — a test that needs its own kernel variant
/// builds it elsewhere and boots it through here.
///
/// # Panics
///
/// If `kernel` does not exist or QEMU cannot be spawned.
pub fn boot_kernel_capture_at(kernel: &Path, wallclock: Duration) -> String {
    boot_capture(kernel, wallclock, &[])
}

fn default_kernel() -> PathBuf {
    workspace_root().join(KERNEL_ELF_REL)
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
    boot_capture(&default_kernel(), wallclock, NET_DEVICE_ARGS)
}

/// Boot the kernel with a VirtIO-net NIC whose traffic is recorded,
/// poke the guest from the host so traffic exists, and return the
/// UART text together with the capture (pcap bytes).
///
/// QEMU forwards a free host TCP port to the demo's port in the
/// guest and dumps every frame on the virtual wire to a file
/// (`-object filter-dump`). `after`, into the run, a host-side TCP
/// connect to that port makes QEMU's user-mode stack ARP for the
/// guest: a frame the guest must **receive** and answer, i.e. one
/// trip through its RX DMA path and one through its TX DMA path.
///
/// # Panics
///
/// If the kernel ELF is missing, QEMU cannot be spawned, or no host
/// port can be reserved.
pub fn boot_kernel_capture_wire(wallclock: Duration, after: Duration) -> (String, Vec<u8>) {
    // Reserve a free port, then release it for QEMU to bind. Another
    // process could take it in between; the run would then fail
    // loudly (QEMU exits), not pass falsely.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("no free host port")
        .port();
    let pcap = std::env::temp_dir().join(format!("wari-wire-{}-{port}.pcap", std::process::id()));
    let _ = std::fs::remove_file(&pcap);

    let netdev =
        format!("user,id=net0,net=192.168.50.0/24,host=192.168.50.2,hostfwd=tcp:127.0.0.1:{port}-192.168.50.10:7000");
    let dump = format!("filter-dump,id=f0,netdev=net0,file={}", pcap.display());
    let args = [
        "-global",
        "virtio-mmio.force-legacy=false",
        "-netdev",
        &netdev,
        "-device",
        "virtio-net-device,netdev=net0",
        "-object",
        &dump,
    ];

    let poke = thread::spawn(move || {
        thread::sleep(after);
        // The connect itself may time out (the guest's TCP answer is
        // not what this harness measures); the ARP exchange it
        // triggers is.
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let _ = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2));
    });
    let text = boot_capture(&default_kernel(), wallclock, &args);
    let _ = poke.join();

    let wire = std::fs::read(&pcap).unwrap_or_default();
    let _ = std::fs::remove_file(&pcap);
    (text, wire)
}

/// Ethernet frames of a classic pcap capture (the format QEMU's
/// `filter-dump` writes), in order. Truncated or foreign input yields
/// the frames read so far, never a panic.
///
/// ```
/// use wari_security_tests::pcap_frames;
/// // 24-byte global header (little-endian magic), then one record
/// // with a 4-byte frame.
/// let mut cap = vec![0xd4, 0xc3, 0xb2, 0xa1];
/// cap.extend_from_slice(&[0; 20]);
/// cap.extend_from_slice(&[0; 8]);                 // timestamp
/// cap.extend_from_slice(&4u32.to_le_bytes());     // captured length
/// cap.extend_from_slice(&4u32.to_le_bytes());     // original length
/// cap.extend_from_slice(&[1, 2, 3, 4]);
/// assert_eq!(pcap_frames(&cap), vec![&[1u8, 2, 3, 4][..]]);
/// assert!(pcap_frames(&[0; 10]).is_empty());      // not a capture
/// ```
pub fn pcap_frames(cap: &[u8]) -> Vec<&[u8]> {
    const GLOBAL_HEADER: usize = 24;
    const RECORD_HEADER: usize = 16;
    let mut frames = Vec::new();
    let Some(magic) = cap.get(0..4) else {
        return frames;
    };
    let little_endian = match magic {
        [0xd4, 0xc3, 0xb2, 0xa1] => true,
        [0xa1, 0xb2, 0xc3, 0xd4] => false,
        _ => return frames,
    };
    let mut at = GLOBAL_HEADER;
    while let Some(len_bytes) = cap.get(at + 8..at + 12) {
        let raw = [len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]];
        let len = if little_endian {
            u32::from_le_bytes(raw)
        } else {
            u32::from_be_bytes(raw)
        } as usize;
        let start = at + RECORD_HEADER;
        let Some(frame) = cap.get(start..start + len) else {
            break;
        };
        frames.push(frame);
        at = start + len;
    }
    frames
}

fn boot_capture(kernel: &Path, wallclock: Duration, extra_args: &[&str]) -> String {
    assert!(
        kernel.exists(),
        "kernel ELF not found at {:?}. Run `make build` first.",
        kernel,
    );

    let mut child = Command::new("qemu-system-riscv64")
        .args(["-machine", "virt", "-nographic", "-bios", "default"])
        .args(extra_args)
        .arg("-kernel")
        .arg(kernel)
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
    /// `kernel/src/umode.rs`: the well-behaved U-mode stub made its
    /// `ecall`, got the right answer back and exited 0 (ADR-004 step 1).
    pub const UMODE_STUB_OK: &str = "[umode] stub: ecall 0 returned, exit 0";

    /// `kernel/src/umode.rs`: the hostile stub's read of a kernel
    /// address faulted in U-mode and only the stub was ended.
    pub const UMODE_HOSTILE_KILLED: &str = "[umode] hostile stub killed: load page fault";

    /// `kernel/src/umode.rs`: process A's read of a VA that holds
    /// process B's page faulted (B itself read it fine), and B's page
    /// kept its contents (ADR-004 step 2, INV-28).
    pub const UMODE_CROSS_SPACE_KILLED: &str = "[umode] cross-space read killed: load page fault";

    /// `kernel/src/umode.rs`: after destroying every test address
    /// space the page allocator has exactly as many free pages as
    /// before the self-test.
    pub const UMODE_SPACES_FREED: &str = "[umode] address spaces destroyed, every page returned";

    /// `kernel/src/umode.rs`: printed after every stub, from S-mode.
    pub const UMODE_KERNEL_CONTINUES: &str = "[umode] kernel continues";

    /// Any self-test outcome other than the expected one.
    pub const UMODE_FAILED: &str = "FAILED, got";

    /// Printed by the kernel after `runtime::run_tier2_uart` succeeds.
    /// If absent, signature verification or driver instantiation
    /// failed — the kernel halted in `kmain`'s wfi loop.
    pub const TIER2_LOADED: &str = "tier-2 uart driver loaded";

    /// Printed by `kmain` when the Tier-2 UART driver is refused
    /// (signature, manifest or instantiation), followed by the typed
    /// error; the kernel then idles in `wfi`.
    pub const TIER2_UART_REFUSED: &str = "tier-2 uart load failed";

    /// Printed by `apps/sockleak` once it holds every free socket,
    /// right before it ends its life (prefix shared by the instance
    /// that traps and the one that revokes its Net cap).
    pub const SOCKLEAK_HOLDING: &str = "[sockleak] holding every free socket";

    /// Printed by `apps/sockleak` when the driver had no socket left
    /// for it: an earlier dead tenant still owns them.
    pub const SOCKLEAK_NONE_LEFT: &str = "[sockleak] no socket available";

    /// Tail of the scheduler's line after it closes the sockets a
    /// terminated tenant left open (`kernel/src/sched/mod.rs`:
    /// `[sched] proc_id=<n> left <k> socket(s) open, closed`).
    pub const SOCKETS_CLOSED_AT_EXIT: &str = "socket(s) open, closed";

    /// `apps/sockleak`: after `cap_delete` of every Socket cap, all
    /// the sockets were available again.
    pub const SOCKLEAK_DELETE_OK: &str = "[sockleak] cap_delete: sockets came back";

    /// `apps/sockleak`: with a `cap_copy` of a Socket cap alive, the
    /// driver did not hand that socket out again.
    pub const SOCKLEAK_COPY_OK: &str = "[sockleak] cap_copy: the copy keeps its socket";

    /// `apps/sockleak`: `cap_delete` freed the caps but not the
    /// driver sockets.
    pub const SOCKLEAK_DELETE_LEAK: &str = "cap_delete: LEAK";

    /// `apps/sockleak`: a socket still named by a cap was handed out
    /// again — two caps, possibly of two tenants, on one socket.
    pub const SOCKLEAK_COPY_ALIAS: &str = "cap_copy: ALIAS";

    /// `apps/sockleak`: after `cap_revoke` of a Socket cap with a
    /// minted child, all the sockets were available again.
    pub const SOCKLEAK_REVOKE_OK: &str = "[sockleak] cap_revoke: sockets came back";

    /// `apps/sockleak`: `cap_revoke` freed the caps but not the
    /// driver socket.
    pub const SOCKLEAK_REVOKE_LEAK: &str = "cap_revoke: LEAK";

    /// Tail of the kernel's line after a `cap_revoke` took the last
    /// cap of some sockets and the kernel closed them
    /// (`kernel/src/cap/syscall.rs`: `[cap] proc_id=<n> revoke left
    /// <k> socket(s) without a cap, closed`).
    pub const SOCKETS_CLOSED_AT_REVOKE: &str = "socket(s) without a cap, closed";

    /// Banner line of a kernel built with `--features dev-keys`: it
    /// trusts the in-tree dev key (`kernel/src/boot.rs`).
    pub const DEV_KEYS_BANNER: &str = "DEV SIGNING KEY ACCEPTED";

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
