//! Register bytecode VM for the primitive-typed core of Spar.
//!
//! # Scope
//!
//! Functions whose parameters, locals and return type are all `int`, `float`,
//! `bool` (or `void` for the return), and whose bodies only use `if`, local
//! stores, returns, typed primitive operations and direct calls to other such
//! functions, are lowered once at program-compile time into register bytecode.
//! Every other function keeps running on the tree-walking `Runtime`; the two
//! tiers interoperate at call boundaries (`Runtime::call_direct_inline`).
//!
//! # Invariants
//!
//! * A register is an untagged `u64`: `int` = `i64` bits, `float` = `f64`
//!   bits, `bool` = 0/1. Types are proven by the type checker; the lowerer
//!   only emits typed opcodes, so the VM never inspects tags.
//! * Local slot `i` of a function is register `i`; parameters occupy
//!   registers `0..nparams` (enforced at lowering). Temporaries follow.
//! * Frame layout: a call places its arguments in consecutive caller
//!   registers `first..first+n`; the callee's base is `caller_base + first`,
//!   so its parameters are already in place (Lua-style register windows, no
//!   copying). Everything at/above `first` in the caller is dead at the call.
//! * Calling convention: result is written to `caller_base + dst`.
//! * Integer `+ - *` and `/` wrap on overflow (matches the tree walker's
//!   release-build behaviour; the language does not yet define overflow).
//! * Call depth accounting mirrors the tree walker: a call made at depth `d`
//!   fails with the same "maximum function call depth" error when
//!   `d >= MAX_CALL_DEPTH`.

use crate::ast::SparType;
use crate::compiled::{
    CompiledExpression, CompiledFunction, CompiledStatement, FunctionId, TypedOperation,
};
use crate::error::{Span, SparError};
use crate::evaluator::ConfigValue;
use crate::recursion::MAX_CALL_DEPTH;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Prim {
    Int,
    Float,
    Bool,
    Void,
}

impl Prim {
    fn of(ty: &SparType) -> Option<Prim> {
        match ty {
            SparType::Int => Some(Prim::Int),
            SparType::Float => Some(Prim::Float),
            SparType::Bool => Some(Prim::Bool),
            SparType::Void => Some(Prim::Void),
            _ => None,
        }
    }
}

type Reg = u32;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Op {
    ConstI { dst: Reg, value: i64 },
    ConstF { dst: Reg, bits: u64 },
    Move { dst: Reg, src: Reg },
    AddI { dst: Reg, a: Reg, b: Reg },
    SubI { dst: Reg, a: Reg, b: Reg },
    MulI { dst: Reg, a: Reg, b: Reg },
    DivI { dst: Reg, a: Reg, b: Reg, at: u32 },
    AddII { dst: Reg, a: Reg, imm: i64 },
    SubII { dst: Reg, a: Reg, imm: i64 },
    NegI { dst: Reg, a: Reg },
    EqI { dst: Reg, a: Reg, b: Reg },
    NeI { dst: Reg, a: Reg, b: Reg },
    LtI { dst: Reg, a: Reg, b: Reg },
    GtI { dst: Reg, a: Reg, b: Reg },
    LeI { dst: Reg, a: Reg, b: Reg },
    GeI { dst: Reg, a: Reg, b: Reg },
    EqII { dst: Reg, a: Reg, imm: i64 },
    NeII { dst: Reg, a: Reg, imm: i64 },
    LtII { dst: Reg, a: Reg, imm: i64 },
    GtII { dst: Reg, a: Reg, imm: i64 },
    LeII { dst: Reg, a: Reg, imm: i64 },
    GeII { dst: Reg, a: Reg, imm: i64 },
    AddF { dst: Reg, a: Reg, b: Reg },
    SubF { dst: Reg, a: Reg, b: Reg },
    MulF { dst: Reg, a: Reg, b: Reg },
    DivF { dst: Reg, a: Reg, b: Reg, at: u32 },
    NegF { dst: Reg, a: Reg },
    EqF { dst: Reg, a: Reg, b: Reg },
    NeF { dst: Reg, a: Reg, b: Reg },
    LtF { dst: Reg, a: Reg, b: Reg },
    GtF { dst: Reg, a: Reg, b: Reg },
    LeF { dst: Reg, a: Reg, b: Reg },
    GeF { dst: Reg, a: Reg, b: Reg },
    EqB { dst: Reg, a: Reg, b: Reg },
    NeB { dst: Reg, a: Reg, b: Reg },
    NotB { dst: Reg, a: Reg },
    // Fused compare-and-branch: jump when `a OP b` (or `a OP imm`) holds.
    JLtI { a: Reg, b: Reg, target: u32 },
    JLeI { a: Reg, b: Reg, target: u32 },
    JGtI { a: Reg, b: Reg, target: u32 },
    JGeI { a: Reg, b: Reg, target: u32 },
    JEqI { a: Reg, b: Reg, target: u32 },
    JNeI { a: Reg, b: Reg, target: u32 },
    JLtII { a: Reg, imm: i64, target: u32 },
    JLeII { a: Reg, imm: i64, target: u32 },
    JGtII { a: Reg, imm: i64, target: u32 },
    JGeII { a: Reg, imm: i64, target: u32 },
    JEqII { a: Reg, imm: i64, target: u32 },
    JNeII { a: Reg, imm: i64, target: u32 },
    Jmp { target: u32 },
    JmpIfFalse { cond: Reg, target: u32 },
    JmpIfTrue { cond: Reg, target: u32 },
    Call { dst: Reg, function: u32, first: Reg },
    Ret { src: Reg },
    RetVoid,
}

pub(crate) struct VmFunction {
    pub(crate) name: String,
    pub(crate) code: Vec<Op>,
    /// Source span for the op at the same index that can raise an error.
    spans: Vec<Span>,
    /// Registers this function needs (params + locals + temporaries).
    nregs: u32,
    pub(crate) params: Vec<Prim>,
    pub(crate) ret: Prim,
}

pub(crate) struct VmProgram {
    functions: Vec<Option<VmFunction>>,
    /// Native code for the lowered functions, compiled on first use.
    #[cfg(not(target_arch = "wasm32"))]
    jit: std::sync::OnceLock<Option<crate::jit::JitProgram>>,
    /// Why each non-lowered function stayed on the tree walker.
    reasons: Vec<Option<String>>,
    names: Vec<String>,
}

impl VmFunction {
    pub(crate) fn nregs(&self) -> usize {
        self.nregs as usize
    }
}

impl VmProgram {
    pub(crate) fn empty() -> Self {
        Self {
            functions: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            jit: std::sync::OnceLock::new(),
            reasons: Vec::new(),
            names: Vec::new(),
        }
    }

    pub(crate) fn function_count(&self) -> usize {
        self.functions.len()
    }

    pub(crate) fn function_at(&self, index: usize) -> Option<&VmFunction> {
        self.functions.get(index)?.as_ref()
    }

    #[inline]
    pub(crate) fn get(&self, id: FunctionId) -> Option<&VmFunction> {
        self.functions.get(id.0 as usize)?.as_ref()
    }

    /// Lowers every eligible function. `functions` must contain each
    /// function of the program exactly once.
    pub(crate) fn build<'a>(
        functions: impl Iterator<Item = &'a CompiledFunction>,
        function_count: usize,
    ) -> Self {
        // Developer escape hatch for A/B benchmarking and differential runs.
        if std::env::var_os("SPAR_DISABLE_VM").is_some() {
            return Self::empty();
        }
        let all: Vec<&CompiledFunction> = functions.collect();
        let mut by_id: Vec<Option<&CompiledFunction>> = vec![None; function_count];
        for f in &all {
            by_id[f.id.0 as usize] = Some(f);
        }
        // Optimistic fixpoint: assume every signature-eligible function is
        // lowerable, then drop any whose body fails to lower against that
        // assumption, until stable.
        let mut eligible: Vec<bool> = by_id
            .iter()
            .map(|f| f.map(signature_eligible).unwrap_or(false))
            .collect();
        let mut lowered: Vec<Option<VmFunction>> = (0..function_count).map(|_| None).collect();
        let mut reasons: Vec<Option<String>> = vec![None; function_count];
        loop {
            let mut changed = false;
            for (index, f) in by_id.iter().enumerate() {
                if !eligible[index] {
                    lowered[index] = None;
                    continue;
                }
                let f = f.expect("eligible implies present");
                match Lowerer::lower(f, &by_id, &eligible) {
                    Ok(vm) => lowered[index] = Some(vm),
                    Err(why) => {
                        eligible[index] = false;
                        lowered[index] = None;
                        reasons[index] = Some(why);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        for (index, f) in by_id.iter().enumerate() {
            if let (Some(f), false) = (f, eligible[index]) {
                if reasons[index].is_none() {
                    reasons[index] = Some(signature_reason(f));
                }
            }
        }
        let names = by_id
            .iter()
            .map(|f| f.map(|f| f.name.clone()).unwrap_or_default())
            .collect();
        Self {
            functions: lowered,
            #[cfg(not(target_arch = "wasm32"))]
            jit: std::sync::OnceLock::new(),
            reasons,
            names,
        }
    }

    /// Human-readable listing of every lowered function (`spar dis`).
    pub(crate) fn disassemble(&self) -> String {
        let mut out = String::new();
        for (index, f) in self.functions.iter().enumerate() {
            let Some(f) = f else { continue };
            out.push_str(&format!(
                "fn {} (id {index}) params={:?} ret={:?} regs={}\n",
                f.name, f.params, f.ret, f.nregs
            ));
            for (ip, op) in f.code.iter().enumerate() {
                out.push_str(&format!("  {ip:04} {op:?}\n"));
            }
        }
        for (index, why) in self.reasons.iter().enumerate() {
            if let Some(why) = why {
                out.push_str(&format!(
                    "fn {} (id {index}): tree walker ({why})\n",
                    self.names[index]
                ));
            }
        }
        out
    }
}

fn signature_reason(f: &CompiledFunction) -> String {
    if f.is_async {
        return "async function".into();
    }
    if Prim::of(&f.return_type).is_none() {
        return "non-primitive return type".into();
    }
    "non-primitive parameter".into()
}

fn signature_eligible(f: &CompiledFunction) -> bool {
    if f.is_async {
        return false;
    }
    if Prim::of(&f.return_type).is_none() {
        return false;
    }
    f.parameter_slots.iter().enumerate().all(|(i, slot)| {
        slot.0 as usize == i
            && f.local_layout
                .types
                .get(i)
                .and_then(Prim::of)
                .map(|p| p != Prim::Void)
                .unwrap_or(false)
    })
}

struct Lowerer<'a> {
    function: &'a CompiledFunction,
    by_id: &'a [Option<&'a CompiledFunction>],
    eligible: &'a [bool],
    code: Vec<Op>,
    spans: Vec<Span>,
    slot_count: u32,
    /// First register available for expression temporaries. Rises while a
    /// loop is being lowered so its hidden counters survive statement resets.
    base_temp: u32,
    next_temp: u32,
    max_reg: u32,
    loops: Vec<LoopPatches>,
    /// First reason lowering gave up (for `spar dis`).
    why: Option<String>,
}

/// Jumps out of / back into the innermost loop, patched when it is closed.
struct LoopPatches {
    breaks: Vec<usize>,
    continues: Vec<usize>,
}

impl<'a> Lowerer<'a> {
    fn lower(
        function: &'a CompiledFunction,
        by_id: &'a [Option<&'a CompiledFunction>],
        eligible: &'a [bool],
    ) -> Result<VmFunction, String> {
        let slot_count = function.slot_count as u32;
        let mut l = Lowerer {
            function,
            by_id,
            eligible,
            code: Vec::new(),
            spans: Vec::new(),
            slot_count,
            base_temp: slot_count,
            next_temp: slot_count,
            max_reg: slot_count,
            loops: Vec::new(),
            why: None,
        };
        if l.statements(&function.body).is_none() {
            return Err(l.why.unwrap_or_else(|| "unsupported construct".into()));
        }
        let Some(ret) = Prim::of(&function.return_type) else {
            return Err("non-primitive return type".into());
        };
        if ret == Prim::Void {
            l.emit(Op::RetVoid, None);
        } else if !matches!(
            function.body.last(),
            Some(CompiledStatement::Return(Some(_), _))
        ) {
            return Err("value-returning function does not end in `return`".into());
        }
        let params = function
            .parameter_slots
            .iter()
            .map(|s| Prim::of(&function.local_layout.types[s.0 as usize]))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| "non-primitive parameter".to_string())?;
        Ok(VmFunction {
            name: function.name.clone(),
            code: l.code,
            spans: l.spans,
            nregs: l.max_reg + 1,
            params,
            ret,
        })
    }

    /// Records why lowering failed (first reason wins) and aborts.
    fn no<T>(&mut self, why: impl Into<String>) -> Option<T> {
        if self.why.is_none() {
            self.why = Some(why.into());
        }
        None
    }

    fn emit(&mut self, op: Op, span: Option<&Span>) -> usize {
        self.code.push(op);
        self.spans.push(span.cloned().unwrap_or_else(Span::dummy));
        self.code.len() - 1
    }

    fn temp(&mut self) -> Reg {
        let r = self.next_temp;
        self.next_temp += 1;
        self.max_reg = self.max_reg.max(self.next_temp);
        r
    }

    fn slot_prim(&self, slot: u32) -> Option<Prim> {
        self.function
            .local_layout
            .types
            .get(slot as usize)
            .and_then(Prim::of)
            .filter(|p| *p != Prim::Void)
    }

    fn statements(&mut self, body: &[CompiledStatement]) -> Option<()> {
        for statement in body {
            self.next_temp = self.base_temp;
            self.statement(statement)?;
        }
        Some(())
    }

    fn statement(&mut self, statement: &CompiledStatement) -> Option<()> {
        match statement {
            CompiledStatement::StoreLocal { slot, value, .. } => {
                if self.slot_prim(slot.0).is_none() {
                    return self.no("non-primitive local");
                }
                // `&&`/`||` re-read their destination after writing it, so
                // they go through a temporary; everything else writes last.
                let direct = !matches!(
                    value,
                    CompiledExpression::Operation {
                        operation: TypedOperation::BoolAnd | TypedOperation::BoolOr,
                        ..
                    }
                );
                let r = self.expr(value, direct.then_some(slot.0))?;
                if r != slot.0 {
                    self.emit(
                        Op::Move {
                            dst: slot.0,
                            src: r,
                        },
                        None,
                    );
                }
                Some(())
            }
            CompiledStatement::Expression(expression, _) => {
                self.expr(expression, None)?;
                Some(())
            }
            CompiledStatement::If {
                condition,
                then_body,
                else_body,
                ..
            } => {
                let jump_else = self.branch_if_false(condition)?;
                self.statements(then_body)?;
                if else_body.is_empty() {
                    let end = self.code.len() as u32;
                    self.patch(jump_else, end);
                } else {
                    let jump_end = self.emit(Op::Jmp { target: 0 }, None);
                    let else_start = self.code.len() as u32;
                    self.patch(jump_else, else_start);
                    self.statements(else_body)?;
                    let end = self.code.len() as u32;
                    self.patch(jump_end, end);
                }
                Some(())
            }
            CompiledStatement::For {
                index_slot,
                value_slot,
                iterable,
                body,
                ..
            } => self.range_loop(*index_slot, *value_slot, iterable, body),
            CompiledStatement::Break(_) => {
                let at = self.emit(Op::Jmp { target: 0 }, None);
                self.loops.last_mut()?.breaks.push(at);
                Some(())
            }
            CompiledStatement::Continue(_) => {
                let at = self.emit(Op::Jmp { target: 0 }, None);
                self.loops.last_mut()?.continues.push(at);
                Some(())
            }
            CompiledStatement::Return(Some(value), _) => {
                let src = self.expr(value, None)?;
                self.emit(Op::Ret { src }, None);
                Some(())
            }
            CompiledStatement::Return(None, _) => {
                if Prim::of(&self.function.return_type)? != Prim::Void {
                    return None;
                }
                self.emit(Op::RetVoid, None);
                Some(())
            }
            other => self.no(format!("unsupported statement: {}", stmt_kind(other))),
        }
    }

    /// `for i in range(end: e)` / `rangeFrom(start: s, end: e)` as a counting
    /// loop over registers; the list is never materialised. The prelude's
    /// `range`/`rangeFrom` names are reserved (user declarations are rejected),
    /// so matching them by name is a compile-time intrinsic, not a guess.
    fn range_loop(
        &mut self,
        index_slot: Option<crate::compiled::LocalSlot>,
        value_slot: crate::compiled::LocalSlot,
        iterable: &CompiledExpression,
        body: &[CompiledStatement],
    ) -> Option<()> {
        let CompiledExpression::DirectCall {
            function,
            arguments,
            ..
        } = iterable
        else {
            return self.no("for over non-range iterable");
        };
        let callee = self.by_id.get(function.0 as usize).copied().flatten()?;
        if callee.key.group.is_some()
            || arguments
                .iter()
                .any(|a| matches!(a, CompiledExpression::DefaultArgument(_)))
        {
            return None;
        }
        let (start, end) = match (callee.name.as_str(), arguments.as_slice()) {
            ("range", [end]) => (None, end),
            ("rangeFrom", [start, end]) => (Some(start), end),
            _ => return None,
        };
        if self.slot_prim(value_slot.0)? != Prim::Int {
            return None;
        }
        if let Some(slot) = index_slot {
            if self.slot_prim(slot.0)? != Prim::Int {
                return None;
            }
        }
        // Hidden registers: current value, end bound, enumeration index.
        let cur = self.base_temp;
        let stop = cur + 1;
        let idx = cur + 2;
        self.base_temp += 3;
        self.next_temp = self.base_temp;
        self.max_reg = self.max_reg.max(self.next_temp);
        match start {
            Some(start) => {
                self.expr(start, Some(cur))?;
            }
            None => {
                self.emit(Op::ConstI { dst: cur, value: 0 }, None);
            }
        }
        self.expr(end, Some(stop))?;
        self.emit(Op::ConstI { dst: idx, value: 0 }, None);
        let top = self.code.len() as u32;
        let exit_jump = self.emit(
            Op::JGeI {
                a: cur,
                b: stop,
                target: 0,
            },
            None,
        );
        self.emit(
            Op::Move {
                dst: value_slot.0,
                src: cur,
            },
            None,
        );
        if let Some(slot) = index_slot {
            self.emit(
                Op::Move {
                    dst: slot.0,
                    src: idx,
                },
                None,
            );
        }
        self.loops.push(LoopPatches {
            breaks: Vec::new(),
            continues: Vec::new(),
        });
        self.statements(body)?;
        let patches = self.loops.pop()?;
        let cont = self.code.len() as u32;
        for at in patches.continues {
            self.patch(at, cont);
        }
        self.emit(
            Op::AddII {
                dst: cur,
                a: cur,
                imm: 1,
            },
            None,
        );
        self.emit(
            Op::AddII {
                dst: idx,
                a: idx,
                imm: 1,
            },
            None,
        );
        self.emit(Op::Jmp { target: top }, None);
        let exit = self.code.len() as u32;
        self.patch(exit_jump, exit);
        for at in patches.breaks {
            self.patch(at, exit);
        }
        self.base_temp -= 3;
        Some(())
    }

    /// Emits a jump taken when `condition` is false (target patched later),
    /// fusing an int comparison into a single compare-and-branch op.
    fn branch_if_false(&mut self, condition: &CompiledExpression) -> Option<usize> {
        use TypedOperation as T;
        if let CompiledExpression::Operation {
            operation,
            operands,
            ..
        } = condition
        {
            if let [left, right] = operands.as_slice() {
                if matches!(
                    operation,
                    T::IntLt | T::IntLtEq | T::IntGt | T::IntGtEq | T::IntEq | T::IntNotEq
                ) {
                    let a = self.expr(left, None)?;
                    // `a OP b` is false exactly when `a NEGATED-OP b` holds.
                    if let CompiledExpression::Constant(ConfigValue::Int(imm), _) = right {
                        let imm = *imm;
                        let op = match operation {
                            T::IntLt => Op::JGeII { a, imm, target: 0 },
                            T::IntLtEq => Op::JGtII { a, imm, target: 0 },
                            T::IntGt => Op::JLeII { a, imm, target: 0 },
                            T::IntGtEq => Op::JLtII { a, imm, target: 0 },
                            T::IntEq => Op::JNeII { a, imm, target: 0 },
                            _ => Op::JEqII { a, imm, target: 0 },
                        };
                        return Some(self.emit(op, None));
                    }
                    let b = self.expr(right, None)?;
                    let op = match operation {
                        T::IntLt => Op::JGeI { a, b, target: 0 },
                        T::IntLtEq => Op::JGtI { a, b, target: 0 },
                        T::IntGt => Op::JLeI { a, b, target: 0 },
                        T::IntGtEq => Op::JLtI { a, b, target: 0 },
                        T::IntEq => Op::JNeI { a, b, target: 0 },
                        _ => Op::JEqI { a, b, target: 0 },
                    };
                    return Some(self.emit(op, None));
                }
            }
        }
        let cond = self.expr(condition, None)?;
        Some(self.emit(Op::JmpIfFalse { cond, target: 0 }, None))
    }

    fn patch(&mut self, at: usize, target: u32) {
        match &mut self.code[at] {
            Op::Jmp { target: t }
            | Op::JLtI { target: t, .. }
            | Op::JLeI { target: t, .. }
            | Op::JGtI { target: t, .. }
            | Op::JGeI { target: t, .. }
            | Op::JEqI { target: t, .. }
            | Op::JNeI { target: t, .. }
            | Op::JLtII { target: t, .. }
            | Op::JLeII { target: t, .. }
            | Op::JGtII { target: t, .. }
            | Op::JGeII { target: t, .. }
            | Op::JEqII { target: t, .. }
            | Op::JNeII { target: t, .. }
            | Op::JmpIfFalse { target: t, .. }
            | Op::JmpIfTrue { target: t, .. } => *t = target,
            _ => unreachable!("patching a non-jump"),
        }
    }

    /// Lowers `expression`; the result lives in the returned register, which
    /// is `want` when given.
    fn expr(&mut self, expression: &CompiledExpression, want: Option<Reg>) -> Option<Reg> {
        match expression {
            CompiledExpression::Local(slot, _) => {
                if self.slot_prim(slot.0).is_none() {
                    return self.no("non-primitive local");
                }
                match want {
                    Some(dst) if dst != slot.0 => {
                        self.emit(Op::Move { dst, src: slot.0 }, None);
                        Some(dst)
                    }
                    Some(dst) => Some(dst),
                    None => Some(slot.0),
                }
            }
            CompiledExpression::Constant(value, _) => {
                let dst = want.unwrap_or_else(|| self.temp());
                match value {
                    ConfigValue::Int(value) => {
                        self.emit(Op::ConstI { dst, value: *value }, None);
                    }
                    ConfigValue::Float(value) => {
                        self.emit(
                            Op::ConstF {
                                dst,
                                bits: value.to_bits(),
                            },
                            None,
                        );
                    }
                    ConfigValue::Bool(value) => {
                        self.emit(
                            Op::ConstI {
                                dst,
                                value: *value as i64,
                            },
                            None,
                        );
                    }
                    _ => return None,
                }
                Some(dst)
            }
            CompiledExpression::Operation {
                operation,
                operands,
                span,
            } => self.operation(*operation, operands, span, want),
            CompiledExpression::DirectCall {
                function,
                arguments,
                span,
                ..
            } => {
                let callee = self.by_id.get(function.0 as usize).copied().flatten()?;
                if !self
                    .eligible
                    .get(function.0 as usize)
                    .copied()
                    .unwrap_or(false)
                    || arguments.len() != callee.parameter_slots.len()
                    || arguments
                        .iter()
                        .any(|a| matches!(a, CompiledExpression::DefaultArgument(_)))
                {
                    return None;
                }
                let dst = want.unwrap_or_else(|| self.temp());
                let first = self.next_temp;
                self.next_temp += arguments.len() as u32;
                self.max_reg = self.max_reg.max(self.next_temp);
                for (index, argument) in arguments.iter().enumerate() {
                    let target = first + index as u32;
                    let r = self.expr(argument, Some(target))?;
                    debug_assert_eq!(r, target);
                }
                self.emit(
                    Op::Call {
                        dst,
                        function: function.0,
                        first,
                    },
                    Some(span),
                );
                // The argument window is dead once the call returns.
                self.next_temp = first.max(dst + 1).max(self.base_temp);
                Some(dst)
            }
            other => self.no(format!("unsupported expression: {}", expr_kind(other))),
        }
    }

    fn operation(
        &mut self,
        operation: TypedOperation,
        operands: &[CompiledExpression],
        span: &Span,
        want: Option<Reg>,
    ) -> Option<Reg> {
        use TypedOperation as T;
        match (operation, operands) {
            (T::BoolAnd | T::BoolOr, [left, right]) => {
                let dst = want.unwrap_or_else(|| self.temp());
                self.expr(left, Some(dst))?;
                let jump = if operation == T::BoolAnd {
                    self.emit(
                        Op::JmpIfFalse {
                            cond: dst,
                            target: 0,
                        },
                        None,
                    )
                } else {
                    self.emit(
                        Op::JmpIfTrue {
                            cond: dst,
                            target: 0,
                        },
                        None,
                    )
                };
                self.expr(right, Some(dst))?;
                let end = self.code.len() as u32;
                self.patch(jump, end);
                Some(dst)
            }
            (T::IntNeg | T::FloatNeg | T::BoolNot, [operand]) => {
                let a = self.expr(operand, None)?;
                let dst = want.unwrap_or_else(|| self.temp());
                let op = match operation {
                    T::IntNeg => Op::NegI { dst, a },
                    T::FloatNeg => Op::NegF { dst, a },
                    _ => Op::NotB { dst, a },
                };
                self.emit(op, None);
                Some(dst)
            }
            (_, [left, right]) => {
                // Immediate forms for `int op constant`.
                if let CompiledExpression::Constant(ConfigValue::Int(imm), _) = right {
                    let imm = *imm;
                    let make: Option<fn(Reg, Reg, i64) -> Op> = match operation {
                        T::IntAdd => Some(|dst, a, imm| Op::AddII { dst, a, imm }),
                        T::IntSub => Some(|dst, a, imm| Op::SubII { dst, a, imm }),
                        T::IntEq => Some(|dst, a, imm| Op::EqII { dst, a, imm }),
                        T::IntNotEq => Some(|dst, a, imm| Op::NeII { dst, a, imm }),
                        T::IntLt => Some(|dst, a, imm| Op::LtII { dst, a, imm }),
                        T::IntGt => Some(|dst, a, imm| Op::GtII { dst, a, imm }),
                        T::IntLtEq => Some(|dst, a, imm| Op::LeII { dst, a, imm }),
                        T::IntGtEq => Some(|dst, a, imm| Op::GeII { dst, a, imm }),
                        _ => None,
                    };
                    if let Some(make) = make {
                        let a = self.expr(left, None)?;
                        let dst = want.unwrap_or_else(|| self.temp());
                        self.emit(make(dst, a, imm), None);
                        return Some(dst);
                    }
                }
                let a = self.expr(left, None)?;
                let b = self.expr(right, None)?;
                let dst = want.unwrap_or_else(|| self.temp());
                let at = self.code.len() as u32;
                let op = match operation {
                    T::IntAdd => Op::AddI { dst, a, b },
                    T::IntSub => Op::SubI { dst, a, b },
                    T::IntMul => Op::MulI { dst, a, b },
                    T::IntDiv => Op::DivI { dst, a, b, at },
                    T::IntEq => Op::EqI { dst, a, b },
                    T::IntNotEq => Op::NeI { dst, a, b },
                    T::IntLt => Op::LtI { dst, a, b },
                    T::IntGt => Op::GtI { dst, a, b },
                    T::IntLtEq => Op::LeI { dst, a, b },
                    T::IntGtEq => Op::GeI { dst, a, b },
                    T::FloatAdd => Op::AddF { dst, a, b },
                    T::FloatSub => Op::SubF { dst, a, b },
                    T::FloatMul => Op::MulF { dst, a, b },
                    T::FloatDiv => Op::DivF { dst, a, b, at },
                    T::FloatEq => Op::EqF { dst, a, b },
                    T::FloatNotEq => Op::NeF { dst, a, b },
                    T::FloatLt => Op::LtF { dst, a, b },
                    T::FloatGt => Op::GtF { dst, a, b },
                    T::FloatLtEq => Op::LeF { dst, a, b },
                    T::FloatGtEq => Op::GeF { dst, a, b },
                    T::BoolEq => Op::EqB { dst, a, b },
                    T::BoolNotEq => Op::NeB { dst, a, b },
                    _ => return None,
                };
                self.emit(op, Some(span));
                Some(dst)
            }
            _ => self.no("unsupported operation (string/dynamic/shell/fallback)"),
        }
    }
}

fn stmt_kind(s: &CompiledStatement) -> &'static str {
    match s {
        CompiledStatement::StoreLocal { .. } => "store local",
        CompiledStatement::StoreGlobal { .. } => "store global",
        CompiledStatement::StoreFieldLocal { .. } => "store field (local)",
        CompiledStatement::StoreFieldGlobal { .. } => "store field (global)",
        CompiledStatement::Expression(..) => "expression",
        CompiledStatement::If { .. } => "if",
        CompiledStatement::For { .. } => "for over non-range iterable",
        CompiledStatement::Return(..) => "return",
        CompiledStatement::Break(_) => "break",
        CompiledStatement::Continue(_) => "continue",
        CompiledStatement::Try { .. } => "try/catch",
    }
}

fn expr_kind(e: &CompiledExpression) -> &'static str {
    use CompiledExpression as E;
    match e {
        E::Constant(..) => "non-primitive constant",
        E::Local(..) => "local",
        E::Global(..) => "global",
        E::DefaultArgument(_) => "default argument",
        E::DirectCall { .. } => "call",
        E::MethodCall { .. } => "method call",
        E::FunctionRef { .. } => "function reference",
        E::Closure { .. } => "closure",
        E::Invoke { .. } => "closure invoke",
        E::HostCall { .. } => "host call",
        E::NativeCall { .. } => "native call (println, len, ...)",
        E::Panic { .. } => "panic",
        E::ImportedValue { .. } => "imported value",
        E::StructConstruct { .. } => "struct construct",
        E::List(..) => "list literal",
        E::Object(..) => "object literal",
        E::Map(..) => "map literal",
        E::Operation { .. } => "operation",
        E::Await { .. } => "await",
        E::Index { .. } => "index",
        E::Field { .. } => "field access",
        E::Interpolation(..) => "string interpolation",
        E::Comprehension { .. } => "comprehension",
        E::Shell(_)
        | E::MixedShell(_)
        | E::ShellProgram { .. }
        | E::ExecShell(_)
        | E::CommandSubstitution(_) => "shell",
    }
}

/// Reusable VM working memory (register stack and suspended call frames).
#[derive(Default)]
pub(crate) struct VmState {
    stack: Vec<u64>,
    frames: Vec<SavedFrame>,
}

struct SavedFrame {
    function: u32,
    ip: u32,
    base: u32,
    dst: u32,
}

#[cfg(not(target_arch = "wasm32"))]
impl VmProgram {
    /// Native code, compiled once on first use. `SPAR_NO_JIT=1` keeps the
    /// bytecode interpreter; any compile failure does the same.
    fn native(&self) -> Option<&crate::jit::JitProgram> {
        self.jit
            .get_or_init(|| {
                if std::env::var_os("SPAR_NO_JIT").is_some() {
                    return None;
                }
                crate::jit::JitProgram::compile(self)
            })
            .as_ref()
    }

    pub(crate) fn run_native(
        &self,
        entry: FunctionId,
        args: &[u64],
        depth: usize,
    ) -> Option<Result<u64, SparError>> {
        let jit = self.native()?;
        let mut ctx = crate::jit::JitCtx::default();
        let result = jit.call(entry.0 as usize, args, depth as i64, &mut ctx)?;
        Some(match ctx.err {
            0 => Ok(result),
            1 => {
                let span = self
                    .function_at(ctx.err_fn as usize)
                    .and_then(|f| f.spans.get(ctx.err_at as usize))
                    .cloned()
                    .unwrap_or_else(Span::dummy);
                Err(division_by_zero(&span))
            }
            _ => Err(depth_error()),
        })
    }
}

impl VmProgram {
    /// Runs `entry` with `args` already converted to register bits.
    /// `depth` is the caller's current call depth.
    pub(crate) fn run(
        &self,
        state: &mut VmState,
        entry: FunctionId,
        args: &[u64],
        depth: usize,
    ) -> Result<u64, SparError> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(result) = self.run_native(entry, args, depth) {
            return result;
        }
        self.run_interpreted(state, entry, args, depth)
    }

    /// The bytecode interpreter loop (the reference for the native backend).
    pub(crate) fn run_interpreted(
        &self,
        state: &mut VmState,
        entry: FunctionId,
        args: &[u64],
        depth: usize,
    ) -> Result<u64, SparError> {
        if depth >= MAX_CALL_DEPTH {
            return Err(depth_error());
        }
        let mut current = self.get(entry).expect("entry must be lowered");
        let mut function = entry.0;
        let mut base: usize = 0;
        let mut ip: usize = 0;
        state.frames.clear();
        let need = current.nregs as usize;
        if state.stack.len() < need {
            state.stack.resize(need.max(1024), 0);
        }
        state.stack[..args.len()].copy_from_slice(args);
        let stack = &mut state.stack;
        let frames = &mut state.frames;

        macro_rules! r {
            ($x:expr) => {
                stack[base + $x as usize]
            };
        }
        macro_rules! i {
            ($x:expr) => {
                stack[base + $x as usize] as i64
            };
        }
        macro_rules! f {
            ($x:expr) => {
                f64::from_bits(stack[base + $x as usize])
            };
        }
        macro_rules! set {
            ($dst:expr, $v:expr) => {
                stack[base + $dst as usize] = $v
            };
        }

        loop {
            match current.code[ip] {
                Op::ConstI { dst, value } => set!(dst, value as u64),
                Op::ConstF { dst, bits } => set!(dst, bits),
                Op::Move { dst, src } => set!(dst, r!(src)),
                Op::AddI { dst, a, b } => set!(dst, i!(a).wrapping_add(i!(b)) as u64),
                Op::SubI { dst, a, b } => set!(dst, i!(a).wrapping_sub(i!(b)) as u64),
                Op::MulI { dst, a, b } => set!(dst, i!(a).wrapping_mul(i!(b)) as u64),
                Op::DivI { dst, a, b, at } => {
                    let divisor = i!(b);
                    if divisor == 0 {
                        return Err(division_by_zero(&current.spans[at as usize]));
                    }
                    set!(dst, i!(a).wrapping_div(divisor) as u64)
                }
                Op::AddII { dst, a, imm } => set!(dst, i!(a).wrapping_add(imm) as u64),
                Op::SubII { dst, a, imm } => set!(dst, i!(a).wrapping_sub(imm) as u64),
                Op::NegI { dst, a } => set!(dst, i!(a).wrapping_neg() as u64),
                Op::EqI { dst, a, b } => set!(dst, (i!(a) == i!(b)) as u64),
                Op::NeI { dst, a, b } => set!(dst, (i!(a) != i!(b)) as u64),
                Op::LtI { dst, a, b } => set!(dst, (i!(a) < i!(b)) as u64),
                Op::GtI { dst, a, b } => set!(dst, (i!(a) > i!(b)) as u64),
                Op::LeI { dst, a, b } => set!(dst, (i!(a) <= i!(b)) as u64),
                Op::GeI { dst, a, b } => set!(dst, (i!(a) >= i!(b)) as u64),
                Op::EqII { dst, a, imm } => set!(dst, (i!(a) == imm) as u64),
                Op::NeII { dst, a, imm } => set!(dst, (i!(a) != imm) as u64),
                Op::LtII { dst, a, imm } => set!(dst, (i!(a) < imm) as u64),
                Op::GtII { dst, a, imm } => set!(dst, (i!(a) > imm) as u64),
                Op::LeII { dst, a, imm } => set!(dst, (i!(a) <= imm) as u64),
                Op::GeII { dst, a, imm } => set!(dst, (i!(a) >= imm) as u64),
                Op::AddF { dst, a, b } => set!(dst, (f!(a) + f!(b)).to_bits()),
                Op::SubF { dst, a, b } => set!(dst, (f!(a) - f!(b)).to_bits()),
                Op::MulF { dst, a, b } => set!(dst, (f!(a) * f!(b)).to_bits()),
                Op::DivF { dst, a, b, at } => {
                    let divisor = f!(b);
                    if divisor == 0.0 {
                        return Err(division_by_zero(&current.spans[at as usize]));
                    }
                    set!(dst, (f!(a) / divisor).to_bits())
                }
                Op::NegF { dst, a } => set!(dst, (-f!(a)).to_bits()),
                Op::EqF { dst, a, b } => set!(dst, (f!(a) == f!(b)) as u64),
                Op::NeF { dst, a, b } => set!(dst, (f!(a) != f!(b)) as u64),
                Op::LtF { dst, a, b } => set!(dst, (f!(a) < f!(b)) as u64),
                Op::GtF { dst, a, b } => set!(dst, (f!(a) > f!(b)) as u64),
                Op::LeF { dst, a, b } => set!(dst, (f!(a) <= f!(b)) as u64),
                Op::GeF { dst, a, b } => set!(dst, (f!(a) >= f!(b)) as u64),
                Op::EqB { dst, a, b } => set!(dst, (r!(a) == r!(b)) as u64),
                Op::NeB { dst, a, b } => set!(dst, (r!(a) != r!(b)) as u64),
                Op::NotB { dst, a } => set!(dst, (r!(a) == 0) as u64),
                Op::JLtI { a, b, target } => {
                    if i!(a) < i!(b) {
                        ip = target as usize;
                        continue;
                    }
                }
                Op::JLeI { a, b, target } => {
                    if i!(a) <= i!(b) {
                        ip = target as usize;
                        continue;
                    }
                }
                Op::JGtI { a, b, target } => {
                    if i!(a) > i!(b) {
                        ip = target as usize;
                        continue;
                    }
                }
                Op::JGeI { a, b, target } => {
                    if i!(a) >= i!(b) {
                        ip = target as usize;
                        continue;
                    }
                }
                Op::JEqI { a, b, target } => {
                    if i!(a) == i!(b) {
                        ip = target as usize;
                        continue;
                    }
                }
                Op::JNeI { a, b, target } => {
                    if i!(a) != i!(b) {
                        ip = target as usize;
                        continue;
                    }
                }
                Op::JLtII { a, imm, target } => {
                    if i!(a) < imm {
                        ip = target as usize;
                        continue;
                    }
                }
                Op::JLeII { a, imm, target } => {
                    if i!(a) <= imm {
                        ip = target as usize;
                        continue;
                    }
                }
                Op::JGtII { a, imm, target } => {
                    if i!(a) > imm {
                        ip = target as usize;
                        continue;
                    }
                }
                Op::JGeII { a, imm, target } => {
                    if i!(a) >= imm {
                        ip = target as usize;
                        continue;
                    }
                }
                Op::JEqII { a, imm, target } => {
                    if i!(a) == imm {
                        ip = target as usize;
                        continue;
                    }
                }
                Op::JNeII { a, imm, target } => {
                    if i!(a) != imm {
                        ip = target as usize;
                        continue;
                    }
                }
                Op::Jmp { target } => {
                    ip = target as usize;
                    continue;
                }
                Op::JmpIfFalse { cond, target } => {
                    if r!(cond) == 0 {
                        ip = target as usize;
                        continue;
                    }
                }
                Op::JmpIfTrue { cond, target } => {
                    if r!(cond) != 0 {
                        ip = target as usize;
                        continue;
                    }
                }
                Op::Call {
                    dst,
                    function: callee,
                    first,
                } => {
                    if depth + frames.len() + 1 >= MAX_CALL_DEPTH {
                        return Err(depth_error());
                    }
                    let callee_fn = self.functions[callee as usize]
                        .as_ref()
                        .expect("call target is lowered");
                    frames.push(SavedFrame {
                        function,
                        ip: (ip + 1) as u32,
                        base: base as u32,
                        dst,
                    });
                    base += first as usize;
                    let top = base + callee_fn.nregs as usize;
                    if stack.len() < top {
                        stack.resize(top.max(stack.len() * 2), 0);
                    }
                    current = callee_fn;
                    function = callee;
                    ip = 0;
                    continue;
                }
                Op::Ret { src } => {
                    let value = r!(src);
                    match frames.pop() {
                        None => return Ok(value),
                        Some(saved) => {
                            base = saved.base as usize;
                            stack[base + saved.dst as usize] = value;
                            function = saved.function;
                            current = self.functions[function as usize]
                                .as_ref()
                                .expect("caller is lowered");
                            ip = saved.ip as usize;
                            continue;
                        }
                    }
                }
                Op::RetVoid => match frames.pop() {
                    None => return Ok(0),
                    Some(saved) => {
                        base = saved.base as usize;
                        stack[base + saved.dst as usize] = 0;
                        function = saved.function;
                        current = self.functions[function as usize]
                            .as_ref()
                            .expect("caller is lowered");
                        ip = saved.ip as usize;
                        continue;
                    }
                },
            }
            ip += 1;
        }
    }
}

fn depth_error() -> SparError {
    SparError::EvalError {
        // Same wording as `runtime::runtime_error`, which the tree walker uses.
        message: format!(
            "internal runtime error: maximum function call depth ({MAX_CALL_DEPTH}) exceeded"
        ),
        span: Span::dummy(),
    }
}

fn division_by_zero(span: &Span) -> SparError {
    SparError::EvalError {
        message: "division by zero".into(),
        span: span.clone(),
    }
}
