//! Minimal WebAssembly → Cranelift IR translator for the G4 spike.
//!
//! **Throwaway.** This covers only the opcode subset the `arith.wat` and
//! `memory.wat` fixtures use. It exists because `cranelift-wasm` — the crate
//! the roadmap named — was removed from the Cranelift workspace and last
//! published as `0.111.11`; the current translator lives inside
//! `wasmtime-cranelift`, which is not usable standalone. See
//! `docs/aot-spike-results.md` §"What could not be used".
//!
//! Deliberate simplifications, all of which the real G6 driver must undo:
//!
//! * only `i32` values; no `i64`/`f32`/`f64`;
//! * the exported function is assumed to be `() -> i32` (Tier-1 `_start`
//!   shape) — the type section is never read;
//! * `block`/`loop` must have an empty block type, so no block parameters
//!   are ever needed;
//! * no calls, no `call_indirect`, no `if`/`else`, no globals, no tables.
//!
//! The memory ABI follows `docs/aot-target-abi.md` §A1 in *shape* only: the
//! linear-memory base and length arrive as the compiled function's two
//! parameters instead of being loaded from a reserved WCTX register, and
//! every access gets an explicit `end > len` compare-and-trap.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{
    types, AbiParam, Function, InstBuilder, MemFlagsData, Signature, TrapCode, UserFuncName,
};
use cranelift_codegen::isa::{CallConv, TargetFrontendConfig};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use wasmparser::{BlockType, Operator, Parser, Payload, ValType};

/// Trap code the spike uses for an out-of-bounds linear-memory access.
///
/// The real compiler must map every wasmi `TrapCode` variant mechanically
/// (`docs/aot-target-abi.md` §4.3); the spike only ever produces this one.
const TRAP_OOB: TrapCode = TrapCode::HEAP_OUT_OF_BOUNDS;

/// Trap code the spike uses for `unreachable`.
///
/// Cranelift 0.134 has no built-in constant for it — Wasmtime allocates a
/// user code, and so do we. Code `1` is arbitrary and spike-local.
const TRAP_UNREACHABLE: TrapCode = TrapCode::unwrap_user(1);

/// A `loop`/`block` frame on the translator's control stack.
struct Frame {
    /// Where a `br`/`br_if` targeting this depth jumps to.
    branch_target: cranelift_codegen::ir::Block,
    /// Where control lands after the matching `end`.
    exit: cranelift_codegen::ir::Block,
    /// `true` for `loop` (branch goes backwards to the header).
    is_loop: bool,
}

/// Extracts the body of the `_start` export from a wasm module.
///
/// # Errors
/// Returns a message if the module has no `_start` export, if `_start` is
/// not a function, or if its body is missing.
fn find_start_body(wasm: &[u8]) -> Result<wasmparser::FunctionBody<'_>, String> {
    let mut start_func: Option<u32> = None;
    let mut imported_funcs: u32 = 0;
    let mut bodies = Vec::new();

    for payload in Parser::new(0).parse_all(wasm) {
        match payload.map_err(|e| format!("wasm parse error: {e}"))? {
            Payload::ImportSection(reader) => {
                for import in reader.into_imports() {
                    let import = import.map_err(|e| format!("bad import: {e}"))?;
                    if matches!(import.ty, wasmparser::TypeRef::Func(_)) {
                        imported_funcs += 1;
                    }
                }
            }
            Payload::ExportSection(reader) => {
                for export in reader {
                    let export = export.map_err(|e| format!("bad export: {e}"))?;
                    if export.name == "_start" {
                        if export.kind != wasmparser::ExternalKind::Func {
                            return Err("_start export is not a function".into());
                        }
                        start_func = Some(export.index);
                    }
                }
            }
            Payload::CodeSectionEntry(body) => bodies.push(body),
            _ => {}
        }
    }

    let idx = start_func.ok_or_else(|| "module has no `_start` export".to_string())?;
    if idx < imported_funcs {
        return Err("_start resolves to an imported function".into());
    }
    let body_idx = (idx - imported_funcs) as usize;
    bodies
        .get(body_idx)
        .cloned()
        .ok_or_else(|| format!("no code-section body at index {body_idx}"))
}

/// Translates the `_start` export of `wasm` into a Cranelift function.
///
/// The produced signature is `fn(i64 mem_base, i64 mem_len) -> i32` under
/// the RV64 SystemV (psABI) calling convention, so `mem_base` lands in `a0`
/// and `mem_len` in `a1`.
///
/// # Errors
/// Returns a message naming the first unsupported construct encountered.
pub fn translate(wasm: &[u8], frontend: TargetFrontendConfig) -> Result<Function, String> {
    let body = find_start_body(wasm)?;

    let mut sig = Signature::new(CallConv::SystemV);
    sig.params.push(AbiParam::new(types::I64)); // a0 = linear-memory base
    sig.params.push(AbiParam::new(types::I64)); // a1 = linear-memory length
    sig.returns.push(AbiParam::new(types::I32));

    let mut func = Function::with_name_signature(UserFuncName::user(0, 0), sig);
    let mut fb_ctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut func, &mut fb_ctx);

    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);
    let mem_base = b.block_params(entry)[0];
    let mem_len = b.block_params(entry)[1];

    // Wasm locals. `_start` is assumed to take no parameters, so every local
    // comes from the body's local declarations.
    let mut locals: Vec<Variable> = Vec::new();
    let locals_reader = body
        .get_locals_reader()
        .map_err(|e| format!("bad locals: {e}"))?;
    for decl in locals_reader {
        let (count, ty) = decl.map_err(|e| format!("bad local decl: {e}"))?;
        if ty != ValType::I32 {
            return Err(format!("spike supports i32 locals only, found {ty:?}"));
        }
        for _ in 0..count {
            let v = b.declare_var(types::I32);
            let zero = b.ins().iconst(types::I32, 0);
            b.def_var(v, zero);
            locals.push(v);
        }
    }

    let mut stack: Vec<cranelift_codegen::ir::Value> = Vec::new();
    let mut control: Vec<Frame> = Vec::new();

    let ops = body
        .get_operators_reader()
        .map_err(|e| format!("bad operators: {e}"))?;

    for op in ops {
        let op = op.map_err(|e| format!("bad operator: {e}"))?;
        match op {
            Operator::Nop => {}
            Operator::I32Const { value } => {
                let v = b.ins().iconst(types::I32, i64::from(value));
                stack.push(v);
            }
            Operator::LocalGet { local_index } => {
                let var = *locals
                    .get(local_index as usize)
                    .ok_or_else(|| format!("local.get {local_index} out of range"))?;
                stack.push(b.use_var(var));
            }
            Operator::LocalSet { local_index } | Operator::LocalTee { local_index } => {
                let var = *locals
                    .get(local_index as usize)
                    .ok_or_else(|| format!("local.set {local_index} out of range"))?;
                let v = stack.pop().ok_or("value stack underflow at local.set")?;
                b.def_var(var, v);
                if matches!(op, Operator::LocalTee { .. }) {
                    stack.push(v);
                }
            }
            Operator::Drop => {
                stack.pop().ok_or("value stack underflow at drop")?;
            }

            // ---- binary arithmetic -----------------------------------
            Operator::I32Add
            | Operator::I32Sub
            | Operator::I32Mul
            | Operator::I32And
            | Operator::I32Or
            | Operator::I32Xor
            | Operator::I32Shl => {
                let rhs = stack.pop().ok_or("value stack underflow")?;
                let lhs = stack.pop().ok_or("value stack underflow")?;
                let v = match op {
                    Operator::I32Add => b.ins().iadd(lhs, rhs),
                    Operator::I32Sub => b.ins().isub(lhs, rhs),
                    Operator::I32Mul => b.ins().imul(lhs, rhs),
                    Operator::I32And => b.ins().band(lhs, rhs),
                    Operator::I32Or => b.ins().bor(lhs, rhs),
                    Operator::I32Xor => b.ins().bxor(lhs, rhs),
                    _ => b.ins().ishl(lhs, rhs),
                };
                stack.push(v);
            }

            // ---- comparisons -----------------------------------------
            Operator::I32Eq
            | Operator::I32Ne
            | Operator::I32LtS
            | Operator::I32LtU
            | Operator::I32GtS
            | Operator::I32LeS
            | Operator::I32GeS => {
                let rhs = stack.pop().ok_or("value stack underflow")?;
                let lhs = stack.pop().ok_or("value stack underflow")?;
                let cc = match op {
                    Operator::I32Eq => IntCC::Equal,
                    Operator::I32Ne => IntCC::NotEqual,
                    Operator::I32LtS => IntCC::SignedLessThan,
                    Operator::I32LtU => IntCC::UnsignedLessThan,
                    Operator::I32GtS => IntCC::SignedGreaterThan,
                    Operator::I32LeS => IntCC::SignedLessThanOrEqual,
                    _ => IntCC::SignedGreaterThanOrEqual,
                };
                let c = b.ins().icmp(cc, lhs, rhs);
                stack.push(b.ins().uextend(types::I32, c));
            }
            Operator::I32Eqz => {
                let v = stack.pop().ok_or("value stack underflow")?;
                let c = b.ins().icmp_imm_s(IntCC::Equal, v, 0);
                stack.push(b.ins().uextend(types::I32, c));
            }

            // ---- linear memory (ABI §A1 shape: explicit bounds check) --
            Operator::I32Load { memarg } => {
                let idx = stack.pop().ok_or("value stack underflow at i32.load")?;
                let addr = bounds_check(&mut b, idx, memarg.offset, 4, mem_base, mem_len)?;
                let v = b.ins().load(types::I32, MemFlagsData::new(), addr, 0);
                stack.push(v);
            }
            Operator::I32Store { memarg } => {
                let val = stack.pop().ok_or("value stack underflow at i32.store")?;
                let idx = stack.pop().ok_or("value stack underflow at i32.store")?;
                let addr = bounds_check(&mut b, idx, memarg.offset, 4, mem_base, mem_len)?;
                b.ins().store(MemFlagsData::new(), val, addr, 0);
            }

            // ---- control flow ----------------------------------------
            Operator::Loop { blockty } => {
                require_empty_blockty(blockty)?;
                if !stack.is_empty() {
                    return Err("spike requires an empty value stack at `loop`".into());
                }
                let header = b.create_block();
                let exit = b.create_block();
                b.ins().jump(header, &[]);
                b.switch_to_block(header);
                control.push(Frame {
                    branch_target: header,
                    exit,
                    is_loop: true,
                });
            }
            Operator::Block { blockty } => {
                require_empty_blockty(blockty)?;
                if !stack.is_empty() {
                    return Err("spike requires an empty value stack at `block`".into());
                }
                let exit = b.create_block();
                control.push(Frame {
                    branch_target: exit,
                    exit,
                    is_loop: false,
                });
            }
            Operator::Br { relative_depth } => {
                let frame = frame_at(&control, relative_depth)?;
                b.ins().jump(frame, &[]);
                // Anything after an unconditional branch is unreachable; the
                // fixtures never have any, so a fresh dead block is enough.
                let dead = b.create_block();
                b.switch_to_block(dead);
                b.seal_block(dead);
            }
            Operator::BrIf { relative_depth } => {
                let target = frame_at(&control, relative_depth)?;
                let cond = stack.pop().ok_or("value stack underflow at br_if")?;
                let cont = b.create_block();
                b.ins().brif(cond, target, &[], cont, &[]);
                b.switch_to_block(cont);
                b.seal_block(cont);
            }
            Operator::Return => {
                let v = stack.pop().ok_or("value stack underflow at return")?;
                b.ins().return_(&[v]);
                let dead = b.create_block();
                b.switch_to_block(dead);
                b.seal_block(dead);
            }
            Operator::Unreachable => {
                b.ins().trap(TRAP_UNREACHABLE);
                let dead = b.create_block();
                b.switch_to_block(dead);
                b.seal_block(dead);
            }
            Operator::End => match control.pop() {
                Some(frame) => {
                    b.ins().jump(frame.exit, &[]);
                    if frame.is_loop {
                        b.seal_block(frame.branch_target);
                    }
                    b.switch_to_block(frame.exit);
                    b.seal_block(frame.exit);
                }
                None => {
                    // Function-level `end`: return the single result value.
                    let v = stack.pop().ok_or("`_start` produced no result value")?;
                    b.ins().return_(&[v]);
                }
            },

            other => return Err(format!("spike does not support operator {other:?}")),
        }
    }

    b.seal_all_blocks();
    b.finalize(frontend);
    Ok(func)
}

/// Emits the §A1 explicit bounds check and returns the checked host address.
///
/// Shape (matching `docs/aot-target-abi.md` §2.3): zero-extend the wasm `i32`
/// index to 64 bits, add the static offset and the access width, compare the
/// end against `mem_len`, branch to a trap block if `end > len`, then add the
/// base.
fn bounds_check(
    b: &mut FunctionBuilder<'_>,
    idx_i32: cranelift_codegen::ir::Value,
    static_offset: u64,
    width: i64,
    mem_base: cranelift_codegen::ir::Value,
    mem_len: cranelift_codegen::ir::Value,
) -> Result<cranelift_codegen::ir::Value, String> {
    let off = i64::try_from(static_offset).map_err(|_| "memarg offset too large".to_string())?;
    let idx64 = b.ins().uextend(types::I64, idx_i32);
    let start = b.ins().iadd_imm_s(idx64, off);
    let end = b.ins().iadd_imm_s(start, width);
    let oob = b.ins().icmp(IntCC::UnsignedGreaterThan, end, mem_len);

    let trap_block = b.create_block();
    let ok_block = b.create_block();
    b.ins().brif(oob, trap_block, &[], ok_block, &[]);

    b.switch_to_block(trap_block);
    b.seal_block(trap_block);
    b.ins().trap(TRAP_OOB);

    b.switch_to_block(ok_block);
    b.seal_block(ok_block);
    Ok(b.ins().iadd(mem_base, start))
}

/// Rejects block types the spike cannot express without block parameters.
fn require_empty_blockty(bt: BlockType) -> Result<(), String> {
    match bt {
        BlockType::Empty => Ok(()),
        other => Err(format!(
            "spike supports only empty block types, found {other:?}"
        )),
    }
}

/// Resolves a relative branch depth to its Cranelift block.
fn frame_at(control: &[Frame], depth: u32) -> Result<cranelift_codegen::ir::Block, String> {
    let idx = control
        .len()
        .checked_sub(depth as usize + 1)
        .ok_or_else(|| format!("branch depth {depth} escapes the function"))?;
    control
        .get(idx)
        .map(|f| f.branch_target)
        .ok_or_else(|| format!("branch depth {depth} out of range"))
}
