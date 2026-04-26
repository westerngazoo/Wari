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
}
