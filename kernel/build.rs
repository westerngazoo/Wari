// SPDX-License-Identifier: AGPL-3.0-only
//! Kernel build script.
//!
//! Emits `-T <absolute>/linker.ld` so the kernel links correctly whether
//! cargo is invoked from the workspace root (`cargo build -p wari-kernel`)
//! or from the crate directory (`cd kernel && cargo build`). The existing
//! rustflags entry in `.cargo/config.toml` passed `-Tlinker.ld` as a
//! bare relative path, which resolves against cargo's CWD — that works
//! from the crate dir but not from the workspace root.

/// Build script entry — emits the platform-appropriate linker-script path.
///
/// Picks `linker-vf2.ld` when the `vf2` feature is active, otherwise
/// `linker.ld`. Resolved as an absolute path so cargo invocations from
/// the workspace root and from `kernel/` both link correctly.
/// The platform this kernel image is being built for.
enum Platform {
    Qemu,
    Vf2,
    /// Orange Pi R2S (Ky X1). Net-less first-light — no signed driver
    /// blob is embedded, so the stale-driver guard is skipped for it.
    R2s,
}

/// Resolve the platform from cargo's feature env vars, refusing to
/// guess.
///
/// This used to be `if vf2 { .. } else { .. }` in two places. A build
/// with no platform feature therefore silently produced a QEMU kernel:
/// linked at QEMU's `ORIGIN` instead of the board's DRAM base, and
/// embedding the VirtIO net driver. Neither failure prints anything —
/// the first never reaches the console, and the second passes the
/// stale-driver build-tag guard because the *tag* matches, which is
/// exactly the builds-107..114 failure re-entering through the
/// platform axis rather than the staleness axis.
///
/// Panicking here costs one confusing build and saves a silent one.
fn platform() -> Platform {
    let qemu = std::env::var("CARGO_FEATURE_QEMU").is_ok();
    let vf2 = std::env::var("CARGO_FEATURE_VF2").is_ok();
    let r2s = std::env::var("CARGO_FEATURE_R2S").is_ok();
    match (qemu, vf2, r2s) {
        (true, false, false) => Platform::Qemu,
        (false, true, false) => Platform::Vf2,
        (false, false, true) => Platform::R2s,
        (false, false, false) => panic!(
            "no platform feature: build with --features qemu, vf2, or r2s. \
             Guessing here would link the wrong address and embed the wrong \
             driver blob, and neither failure prints anything."
        ),
        _ => panic!("qemu, vf2, and r2s are mutually exclusive"),
    }
}

#[allow(clippy::expect_used)] // build script: cargo guarantees CARGO_MANIFEST_DIR
fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("cargo always sets CARGO_MANIFEST_DIR for build scripts");
    let script = match platform() {
        Platform::Vf2 => "linker-vf2.ld",
        Platform::Qemu => "linker.ld",
        Platform::R2s => "linker-r2s.ld",
    };
    println!("cargo:rustc-link-arg=-T{}/{}", dir, script);
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-changed=linker-vf2.ld");
    println!("cargo:rerun-if-changed=linker-r2s.ld");
    println!("cargo:rerun-if-changed=src/boot.S");
    // CRITICAL: without this, cargo's incremental build cache does
    // NOT detect WARI_BUILD changes, and the kernel binary embeds
    // a stale build number forever. Bumping .build_number then
    // running `cargo build` is a silent no-op without this line.
    // Diagnosed May 2026 after VF2 stayed at "build 19" across ~10
    // deploys despite local + origin showing later numbers.
    println!("cargo:rerun-if-env-changed=WARI_BUILD");

    // ── Stale-driver guard ────────────────────────────────────────
    //
    // The kernel `include_bytes!`s a signed Tier-2 net-driver wasm
    // (`build/drivers/net-{qemu,vf2}.signed.wasm`). If you bypass
    // `make kernel-vf2` and run `cd kernel && cargo build` after
    // editing driver source, cargo will happily embed the last-
    // known-good driver blob — which may be many builds stale.
    //
    // Builds 107..114 hit exactly this trap: a RISC-V `fence ow,ow`
    // I added to driver code broke the wasm32 build, and cargo
    // silently reused the build-106 artifact while the kernel
    // banner read "build 114". Every diagnostic we added to the
    // driver during that window was a no-op because the kernel
    // wasn't running our updated code.
    //
    // Guard: grep the embedded signed wasm for the build tag the
    // driver's own build.rs embedded, compare to our WARI_BUILD,
    // fail loud if mismatched.
    check_driver_blob_freshness(&dir);
}

/// Greps the platform-appropriate signed driver wasm for its
/// embedded `WARI-DRV-BUILD-TAG-N` rodata string and asserts that
/// `N == WARI_BUILD`. On mismatch, emits a `cargo::error` that
/// stops the build with a clear remediation.
///
/// No-ops when `WARI_BUILD` is unset (e.g. `cargo check` from
/// rust-analyzer in the IDE) — we'd rather not gate IDE flows on
/// having a fully-staged signed blob.
fn check_driver_blob_freshness(kernel_dir: &str) {
    let Ok(want) = std::env::var("WARI_BUILD") else {
        return;
    };
    let plat = platform();
    if matches!(plat, Platform::R2s) {
        // R2S first-light is net-less — no signed driver blob is
        // embedded (net_blob/uart_blob resolve to empty slices), so
        // there is nothing to check for staleness.
        return;
    }
    let blob = match plat {
        Platform::Vf2 => format!("{}/../build/drivers/net-vf2.signed.wasm", kernel_dir),
        Platform::Qemu => format!("{}/../build/drivers/net-qemu.signed.wasm", kernel_dir),
        Platform::R2s => unreachable!("handled by the early return above"),
    };
    let bytes = match std::fs::read(&blob) {
        Ok(b) => b,
        Err(e) => {
            println!(
                "cargo::error=stale-driver-guard: cannot read {} ({}). \
                 Run `make kernel-vf2` (or `make build`) — never `cd kernel && cargo build` alone.",
                blob, e
            );
            return;
        }
    };
    // Embedded tag format: literal ASCII "WARI-DRV-BUILD-TAG-N".
    let needle = b"WARI-DRV-BUILD-TAG-";
    let pos = bytes.windows(needle.len()).position(|w| w == needle);
    let Some(pos) = pos else {
        println!(
            "cargo::error=stale-driver-guard: {} contains no WARI-DRV-BUILD-TAG. \
             Driver wasm pre-dates the build-tag harness — rebuild with `make kernel-vf2`.",
            blob
        );
        return;
    };
    let tail = &bytes[pos + needle.len()..];
    let n_end = tail
        .iter()
        .position(|c| !c.is_ascii_digit())
        .unwrap_or(tail.len());
    let got = std::str::from_utf8(&tail[..n_end]).unwrap_or("?");
    if got != want {
        println!(
            "cargo::error=stale-driver-guard: embedded driver build {} != WARI_BUILD {}. \
             The signed driver wasm at {} is stale. Run `make kernel-vf2` \
             — that rebuilds drivers/net to wasm32 BEFORE linking the kernel. \
             Never run `cd kernel && cargo build` directly; cargo will happily \
             reuse the last-known-good blob and the bug we're trying to fix \
             will never reach silicon. (Diagnosed build 115, May 2026.)",
            got, want, blob
        );
    }
    println!("cargo:rerun-if-changed={}", blob);
}
