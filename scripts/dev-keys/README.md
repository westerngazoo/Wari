# Wari dev keypair — **NOT FOR PRODUCTION**

> **This keypair is committed in-tree for reproducible Phase-0 dev
> builds (CLAUDE R8). Do not use it on any system that processes
> real workloads.**

## What lives here

- `wari-dev.ed25519.sec` — 32-byte raw ed25519 secret key. Used by
  `scripts/sign-module.rs` to sign Tier-2 driver blobs.
- `wari-dev.ed25519.pub` — 32-byte raw ed25519 public key. Pasted into
  `ACCEPTED_PUBKEY` in `kernel/src/runtime/sign.rs`.

## Why it's committed

R8 says "reproducible builds." A committed dev keypair lets any
contributor (or CI) regenerate `build/drivers/uart.signed.wasm`
bit-for-bit from a clean checkout. Production deployments end the
dev-key era — Phase 1 introduces a signing pipeline whose private key
never enters the repo (offline signer, HSM, or hardware token, decided
at the time).

## Threat model — why this is acceptable in Phase 0

- The keypair is dev-only; production VF2/cloud images use a
  Phase-1+ key the public never sees.
- The committed pubkey lets anyone verify the *provenance* of a
  shipped Phase-0 driver blob; the committed seckey is irrelevant to
  production trust because production refuses any Phase-0 dev pubkey.
- A leaked dev seckey forges only Phase-0 dev blobs, which never run
  on a production-keyed kernel.

## Bootstrapping (one-time, parent agent)

```bash
cargo run --manifest-path scripts/Cargo.toml --bin gen-keypair -- \
    scripts/dev-keys/wari-dev.ed25519
```

Then paste the 32 bytes from `wari-dev.ed25519.pub` into
`ACCEPTED_PUBKEY` in `kernel/src/runtime/sign.rs`.

## Phase-1 retirement

When the signing pipeline lands (PR-series following the capability
system), this directory becomes either:
  - empty (production builds use an external signer); or
  - holds only the **public** half of the production key, with the
    secret kept offline.

Either way, the secret key disappears from the repo. The dev-key
README is then rewritten to reflect the new flow.
