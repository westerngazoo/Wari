// SPDX-License-Identifier: AGPL-3.0-only
//! `wari-aot` — the AOT compiler driver (roadmap **G6**).
//!
//! Pipeline: `.wasm → native RV64 .text (Cranelift) → WNM container`.
//! A host-side tool (std allowed; it never runs on device). It promotes
//! the throwaway G4 spike (`tools/wari-aot-spike`) into the real driver:
//! same Cranelift backend and same ported wasm→CLIF translator, but the
//! output is a signed-able [WNM] container the kernel loader accepts,
//! not a standalone Linux ELF.
//!
//! ## What this slice of G6 delivers
//!
//! The end-to-end driver + a **bitwise-reproducible** (R8) WNM carrying
//! `Text` (reloc-free native code), `Wasm` (the source, for the loader's
//! independent re-validation), and a `SafetyCert` **placeholder**. Its
//! output is accepted by `wari_wnm::load_plan`.
//!
//! ## Documented follow-ups (the G4-flagged hard parts)
//!
//! - **Translator coverage.** The ported translator handles the fixture
//!   opcode subset. Broadening it to full Wari-Tier-1 wasm is ongoing G6.
//! - **Trap thunk (target-ABI §A3).** Cranelift lowers `trap` to `unimp`,
//!   not the ABI thunk call; post-processing the emitted trap edge is
//!   required before the cert checker (G7b) can see it. Not yet done — so
//!   a module whose compilation emits a trap or a residual relocation is
//!   rejected here rather than silently mis-encoded.
//! - **Real `SafetyCert`.** Replaces the placeholder when G7b lands.
//!
//! [WNM]: wari_wnm

mod translate;
mod wnm;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::isa::{self, TargetIsa};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use target_lexicon::Triple;
use wari_wnm::WnmSection;

/// `SafetyCert` placeholder until G7b lands the real cert checker.
///
/// Deliberately a clearly-marked **uncertified** stub, not an empty
/// section: a future loader/checker must treat this WNM as unproven
/// (reject in production) rather than mistake a zero-length cert for a
/// valid one. The real format is G7a's RFC (`docs/aot-safety-cert-*`).
const CERT_PLACEHOLDER: &[u8] = b"WARI-AOT-UNCERTIFIED-G6\x00";

/// `wari-aot` command line.
#[derive(Parser)]
#[command(about = "Wari AOT compiler: .wasm -> native RV64 WNM (roadmap G6)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// Sub-commands.
#[derive(Subcommand)]
enum Cmd {
    /// Compile a `.wasm` module to a native WNM container.
    Compile {
        /// Input `.wasm` (must export `_start`).
        wasm: PathBuf,
        /// Output `.wnm` path.
        #[arg(long)]
        out: PathBuf,
    },
}

/// Build the RV64 target ISA the driver compiles for.
///
/// Flags mirror the G4 spike exactly (they are part of what makes the
/// output reproducible): speed, non-PIC, verifier on, and — per
/// target-ABI §A1 — no probestack/guard pages, because bounds checks are
/// emitted explicitly by the translator.
fn make_isa() -> Result<std::sync::Arc<dyn TargetIsa>, String> {
    let mut flags = settings::builder();
    for (k, v) in [
        ("opt_level", "speed"),
        ("is_pic", "false"),
        ("enable_verifier", "true"),
        ("enable_probestack", "false"),
    ] {
        flags
            .set(k, v)
            .map_err(|e| format!("cranelift flag {k}={v}: {e}"))?;
    }
    let triple = "riscv64gc-unknown-linux-gnu"
        .parse::<Triple>()
        .map_err(|e| format!("bad triple: {e}"))?;
    let builder = isa::lookup(triple).map_err(|e| format!("no such cranelift target: {e}"))?;
    builder
        .finish(settings::Flags::new(flags))
        .map_err(|e| format!("isa finish: {e}"))
}

/// Compile `wasm` to RV64 machine code. The result is **relocation-free**
/// (target-ABI §5.1); a residual relocation means an unhandled construct,
/// which is rejected rather than silently dropped.
fn compile_text(wasm: &[u8]) -> Result<Vec<u8>, String> {
    let isa = make_isa()?;
    let func = translate::translate(wasm, isa.frontend_config())?;
    let mut ctx = Context::for_function(func);
    let compiled = ctx
        .compile(isa.as_ref(), &mut ControlPlane::default())
        .map_err(|e| format!("cranelift compile failed: {:?}", e.inner))?;
    let reloc_count = compiled.buffer.relocs().len();
    if reloc_count != 0 {
        // A residual `.text` relocation means the translator emitted a
        // construct Cranelift could not fully resolve (an unhandled
        // call/reference) — broadening translator coverage is ongoing G6.
        // Note: the WNM `Relocs` section (target-ABI §A4) initializes the
        // per-instance arena — the host-fn import vector, trap thunk, and
        // text-address table — and never patches `.text`, so it does not
        // apply here; §5.1 requires `.text` itself to be relocation-free.
        return Err(format!(
            "compiled .text carries {reloc_count} residual relocation(s); \
             target-ABI §5.1 requires .text to be relocation-free (an \
             unhandled construct reached codegen — see translator coverage)."
        ));
    }
    Ok(compiled.code_buffer().to_vec())
}

/// Full pipeline: `.wasm` bytes → WNM container bytes.
///
/// Bitwise-reproducible (R8): the Cranelift backend is deterministic, the
/// translator is a pure function of the input, and the WNM encoder emits
/// a fixed section order with no timestamps or paths.
fn compile_to_wnm(wasm: &[u8]) -> Result<Vec<u8>, String> {
    let text = compile_text(wasm)?;
    // Reloc-free (asserted above), so no Relocs section is emitted.
    wnm::encode(vec![
        wnm::Section {
            kind: WnmSection::Text,
            data: &text,
        },
        wnm::Section {
            kind: WnmSection::Wasm,
            data: wasm,
        },
        wnm::Section {
            kind: WnmSection::SafetyCert,
            data: CERT_PLACEHOLDER,
        },
    ])
}

fn main() -> Result<(), String> {
    match Cli::parse().cmd {
        Cmd::Compile { wasm, out } => {
            let src = std::fs::read(&wasm).map_err(|e| format!("read {:?}: {e}", wasm))?;
            let container = compile_to_wnm(&src)?;
            std::fs::write(&out, &container)
                .map_err(|e| format!("write {:?}: {e}", out))?;
            println!(
                "compiled {} -> {} ({} bytes, {} wasm)",
                wasm.display(),
                out.display(),
                container.len(),
                src.len()
            );
            Ok(())
        }
    }
}

// clippy::{unwrap,expect,panic} allowed in tests: a failed precondition
// should abort the test loudly.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const ARITH: &str = "../../tests/fixtures/aot/arith.wasm";

    #[test]
    fn output_is_loadable_by_wari_wnm() {
        let wasm = std::fs::read(ARITH).expect("fixture must exist");
        let bytes = compile_to_wnm(&wasm).expect("compile");
        let plan = wari_wnm::load_plan(&bytes).expect("load_plan must accept G6 output");
        // The embedded wasm round-trips byte-for-byte.
        let (wo, wl) = plan.wasm;
        assert_eq!(&bytes[wo as usize..(wo + wl) as usize], &wasm[..]);
        // Text is non-empty native code; no relocs (reloc-free §5.1).
        let (_to, tl) = plan.text;
        assert!(tl > 0, "empty .text");
        assert!(plan.relocs.is_none(), "reloc-free module should carry no Relocs");
    }

    #[test]
    fn output_is_byte_reproducible() {
        let wasm = std::fs::read(ARITH).expect("fixture must exist");
        let a = compile_to_wnm(&wasm).expect("compile");
        let b = compile_to_wnm(&wasm).expect("compile");
        assert_eq!(a, b, "G6 output is not reproducible (violates R8)");
    }
}
