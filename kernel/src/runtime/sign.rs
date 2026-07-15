// SPDX-License-Identifier: AGPL-3.0-only
//! Tier-2 signed-envelope verifier (INV-13).
//!
//! Phase 0 ships a single compiled-in ed25519 public key
//! (`ACCEPTED_PUBKEY`). A Tier-2 module is delivered as a signed
//! envelope; this module verifies the signature before the loader
//! hands the bytes to wasmi.
//!
//! ## Envelope layout (96-byte fixed header)
//!
//! ```text
//! offset  length  field
//! 0       32      ed25519 public key (must equal ACCEPTED_PUBKEY)
//! 32      64      ed25519 signature over the trailing wasm_bytes
//! 96      ..      raw .wasm bytes (passed to wasmi on success)
//! ```
//!
//! ## Why a 96-byte header (Why/How depth)
//!
//! Picked: pubkey || signature || wasm_bytes in one contiguous file.
//! Considered:
//!   - detached `.sig` next to the `.wasm` → rejected: two
//!     `include_bytes!` sites + two paths to keep in sync;
//!   - PKCS #7 / CMS envelope → rejected: pulls a parser + ASN.1 dep
//!     into Tier 0, breaking R5/R8 minimal-trust-base discipline;
//!   - DER-encoded SignatureBundle → same objection.
//! Why this won: smallest verifier (~30 LOC of logic), no parser,
//! self-contained in `include_bytes!`. Cost accepted: a non-standard
//! envelope, documented in this docstring; trivially translated to a
//! standard format in Phase 1's signing pipeline if needed.
//!
//! ## Why ed25519-dalek (Why/How depth)
//!
//! Picked: `ed25519-dalek = "2"` with `default-features = false`,
//! features = `["ed25519"]`. Considered:
//!   - hand-rolled ed25519 → rejected on R5 + R7 grounds (never roll
//!     crypto); also outside Phase-0 scope;
//!   - `ring` → rejected: pulls in a C/asm trust base larger than the
//!     entire Tier-0 kernel;
//!   - `ed25519-compact` → smaller but less audited than dalek.
//! Why this won: dalek 2.x compiles `no_std` cleanly with the right
//! feature set; the verify path is small and well-reviewed. Cost
//! accepted: dalek is a non-trivial dep — Phase 0 audit (criterion 9)
//! must include it explicitly.

#![allow(dead_code)]
#![allow(clippy::doc_lazy_continuation)]

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use crate::error::KernelError;

/// Length of the public-key field in the envelope.
pub const PUBKEY_LEN: usize = 32;

/// Length of the signature field in the envelope.
pub const SIGNATURE_LEN: usize = 64;

/// Total fixed header length.
pub const HEADER_LEN: usize = PUBKEY_LEN + SIGNATURE_LEN;

/// Compiled-in accepted public key — **Phase-0 DEV KEY**.
///
/// These 32 bytes are the public half of the dev keypair committed at
/// `scripts/dev-keys/wari-dev.ed25519.{pub,sec}`. The matching secret
/// key is in-tree by design, so every contributor and CI can re-sign
/// Tier-2 driver blobs bit-for-bit from a clean checkout (R8). See
/// `scripts/dev-keys/README.md` for the full threat model and rationale.
///
/// **Before any production deploy** this constant MUST be replaced
/// with the pubkey of a Phase-1+ signing key whose secret never enters
/// the repo (offline signer / HSM / hardware token — pipeline TBD per
/// `dev-keys/README.md`). Driver blobs must then be re-signed with the
/// new key and re-flashed; mixing keys yields `KernelError::BadWasm`
/// on every Tier-2 load (INV-13), which is the safe failure mode.
///
/// Replacing only this constant without re-signing the drivers is
/// itself safe: every signed envelope will be rejected.
pub const ACCEPTED_PUBKEY: [u8; PUBKEY_LEN] = [
    0xf6, 0x09, 0xfc, 0x9a, 0xe4, 0x6f, 0xd9, 0x09, 0xbe, 0x0f, 0x6d, 0xf9, 0x78, 0x9d, 0xc4, 0x32,
    0xa1, 0xb8, 0x2c, 0x70, 0xf1, 0x56, 0x44, 0xbf, 0xf7, 0x94, 0x9d, 0xac, 0x12, 0x05, 0x70, 0x1a,
];

/// Verify a signed Tier-2 envelope.
///
/// On success returns a slice over the raw `.wasm` bytes (the suffix
/// after the 96-byte header) — the caller passes this slice to wasmi.
///
/// # Errors
///
/// Returns `KernelError::BadWasm` for any of:
///   - envelope shorter than `HEADER_LEN`,
///   - pubkey field does not equal `ACCEPTED_PUBKEY`,
///   - signature does not verify against the wasm-bytes suffix,
///   - pubkey or signature bytes are not a well-formed ed25519
///     point/scalar.
///
/// `BadWasm` collapses every failure mode at the syscall boundary; the
/// kernel does not need to distinguish them, and collapsing limits
/// information leakage about which check failed (R5 spirit).
///
/// # Invariants
///
/// INV-13: every Tier-2 instance reachable by the runtime has passed
/// this verification step in this boot.
pub fn verify(envelope: &[u8]) -> Result<&[u8], KernelError> {
    if envelope.len() < HEADER_LEN {
        return Err(KernelError::BadWasm);
    }

    let (header, wasm_bytes) = envelope.split_at(HEADER_LEN);
    let (pubkey_bytes, sig_bytes) = header.split_at(PUBKEY_LEN);

    // Pubkey must match the compiled-in trust root.
    if pubkey_bytes != ACCEPTED_PUBKEY {
        return Err(KernelError::BadWasm);
    }

    // Reconstruct typed key + signature from the raw bytes.
    let pubkey_array: [u8; PUBKEY_LEN] =
        pubkey_bytes.try_into().map_err(|_| KernelError::BadWasm)?;
    let sig_array: [u8; SIGNATURE_LEN] = sig_bytes.try_into().map_err(|_| KernelError::BadWasm)?;

    let verifying_key =
        VerifyingKey::from_bytes(&pubkey_array).map_err(|_| KernelError::BadWasm)?;
    let signature = Signature::from_bytes(&sig_array);

    verifying_key
        .verify(wasm_bytes, &signature)
        .map_err(|_| KernelError::BadWasm)?;

    Ok(wasm_bytes)
}
