// PM-36 probe 2: the proposed fix for target-ABI A2 + A3 on riscv64.
//   A2 -> WCTX as an EXPLICIT first parameter (what Wasmtime does with vmctx)
//         instead of a pinned register, which riscv64 does not support.
//   A3 -> the translator never emits ir::trap; it emits an explicit branch to
//         a block that CALLS WCTX.trap_entry, so the cert checker sees a real
//         call edge instead of an opaque `udf`.
use cranelift_codegen::ir::{types, AbiParam, Function, InstBuilder, MachMemFlags, Signature, UserFuncName};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::settings::{self};
use cranelift_codegen::{isa, Context};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use std::str::FromStr;

const TRAP_ENTRY_OFF: i32 = 16; // WCTX.trap_entry

fn main() {
    let flags = settings::Flags::new(settings::builder());
    let triple = target_lexicon::Triple::from_str("riscv64gc-unknown-none-elf").unwrap();
    let isa = isa::lookup(triple).unwrap().finish(flags).unwrap();

    // fn(wctx: i64, addr: i64, len: i64) -> i64
    let mut sig = Signature::new(CallConv::SystemV);
    for _ in 0..3 { sig.params.push(AbiParam::new(types::I64)); }
    sig.returns.push(AbiParam::new(types::I64));

    let mut func = Function::with_name_signature(UserFuncName::default(), sig);
    // signature of the trap thunk: fn(wctx, code)
    let mut tsig = Signature::new(CallConv::SystemV);
    tsig.params.push(AbiParam::new(types::I64));
    tsig.params.push(AbiParam::new(types::I32));
    let tsig = func.import_signature(tsig);

    let mut fctx = FunctionBuilderContext::new();
    {
        let mut fb = FunctionBuilder::new(&mut func, &mut fctx);
        let entry = fb.create_block();
        let okb = fb.create_block();
        let trapb = fb.create_block();
        fb.append_block_params_for_function_params(entry);
        fb.switch_to_block(entry);
        let wctx = fb.block_params(entry)[0];
        let addr = fb.block_params(entry)[1];
        let len  = fb.block_params(entry)[2];
        // the A3 bounds check: branch to the trap block, no ir::trap anywhere
        let oob = fb.ins().icmp(cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThanOrEqual, addr, len);
        fb.ins().brif(oob, trapb, &[], okb, &[]);
        fb.seal_block(entry);

        fb.switch_to_block(trapb);
        // load WCTX.trap_entry through the EXPLICIT wctx param and call it
        let entry_ptr = fb.ins().load(types::I64, MachMemFlags::new(), wctx, TRAP_ENTRY_OFF);
        let code = fb.ins().iconst(types::I32, 1);
        fb.ins().call_indirect(tsig, entry_ptr, &[wctx, code]);
        let z = fb.ins().iconst(types::I64, 0);
        fb.ins().return_(&[z]);
        fb.seal_block(trapb);

        fb.switch_to_block(okb);
        let v = fb.ins().load(types::I64, MachMemFlags::new(), addr, 0);
        fb.ins().return_(&[v]);
        fb.seal_block(okb);
        let fc = isa.frontend_config();
        fb.finalize(fc);
    }
    let mut ctx = Context::for_function(func);
    ctx.set_disasm(true);
    match ctx.compile(&*isa, &mut Default::default()) {
        Ok(c) => {
            println!("OK  {} bytes, {} relocs", c.code_buffer().len(), c.buffer.relocs().len());
            println!("{}", c.vcode.as_ref().unwrap());
        }
        Err(e) => println!("COMPILE FAILED: {e:?}"),
    }
}
