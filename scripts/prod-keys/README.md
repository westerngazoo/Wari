# Wari production signing key — public half only

- `wari-prod.ed25519.pub` — 32-byte raw ed25519 public key. A kernel
  built without `--features dev-keys` embeds it
  (`kernel/src/runtime/sign.rs`) and accepts no other.

The secret half is **never** stored in this repository. Its holder
keeps it outside every checkout and points the build at it:

```bash
WARI_SIGN_KEY=/path/to/wari-prod.ed25519.sec scripts/build.sh release
```

(default path: `~/.config/wari/wari-prod.ed25519.sec`).

## Replacing the key

```bash
cargo run --manifest-path scripts/Cargo.toml --bin gen-keypair -- ~/.config/wari/wari-prod.ed25519
```

```bash
cp ~/.config/wari/wari-prod.ed25519.pub scripts/prod-keys/
```

Commit only the `.pub`. Every board must then be reflashed with a
kernel built after the change: an older kernel rejects blobs signed
with the new key, and the reverse.

Model and rationale: `docs/security-model.md`, "Signing keys".
