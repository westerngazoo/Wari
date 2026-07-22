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
#[allow(clippy::expect_used)] // build script: cargo guarantees CARGO_MANIFEST_DIR
fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("cargo always sets CARGO_MANIFEST_DIR for build scripts");
    let script = if std::env::var("CARGO_FEATURE_VF2").is_ok() {
        "linker-vf2.ld"
    } else {
        "linker.ld"
    };
    println!("cargo:rustc-link-arg=-T{}/{}", dir, script);
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-changed=linker-vf2.ld");
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
    let blob = if std::env::var("CARGO_FEATURE_VF2").is_ok() {
        format!("{}/../build/drivers/net-vf2.signed.wasm", kernel_dir)
    } else {
        format!("{}/../build/drivers/net-qemu.signed.wasm", kernel_dir)
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
