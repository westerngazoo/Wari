use clap::Parser;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;
use wasmi::{Config, Engine, Linker, Module, Store};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Input WASM files to benchmark
    #[arg(required = true)]
    files: Vec<PathBuf>,

    /// Number of runs per file
    #[arg(long, default_value_t = 5)]
    runs: usize,

    /// Output JSON path
    #[arg(long)]
    json: Option<PathBuf>,
}

#[derive(Serialize)]
struct BenchmarkResult {
    module: String,
    fuel: u64,
    wall_ms_min: f64,
    wall_ms_median: f64,
    peak_linmem_pages: usize,
}

fn main() {
    let args = Args::parse();
    let mut results_map = BTreeMap::new();

    println!("{:<20} | {:<10} | {:<15} | {:<15} | {:<20}", "module", "fuel", "wall_ms_min", "wall_ms_median", "peak_linmem_pages");
    println!("{:-<20}-|-{:-<10}-|-{:-<15}-|-{:-<15}-|-{:-<20}", "", "", "", "", "");

    for file in &args.files {
        let wasm_bytes = fs::read(file).expect("Failed to read file");
        let module_name = file.file_name().unwrap().to_string_lossy().to_string();

        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, &wasm_bytes).expect("Failed to parse module");

        let mut run_times = Vec::new();
        let mut fuel_consumed = 0;
        let mut peak_pages = 0;

        for _ in 0..args.runs {
            let mut store = Store::new(&engine, ());
            store.set_fuel(100_000_000).unwrap(); // Give it plenty of fuel, or maybe test fails if infinite loop

            let mut linker = Linker::new(&engine);
            
            // Stub wari yield for hostcall.wat
            linker.func_wrap("wari", "yield", |_caller: wasmi::Caller<'_, ()>, val: i32| -> i32 {
                val
            }).unwrap();

            let instance = match linker.instantiate(&mut store, &module) {
                Ok(pre) => match pre.start(&mut store) {
                    Ok(i) => i,
                    Err(_) => continue, // e.g. trap on start
                },
                Err(_) => continue,
            };

            if let Some(mem) = instance.get_memory(&store, "memory") {
                peak_pages = u32::from(mem.current_pages(&store)) as usize;
            }

            let func = instance.get_export(&store, "_start").and_then(wasmi::Extern::into_func).expect("Failed to get _start");
            // Size the results buffer to the function's arity. The
            // fixtures' `_start` returns an i32, and wasmi errors on a
            // results-length mismatch BEFORE executing any instruction
            // — with the error swallowed below, that silently reported
            // zero fuel/time for every module (even the fuel bomb).
            let n_results = func.ty(&store).results().len();
            let mut results = vec![wasmi::Val::I32(0); n_results];

            let start = Instant::now();
            let _ = func.call(&mut store, &[], &mut results);
            let duration = start.elapsed();
            
            let remaining = store.get_fuel().unwrap();
            let consumed = 100_000_000 - remaining;
            fuel_consumed = consumed;
            run_times.push(duration.as_secs_f64() * 1000.0);
        }

        if run_times.is_empty() {
            println!("{:<20} | {:<10} | {:<15} | {:<15} | {:<20}", module_name, "TRAP", "TRAP", "TRAP", "TRAP");
            continue;
        }

        run_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let min = run_times[0];
        let median = run_times[run_times.len() / 2];

        let res = BenchmarkResult {
            module: module_name.clone(),
            fuel: fuel_consumed,
            wall_ms_min: min,
            wall_ms_median: median,
            peak_linmem_pages: peak_pages,
        };

        println!("{:<20} | {:<10} | {:<15.3} | {:<15.3} | {:<20}", 
                 res.module, res.fuel, res.wall_ms_min, res.wall_ms_median, res.peak_linmem_pages);

        results_map.insert(module_name, res);
    }

    if let Some(json_path) = args.json {
        let json_str = serde_json::to_string_pretty(&results_map).unwrap();
        fs::write(json_path, json_str).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuel_determinism() {
        let wat = r#"
        (module
            (func (export "_start") (result i32)
                (local $i i32)
                (local.set $i (i32.const 0))
                (loop $loop
                    (local.set $i (i32.add (local.get $i) (i32.const 1)))
                    (br_if $loop (i32.lt_s (local.get $i) (i32.const 10)))
                )
                (local.get $i)
            )
        )
        "#;
        let wasm_bytes = wat::parse_str(wat).unwrap();

        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, &wasm_bytes).unwrap();

        let get_fuel = || {
            let mut store = Store::new(&engine, ());
            store.set_fuel(10_000).unwrap();
            let mut linker = Linker::new(&engine);
            let instance = linker.instantiate(&mut store, &module).unwrap().start(&mut store).unwrap();
            let func = instance.get_export(&store, "_start").unwrap().into_func().unwrap();
            let n = func.ty(&store).results().len();
            let mut out = vec![wasmi::Val::I32(0); n];
            let _ = func.call(&mut store, &[], &mut out);
            10_000 - store.get_fuel().unwrap()
        };

        let run1 = get_fuel();
        let run2 = get_fuel();
        assert_eq!(run1, run2, "Fuel must be bit-identical across runs");
        assert!(run1 > 0);
    }

    #[test]
    fn test_peak_pages() {
        let wat = r#"
        (module
            (memory (export "memory") 2)
            (func (export "_start"))
        )
        "#;
        let wasm_bytes = wat::parse_str(wat).unwrap();

        let engine = Engine::default();
        let module = Module::new(&engine, &wasm_bytes).unwrap();
        let mut store = Store::new(&engine, ());
        let linker = Linker::new(&engine);
        let instance = linker.instantiate(&mut store, &module).unwrap().start(&mut store).unwrap();
        
        let mem = instance.get_memory(&store, "memory").unwrap();
        assert_eq!(u32::from(mem.current_pages(&store)), 2);
    }
}
