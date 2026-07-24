//! `wari-aot-spike` — **throwaway** Cranelift → RV64 spike (roadmap **G4**).
//!
//! Per `docs/aot-parallel-roadmap.md` §4 G4 this crate is explicitly
//! disposable: it exists to produce the M0-gate evidence in
//! `docs/aot-spike-results.md` and is deleted when G6 (`tools/wari-aot`)
//! lands. It is not on the road to being the real compiler driver — it hard-
//! codes a tiny wasm subset, has no WNM output, no signing, and no safety
//! certificate.
//!
//! What it does: drive `cranelift-codegen` as a library to compile the
//! `_start` export of a `.wasm` fixture to RV64 machine code, emit the raw
//! code buffer and a freestanding static RV64 Linux ELF that calls it, and
//! report Cranelift's compile time, the `.text` size and whether two
//! compilations of the same input are byte-identical (R8).
//!
//! It deliberately does **not** execute anything: `qemu-riscv64` (user-mode)
//! is Linux-only and unavailable on the macOS dev host. Execution is Phase B
//! in the results doc, on the VF2's Debian riscv64 install.

mod elf;
mod translate;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Parser as ClapParser;
use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::isa::{self, TargetIsa};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use target_lexicon::Triple;

/// Command line for the spike.
#[derive(ClapParser, Debug)]
#[command(about = "Throwaway Cranelift→RV64 AOT spike (roadmap G4)")]
struct Args {
    /// Input `.wasm` fixture (must export `_start`).
    wasm: PathBuf,
    /// Output path for the static RV64 Linux ELF harness.
    #[arg(long)]
    out: PathBuf,
    /// Output path for the raw machine-code buffer (default: `<out>.bin`).
    #[arg(long)]
    bin: Option<PathBuf>,
    /// How many times the ELF harness calls the compiled function.
    ///
    /// 1 reproduces the module's observable result exactly; a large value
    /// makes the Phase-B wall-clock measurement on the board meaningful.
    #[arg(long, default_value_t = 1)]
    repeat: u32,
    /// Print the Cranelift IR before compilation.
    #[arg(long)]
    emit_clif: bool,
    /// Print the resolved ISA flags (which RV64 extensions are enabled).
    #[arg(long)]
    print_isa_flags: bool,
}

/// Builds the RV64 target ISA the spike compiles for.
fn make_isa() -> Result<std::sync::Arc<dyn TargetIsa>, String> {
    let mut flags = settings::builder();
    for (k, v) in [
        ("opt_level", "speed"),
        ("is_pic", "false"),
        ("enable_verifier", "true"),
        // No signal handlers, no guard pages: the ABI RFC's A1 recommendation
        // is explicit bounds checks, which this translator emits itself.
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

/// Compiles `wasm` once, returning `(machine code, reloc count, timings)`.
fn compile_once(
    wasm: &[u8],
    isa: &dyn TargetIsa,
    emit_clif: bool,
) -> Result<(Vec<u8>, usize, Duration, Duration), String> {
    let t_translate = Instant::now();
    let func = translate::translate(wasm, isa.frontend_config())?;
    let translate_time = t_translate.elapsed();

    if emit_clif {
        println!("--- CLIF ---\n{func}");
    }

    let mut ctx = Context::for_function(func);
    let t_compile = Instant::now();
    let compiled = ctx
        .compile(isa, &mut ControlPlane::default())
        .map_err(|e| format!("cranelift compile failed: {:?}", e.inner))?;
    let compile_time = t_compile.elapsed();

    let code = compiled.code_buffer().to_vec();
    let relocs = compiled.buffer.relocs().len();
    Ok((code, relocs, translate_time, compile_time))
}

fn main() -> Result<(), String> {
    let args = Args::parse();
    let wasm = std::fs::read(&args.wasm).map_err(|e| format!("read {:?}: {e}", args.wasm))?;
    let isa = make_isa()?;

    if args.print_isa_flags {
        println!("--- isa flags ({}) ---", isa.name());
        for f in isa.isa_flags() {
            println!("{} = {}", f.name, f.value_string());
        }
    }

    let (code, relocs, translate_time, compile_time) =
        compile_once(&wasm, isa.as_ref(), args.emit_clif)?;
    // R8 evidence: the same input compiled twice must give the same bytes.
    let (code2, _, _, compile_time2) = compile_once(&wasm, isa.as_ref(), false)?;
    let deterministic = code == code2;

    let bin_path = args
        .bin
        .clone()
        .unwrap_or_else(|| args.out.with_extension("bin"));
    std::fs::write(&bin_path, &code).map_err(|e| format!("write {bin_path:?}: {e}"))?;

    let image = elf::build(&code, args.repeat);
    std::fs::write(&args.out, &image.bytes).map_err(|e| format!("write {:?}: {e}", args.out))?;

    println!("input                : {}", args.wasm.display());
    println!("wasm bytes           : {}", wasm.len());
    println!("wasm→CLIF time (us)  : {}", translate_time.as_micros());
    println!("cranelift time (us)  : {}", compile_time.as_micros());
    println!("cranelift time #2(us): {}", compile_time2.as_micros());
    println!("text bytes           : {}", code.len());
    println!(
        "text/wasm ratio      : {:.2}",
        code.len() as f64 / wasm.len() as f64
    );
    println!("residual relocs      : {relocs}");
    println!("deterministic        : {deterministic}");
    println!("elf bytes            : {}", image.bytes.len());
    println!("elf entry            : {:#x}", image.entry);
    println!("compiled fn vaddr    : {:#x}", image.func_vaddr);
    println!("harness shim bytes   : {}", image.shim_len);
    println!("harness repeat       : {}", args.repeat);
    println!("wrote bin            : {}", bin_path.display());
    println!("wrote elf            : {}", args.out.display());

    if !deterministic {
        return Err("NON-DETERMINISTIC: two compilations differ (violates R8)".into());
    }
    Ok(())
}

// clippy::{panic,expect_used} allowed: test code, where a failed
// precondition should abort the test loudly.
#[cfg(test)]
#[allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    const ARITH: &str = "../../tests/fixtures/aot/arith.wasm";

    #[test]
    fn compiles_arith_to_nonempty_rv64() {
        let wasm = std::fs::read(ARITH).expect("fixture must exist");
        let isa = make_isa().expect("riscv64 isa");
        let (code, relocs, _, _) = compile_once(&wasm, isa.as_ref(), false).expect("compile");
        assert!(!code.is_empty(), "cranelift emitted no code");
        // ABI RFC §5.1: `.text` must be relocation-free.
        assert_eq!(relocs, 0, "compiled text carries residual relocations");
    }

    #[test]
    fn compilation_is_byte_reproducible() {
        let wasm = std::fs::read(ARITH).expect("fixture must exist");
        let isa = make_isa().expect("riscv64 isa");
        let (a, _, _, _) = compile_once(&wasm, isa.as_ref(), false).expect("compile");
        let (b, _, _, _) = compile_once(&wasm, isa.as_ref(), false).expect("compile");
        assert_eq!(a, b, "cranelift output is not reproducible (R8)");
    }
}
