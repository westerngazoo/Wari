// SPDX-License-Identifier: AGPL-3.0-only
//! Host-side ed25519 keypair generator.
//!
//! Writes the 32-byte raw secret key to `<prefix>.sec` and the 32-byte
//! raw public key to `<prefix>.pub`. Run once when bootstrapping the
//! Phase-0 dev keypair.
//!
//! Usage:
//!
//! ```text
//! cargo run --manifest-path scripts/Cargo.toml --bin gen-keypair -- \
//!     scripts/dev-keys/wari-dev.ed25519
//! ```
//!
//! The output files are committed in-tree — see
//! `scripts/dev-keys/README.md` for the NOT-FOR-PRODUCTION rationale.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: gen-keypair <prefix>");
        eprintln!("  writes <prefix>.sec (32 bytes) and <prefix>.pub (32 bytes)");
        return ExitCode::from(2);
    }
    let prefix = PathBuf::from(&args[1]);

    let mut csprng = OsRng;
    let signing_key = SigningKey::generate(&mut csprng);
    let verifying_key = signing_key.verifying_key();

    let sec_path = with_ext(&prefix, "sec");
    let pub_path = with_ext(&prefix, "pub");

    if let Err(e) = fs::write(&sec_path, signing_key.to_bytes()) {
        eprintln!("write {}: {}", sec_path.display(), e);
        return ExitCode::from(1);
    }
    if let Err(e) = fs::write(&pub_path, verifying_key.to_bytes()) {
        eprintln!("write {}: {}", pub_path.display(), e);
        return ExitCode::from(1);
    }

    println!("generated:");
    println!("  secret -> {}", sec_path.display());
    println!("  public -> {}", pub_path.display());
    println!();
    println!(
        "next: paste the 32 bytes of {} into ACCEPTED_PUBKEY in",
        pub_path.display()
    );
    println!("       kernel/src/runtime/sign.rs");
    ExitCode::SUCCESS
}

fn with_ext(prefix: &PathBuf, ext: &str) -> PathBuf {
    let mut s = prefix.as_os_str().to_owned();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}
