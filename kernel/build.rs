// SPDX-License-Identifier: AGPL-3.0-only
//! Kernel build script.
//!
//! Emits `-T <absolute>/linker.ld` so the kernel links correctly whether
//! cargo is invoked from the workspace root (`cargo build -p wari-kernel`)
//! or from the crate directory (`cd kernel && cargo build`). The existing
//! rustflags entry in `.cargo/config.toml` passed `-Tlinker.ld` as a
//! bare relative path, which resolves against cargo's CWD — that works
//! from the crate dir but not from the workspace root.

/// Build script entry — emits the linker-script path.
#[allow(clippy::expect_used)] // build script: cargo guarantees CARGO_MANIFEST_DIR
fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("cargo always sets CARGO_MANIFEST_DIR for build scripts");
    println!("cargo:rustc-link-arg=-T{}/linker.ld", dir);
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-changed=src/boot.S");
}
