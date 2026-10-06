//! Cranelift native backend for tier 1 (`crate::vm`).
//!
//! Every function the register VM lowered is compiled to machine code with
//! the same semantics as the interpreter loop in `VmProgram::run`:
//!
//! * registers are untagged `i64` SSA variables (floats live as their bit
//!   pattern and are bit-cast around each float operation);
//! * `+ - *`, unary `-` and `/` are checked: overflow (including
//!   `i64::MIN / -1`) reports "integer overflow in <op>" with the op's source
//!   span, and `/` by zero reports "division by zero";
//! * a call made at depth `d` fails with the interpreter's call-depth error
//!   when `d >= MAX_CALL_DEPTH`.
//!
//! # Native ABI
//!
//! `extern "C" fn(depth: i64, ctx: *mut JitCtx, arg0: i64, ..., argN: i64) -> i64`
//! (at most 8 arguments, the same limit the interpreter bridge has). A
//! non-zero `ctx.err` after the call means the returned value is meaningless:
//! `1` = division by zero, `3` = integer overflow (function/op recorded in
//! `err_fn`/`err_at`), `2` = call depth exceeded. Callee errors propagate by returning `0` as soon
//! as the caller observes a non-zero `ctx.err`.
//!
//! The code lives as long as the `JitProgram`; function pointers are only used
//! through `JitProgram::call`.

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{types, AbiParam, InstBuilder, MemFlagsData, Value as CValue};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};

use crate::recursion::MAX_CALL_DEPTH;
use crate::vm::{Op, VmFunction, VmProgram};

const MAX_ARGS: usize = 8;

pub(crate) const MAX_JIT_FRAMES: usize = 64;

/// One caller recorded while an error unwinds: which function, and the index
/// of its call op.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct JitFrame {
    pub(crate) function: u32,
    pub(crate) at: u32,
}

// Field offsets are baked into the generated code: err 0, err_fn 4, err_at 8,
// frame_count 12, frames from 16 (8 bytes each).
#[repr(C)]
pub(crate) struct JitCtx {
    pub(crate) err: u32,
    pub(crate) err_fn: u32,
    pub(crate) err_at: u32,
    pub(crate) frame_count: u32,
    pub(crate) frames: [JitFrame; MAX_JIT_FRAMES],
}

impl Default for JitCtx {
    fn default() -> Self {
        Self {
            err: 0,
            err_fn: 0,
            err_at: 0,
            frame_count: 0,
            frames: [JitFrame::default(); MAX_JIT_FRAMES],
        }
    }
}

pub(crate) struct JitProgram {
    /// Keeps the generated code mapped.
    _module: JITModule,
    entries: Vec<Option<(*const u8, usize)>>,
}

// SAFETY: the code is immutable after `finalize_definitions`, and the
// function pointers are only ever called, never written through.
unsafe impl Send for JitProgram {}
unsafe impl Sync for JitProgram {}

impl JitProgram {
    /// Compiles every lowered function; `None` if the host has no backend or
    /// anything fails to compile (the interpreter then keeps running them).
    pub(crate) fn compile(vm: &VmProgram) -> Option<JitProgram> {
        let mut flags = settings::builder();
        flags.set("opt_level", "speed").ok()?;
        flags.set("preserve_frame_pointers", "false").ok()?;
        let isa = cranelift_native::builder()
            .ok()?
            .finish(settings::Flags::new(flags))
            .ok()?;
        let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        builder.symbol("spar_fmod", spar_fmod as *const u8);
        let mut module = JITModule::new(builder);

        let count = vm.function_count();
        let mut ids: Vec<Option<FuncId>> = vec![None; count];
        let mut sigs: Vec<Option<cranelift_codegen::ir::Signature>> = vec![None; count];
        for index in 0..count {
            let Some(f) = vm.function_at(index) else {
                continue;
            };
            if f.params.len() > MAX_ARGS {
                return None;
            }
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64)); // depth
            sig.params.push(AbiParam::new(types::I64)); // ctx
            for _ in 0..f.params.len() {
                sig.params.push(AbiParam::new(types::I64));
            }
            sig.returns.push(AbiParam::new(types::I64));
            ids[index] = Some(
                module
                    .declare_function(&format!("spar_fn_{index}"), Linkage::Local, &sig)
                    .ok()?,
            );
            sigs[index] = Some(sig);
        }

        let mut fctx = FunctionBuilderContext::new();
        for index in 0..count {
            let Some(f) = vm.function_at(index) else {
                continue;
            };
            let mut ctx = module.make_context();
            ctx.func.signature = sigs[index].clone()?;
            lower_function(&mut module, &ids, vm, index, f, &mut ctx, &mut fctx)?;
            module.define_function(ids[index]?, &mut ctx).ok()?;
            module.clear_context(&mut ctx);
        }
        module.finalize_definitions().ok()?;

        let entries = (0..count)
            .map(|index| {
                let id = ids[index]?;
                let f = vm.function_at(index)?;
                Some((module.get_finalized_function(id), f.params.len()))
            })
            .collect();
        Some(JitProgram {
            _module: module,
            entries,
        })
    }

    /// Calls function `index` natively.
    ///
    /// # Safety
    /// `args.len()` must equal the function's arity (checked here), and
    /// `index` must be a lowered function.
    pub(crate) fn call(
        &self,
        index: usize,
        args: &[u64],
        depth: i64,
        ctx: &mut JitCtx,
    ) -> Option<u64> {
        let (ptr, arity) = (*self.entries.get(index)?)?;
        if args.len() != arity {
            return None;
        }
        let ctx_ptr: *mut JitCtx = ctx;
        let a = |i: usize| args[i] as i64;
        // SAFETY: `ptr` was produced by Cranelift for exactly this signature
        // (`depth`, `ctx`, `arity` i64 parameters, i64 result, C ABI).
        let result: i64 = unsafe {
            use std::mem::transmute as t;
            match arity {
                0 => t::<_, extern "C" fn(i64, *mut JitCtx) -> i64>(ptr)(depth, ctx_ptr),
                1 => t::<_, extern "C" fn(i64, *mut JitCtx, i64) -> i64>(ptr)(depth, ctx_ptr, a(0)),
                2 => t::<_, extern "C" fn(i64, *mut JitCtx, i64, i64) -> i64>(ptr)(
                    depth,
                    ctx_ptr,
                    a(0),
                    a(1),
                ),
                3 => t::<_, extern "C" fn(i64, *mut JitCtx, i64, i64, i64) -> i64>(ptr)(
                    depth,
                    ctx_ptr,
                    a(0),
                    a(1),
                    a(2),
                ),
                4 => t::<_, extern "C" fn(i64, *mut JitCtx, i64, i64, i64, i64) -> i64>(ptr)(
                    depth,
                    ctx_ptr,
                    a(0),
                    a(1),
                    a(2),
                    a(3),
                ),
                5 => t::<_, extern "C" fn(i64, *mut JitCtx, i64, i64, i64, i64, i64) -> i64>(ptr)(
                    depth,
                    ctx_ptr,
                    a(0),
                    a(1),
                    a(2),
                    a(3),
                    a(4),
                ),
                6 => t::<_, extern "C" fn(i64, *mut JitCtx, i64, i64, i64, i64, i64, i64) -> i64>(
                    ptr,
                )(depth, ctx_ptr, a(0), a(1), a(2), a(3), a(4), a(5)),
                7 => {
                    t::<_, extern "C" fn(i64, *mut JitCtx, i64, i64, i64, i64, i64, i64, i64) -> i64>(
                        ptr,
                    )(depth, ctx_ptr, a(0), a(1), a(2), a(3), a(4), a(5), a(6))
                }
                8 => t::<
                    _,
                    extern "C" fn(i64, *mut JitCtx, i64, i64, i64, i64, i64, i64, i64, i64) -> i64,
                >(ptr)(
                    depth,
                    ctx_ptr,
                    a(0),
                    a(1),
                    a(2),
                    a(3),
                    a(4),
                    a(5),
                    a(6),
                    a(7),
                ),
                _ => return None,
            }
        };
        Some(result as u64)
    }
}

/// Host `fmod` for float `%`: sign of the dividend, exactly Rust's `f64 % f64`.
extern "C" fn spar_fmod(a: f64, b: f64) -> f64 {
    a % b
}

/// Basic-block leaders: the entry, every jump target, and every op that
/// follows a branch or return.
fn leaders(code: &[Op]) -> Vec<bool> {
    let mut leader = vec![false; code.len() + 1];
    leader[0] = true;
    for (ip, op) in code.iter().enumerate() {
        let target = match op {
            Op::Jmp { target }
            | Op::JmpIfFalse { target, .. }
            | Op::JmpIfTrue { target, .. }
            | Op::JLtI { target, .. }
            | Op::JLeI { target, .. }
            | Op::JGtI { target, .. }
            | Op::JGeI { target, .. }
            | Op::JEqI { target, .. }
            | Op::JNeI { target, .. }
            | Op::JLtII { target, .. }
            | Op::JLeII { target, .. }
            | Op::JGtII { target, .. }
            | Op::JGeII { target, .. }
            | Op::JEqII { target, .. }
            | Op::JNeII { target, .. } => Some(*target as usize),
            _ => None,
        };
        if let Some(target) = target {
            leader[target] = true;
            leader[ip + 1] = true;
        }
        if matches!(op, Op::Ret { .. } | Op::RetVoid | Op::Call { .. }) {
            leader[ip + 1] = true;
        }
    }
    leader
}

fn lower_function(
    module: &mut JITModule,
    ids: &[Option<FuncId>],
    vm: &VmProgram,
    index: usize,
    f: &VmFunction,
    ctx: &mut cranelift_codegen::Context,
    fctx: &mut FunctionBuilderContext,
) -> Option<()> {
    let mut b = FunctionBuilder::new(&mut ctx.func, fctx);
    let code = &f.code;
    let leader = leaders(code);

    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    let params: Vec<CValue> = b.block_params(entry).to_vec();
    let (depth, ctx_ptr) = (params[0], params[1]);

    let vars: Vec<Variable> = (0..f.nregs()).map(|_| b.declare_var(types::I64)).collect();
    let zero = b.ins().iconst(types::I64, 0);
    for (reg, var) in vars.iter().enumerate() {
        let init = if reg < f.params.len() {
            params[2 + reg]
        } else {
            zero
        };
        b.def_var(*var, init);
    }

    // Shared exits.
    let propagate = b.create_block();
    b.set_cold_block(propagate);
    let depth_err = b.create_block();
    b.set_cold_block(depth_err);

    // Depth check, exactly the interpreter's entry check.
    let body = b.create_block();
    let over = b.ins().icmp_imm_s(
        IntCC::SignedGreaterThanOrEqual,
        depth,
        MAX_CALL_DEPTH as i64,
    );
    b.ins().brif(over, depth_err, &[], body, &[]);

    let blocks: Vec<_> = (0..=code.len()).map(|_| b.create_block()).collect();
    b.switch_to_block(body);
    b.ins().jump(blocks[0], &[]);

    let mut terminated = true; // `body` ended with the jump above
    for (ip, op) in code.iter().enumerate() {
        if leader[ip] {
            if !terminated {
                b.ins().jump(blocks[ip], &[]);
            }
            b.switch_to_block(blocks[ip]);
            terminated = false;
        } else if terminated {
            // Unreachable straight-line code after a terminator; skip it.
            continue;
        }
        let get = |b: &mut FunctionBuilder, r: u32| b.use_var(vars[r as usize]);
        let set = |b: &mut FunctionBuilder, r: u32, v: CValue| b.def_var(vars[r as usize], v);
        let as_f = |b: &mut FunctionBuilder, v: CValue| {
            b.ins().bitcast(types::F64, MemFlagsData::new(), v)
        };
        let from_f = |b: &mut FunctionBuilder, v: CValue| {
            b.ins().bitcast(types::I64, MemFlagsData::new(), v)
        };
        let bool_val = |b: &mut FunctionBuilder, c: CValue| b.ins().uextend(types::I64, c);

        macro_rules! bin {
            ($dst:expr, $a:expr, $b:expr, $method:ident) => {{
                let x = get(&mut b, $a);
                let y = get(&mut b, $b);
                let r = b.ins().$method(x, y);
                set(&mut b, $dst, r);
            }};
        }
        macro_rules! cmp {
            ($dst:expr, $a:expr, $b:expr, $cc:expr) => {{
                let x = get(&mut b, $a);
                let y = get(&mut b, $b);
                let c = b.ins().icmp($cc, x, y);
                let r = bool_val(&mut b, c);
                set(&mut b, $dst, r);
            }};
        }
        macro_rules! cmp_imm {
            ($dst:expr, $a:expr, $imm:expr, $cc:expr) => {{
                let x = get(&mut b, $a);
                let c = b.ins().icmp_imm_s($cc, x, $imm);
                let r = bool_val(&mut b, c);
                set(&mut b, $dst, r);
            }};
        }
        macro_rules! fbin {
            ($dst:expr, $a:expr, $b:expr, $method:ident) => {{
                let x = get(&mut b, $a);
                let y = get(&mut b, $b);
                let (x, y) = (as_f(&mut b, x), as_f(&mut b, y));
                let r = b.ins().$method(x, y);
                let r = from_f(&mut b, r);
                set(&mut b, $dst, r);
            }};
        }
        macro_rules! fcmp {
            ($dst:expr, $a:expr, $b:expr, $cc:expr) => {{
                let x = get(&mut b, $a);
                let y = get(&mut b, $b);
                let (x, y) = (as_f(&mut b, x), as_f(&mut b, y));
                let c = b.ins().fcmp($cc, x, y);
                let r = bool_val(&mut b, c);
                set(&mut b, $dst, r);
            }};
        }
        // Checked arithmetic: an overflow records error 3 (with this op's index,
        // from which the caller recovers the operation name and span) and returns.
        macro_rules! checked {
            ($dst:expr, $x:expr, $y:expr, $method:ident) => {{
                let (r, overflowed) = b.ins().$method($x, $y);
                let ok = b.create_block();
                let bad = b.create_block();
                b.set_cold_block(bad);
                b.ins().brif(overflowed, bad, &[], ok, &[]);
                b.switch_to_block(bad);
                fail!(3, ip);
                b.switch_to_block(ok);
                set(&mut b, $dst, r);
            }};
        }
        macro_rules! branch_cmp {
            ($a:expr, $b:expr, $target:expr, $cc:expr) => {{
                let x = get(&mut b, $a);
                let y = get(&mut b, $b);
                let c = b.ins().icmp($cc, x, y);
                b.ins()
                    .brif(c, blocks[$target as usize], &[], blocks[ip + 1], &[]);
                terminated = true;
            }};
        }
        macro_rules! branch_cmp_imm {
            ($a:expr, $imm:expr, $target:expr, $cc:expr) => {{
                let x = get(&mut b, $a);
                let c = b.ins().icmp_imm_s($cc, x, $imm);
                b.ins()
                    .brif(c, blocks[$target as usize], &[], blocks[ip + 1], &[]);
                terminated = true;
            }};
        }
        // Records an error in the context and returns from the function.
        macro_rules! fail {
            ($code:expr, $at:expr) => {{
                let code = b.ins().iconst(types::I32, $code as i64);
                b.ins().store(MemFlagsData::trusted(), code, ctx_ptr, 0);
                let func = b.ins().iconst(types::I32, index as i64);
                b.ins().store(MemFlagsData::trusted(), func, ctx_ptr, 4);
                let at = b.ins().iconst(types::I32, $at as i64);
                b.ins().store(MemFlagsData::trusted(), at, ctx_ptr, 8);
                b.ins().return_(&[zero]);
            }};
        }

        match *op {
            Op::ConstI { dst, value } => {
                let v = b.ins().iconst(types::I64, value);
                set(&mut b, dst, v);
            }
            Op::ConstF { dst, bits } => {
                let v = b.ins().iconst(types::I64, bits as i64);
                set(&mut b, dst, v);
            }
            Op::Move { dst, src } => {
                let v = get(&mut b, src);
                set(&mut b, dst, v);
            }
            Op::AddI { dst, a, b: rb } => {
                let (x, y) = (get(&mut b, a), get(&mut b, rb));
                checked!(dst, x, y, sadd_overflow);
            }
            Op::SubI { dst, a, b: rb } => {
                let (x, y) = (get(&mut b, a), get(&mut b, rb));
                checked!(dst, x, y, ssub_overflow);
            }
            Op::MulI { dst, a, b: rb } => {
                let (x, y) = (get(&mut b, a), get(&mut b, rb));
                checked!(dst, x, y, smul_overflow);
            }
            Op::DivI { dst, a, b: rb, at } => {
                let x = get(&mut b, a);
                let y = get(&mut b, rb);
                let ok = b.create_block();
                let fail_block = b.create_block();
                b.set_cold_block(fail_block);
                b.ins().brif(y, ok, &[], fail_block, &[]);
                b.switch_to_block(fail_block);
                fail!(1, at);
                b.switch_to_block(ok);
                // i64::MIN / -1 overflows (and sdiv would trap): report it.
                let is_min = b.ins().icmp_imm_s(IntCC::Equal, x, i64::MIN);
                let is_neg1 = b.ins().icmp_imm_s(IntCC::Equal, y, -1);
                let both = b.ins().band(is_min, is_neg1);
                let safe = b.create_block();
                let overflow_block = b.create_block();
                b.set_cold_block(overflow_block);
                b.ins().brif(both, overflow_block, &[], safe, &[]);
                b.switch_to_block(overflow_block);
                fail!(3, ip);
                b.switch_to_block(safe);
                let r = b.ins().sdiv(x, y);
                set(&mut b, dst, r);
            }
            Op::RemI { dst, a, b: rb, at } => {
                let x = get(&mut b, a);
                let y = get(&mut b, rb);
                let ok = b.create_block();
                let fail_block = b.create_block();
                b.set_cold_block(fail_block);
                b.ins().brif(y, ok, &[], fail_block, &[]);
                b.switch_to_block(fail_block);
                fail!(1, at);
                b.switch_to_block(ok);
                // i64::MIN % -1 overflows (and srem would trap): report it.
                let is_min = b.ins().icmp_imm_s(IntCC::Equal, x, i64::MIN);
                let is_neg1 = b.ins().icmp_imm_s(IntCC::Equal, y, -1);
                let both = b.ins().band(is_min, is_neg1);
                let safe = b.create_block();
                let overflow_block = b.create_block();
                b.set_cold_block(overflow_block);
                b.ins().brif(both, overflow_block, &[], safe, &[]);
                b.switch_to_block(overflow_block);
                fail!(3, ip);
                b.switch_to_block(safe);
                let r = b.ins().srem(x, y);
                set(&mut b, dst, r);
            }
            Op::AddII { dst, a, imm } => {
                let x = get(&mut b, a);
                let y = b.ins().iconst(types::I64, imm);
                checked!(dst, x, y, sadd_overflow);
            }
            Op::SubII { dst, a, imm } => {
                let x = get(&mut b, a);
                let y = b.ins().iconst(types::I64, imm);
                checked!(dst, x, y, ssub_overflow);
            }
            Op::IncI { reg } => {
                let x = get(&mut b, reg);
                let r = b.ins().iadd_imm_s(x, 1);
                set(&mut b, reg, r);
            }
            Op::NegI { dst, a } => {
                let x = get(&mut b, a);
                checked!(dst, zero, x, ssub_overflow);
            }
            Op::EqI { dst, a, b: rb } => cmp!(dst, a, rb, IntCC::Equal),
            Op::NeI { dst, a, b: rb } => cmp!(dst, a, rb, IntCC::NotEqual),
            Op::LtI { dst, a, b: rb } => cmp!(dst, a, rb, IntCC::SignedLessThan),
            Op::GtI { dst, a, b: rb } => cmp!(dst, a, rb, IntCC::SignedGreaterThan),
            Op::LeI { dst, a, b: rb } => cmp!(dst, a, rb, IntCC::SignedLessThanOrEqual),
            Op::GeI { dst, a, b: rb } => cmp!(dst, a, rb, IntCC::SignedGreaterThanOrEqual),
            Op::EqII { dst, a, imm } => cmp_imm!(dst, a, imm, IntCC::Equal),
            Op::NeII { dst, a, imm } => cmp_imm!(dst, a, imm, IntCC::NotEqual),
            Op::LtII { dst, a, imm } => cmp_imm!(dst, a, imm, IntCC::SignedLessThan),
            Op::GtII { dst, a, imm } => cmp_imm!(dst, a, imm, IntCC::SignedGreaterThan),
            Op::LeII { dst, a, imm } => cmp_imm!(dst, a, imm, IntCC::SignedLessThanOrEqual),
            Op::GeII { dst, a, imm } => cmp_imm!(dst, a, imm, IntCC::SignedGreaterThanOrEqual),
            Op::AddF { dst, a, b: rb } => fbin!(dst, a, rb, fadd),
            Op::SubF { dst, a, b: rb } => fbin!(dst, a, rb, fsub),
            Op::MulF { dst, a, b: rb } => fbin!(dst, a, rb, fmul),
            Op::DivF { dst, a, b: rb, at } => {
                let x = get(&mut b, a);
                let y = get(&mut b, rb);
                let (fx, fy) = (as_f(&mut b, x), as_f(&mut b, y));
                let zero_f = b.ins().f64const(0.0);
                let is_zero = b.ins().fcmp(FloatCC::Equal, fy, zero_f);
                let ok = b.create_block();
                let fail_block = b.create_block();
                b.set_cold_block(fail_block);
                b.ins().brif(is_zero, fail_block, &[], ok, &[]);
                b.switch_to_block(fail_block);
                fail!(1, at);
                b.switch_to_block(ok);
                let r = b.ins().fdiv(fx, fy);
                let r = from_f(&mut b, r);
                set(&mut b, dst, r);
            }
            Op::RemF { dst, a, b: rb, at } => {
                let x = get(&mut b, a);
                let y = get(&mut b, rb);
                let (fx, fy) = (as_f(&mut b, x), as_f(&mut b, y));
                let zero_f = b.ins().f64const(0.0);
                let is_zero = b.ins().fcmp(FloatCC::Equal, fy, zero_f);
                let ok = b.create_block();
                let fail_block = b.create_block();
                b.set_cold_block(fail_block);
                b.ins().brif(is_zero, fail_block, &[], ok, &[]);
                b.switch_to_block(fail_block);
                fail!(1, at);
                b.switch_to_block(ok);
                // Cranelift has no frem; call the host `fmod` (same result as Rust's `%`).
                let mut sig = module.make_signature();
                sig.params.push(AbiParam::new(types::F64));
                sig.params.push(AbiParam::new(types::F64));
                sig.returns.push(AbiParam::new(types::F64));
                let callee = module
                    .declare_function("spar_fmod", Linkage::Import, &sig)
                    .ok()?;
                let callee = module.declare_func_in_func(callee, b.func);
                let call = b.ins().call(callee, &[fx, fy]);
                let r = b.inst_results(call)[0];
                let r = from_f(&mut b, r);
                set(&mut b, dst, r);
            }
            Op::NegF { dst, a } => {
                let x = get(&mut b, a);
                let fx = as_f(&mut b, x);
                let r = b.ins().fneg(fx);
                let r = from_f(&mut b, r);
                set(&mut b, dst, r);
            }
            Op::EqF { dst, a, b: rb } => fcmp!(dst, a, rb, FloatCC::Equal),
            Op::NeF { dst, a, b: rb } => fcmp!(dst, a, rb, FloatCC::NotEqual),
            Op::LtF { dst, a, b: rb } => fcmp!(dst, a, rb, FloatCC::LessThan),
            Op::GtF { dst, a, b: rb } => fcmp!(dst, a, rb, FloatCC::GreaterThan),
            Op::LeF { dst, a, b: rb } => fcmp!(dst, a, rb, FloatCC::LessThanOrEqual),
            Op::GeF { dst, a, b: rb } => fcmp!(dst, a, rb, FloatCC::GreaterThanOrEqual),
            Op::EqB { dst, a, b: rb } => cmp!(dst, a, rb, IntCC::Equal),
            Op::NeB { dst, a, b: rb } => cmp!(dst, a, rb, IntCC::NotEqual),
            Op::NotB { dst, a } => {
                let x = get(&mut b, a);
                let c = b.ins().icmp_imm_s(IntCC::Equal, x, 0);
                let r = bool_val(&mut b, c);
                set(&mut b, dst, r);
            }
            Op::Jmp { target } => {
                b.ins().jump(blocks[target as usize], &[]);
                terminated = true;
            }
            Op::JmpIfFalse { cond, target } => {
                let c = get(&mut b, cond);
                b.ins()
                    .brif(c, blocks[ip + 1], &[], blocks[target as usize], &[]);
                terminated = true;
            }
            Op::JmpIfTrue { cond, target } => {
                let c = get(&mut b, cond);
                b.ins()
                    .brif(c, blocks[target as usize], &[], blocks[ip + 1], &[]);
                terminated = true;
            }
            Op::JLtI { a, b: rb, target } => branch_cmp!(a, rb, target, IntCC::SignedLessThan),
            Op::JLeI { a, b: rb, target } => {
                branch_cmp!(a, rb, target, IntCC::SignedLessThanOrEqual)
            }
            Op::JGtI { a, b: rb, target } => branch_cmp!(a, rb, target, IntCC::SignedGreaterThan),
            Op::JGeI { a, b: rb, target } => {
                branch_cmp!(a, rb, target, IntCC::SignedGreaterThanOrEqual)
            }
            Op::JEqI { a, b: rb, target } => branch_cmp!(a, rb, target, IntCC::Equal),
            Op::JNeI { a, b: rb, target } => branch_cmp!(a, rb, target, IntCC::NotEqual),
            Op::JLtII { a, imm, target } => branch_cmp_imm!(a, imm, target, IntCC::SignedLessThan),
            Op::JLeII { a, imm, target } => {
                branch_cmp_imm!(a, imm, target, IntCC::SignedLessThanOrEqual)
            }
            Op::JGtII { a, imm, target } => {
                branch_cmp_imm!(a, imm, target, IntCC::SignedGreaterThan)
            }
            Op::JGeII { a, imm, target } => {
                branch_cmp_imm!(a, imm, target, IntCC::SignedGreaterThanOrEqual)
            }
            Op::JEqII { a, imm, target } => branch_cmp_imm!(a, imm, target, IntCC::Equal),
            Op::JNeII { a, imm, target } => branch_cmp_imm!(a, imm, target, IntCC::NotEqual),
            Op::Call {
                dst,
                function,
                first,
            } => {
                let callee = vm.function_at(function as usize)?;
                let callee_id = ids[function as usize]?;
                let callee_ref = module.declare_func_in_func(callee_id, b.func);
                let next_depth = b.ins().iadd_imm_s(depth, 1);
                let mut args = vec![next_depth, ctx_ptr];
                for k in 0..callee.params.len() as u32 {
                    args.push(get(&mut b, first + k));
                }
                let call = b.ins().call(callee_ref, &args);
                let result = b.inst_results(call)[0];
                set(&mut b, dst, result);
                // A callee error is already recorded in the context; add this
                // function as one more caller of it before unwinding.
                let err = b
                    .ins()
                    .load(types::I32, MemFlagsData::trusted(), ctx_ptr, 0);
                let record = b.create_block();
                b.set_cold_block(record);
                b.ins().brif(err, record, &[], blocks[ip + 1], &[]);
                b.switch_to_block(record);
                let count = b
                    .ins()
                    .load(types::I32, MemFlagsData::trusted(), ctx_ptr, 12);
                let cap = b.ins().iconst(types::I32, (MAX_JIT_FRAMES - 1) as i64);
                let below = b.ins().icmp(IntCC::UnsignedLessThan, count, cap);
                let slot = b.ins().select(below, count, cap);
                let slot64 = b.ins().uextend(types::I64, slot);
                let eight = b.ins().iconst(types::I64, 8);
                let offset = b.ins().imul(slot64, eight);
                let entry_ptr = b.ins().iadd(ctx_ptr, offset);
                let caller_fn = b.ins().iconst(types::I32, index as i64);
                let call_at = b.ins().iconst(types::I32, ip as i64);
                b.ins()
                    .store(MemFlagsData::trusted(), caller_fn, entry_ptr, 16);
                b.ins()
                    .store(MemFlagsData::trusted(), call_at, entry_ptr, 20);
                let one = b.ins().iconst(types::I32, 1);
                let next = b.ins().iadd(count, one);
                b.ins().store(MemFlagsData::trusted(), next, ctx_ptr, 12);
                b.ins().jump(propagate, &[]);
                terminated = true;
            }
            Op::Ret { src } => {
                let v = get(&mut b, src);
                b.ins().return_(&[v]);
                terminated = true;
            }
            Op::RetVoid => {
                b.ins().return_(&[zero]);
                terminated = true;
            }
        }
    }
    // Falling off the end (never emitted by the lowerer, but keep the IR valid).
    if !terminated {
        b.ins().jump(blocks[code.len()], &[]);
    }
    b.switch_to_block(blocks[code.len()]);
    b.ins().return_(&[zero]);

    b.switch_to_block(propagate);
    b.ins().return_(&[zero]);

    b.switch_to_block(depth_err);
    {
        let code = b.ins().iconst(types::I32, 2);
        b.ins().store(MemFlagsData::trusted(), code, ctx_ptr, 0);
        b.ins().return_(&[zero]);
    }

    b.seal_all_blocks();
    let frontend = module.target_config();
    b.finalize(frontend);
    Some(())
}
