// SPDX-License-Identifier: AGPL-3.0-only
//! Host-side signer for Tier-2 envelopes — with manifest
//! cross-check (PR DI-5).
//!
//! Reads `<input>.wasm`, asserts the embedded `wari_driver_manifest`
//! custom section is structurally well-formed AND agrees with the
//! WASM binary's actual export + import surface, then signs the
//! result with the dev key and writes the envelope to
//! `<output>.signed.wasm`.
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
//! ## Why pre-sign verification
//!
//! Without it: a driver author could ship a binary whose manifest
//! claims `kind = Uart, exports = [write, _start]` while the wasm
//! actually exports something else. The kernel verifies declared
//! exports DO resolve, but the kernel does NOT verify there are no
//! UNDECLARED exports (which could be invoked by accident by the
//! kernel via name) or undeclared imports (which the kernel must
//! still register on the linker, and which surface a different
//! cap need than the manifest implies).
//!
//! With it: the sign tool walks the WASM exports + imports via
//! `wasmparser`, looks up each one in the embedded manifest, and
//! refuses to sign on any mismatch. The kernel can then trust
//! that "what the manifest declares" is exactly "what the binary
//! exposes."
//!
//! Usage:
//!
//! ```text
//! cargo run --manifest-path scripts/Cargo.toml --bin sign-module -- \
//!     <input.wasm> <output.signed.wasm>
//! ```

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use ed25519_dalek::{Signer, SigningKey};
use wari_driver_iface::{
    parse::{self, trim_nul},
    FuncSig, WasmSigShape, WasmValType,
};
use wasmparser::{ExternalKind, Parser, Payload, TypeRef, ValType};

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

    // PR DI-5: refuse to sign a binary whose manifest is missing,
    // malformed, or disagrees with its actual WASM exports/imports.
    if let Err(e) = verify_manifest(&wasm_bytes) {
        eprintln!("sign-module: refusing to sign — {}", e);
        return ExitCode::from(1);
    }

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

// ── Manifest cross-check ─────────────────────────────────────────

/// Scan the WASM, parse the embedded manifest via the same
/// `driver-iface::parse` module the kernel uses, then walk the
/// WASM's actual exports + imports and assert they match the
/// manifest's declarations.
///
/// Returns `Ok(())` iff:
///   1. WASM parses cleanly
///   2. `wari_driver_manifest` section exists, magic + abi_version
///      OK
///   3. Every manifest-declared export is present in the WASM
///      with the declared signature
///   4. Every WASM-defined export is present in the manifest
///      (no undeclared surface)
///   5. Every manifest-declared import is requested by the WASM
///      with the declared signature
///   6. Every WASM-requested import is present in the manifest
///      (no undeclared host-fn need)
fn verify_manifest(wasm: &[u8]) -> Result<(), String> {
    let view = parse::parse_from_wasm(wasm).map_err(|e| {
        format!("manifest parse failed: {:?}", e)
    })?;
    let kind = view.kind().map_err(|e| format!("manifest kind: {:?}", e))?;
    let abi_version = view.abi_version();

    // Walk wasm: collect (name → FuncSig) for both directions.
    let WasmFuncSurface { exports, imports } =
        walk_wasm(wasm).map_err(|e| format!("wasm walk failed: {}", e))?;

    // Collect manifest declarations into BTreeMaps for symmetric
    // comparison. Names are NUL-trimmed; sigs decoded.
    let mut manifest_exports: BTreeMap<Vec<u8>, FuncSig> = BTreeMap::new();
    for e in view.exports {
        let name = trim_nul(&e.name).to_vec();
        let sig = FuncSig::from_raw(e.sig)
            .ok_or_else(|| format!("export {:?}: unknown sig {}", String::from_utf8_lossy(&name), e.sig))?;
        manifest_exports.insert(name, sig);
    }
    let mut manifest_imports: BTreeMap<(Vec<u8>, Vec<u8>), FuncSig> = BTreeMap::new();
    for im in view.imports {
        let module = trim_nul(&im.module).to_vec();
        let name = trim_nul(&im.name).to_vec();
        let sig = FuncSig::from_raw(im.sig).ok_or_else(|| {
            format!(
                "import {}::{}: unknown sig {}",
                String::from_utf8_lossy(&module),
                String::from_utf8_lossy(&name),
                im.sig
            )
        })?;
        manifest_imports.insert((module, name), sig);
    }

    // (3) every manifest export resolves in wasm with matching shape
    for (name, sig) in &manifest_exports {
        let manifest_shape = sig.wasm_shape();
        match exports.get(name) {
            None => {
                return Err(format!(
                    "manifest declares export {:?} but wasm does not export it",
                    String::from_utf8_lossy(name)
                ))
            }
            Some(actual) if *actual != manifest_shape => {
                return Err(format!(
                    "export {:?}: manifest claims {:?} (shape {:?}), wasm has {:?}",
                    String::from_utf8_lossy(name),
                    sig,
                    manifest_shape,
                    actual
                ))
            }
            _ => {}
        }
    }
    // (4) every wasm export is in the manifest
    for (name, _) in &exports {
        if !manifest_exports.contains_key(name) {
            return Err(format!(
                "wasm exports {:?} but manifest does not declare it",
                String::from_utf8_lossy(name)
            ));
        }
    }
    // (5) every manifest import is requested by wasm with matching shape
    for ((module, name), sig) in &manifest_imports {
        let manifest_shape = sig.wasm_shape();
        let key = (module.clone(), name.clone());
        match imports.get(&key) {
            None => {
                return Err(format!(
                    "manifest declares import {}::{:?} but wasm does not import it",
                    String::from_utf8_lossy(module),
                    String::from_utf8_lossy(name)
                ))
            }
            Some(actual) if *actual != manifest_shape => {
                return Err(format!(
                    "import {}::{:?}: manifest claims {:?} (shape {:?}), wasm wants {:?}",
                    String::from_utf8_lossy(module),
                    String::from_utf8_lossy(name),
                    sig,
                    manifest_shape,
                    actual
                ))
            }
            _ => {}
        }
    }
    // (6) every wasm import is in the manifest
    for ((module, name), _) in &imports {
        let key = (module.clone(), name.clone());
        if !manifest_imports.contains_key(&key) {
            return Err(format!(
                "wasm imports {}::{:?} but manifest does not declare it",
                String::from_utf8_lossy(module),
                String::from_utf8_lossy(name)
            ));
        }
    }

    println!(
        "verified: manifest abi={}, kind={:?}, {} exports, {} imports — wasm matches",
        abi_version,
        kind,
        exports.len(),
        imports.len()
    );
    Ok(())
}

// ── WASM walker ──────────────────────────────────────────────────

/// Walked WASM export/import surface. Stored as `WasmSigShape`
/// because that is what the WASM type section actually expresses;
/// the manifest's narrower `FuncSig` is checked against this via
/// shape equality (see `verify_manifest`).
struct WasmFuncSurface {
    exports: BTreeMap<Vec<u8>, WasmSigShape>,
    imports: BTreeMap<(Vec<u8>, Vec<u8>), WasmSigShape>,
}

fn walk_wasm(wasm: &[u8]) -> Result<WasmFuncSurface, String> {
    let parser = Parser::new(0);
    let mut types: Vec<Option<WasmSigShape>> = Vec::new();
    let mut import_func_types: Vec<u32> = Vec::new();
    let mut local_func_types: Vec<u32> = Vec::new();
    let mut imports_by_funcidx: BTreeMap<u32, (Vec<u8>, Vec<u8>)> =
        BTreeMap::new();
    let mut exports_collected: BTreeMap<Vec<u8>, WasmSigShape> = BTreeMap::new();
    let mut imports_collected: BTreeMap<(Vec<u8>, Vec<u8>), WasmSigShape> =
        BTreeMap::new();

    for payload in parser.parse_all(wasm) {
        let payload = payload.map_err(|e| format!("wasmparser: {}", e))?;
        match payload {
            Payload::TypeSection(reader) => {
                for ty_grp in reader {
                    let grp = ty_grp.map_err(|e| format!("type section: {}", e))?;
                    // wasmparser groups types into recursion groups
                    // (component-model artifact). Walk each member.
                    for sub in grp.types() {
                        let shape = match &sub.composite_type.inner {
                            wasmparser::CompositeInnerType::Func(f) => {
                                wasm_sig_shape(f.params(), f.results())
                            }
                            _ => None,
                        };
                        types.push(shape);
                    }
                }
            }
            Payload::ImportSection(reader) => {
                for im in reader {
                    let im = im.map_err(|e| format!("import section: {}", e))?;
                    if let TypeRef::Func(type_idx) = im.ty {
                        let func_idx = import_func_types.len() as u32;
                        import_func_types.push(type_idx);
                        let module = im.module.as_bytes().to_vec();
                        let name = im.name.as_bytes().to_vec();
                        imports_by_funcidx.insert(func_idx, (module.clone(), name.clone()));
                        let sig = types
                            .get(type_idx as usize)
                            .copied()
                            .flatten()
                            .ok_or_else(|| {
                                format!(
                                    "import {}.{}: type idx {} not classified",
                                    im.module, im.name, type_idx
                                )
                            })?;
                        imports_collected.insert((module, name), sig);
                    }
                }
            }
            Payload::FunctionSection(reader) => {
                for ty_idx in reader {
                    let ty_idx = ty_idx.map_err(|e| format!("function section: {}", e))?;
                    local_func_types.push(ty_idx);
                }
            }
            Payload::ExportSection(reader) => {
                for ex in reader {
                    let ex = ex.map_err(|e| format!("export section: {}", e))?;
                    if ex.kind == ExternalKind::Func {
                        let func_idx = ex.index;
                        // Func index space: imported funcs first,
                        // then locally-defined.
                        let import_count = import_func_types.len() as u32;
                        let type_idx = if func_idx < import_count {
                            import_func_types[func_idx as usize]
                        } else {
                            let local_idx = (func_idx - import_count) as usize;
                            *local_func_types.get(local_idx).ok_or_else(|| {
                                format!(
                                    "export {}: local func idx {} OOB",
                                    ex.name, local_idx
                                )
                            })?
                        };
                        let sig = types
                            .get(type_idx as usize)
                            .copied()
                            .flatten()
                            .ok_or_else(|| {
                                format!(
                                    "export {}: type idx {} not classified",
                                    ex.name, type_idx
                                )
                            })?;
                        exports_collected.insert(ex.name.as_bytes().to_vec(), sig);
                    }
                }
            }
            _ => {}
        }
    }

    Ok(WasmFuncSurface {
        exports: exports_collected,
        imports: imports_collected,
    })
}

/// Convert a WASM function signature to its `WasmSigShape`
/// representation, dropping any val type Wari's ABI does not use.
/// `None` means the driver imports/exports a function whose
/// param or result types are outside Wari's modeled set
/// (currently i32 / i64); the verifier rejects such drivers.
fn wasm_sig_shape(
    params: &[ValType],
    results: &[ValType],
) -> Option<WasmSigShape> {
    fn to_wari(v: ValType) -> Option<WasmValType> {
        match v {
            ValType::I32 => Some(WasmValType::I32),
            ValType::I64 => Some(WasmValType::I64),
            _ => None,
        }
    }
    // Compare against the closed shape set in driver-iface — this
    // way the verifier doesn't need to allocate; it just walks the
    // FuncSig variants and finds a shape match. We materialize a
    // thin Vec only to check element-by-element.
    let p: Vec<WasmValType> = params.iter().copied().map(to_wari).collect::<Option<_>>()?;
    let r: Vec<WasmValType> = results.iter().copied().map(to_wari).collect::<Option<_>>()?;

    // Search the closed FuncSig table for a shape whose params /
    // results match. There are 7 variants today; bounded.
    use FuncSig::*;
    for s in [
        UnitUnit, U32xU32I32, U32I32, U32U32, U64I32, UnitU64, U32x5I32,
    ] {
        let shape = s.wasm_shape();
        if shape.params == p.as_slice() && shape.results == r.as_slice() {
            return Some(shape);
        }
    }
    None
}
