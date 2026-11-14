// SPDX-License-Identifier: AGPL-3.0-only
//! Adversarial: a production-keyed kernel refuses dev-signed modules.
//!
//! ## What this test verifies
//!
//! The dev signing key's secret half is committed
//! (`scripts/dev-keys/`), so anyone can produce an envelope signed
//! with it. INV-13 is only worth something if a kernel built without
//! `--features dev-keys` rejects such an envelope. This is the
//! attacker's move made literally: take the drivers `make build`
//! signed with the dev key, embed them in a kernel that trusts the
//! production key, and boot it.
//!
//! Expected: `sign::verify` returns `BadWasm` for the first Tier-2
//! load, `kmain` reports it and idles. No driver runs, nothing panics.
//!
//! ## Why the test builds its own kernel
//!
//! Every other test boots the dev-keyed kernel from `make build`.
//! This one needs the production trust root, so it compiles a second
//! kernel into its own target directory (`target/prod-key/`), leaving
//! the shared ELF untouched for the tests that run after it.

use std::process::Command;

use wari_security_tests::{
    boot_kernel_capture, boot_kernel_capture_at, markers, workspace_root, DEFAULT_WALLCLOCK,
};

#[test]
fn production_kernel_rejects_dev_signed_drivers() {
    let root = workspace_root();

    // Precondition: the staged driver really is dev-signed (envelope
    // bytes 0..32 are the signer's public key). `make build` signs it
    // so; after a production `scripts/build.sh` it would not be, and
    // the refusal below would prove nothing about the dev key.
    let envelope = std::fs::read(root.join("build/drivers/uart-qemu.signed.wasm"))
        .expect("staged UART driver missing: run `make build` first");
    let dev_pubkey = std::fs::read(root.join("scripts/dev-keys/wari-dev.ed25519.pub"))
        .expect("dev public key missing");
    assert!(
        envelope.starts_with(&dev_pubkey),
        "staged driver is not dev-signed: run `make build` first",
    );

    let target_dir = root.join("target/prod-key");
    let built = Command::new("cargo")
        .current_dir(root.join("kernel"))
        .args(["build", "--release", "--no-default-features"])
        .args(["--features", "qemu"])
        .arg("--target-dir")
        .arg(&target_dir)
        .status()
        .expect("failed to spawn cargo");
    assert!(built.success(), "production-keyed kernel did not build");

    let kernel = target_dir.join("riscv64gc-unknown-none-elf/release/wari");
    let text = boot_kernel_capture_at(&kernel, DEFAULT_WALLCLOCK);

    assert!(
        text.contains(markers::BOOT_BANNER),
        "kernel did not boot:\n{text}",
    );
    assert!(
        !text.contains(markers::DEV_KEYS_BANNER),
        "kernel built without dev-keys still announces the dev key:\n{text}",
    );
    assert!(
        text.contains(markers::TIER2_UART_REFUSED),
        "dev-signed driver was not refused:\n{text}",
    );
    assert!(
        !text.contains(markers::TIER2_LOADED),
        "a dev-signed driver loaded on a production-keyed kernel:\n{text}",
    );
    assert!(!text.contains("panicked at"), "rust panic:\n{text}");
}

/// Control: the same envelopes are accepted by the kernel they were
/// signed for, and that kernel says which key it trusts.
#[test]
fn dev_kernel_accepts_dev_signed_drivers_and_says_so() {
    let text = boot_kernel_capture(DEFAULT_WALLCLOCK);

    assert!(
        text.contains(markers::DEV_KEYS_BANNER),
        "dev-keyed kernel does not announce the dev key:\n{text}",
    );
    assert!(
        text.contains(markers::TIER2_LOADED),
        "dev-signed driver refused by the dev-keyed kernel:\n{text}",
    );
}
