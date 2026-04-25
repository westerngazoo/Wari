// SPDX-License-Identifier: AGPL-3.0-only
//! Host-side signer for Tier-2 envelopes.
//!
//! Reads `<input>.wasm`, signs it with the secret key from
//! `scripts/dev-keys/wari-dev.ed25519.sec`, and writes the
//! 96-byte-header signed envelope to `<output>.signed.wasm`.
//!
//! Envelope layout (matches `kernel/src/runtime/sign.rs`):
//!
//! ```text
//! offset  length  field
//! 0       32      ed25519 public key
//! 32      64      ed25519 signature over the trailing wasm_bytes
//! 96      ..      raw .wasm bytes
//! ```
//!
//! Usage:
//!
//! ```text
//! cargo run --manifest-path scripts/Cargo.toml --bin sign-module -- \
//!     <input.wasm> <output.signed.wasm>
//! ```

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use ed25519_dalek::{Signer, SigningKey};

const SECRET_PATH: &str = "scripts/dev-keys/wari-dev.ed25519.sec";
const PUBKEY_PATH: &str = "scripts/dev-keys/wari-dev.ed25519.pub";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: sign-module <input.wasm> <output.signed.wasm>");
        return ExitCode::from(2);
    }
    let input = PathBuf::from(&args[1]);
    let output = PathBuf::from(&args[2]);

    let wasm_bytes = match fs::read(&input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("read {}: {}", input.display(), e);
            return ExitCode::from(1);
        }
    };

    let secret_bytes = match fs::read(SECRET_PATH) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("read {}: {}", SECRET_PATH, e);
            return ExitCode::from(1);
        }
    };
    if secret_bytes.len() != 32 {
        eprintln!(
            "{} must be exactly 32 raw bytes, got {}",
            SECRET_PATH,
            secret_bytes.len()
        );
        return ExitCode::from(1);
    }
    let secret_array: [u8; 32] = secret_bytes
        .as_slice()
        .try_into()
        .expect("checked length above");
    let signing_key = SigningKey::from_bytes(&secret_array);

    let pubkey_bytes = signing_key.verifying_key().to_bytes();

    // Sanity-check the pubkey on disk matches the secret-derived one.
    if let Ok(on_disk) = fs::read(PUBKEY_PATH) {
        if on_disk.len() == 32 && on_disk[..] != pubkey_bytes[..] {
            eprintln!(
                "warning: {} does not match the pubkey derived from the secret",
                PUBKEY_PATH
            );
        }
    }

    let signature = signing_key.sign(&wasm_bytes);

    let mut envelope = Vec::with_capacity(96 + wasm_bytes.len());
    envelope.extend_from_slice(&pubkey_bytes);
    envelope.extend_from_slice(&signature.to_bytes());
    envelope.extend_from_slice(&wasm_bytes);

    if let Err(e) = fs::write(&output, &envelope) {
        eprintln!("write {}: {}", output.display(), e);
        return ExitCode::from(1);
    }

    println!(
        "signed: {} bytes wasm, envelope {} bytes -> {}",
        wasm_bytes.len(),
        envelope.len(),
        output.display()
    );
    ExitCode::SUCCESS
}
