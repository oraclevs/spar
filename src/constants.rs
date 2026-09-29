//! `const` declarations: compile-time evaluation and validation.
//!
//! A `const` initializer may only use literals, other constants, parentheses, unary `-`/`!`,
//! arithmetic (`+ - * / %`), comparisons, `&&`/`||`, and string literals with no interpolation.
//! Anything else (calls, lists, objects, closures, shell, ...) is rejected. Overflow and
//! division by zero are compile-time errors rather than runtime ones.
//!
//! [`check_program`] validates every top-level and function-local `const` and returns the
//! values of the top-level ones so the resolver can publish them; the lowerer folds reads of
//! a constant into literal values using [`eval`].

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::error::{Span, SparError};
use crate::ConfigValue;

/// Why an expression is not a valid constant.
#[derive(Debug, Clone, PartialEq)]
pub struct ConstError {
    pub message: String,
    pub span: Span,
}

fn err<T>(message: impl Into<String>, span: &Span) -> Result<T, ConstError> {
    Err(ConstError {
        message: message.into(),
        span: span.clone(),
    })
}

/// Evaluates `expr` as a constant. `lookup` resolves a bare name to a constant value, or
/// `None` when the name is not a constant. `fallback` is used for error spans on literals.
pub fn eval(
    expr: &Expr,
    lookup: &dyn Fn(&str) -> Option<ConfigValue>,
    fallback: &Span,
) -> Result<ConfigValue, ConstError> {
    match expr {
        Expr::Literal(Literal::Int(v)) => Ok(ConfigValue::Int(*v)),
        Expr::Literal(Literal::Float(v)) => Ok(ConfigValue::Float(*v)),
        Expr::Literal(Literal::Bool(v)) => Ok(ConfigValue::Bool(*v)),
        Expr::String(text) => {
            let mut out = String::new();
            for part in &text.parts {
                match part {
                    StringPart::Literal(s) => out.push_str(s),
                    StringPart::Expr(_) => {
                        return err(
                            "string interpolation is not allowed in a const initializer",
                            &text.span,
                        )
                    }
                }
            }
            Ok(ConfigValue::Str(out))
        }
        Expr::Grouped(inner, span) => eval(inner, lookup, span),
        Expr::NamespaceRef(reference) => {
            if let [name] = reference.segments.as_slice() {
                if let Some(value) = lookup(name) {
                    return Ok(value);
                }
                return err(
                    format!("'{name}' is not a constant, so it cannot be used in a const initializer"),
                    &reference.span,
                );
            }
            err(
                "only constants declared in this file can be used in a const initializer",
                &reference.span,
            )
        }
        Expr::Unary { op, operand, span } => {
            let value = eval(operand, lookup, span)?;
            match (op, value) {
                (UnOp::Neg, ConfigValue::Int(v)) => v
                    .checked_neg()
                    .map(ConfigValue::Int)
                    .ok_or_else(|| overflow("negation", span)),
                (UnOp::Neg, ConfigValue::Float(v)) => Ok(ConfigValue::Float(-v)),
                (UnOp::Not, ConfigValue::Bool(v)) => Ok(ConfigValue::Bool(!v)),
                _ => err("this unary operator does not apply to that constant's type", span),
            }
        }
        Expr::BinaryOp(op) => {
            let lhs = eval(&op.lhs, lookup, &op.span)?;
            let rhs = eval(&op.rhs, lookup, &op.span)?;
            binary(&op.op, lhs, rhs, &op.span)
        }
        Expr::FnCall(call) => err(
            "function calls are not allowed in a const initializer",
            &call.span,
        ),
        Expr::Call { span, .. } | Expr::MethodCall { span, .. } => err(
            "function and method calls are not allowed in a const initializer",
            span,
        ),
        other => err(
            "this expression is not a compile-time constant",
            other.span().unwrap_or(fallback),
        ),
    }
}

fn overflow(what: &str, span: &Span) -> ConstError {
    ConstError {
        message: format!("integer overflow in constant {what}"),
        span: span.clone(),
    }
}

fn binary(op: &BinOp, lhs: ConfigValue, rhs: ConfigValue, span: &Span) -> Result<ConfigValue, ConstError> {
    use ConfigValue as V;
    let zero = || -> Result<V, ConstError> { err("division by zero in constant expression", span) };
    Ok(match (op, lhs, rhs) {
        (BinOp::Add, V::Int(a), V::Int(b)) => V::Int(a.checked_add(b).ok_or_else(|| overflow("addition", span))?),
        (BinOp::Sub, V::Int(a), V::Int(b)) => V::Int(a.checked_sub(b).ok_or_else(|| overflow("subtraction", span))?),
        (BinOp::Mul, V::Int(a), V::Int(b)) => V::Int(a.checked_mul(b).ok_or_else(|| overflow("multiplication", span))?),
        (BinOp::Div, V::Int(_), V::Int(0)) | (BinOp::Rem, V::Int(_), V::Int(0)) => return zero(),
        (BinOp::Div, V::Int(a), V::Int(b)) => V::Int(a.checked_div(b).ok_or_else(|| overflow("division", span))?),
        (BinOp::Rem, V::Int(a), V::Int(b)) => V::Int(a.checked_rem(b).ok_or_else(|| overflow("remainder", span))?),
        (BinOp::Add, V::Float(a), V::Float(b)) => V::Float(a + b),
        (BinOp::Sub, V::Float(a), V::Float(b)) => V::Float(a - b),
        (BinOp::Mul, V::Float(a), V::Float(b)) => V::Float(a * b),
        (BinOp::Div, V::Float(_), V::Float(b)) | (BinOp::Rem, V::Float(_), V::Float(b)) if b == 0.0 => return zero(),
        (BinOp::Div, V::Float(a), V::Float(b)) => V::Float(a / b),
        (BinOp::Rem, V::Float(a), V::Float(b)) => V::Float(a % b),
        (BinOp::Add, V::Str(a), V::Str(b)) => V::Str(a + &b),
        (BinOp::And, V::Bool(a), V::Bool(b)) => V::Bool(a && b),
        (BinOp::Or, V::Bool(a), V::Bool(b)) => V::Bool(a || b),
        (BinOp::Eq, a, b) => V::Bool(compare(&a, &b, span)? == std::cmp::Ordering::Equal),
        (BinOp::NotEq, a, b) => V::Bool(compare(&a, &b, span)? != std::cmp::Ordering::Equal),
        (BinOp::Lt, a, b) => V::Bool(compare(&a, &b, span)? == std::cmp::Ordering::Less),
        (BinOp::Gt, a, b) => V::Bool(compare(&a, &b, span)? == std::cmp::Ordering::Greater),
        (BinOp::LtEq, a, b) => V::Bool(compare(&a, &b, span)? != std::cmp::Ordering::Greater),
        (BinOp::GtEq, a, b) => V::Bool(compare(&a, &b, span)? != std::cmp::Ordering::Less),
        (BinOp::Fallback, ..) => return err("`??` is not allowed in a const initializer", span),
        _ => return err("operand types do not match for this constant expression", span),
    })
}

fn compare(a: &ConfigValue, b: &ConfigValue, span: &Span) -> Result<std::cmp::Ordering, ConstError> {
    use ConfigValue as V;
    match (a, b) {
        (V::Int(a), V::Int(b)) => Ok(a.cmp(b)),
        (V::Float(a), V::Float(b)) => a
            .partial_cmp(b)
            .map_or_else(|| err("cannot compare NaN in a constant", span), Ok),
        (V::Str(a), V::Str(b)) => Ok(a.cmp(b)),
        (V::Bool(a), V::Bool(b)) => Ok(a.cmp(b)),
        _ => err("operand types do not match for this constant comparison", span),
    }
}

// ── program-level checking ───────────────────────────────────────────────────

/// Validates every `const` in `program`. Returns the values of the top-level constants.
pub fn check_program(program: &Program) -> (HashMap<String, ConfigValue>, Vec<SparError>) {
    let mut checker = Checker {
        decls: HashMap::new(),
        values: HashMap::new(),
        visiting: HashSet::new(),
        errors: Vec::new(),
    };
    for item in &program.items {
        if let TopLevelItem::Var(decl) = item {
            if decl.is_const {
                checker.decls.insert(decl.name.clone(), decl);
            }
        }
    }
    let mut names: Vec<&String> = checker.decls.keys().collect();
    names.sort();
    for name in names.into_iter().cloned().collect::<Vec<_>>() {
        checker.global(&name);
    }
    for item in &program.items {
        match item {
            TopLevelItem::Function(f) => checker.function(&f.params, &f.body.stmts),
            TopLevelItem::Impl(block) => {
                for method in &block.methods {
                    checker.function(&method.function.params, &method.function.body.stmts);
                }
            }
            TopLevelItem::Statement(stmt) => {
                let mut scopes = vec![HashMap::new()];
                checker.statement(stmt, &mut scopes);
            }
            _ => {}
        }
    }
    (checker.values, checker.errors)
}

struct Checker<'a> {
    decls: HashMap<String, &'a VarDecl>,
    values: HashMap<String, ConfigValue>,
    visiting: HashSet<String>,
    errors: Vec<SparError>,
}

type Scopes = Vec<HashMap<String, Option<ConfigValue>>>;

impl<'a> Checker<'a> {
    fn push_error(&mut self, error: ConstError) {
        self.errors.push(SparError::ResolveError {
            message: error.message,
            hint: None,
            span: error.span,
        });
    }

    /// Value of top-level const `name`, evaluating it (once) on first use.
    fn global(&mut self, name: &str) -> Option<ConfigValue> {
        if let Some(value) = self.values.get(name) {
            return Some(value.clone());
        }
        let decl = *self.decls.get(name)?;
        if !self.visiting.insert(name.to_string()) {
            self.errors.push(SparError::ResolveError {
                message: format!("const '{name}' depends on itself"),
                hint: None,
                span: decl.span.clone(),
            });
            return None;
        }
        let result = match &decl.value {
            None => Err(ConstError {
                message: format!("const '{name}' needs an initializer"),
                span: decl.span.clone(),
            }),
            Some(expr) => {
                let deps = referenced_names(expr);
                let mut resolved = HashMap::new();
                for dep in deps {
                    if let Some(v) = self.global(&dep) {
                        resolved.insert(dep, v);
                    }
                }
                eval(expr, &|n| resolved.get(n).cloned(), &decl.span)
            }
        };
        self.visiting.remove(name);
        match result {
            Ok(value) => {
                self.values.insert(name.to_string(), value.clone());
                Some(value)
            }
            Err(error) => {
                self.push_error(error);
                None
            }
        }
    }

    fn lookup(&mut self, name: &str, scopes: &Scopes) -> Option<ConfigValue> {
        for scope in scopes.iter().rev() {
            if let Some(entry) = scope.get(name) {
                return entry.clone();
            }
        }
        self.global(name)
    }

    fn function(&mut self, params: &[Param], body: &[FuncStmt]) {
        let mut scope = HashMap::new();
        for param in params {
            scope.insert(param.name.clone(), None);
        }
        let mut scopes = vec![scope];
        self.block(body, &mut scopes);
    }

    fn block(&mut self, body: &[FuncStmt], scopes: &mut Scopes) {
        scopes.push(HashMap::new());
        for stmt in body {
            self.statement(stmt, scopes);
        }
        scopes.pop();
    }

    fn statement(&mut self, stmt: &FuncStmt, scopes: &mut Scopes) {
        match stmt {
            FuncStmt::LocalVar(local) => {
                let entry = if local.is_const {
                    let mut resolved = HashMap::new();
                    for dep in referenced_names(&local.value) {
                        if let Some(v) = self.lookup(&dep, scopes) {
                            resolved.insert(dep, v);
                        }
                    }
                    match eval(&local.value, &|n| resolved.get(n).cloned(), &local.span) {
                        Ok(value) => Some(value),
                        Err(error) => {
                            self.push_error(error);
                            None
                        }
                    }
                } else {
                    None
                };
                if let Some(scope) = scopes.last_mut() {
                    scope.insert(local.name.clone(), entry);
                }
            }
            FuncStmt::If(branch) => {
                self.block(&branch.then_stmts, scopes);
                self.block(&branch.else_stmts, scopes);
            }
            FuncStmt::For(looped) => {
                let mut scope = HashMap::new();
                match &looped.binding {
                    ForBinding::Value { name, .. } => {
                        scope.insert(name.clone(), None);
                    }
                    ForBinding::Indexed { index_name, value_name, .. } => {
                        scope.insert(index_name.clone(), None);
                        scope.insert(value_name.clone(), None);
                    }
                }
                scopes.push(scope);
                self.block(&looped.body, scopes);
                scopes.pop();
            }
            FuncStmt::While(looped) => self.block(&looped.body, scopes),
            FuncStmt::Try(attempt) => {
                self.block(&attempt.body, scopes);
                let mut scope = HashMap::new();
                if let Some(name) = &attempt.catch_name {
                    scope.insert(name.clone(), None);
                }
                scopes.push(scope);
                self.block(&attempt.handler, scopes);
                scopes.pop();
            }
            _ => {}
        }
    }
}

/// Bare names an initializer mentions (used to pre-resolve constants it depends on).
fn referenced_names(expr: &Expr) -> Vec<String> {
    let mut out = Vec::new();
    collect(expr, &mut out);
    out
}

fn collect(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::NamespaceRef(reference) => {
            if let [name] = reference.segments.as_slice() {
                out.push(name.clone());
            }
        }
        Expr::Grouped(inner, _) => collect(inner, out),
        Expr::Unary { operand, .. } => collect(operand, out),
        Expr::BinaryOp(op) => {
            collect(&op.lhs, out);
            collect(&op.rhs, out);
        }
        _ => {}
    }
}
