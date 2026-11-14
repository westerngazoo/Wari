# Wari dev keypair — **NOT FOR PRODUCTION**

> **The secret key in this directory is public.** It is accepted only
> by a kernel built with `--features dev-keys`, which announces it in
> the boot banner. A default (production) kernel rejects everything
> signed with it.

## What lives here

- `wari-dev.ed25519.sec` — 32-byte raw ed25519 secret key.
- `wari-dev.ed25519.pub` — 32-byte raw ed25519 public key, embedded by
  `kernel/src/runtime/sign.rs` when the `dev-keys` feature is on.

## Why it is committed

So QEMU, the test suites and a contributor's own board build and boot
from a clean checkout with no secret to obtain. The `qemu` profile of
`scripts/build.sh`, its `--dev-keys` option and every `make` target
sign with this pair.

## Why it is not rotated

The secret has been public since the commit that introduced it, and
any replacement stored here would be public too. Safety comes from the
production kernel not trusting this key, not from the key being fresh.

The production key and the full model: `scripts/prod-keys/README.md`
and `docs/security-model.md`, "Signing keys".
