// SPDX-License-Identifier: AGPL-3.0-only
//! Tier-2 signed-envelope verifier (INV-13).
//!
//! The kernel carries one compiled-in ed25519 public key
//! (`ACCEPTED_PUBKEY`), selected at build time: the production key by
//! default, the in-tree dev key under `--features dev-keys`. A signed
//! module is delivered as an envelope; this module verifies the
//! signature before the loader hands the bytes to wasmi.
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

use ed25519_dalek::{Signature, VerifyingKey};

use crate::error::KernelError;

/// Length of the public-key field in the envelope.
pub const PUBKEY_LEN: usize = 32;

/// Length of the signature field in the envelope.
pub const SIGNATURE_LEN: usize = 64;

/// Total fixed header length.
pub const HEADER_LEN: usize = PUBKEY_LEN + SIGNATURE_LEN;

/// Compiled-in accepted public key — the trust root of INV-13.
///
/// Exactly one key is accepted per kernel image, picked at build time:
///
/// - default: the **production** key. Only its public half is in the
///   repo (`scripts/prod-keys/`); the secret is held outside it by
///   the architect and never enters git.
/// - `--features dev-keys`: the **dev** key, whose secret half is
///   committed (`scripts/dev-keys/`) and therefore public. Such an
///   image announces itself at boot ([`DEV_KEYS`]).
///
/// The two are exclusive rather than additive so that a production
/// image rejects every dev-signed envelope (`KernelError::BadWasm`),
/// which is the safe failure mode. The bytes come straight from the
/// key file — `include_bytes!` of anything but 32 bytes fails to
/// compile — so the file and the kernel cannot drift. Key model:
/// `docs/security-model.md`, "Signing keys".
#[cfg(not(feature = "dev-keys"))]
pub const ACCEPTED_PUBKEY: [u8; PUBKEY_LEN] =
    *include_bytes!("../../../scripts/prod-keys/wari-prod.ed25519.pub");
#[cfg(feature = "dev-keys")]
pub const ACCEPTED_PUBKEY: [u8; PUBKEY_LEN] =
    *include_bytes!("../../../scripts/dev-keys/wari-dev.ed25519.pub");

/// Whether this image trusts the public dev key (see
/// [`ACCEPTED_PUBKEY`]). Read by the boot banner.
pub const DEV_KEYS: bool = cfg!(feature = "dev-keys");

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
///     point/scalar, or the signature is not in canonical form.
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

    // `verify_strict`, not `verify`: it also rejects small-order keys
    // and non-canonical signature encodings, so one message has one
    // accepted signature (dalek docs; Chalkias et al., "Taming the
    // many EdDSAs", 2020).
    verifying_key
        .verify_strict(wasm_bytes, &signature)
        .map_err(|_| KernelError::BadWasm)?;

    Ok(wasm_bytes)
}
