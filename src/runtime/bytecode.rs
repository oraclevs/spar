//! Universal bytecode tier.
//!
//! Every function body is lowered once, at program-compile time, into a flat
//! instruction list that runs against the function's ordinary `Frame` (slot
//! `i` of the frame is register `i`; temporaries follow the locals). Control
//! flow, local access, primitive operations and direct calls are real
//! instructions. Anything else (strings, records, shell, closures, `try`,
//! native calls, ...) is a *fallback node*: the original compiled tree is kept
//! and executed by the tree-walking evaluator against the very same frame, so
//! no construct is ever unsupported and behaviour stays identical.
//!
//! This is tier 2. Functions made only of `int`/`float`/`bool` values run on
//! the faster untagged-register tier in `crate::vm` (tier 1) when eligible.
//!
//! # Invariants
//!
//! * Register `r < slot_count` is a source-level local; `r >= slot_count` is
//!   a temporary owned by exactly one pending instruction (it may be moved
//!   out with `take`). Locals are only ever cloned.
//! * `Frame.slots` is sized to `nslots` before execution starts.
//! * Fallback nodes see exactly the frame the tree walker would have seen,
//!   so generic type bindings and slot contents are shared with them.
//! * `RuntimeFlow` results of fallback statements are translated to jumps
//!   (`brk`/`cont` targets) or propagated unchanged, matching the walker.

use crate::compiled::{
    CompiledExpression, CompiledMethodTarget, CompiledFunction, CompiledStatement, FunctionId, LocalSlot, ModuleId,
    TypedOperation,
};
use crate::error::Span;
use crate::evaluator::ConfigValue;

use super::{
    eval_int_binary_value, eval_operation, field_of_value, module_state_error, resolve_runtime_type,
    runtime_error, type_error, Frame, Runtime, RuntimeFault, RuntimeFlow, Value,
};

const NONE: u32 = u32::MAX;

type Reg = u32;

#[derive(Clone, Debug)]
pub(crate) enum BOp {
    ConstI { dst: Reg, value: i64 },
    ConstF { dst: Reg, value: f64 },
    ConstB { dst: Reg, value: bool },
    /// Non-scalar constant (`consts[k]`), converted on each use.
    ConstK { dst: Reg, k: u32 },
    /// `dst = clone(local src)`.
    Move { dst: Reg, src: Reg, at: u32 },
    Bin { op: TypedOperation, dst: Reg, a: Reg, b: Reg, a_tmp: bool, b_tmp: bool, at: u32 },
    Un { op: TypedOperation, dst: Reg, a: Reg, a_tmp: bool, at: u32 },
    /// `dst = base.field` (`names[name]`), reading an object base in place.
    Field { dst: Reg, base: Reg, base_tmp: bool, name: u32, at: u32 },
    /// Native function call with arguments in `first..first+n`.
    Native { dst: Reg, function: u32, first: Reg, n: u32, ret: u32, at: u32 },
    /// Native method call. `recv` is a local receiver's register (moved out
    /// and back, as the tree walker does) or `NONE` when the receiver is the
    /// first register of the argument window.
    Method {
        dst: Reg,
        recv: Reg,
        first: Reg,
        n: u32,
        method: crate::runtime::NativeMethodId,
        mutates: bool,
        at: u32,
    },
    /// String interpolation: `templates[t]` with expression parts read from
    /// consecutive registers starting at `first`.
    Interp { dst: Reg, t: u32, first: Reg, at: u32 },
    /// `dst = [regs first..first+n]`.
    ListNew { dst: Reg, first: Reg, n: u32 },
    /// Evaluate `exprs[e]` with the tree walker into `dst`.
    Tree { dst: Reg, e: u32 },
    /// Evaluate `exprs[e]` with the tree walker and discard the result.
    Eval { e: u32 },
    /// Execute `stmts[s]` with the tree walker.
    TreeStmt { s: u32, brk: u32, cont: u32 },
    Jmp { target: u32 },
    JmpIfFalse { cond: Reg, target: u32, at: u32 },
    Call { dst: Reg, function: u32, first: Reg, n: u32 },
    Ret { src: Reg, is_tmp: bool, at: u32 },
    RetVoid,
    /// Post-statement check the tree walker performs: a pending `exit()` ends
    /// the function with the exit code; a shell-level exit stops the block.
    CheckExit,
    /// Counting loop head: exit if `cur >= stop`, else bind loop variables.
    ForRange { cur: Reg, stop: Reg, idx: Reg, value: Reg, index: u32, exit: u32 },
    /// Advance a counting loop.
    ForRangeStep { cur: Reg, idx: Reg, top: u32 },
    /// Check that `list` holds a list and reset `idx`.
    ForListInit { list: Reg, idx: Reg, at: u32 },
    /// List loop head: exit when exhausted, else bind loop variables.
    ForListNext { list: Reg, idx: Reg, value: Reg, index: u32, exit: u32, at: u32 },
}

pub(crate) struct BcFunction {
    pub(crate) code: Vec<BOp>,
    spans: Vec<Span>,
    consts: Vec<ConfigValue>,
    exprs: Vec<CompiledExpression>,
    stmts: Vec<CompiledStatement>,
    names: Vec<String>,
    types: Vec<crate::ast::SparType>,
    templates: Vec<Vec<Part>>,
    pub(crate) nslots: usize,
}

/// One piece of an interpolated string.
#[derive(Clone, Debug)]
enum Part {
    Literal(String),
    Expr,
}

pub(crate) struct BcProgram {
    functions: Vec<Option<BcFunction>>,
}

impl BcProgram {
    pub(crate) fn empty() -> Self {
        Self { functions: Vec::new() }
    }

    #[inline]
    pub(crate) fn get(&self, id: FunctionId) -> Option<&BcFunction> {
        self.functions.get(id.0 as usize)?.as_ref()
    }

    pub(crate) fn build<'a>(
        functions: impl Iterator<Item = &'a CompiledFunction>,
        function_count: usize,
    ) -> Self {
        if !crate::runtime_config::bytecode_enabled() {
            return Self::empty();
        }
        let all: Vec<&CompiledFunction> = functions.collect();
        let mut by_id: Vec<Option<&CompiledFunction>> = vec![None; function_count];
        for f in &all {
            by_id[f.id.0 as usize] = Some(f);
        }
        let functions = by_id
            .iter()
            .map(|f| f.map(|f| Lowerer::lower(f, &by_id)))
            .collect();
        Self { functions }
    }

    /// Listing for `spar dis --bytecode`.
    pub(crate) fn disassemble(&self, names: impl Fn(usize) -> String) -> String {
        let mut out = String::new();
        for (index, f) in self.functions.iter().enumerate() {
            let Some(f) = f else { continue };
            out.push_str(&format!("fn {} (id {index}) slots={}\n", names(index), f.nslots));
            for (ip, op) in f.code.iter().enumerate() {
                out.push_str(&format!("  {ip:04} {op:?}\n"));
            }
        }
        out
    }
}

struct LoopPatches {
    breaks: Vec<usize>,
    continues: Vec<usize>,
}

struct Lowerer<'a> {
    function: &'a CompiledFunction,
    by_id: &'a [Option<&'a CompiledFunction>],
    code: Vec<BOp>,
    spans: Vec<Span>,
    consts: Vec<ConfigValue>,
    exprs: Vec<CompiledExpression>,
    stmts: Vec<CompiledStatement>,
    names: Vec<String>,
    types: Vec<crate::ast::SparType>,
    templates: Vec<Vec<Part>>,
    slot_count: u32,
    base_temp: u32,
    next_temp: u32,
    max_reg: u32,
    loops: Vec<LoopPatches>,
}

impl<'a> Lowerer<'a> {
    fn lower(function: &'a CompiledFunction, by_id: &'a [Option<&'a CompiledFunction>]) -> BcFunction {
        let slot_count = function.slot_count as u32;
        let mut l = Lowerer {
            function,
            by_id,
            code: Vec::new(),
            spans: Vec::new(),
            consts: Vec::new(),
            exprs: Vec::new(),
            stmts: Vec::new(),
            names: Vec::new(),
            types: Vec::new(),
            templates: Vec::new(),
            slot_count,
            base_temp: slot_count,
            next_temp: slot_count,
            max_reg: slot_count,
            loops: Vec::new(),
        };
        l.statements(&function.body);
        l.emit(BOp::RetVoid, None);
        BcFunction {
            code: l.code,
            spans: l.spans,
            consts: l.consts,
            exprs: l.exprs,
            stmts: l.stmts,
            names: l.names,
            types: l.types,
            templates: l.templates,
            nslots: (l.max_reg.max(slot_count) + 1) as usize,
        }
    }

    fn emit(&mut self, op: BOp, span: Option<&Span>) -> usize {
        self.code.push(op);
        self.spans.push(span.cloned().unwrap_or_else(Span::dummy));
        self.code.len() - 1
    }

    fn here(&self) -> u32 {
        self.code.len() as u32
    }

    fn temp(&mut self) -> Reg {
        let r = self.next_temp;
        self.next_temp += 1;
        self.max_reg = self.max_reg.max(self.next_temp);
        r
    }

    fn at(&self) -> u32 {
        self.code.len() as u32
    }

    fn patch(&mut self, at: usize, target: u32) {
        match &mut self.code[at] {
            BOp::Jmp { target: t }
            | BOp::JmpIfFalse { target: t, .. }
            | BOp::ForRange { exit: t, .. }
            | BOp::ForListNext { exit: t, .. }
            | BOp::TreeStmt { brk: t, .. } => *t = target,
            _ => unreachable!("patching a non-jump"),
        }
    }

    fn tree_expr(&mut self, expression: &CompiledExpression, dst: Reg) {
        let e = self.exprs.len() as u32;
        self.exprs.push(expression.clone());
        self.emit(BOp::Tree { dst, e }, None);
    }

    fn statements(&mut self, body: &[CompiledStatement]) {
        for statement in body {
            self.next_temp = self.base_temp;
            let start = self.code.len();
            self.statement(statement);
            if self.may_run_user_code(start) {
                self.emit(BOp::CheckExit, None);
            }
        }
    }

    /// Whether ops emitted since `start` can run arbitrary user code (and so
    /// may request an exit), mirroring where the tree walker re-checks.
    fn may_run_user_code(&self, start: usize) -> bool {
        self.code[start..].iter().any(|op| {
            matches!(
                op,
                BOp::Tree { .. }
                    | BOp::Method { .. }
                    | BOp::Eval { .. }
                    | BOp::TreeStmt { .. }
                    | BOp::Call { .. }
                    | BOp::Native { .. }
            )
        })
    }

    fn tree_stmt(&mut self, statement: &CompiledStatement) {
        let s = self.stmts.len() as u32;
        self.stmts.push(statement.clone());
        let at = self.emit(BOp::TreeStmt { s, brk: NONE, cont: NONE }, None);
        // Break/continue escaping a fallback statement target the innermost
        // bytecode loop, if any; recorded here and patched when it closes.
        if let Some(l) = self.loops.last_mut() {
            l.breaks.push(at);
            l.continues.push(at);
        }
    }

    fn statement(&mut self, statement: &CompiledStatement) {
        match statement {
            CompiledStatement::StoreLocal { slot, value, .. } => {
                self.expr_into(value, slot.0);
            }
            CompiledStatement::Expression(expression, _) => match expression {
                CompiledExpression::DirectCall { .. } if self.call_is_lowerable(expression) => {
                    let dst = self.temp();
                    self.expr_into(expression, dst);
                }
                _ => {
                    let e = self.exprs.len() as u32;
                    self.exprs.push(expression.clone());
                    self.emit(BOp::Eval { e }, None);
                }
            },
            CompiledStatement::If {
                condition,
                then_body,
                else_body,
                span,
            } => {
                let (cond, _) = self.operand(condition);
                let at = self.at();
                let jump_else = self.emit(BOp::JmpIfFalse { cond, target: 0, at }, Some(span));
                self.statements(then_body);
                if else_body.is_empty() {
                    let end = self.here();
                    self.patch(jump_else, end);
                } else {
                    let jump_end = self.emit(BOp::Jmp { target: 0 }, None);
                    let else_start = self.here();
                    self.patch(jump_else, else_start);
                    self.statements(else_body);
                    let end = self.here();
                    self.patch(jump_end, end);
                }
            }
            CompiledStatement::For {
                index_slot,
                value_slot,
                iterable,
                body,
                span,
            } => self.for_loop(*index_slot, *value_slot, iterable, body, span),
            CompiledStatement::Return(Some(value), span) => {
                let start = self.code.len();
                let (src, is_tmp) = self.operand(value);
                if self.may_run_user_code(start) {
                    // The tree walker lets a pending exit override the value.
                    self.emit(BOp::CheckExit, None);
                }
                let at = self.at();
                self.emit(BOp::Ret { src, is_tmp, at }, Some(span));
            }
            CompiledStatement::Return(None, _) => {
                self.emit(BOp::RetVoid, None);
            }
            CompiledStatement::Break(_) if !self.loops.is_empty() => {
                let at = self.emit(BOp::Jmp { target: 0 }, None);
                self.loops.last_mut().expect("checked").breaks.push(at);
            }
            CompiledStatement::Continue(_) if !self.loops.is_empty() => {
                let at = self.emit(BOp::Jmp { target: 0 }, None);
                self.loops.last_mut().expect("checked").continues.push(at);
            }
            other => self.tree_stmt(other),
        }
    }

    fn for_loop(
        &mut self,
        index_slot: Option<LocalSlot>,
        value_slot: LocalSlot,
        iterable: &CompiledExpression,
        body: &[CompiledStatement],
        span: &Span,
    ) {
        let index = index_slot.map(|s| s.0).unwrap_or(NONE);
        // Hidden registers: list-or-current, stop-or-index, enumeration index.
        let a = self.base_temp;
        let b = a + 1;
        let c = a + 2;
        self.base_temp += 3;
        self.next_temp = self.base_temp;
        self.max_reg = self.max_reg.max(self.next_temp);

        let range = self.range_bounds(iterable);
        let (top, exit_jump) = match range {
            Some((start, end)) => {
                match start {
                    Some(start) => self.expr_into(start, a),
                    None => {
                        self.emit(BOp::ConstI { dst: a, value: 0 }, None);
                    }
                }
                self.expr_into(end, b);
                self.emit(BOp::ConstI { dst: c, value: 0 }, None);
                let top = self.here();
                let exit_jump = self.emit(
                    BOp::ForRange { cur: a, stop: b, idx: c, value: value_slot.0, index, exit: 0 },
                    None,
                );
                (top, exit_jump)
            }
            None => {
                self.expr_into(iterable, a);
                let at = self.at();
                self.emit(BOp::ForListInit { list: a, idx: b, at }, Some(span));
                let top = self.here();
                let at = self.at();
                let exit_jump = self.emit(
                    BOp::ForListNext { list: a, idx: b, value: value_slot.0, index, exit: 0, at },
                    Some(span),
                );
                (top, exit_jump)
            }
        };
        self.loops.push(LoopPatches { breaks: Vec::new(), continues: Vec::new() });
        self.statements(body);
        let patches = self.loops.pop().expect("loop stack");
        let cont = self.here();
        for at in patches.continues {
            self.patch_cont(at, cont);
        }
        if range.is_some() {
            self.emit(BOp::ForRangeStep { cur: a, idx: c, top }, None);
        } else {
            self.emit(BOp::Jmp { target: top }, None);
        }
        let exit = self.here();
        self.patch(exit_jump, exit);
        for at in patches.breaks {
            self.patch(at, exit);
        }
        self.base_temp -= 3;
    }

    fn patch_cont(&mut self, at: usize, target: u32) {
        match &mut self.code[at] {
            BOp::TreeStmt { cont, .. } => *cont = target,
            _ => self.patch(at, target),
        }
    }

    /// `range(end:)` / `rangeFrom(start:, end:)` from the reserved prelude.
    fn range_bounds<'e>(
        &self,
        iterable: &'e CompiledExpression,
    ) -> Option<(Option<&'e CompiledExpression>, &'e CompiledExpression)> {
        let CompiledExpression::DirectCall { function, arguments, .. } = iterable else {
            return None;
        };
        let callee = self.by_id.get(function.0 as usize).copied().flatten()?;
        if callee.key.group.is_some()
            || arguments
                .iter()
                .any(|a| matches!(a, CompiledExpression::DefaultArgument(_)))
        {
            return None;
        }
        match (callee.name.as_str(), arguments.as_slice()) {
            ("range", [end]) => Some((None, end)),
            ("rangeFrom", [start, end]) => Some((Some(start), end)),
            _ => None,
        }
    }

    /// A value usable as an instruction operand: a local's own register, or a
    /// fresh temporary holding the lowered expression.
    fn operand(&mut self, expression: &CompiledExpression) -> (Reg, bool) {
        if let CompiledExpression::Local(slot, _) = expression {
            return (slot.0, false);
        }
        let dst = self.temp();
        self.expr_into(expression, dst);
        (dst, true)
    }

    fn call_is_lowerable(&self, expression: &CompiledExpression) -> bool {
        let CompiledExpression::DirectCall { function, arguments, .. } = expression else {
            return false;
        };
        let Some(callee) = self.by_id.get(function.0 as usize).copied().flatten() else {
            return false;
        };
        !callee.is_async
            && !crate::typechecker::mentions_type_parameter(&callee.return_type)
            && arguments.len() == callee.parameter_slots.len()
            && !arguments
                .iter()
                .any(|a| matches!(a, CompiledExpression::DefaultArgument(_)))
    }

    /// Lowers `expression` so its value ends up in register `dst`.
    fn expr_into(&mut self, expression: &CompiledExpression, dst: Reg) {
        match expression {
            CompiledExpression::Local(slot, span) => {
                if slot.0 != dst {
                    let at = self.at();
                    self.emit(BOp::Move { dst, src: slot.0, at }, Some(span));
                }
            }
            CompiledExpression::Constant(value, _) => match value {
                ConfigValue::Int(value) => {
                    self.emit(BOp::ConstI { dst, value: *value }, None);
                }
                ConfigValue::Float(value) => {
                    self.emit(BOp::ConstF { dst, value: *value }, None);
                }
                ConfigValue::Bool(value) => {
                    self.emit(BOp::ConstB { dst, value: *value }, None);
                }
                other => {
                    let k = self.consts.len() as u32;
                    self.consts.push(other.clone());
                    self.emit(BOp::ConstK { dst, k }, None);
                }
            },
            CompiledExpression::Operation { operation, operands, span }
                if !matches!(
                    operation,
                    TypedOperation::BoolAnd | TypedOperation::BoolOr | TypedOperation::Fallback
                ) && matches!(operands.len(), 1 | 2) =>
            {
                let saved = self.next_temp;
                if let [operand] = operands.as_slice() {
                    let (a, a_tmp) = self.operand(operand);
                    self.emit(BOp::Un { op: *operation, dst, a, a_tmp, at: self.at() }, Some(span));
                } else if let [left, right] = operands.as_slice() {
                    let (a, a_tmp) = self.operand(left);
                    let (b, b_tmp) = self.operand(right);
                    self.emit(
                        BOp::Bin { op: *operation, dst, a, b, a_tmp, b_tmp, at: self.at() },
                        Some(span),
                    );
                }
                self.next_temp = saved.max(self.base_temp).max(dst + 1);
            }
            CompiledExpression::DirectCall { function, arguments, span, .. }
                if self.call_is_lowerable(expression) =>
            {
                let saved = self.next_temp;
                let first = self.next_temp;
                self.next_temp += arguments.len() as u32;
                self.max_reg = self.max_reg.max(self.next_temp);
                for (index, argument) in arguments.iter().enumerate() {
                    self.expr_into(argument, first + index as u32);
                }
                self.emit(
                    BOp::Call { dst, function: function.0, first, n: arguments.len() as u32 },
                    Some(span),
                );
                self.next_temp = saved.max(self.base_temp).max(dst + 1);
            }
            CompiledExpression::MethodCall {
                target: CompiledMethodTarget::Native(method),
                receiver: Some(receiver),
                arguments,
                mutates_receiver,
                span,
                ..
            } if matches!(**receiver, CompiledExpression::Local(..)) || !*mutates_receiver => {
                let saved = self.next_temp;
                let first = self.next_temp;
                let recv = match &**receiver {
                    CompiledExpression::Local(slot, _) => slot.0,
                    _ => NONE,
                };
                let window = arguments.len() as u32 + u32::from(recv == NONE);
                self.next_temp += window;
                self.max_reg = self.max_reg.max(self.next_temp);
                let mut reg = first;
                if recv == NONE {
                    self.expr_into(receiver, reg);
                    reg += 1;
                }
                for argument in arguments {
                    self.expr_into(argument, reg);
                    reg += 1;
                }
                let at = self.at();
                self.emit(
                    BOp::Method {
                        dst,
                        recv,
                        first,
                        n: window,
                        method: *method,
                        mutates: *mutates_receiver,
                        at,
                    },
                    Some(span),
                );
                self.next_temp = saved.max(self.base_temp).max(dst + 1);
            }
            CompiledExpression::Field { base, field, span } => {
                let saved = self.next_temp;
                let (base, base_tmp) = self.operand(base);
                let name = self.names.len() as u32;
                self.names.push(field.clone());
                let at = self.at();
                self.emit(BOp::Field { dst, base, base_tmp, name, at }, Some(span));
                self.next_temp = saved.max(self.base_temp).max(dst + 1);
            }
            CompiledExpression::NativeCall { function, arguments, return_type, span } => {
                let saved = self.next_temp;
                let first = self.next_temp;
                self.next_temp += arguments.len() as u32;
                self.max_reg = self.max_reg.max(self.next_temp);
                for (index, argument) in arguments.iter().enumerate() {
                    self.expr_into(argument, first + index as u32);
                }
                let ret = match return_type {
                    Some(ty) => {
                        self.types.push(ty.clone());
                        (self.types.len() - 1) as u32
                    }
                    None => NONE,
                };
                let at = self.at();
                self.emit(
                    BOp::Native {
                        dst,
                        function: function.0,
                        first,
                        n: arguments.len() as u32,
                        ret,
                        at,
                    },
                    Some(span),
                );
                self.next_temp = saved.max(self.base_temp).max(dst + 1);
            }
            CompiledExpression::Interpolation(parts, span) => {
                let saved = self.next_temp;
                let first = self.next_temp;
                let mut template = Vec::with_capacity(parts.len());
                let mut exprs = Vec::new();
                for part in parts {
                    match part {
                        crate::compiled::CompiledStringPart::Literal(text) => {
                            template.push(Part::Literal(text.clone()))
                        }
                        crate::compiled::CompiledStringPart::Expression(expression) => {
                            template.push(Part::Expr);
                            exprs.push(expression);
                        }
                    }
                }
                self.next_temp += exprs.len() as u32;
                self.max_reg = self.max_reg.max(self.next_temp);
                for (index, expression) in exprs.into_iter().enumerate() {
                    self.expr_into(expression, first + index as u32);
                }
                let t = self.templates.len() as u32;
                self.templates.push(template);
                let at = self.at();
                self.emit(BOp::Interp { dst, t, first, at }, Some(span));
                self.next_temp = saved.max(self.base_temp).max(dst + 1);
            }
            CompiledExpression::List(items, _) => {
                let saved = self.next_temp;
                let first = self.next_temp;
                self.next_temp += items.len() as u32;
                self.max_reg = self.max_reg.max(self.next_temp);
                for (index, item) in items.iter().enumerate() {
                    self.expr_into(item, first + index as u32);
                }
                self.emit(BOp::ListNew { dst, first, n: items.len() as u32 }, None);
                self.next_temp = saved.max(self.base_temp).max(dst + 1);
            }
            other => self.tree_expr(other, dst),
        }
    }
}

// ── Execution ────────────────────────────────────────────────────────────────

impl Runtime<'_> {
    /// Runs a lowered function body against `frame`.
    pub(super) fn run_bytecode(
        &mut self,
        bc: &BcFunction,
        frame: &mut Frame,
        module: ModuleId,
    ) -> Result<RuntimeFlow, RuntimeFault> {
        if frame.slots.len() < bc.nslots {
            frame.slots.resize(bc.nslots, None);
        }
        let mut ip = 0usize;
        loop {
            match &bc.code[ip] {
                BOp::ConstI { dst, value } => {
                    frame.slots[*dst as usize] = Some(Value::Int(*value));
                }
                BOp::ConstF { dst, value } => {
                    frame.slots[*dst as usize] = Some(Value::Float(*value));
                }
                BOp::ConstB { dst, value } => {
                    frame.slots[*dst as usize] = Some(Value::Bool(*value));
                }
                BOp::ConstK { dst, k } => {
                    frame.slots[*dst as usize] =
                        Some(Value::from_config(bc.consts[*k as usize].clone()));
                }
                BOp::Move { dst, src, at } => {
                    let value = frame.read(LocalSlot(*src), &bc.spans[*at as usize])?.clone();
                    frame.slots[*dst as usize] = Some(value);
                }
                BOp::Bin { op, dst, a, b, a_tmp, b_tmp, at } => {
                    let span = &bc.spans[*at as usize];
                    let fast = {
                        let left = frame.read(LocalSlot(*a), span)?;
                        let right = frame.read(LocalSlot(*b), span)?;
                        eval_int_binary_value(*op, left, right)
                    };
                    let result = match fast {
                        Some(value) => value,
                        None => {
                            let left = take_or_clone(frame, *a, *a_tmp, span)?;
                            let right = take_or_clone(frame, *b, *b_tmp, span)?;
                            eval_operation(*op, &[left, right], span)?
                        }
                    };
                    frame.slots[*dst as usize] = Some(result);
                }
                BOp::Un { op, dst, a, a_tmp, at } => {
                    let span = &bc.spans[*at as usize];
                    let operand = take_or_clone(frame, *a, *a_tmp, span)?;
                    let result = eval_operation(*op, &[operand], span)?;
                    frame.slots[*dst as usize] = Some(result);
                }
                BOp::Field { dst, base, base_tmp, name, at } => {
                    let span = &bc.spans[*at as usize];
                    let field = bc.names[*name as usize].as_str();
                    let direct = match frame.slots[*base as usize].as_ref() {
                        Some(Value::Object(fields)) => Some(fields.get(field).cloned()),
                        _ => None,
                    };
                    let value = match direct {
                        Some(Some(value)) => value,
                        _ => {
                            let base = take_or_clone(frame, *base, *base_tmp, span)?;
                            field_of_value(base, field, span)?
                        }
                    };
                    frame.slots[*dst as usize] = Some(value);
                }
                BOp::Native { dst, function, first, n, ret, at } => {
                    let span = &bc.spans[*at as usize];
                    let mut values = Vec::with_capacity(*n as usize);
                    for index in 0..*n {
                        values.push(frame.take(LocalSlot(*first + index), span)?);
                    }
                    let id = crate::runtime::NativeFunctionId(*function);
                    let intrinsic = self
                        .state
                        .as_ref()
                        .ok_or_else(|| module_state_error(span))?
                        .natives
                        .intrinsic(id);
                    let value = if let Some(intrinsic) = intrinsic {
                        let requested = (*ret != NONE)
                            .then(|| resolve_runtime_type(&bc.types[*ret as usize], frame, module));
                        self.execute_native_intrinsic(intrinsic, &values, requested.as_ref(), span)?
                    } else {
                        let natives = &self
                            .state
                            .as_ref()
                            .ok_or_else(|| module_state_error(span))?
                            .natives;
                        natives.call(id, &mut self.context, &values, span)?
                    };
                    frame.slots[*dst as usize] = Some(value);
                }
                BOp::Method { dst, recv, first, n, method, mutates, at } => {
                    let span = &bc.spans[*at as usize];
                    let mut values = Vec::with_capacity(*n as usize + 1);
                    for index in 0..*n {
                        values.push(frame.take(LocalSlot(*first + index), span)?);
                    }
                    let intrinsic = self
                        .state
                        .as_ref()
                        .ok_or_else(|| module_state_error(span))?
                        .natives
                        .method_intrinsic(*method);
                    let result = if *recv == NONE {
                        // Receiver is `values[0]`; non-mutating by construction.
                        if let Some(intrinsic) = intrinsic {
                            self.execute_native_intrinsic(intrinsic, &values, None, span)?
                        } else {
                            let natives = &self
                                .state
                                .as_ref()
                                .ok_or_else(|| module_state_error(span))?
                                .natives;
                            natives.call_method(*method, &mut self.context, &values, span)?
                        }
                    } else {
                        // Local receiver: move it out, call, always move it back.
                        let mut receiver_value = frame.take(LocalSlot(*recv), span)?;
                        let outcome: Result<Value, RuntimeFault> = if *mutates {
                            let natives = &self
                                .state
                                .as_ref()
                                .ok_or_else(|| module_state_error(span))?
                                .natives;
                            natives
                                .call_method_mut(
                                    *method,
                                    &mut self.context,
                                    &mut receiver_value,
                                    &values,
                                    span,
                                )
                                .map_err(Into::into)
                        } else {
                            values.insert(0, receiver_value);
                            let outcome = if let Some(intrinsic) = intrinsic {
                                self.execute_native_intrinsic(intrinsic, &values, None, span)
                            } else {
                                let natives = &self
                                    .state
                                    .as_ref()
                                    .ok_or_else(|| module_state_error(span))?
                                    .natives;
                                natives
                                    .call_method(*method, &mut self.context, &values, span)
                                    .map_err(Into::into)
                            };
                            receiver_value = values.swap_remove(0);
                            outcome
                        };
                        frame.slots[*recv as usize] = Some(receiver_value);
                        outcome?
                    };
                    frame.slots[*dst as usize] = Some(result);
                }
                BOp::Interp { dst, t, first, at } => {
                    let span = &bc.spans[*at as usize];
                    let mut output = String::new();
                    let mut reg = *first;
                    for part in &bc.templates[*t as usize] {
                        match part {
                            Part::Literal(text) => output.push_str(text),
                            Part::Expr => {
                                match frame.read(LocalSlot(reg), span)? {
                                    Value::String(value) => output.push_str(value),
                                    Value::Int(value) => output.push_str(&value.to_string()),
                                    Value::Float(value) => output.push_str(&value.to_string()),
                                    Value::Bool(value) => output.push_str(&value.to_string()),
                                    other => return Err(type_error("primitive", other, span).into()),
                                }
                                reg += 1;
                            }
                        }
                    }
                    frame.slots[*dst as usize] = Some(Value::String(output));
                }
                BOp::ListNew { dst, first, n } => {
                    let mut items = Vec::with_capacity(*n as usize);
                    for index in 0..*n {
                        items.push(frame.take(LocalSlot(*first + index), &Span::dummy())?);
                    }
                    frame.slots[*dst as usize] = Some(Value::List(items.into()));
                }
                BOp::Tree { dst, e } => {
                    let value = self.eval_expression(&bc.exprs[*e as usize], frame, module)?;
                    frame.slots[*dst as usize] = Some(value);
                }
                BOp::Eval { e } => {
                    self.eval_expression(&bc.exprs[*e as usize], frame, module)?;
                }
                BOp::TreeStmt { s, brk, cont } => {
                    let statement = std::slice::from_ref(&bc.stmts[*s as usize]);
                    match self.execute_statements(statement, frame, module)? {
                        RuntimeFlow::Normal => {}
                        flow @ RuntimeFlow::Return(_) => return Ok(flow),
                        RuntimeFlow::Break => {
                            if *brk == NONE {
                                return Ok(RuntimeFlow::Break);
                            }
                            ip = *brk as usize;
                            continue;
                        }
                        RuntimeFlow::Continue => {
                            if *cont == NONE {
                                return Ok(RuntimeFlow::Continue);
                            }
                            ip = *cont as usize;
                            continue;
                        }
                    }
                }
                BOp::Jmp { target } => {
                    ip = *target as usize;
                    continue;
                }
                BOp::JmpIfFalse { cond, target, at } => {
                    let span = &bc.spans[*at as usize];
                    match frame.read(LocalSlot(*cond), span)? {
                        Value::Bool(true) => {}
                        Value::Bool(false) => {
                            ip = *target as usize;
                            continue;
                        }
                        value => return Err(type_error("bool", value, span).into()),
                    }
                }
                BOp::Call { dst, function, first, n } => {
                    let value = self.bytecode_call(FunctionId(*function), frame, *first, *n)?;
                    frame.slots[*dst as usize] = Some(value);
                }
                BOp::Ret { src, is_tmp, at } => {
                    let span = &bc.spans[*at as usize];
                    let value = take_or_clone(frame, *src, *is_tmp, span)?;
                    return Ok(RuntimeFlow::Return(value));
                }
                BOp::RetVoid => return Ok(RuntimeFlow::Normal),
                BOp::CheckExit => {
                    if let Some(code) = self.context.requested_exit() {
                        return Ok(RuntimeFlow::Return(Value::Int(i64::from(code))));
                    }
                    if self.shell_exit && self.shell_depth > 0 {
                        return Ok(RuntimeFlow::Normal);
                    }
                }
                BOp::ForRange { cur, stop, idx, value, index, exit } => {
                    let (c, s) = match (&frame.slots[*cur as usize], &frame.slots[*stop as usize]) {
                        (Some(Value::Int(c)), Some(Value::Int(s))) => (*c, *s),
                        _ => {
                            return Err(runtime_error(
                                "checked range loop received a non-int bound",
                                &Span::dummy(),
                            )
                            .into())
                        }
                    };
                    if c >= s {
                        ip = *exit as usize;
                        continue;
                    }
                    frame.slots[*value as usize] = Some(Value::Int(c));
                    if *index != NONE {
                        let i = match &frame.slots[*idx as usize] {
                            Some(Value::Int(i)) => *i,
                            _ => 0,
                        };
                        frame.slots[*index as usize] = Some(Value::Int(i));
                    }
                }
                BOp::ForRangeStep { cur, idx, top } => {
                    if let Some(Value::Int(c)) = &mut frame.slots[*cur as usize] {
                        *c = c.wrapping_add(1);
                    }
                    if let Some(Value::Int(i)) = &mut frame.slots[*idx as usize] {
                        *i = i.wrapping_add(1);
                    }
                    ip = *top as usize;
                    continue;
                }
                BOp::ForListInit { list, idx, at } => {
                    if !matches!(frame.slots[*list as usize], Some(Value::List(_))) {
                        return Err(runtime_error(
                            "checked loop received a non-list",
                            &bc.spans[*at as usize],
                        )
                        .into());
                    }
                    frame.slots[*idx as usize] = Some(Value::Int(0));
                }
                BOp::ForListNext { list, idx, value, index, exit, at } => {
                    let i = match &frame.slots[*idx as usize] {
                        Some(Value::Int(i)) => *i as usize,
                        _ => 0,
                    };
                    let item = match &mut frame.slots[*list as usize] {
                        Some(Value::List(items)) if i < items.len() => {
                            // The list is a private temporary: move the element out.
                            Some(std::mem::replace(&mut items[i], Value::Void))
                        }
                        Some(Value::List(_)) => None,
                        _ => {
                            return Err(runtime_error(
                                "checked loop received a non-list",
                                &bc.spans[*at as usize],
                            )
                            .into())
                        }
                    };
                    let Some(item) = item else {
                        ip = *exit as usize;
                        continue;
                    };
                    if *index != NONE {
                        frame.slots[*index as usize] = Some(Value::Int(i as i64));
                    }
                    frame.slots[*value as usize] = Some(item);
                    frame.slots[*idx as usize] = Some(Value::Int(i as i64 + 1));
                }
            }
            ip += 1;
        }
    }
}

impl Runtime<'_> {
    /// Direct call whose arguments already sit in `caller` registers
    /// `first..first+n`. Moves them into the callee frame (or register bits
    /// for a tier-1 callee) without an intermediate argument vector.
    pub(super) fn bytecode_call(
        &mut self,
        id: FunctionId,
        caller: &mut Frame,
        first: u32,
        n: u32,
    ) -> Result<Value, RuntimeFault> {
        crate::recursion::with_stack(|| self.bytecode_call_inner(id, caller, first as usize, n as usize))
    }

    #[inline(never)]
    fn bytecode_call_inner(
        &mut self,
        id: FunctionId,
        caller: &mut Frame,
        first: usize,
        n: usize,
    ) -> Result<Value, RuntimeFault> {
        use crate::vm::Prim;
        if self.call_depth >= crate::recursion::MAX_CALL_DEPTH {
            return Err(runtime_error(
                &format!(
                    "maximum function call depth ({}) exceeded",
                    crate::recursion::MAX_CALL_DEPTH
                ),
                &Span::dummy(),
            )
            .into());
        }
        // SAFETY: see `call_direct_inline_inner` — `self.program` is never
        // replaced while this `Runtime` lives and the borrow ends with this call.
        let program: &crate::compiled::CompiledProgram =
            unsafe { &*std::sync::Arc::as_ptr(&self.program) };
        let function = program.function(id).ok_or_else(|| {
            runtime_error(&format!("unknown function ID {}", id.0), &Span::dummy())
        })?;
        let parameter_slots = &function.parameter_slots;
        let function_span = &function.span;
        if n > parameter_slots.len() {
            return Err(runtime_error("too many direct-call arguments", function_span).into());
        }
        if let Some(vm_function) = program.vm.get(id) {
            if n == vm_function.params.len() && n <= 8 {
                let mut bits = [0u64; 8];
                for (index, prim) in vm_function.params.iter().enumerate() {
                    let value = caller.slots[first + index].take();
                    bits[index] = match (prim, value) {
                        (Prim::Int, Some(Value::Int(v))) => v as u64,
                        (Prim::Float, Some(Value::Float(v))) => v.to_bits(),
                        (Prim::Bool, Some(Value::Bool(v))) => v as u64,
                        _ => {
                            return Err(runtime_error(
                                "bytecode call received an argument of the wrong primitive type",
                                &Span::dummy(),
                            )
                            .into())
                        }
                    };
                }
                let result = program.vm.run(&mut self.vm_state, id, &bits[..n], self.call_depth)?;
                return Ok(match vm_function.ret {
                    Prim::Int => Value::Int(result as i64),
                    Prim::Float => Value::Float(f64::from_bits(result)),
                    Prim::Bool => Value::Bool(result != 0),
                    Prim::Void => Value::Void,
                });
            }
        }
        let module = function.key.module;
        let size = program
            .bc
            .get(id)
            .map(|bc| bc.nslots)
            .unwrap_or(function.slot_count)
            .max(function.slot_count);
        let mut frame = Frame::new(size);
        for index in 0..n {
            let value = caller.slots[first + index]
                .take()
                .ok_or_else(|| runtime_error("call argument is uninitialized", function_span))?;
            // A `void` argument stands for a skipped parameter: use its default.
            if !matches!(value, Value::Void) {
                frame.write(parameter_slots[index], value, function_span)?;
            }
        }
        self.call_depth += 1;
        let result = (|| {
            for (index, slot) in parameter_slots.iter().copied().enumerate() {
                if frame.slots[slot.0 as usize].is_some() {
                    continue;
                }
                let default = function
                    .default_values
                    .get(index)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| {
                        runtime_error("missing required direct-call argument", function_span)
                    })?;
                let value = self.eval_expression(default, &mut frame, module)?;
                frame.write(slot, value, function_span)?;
            }
            match self.run_body(program, id, &function.body, &mut frame, module)? {
                RuntimeFlow::Return(value) => Ok(value),
                RuntimeFlow::Normal => Ok(Value::Void),
                RuntimeFlow::Break | RuntimeFlow::Continue => Err(runtime_error(
                    "loop control escaped a compiled function",
                    function_span,
                )
                .into()),
            }
        })();
        self.call_depth -= 1;
        result
    }

    /// Executes a function body on the bytecode tier when it was lowered,
    /// otherwise on the tree walker.
    #[inline]
    pub(super) fn run_body(
        &mut self,
        program: &crate::compiled::CompiledProgram,
        id: FunctionId,
        body: &[CompiledStatement],
        frame: &mut Frame,
        module: ModuleId,
    ) -> Result<RuntimeFlow, RuntimeFault> {
        match program.bc.get(id) {
            Some(bc) => self.run_bytecode(bc, frame, module),
            None => self.execute_statements(body, frame, module),
        }
    }
}

#[inline]
fn take_or_clone(
    frame: &mut Frame,
    reg: Reg,
    is_tmp: bool,
    span: &Span,
) -> Result<Value, RuntimeFault> {
    if is_tmp {
        Ok(frame.take(LocalSlot(reg), span)?)
    } else {
        Ok(frame.read(LocalSlot(reg), span)?.clone())
    }
}
