//! Bridges `ast::TaskDecl` to the parser-independent `runner` IR. This is
//! the *sole* module allowed to translate `ast::TaskDecl` (and evaluated
//! Spar values) into `runner` types — everything under `src/runner/` stays
//! free of `crate::ast`/`Parser`/`Resolver`/`Evaluator`, so the runner
//! itself is testable without parsing a `.spar` file at all.
//!
//! Ordinary Spar expressions in task metadata (`description`, `default`,
//! `quiet`, `cwd`, `env` values) are pre-evaluated here via
//! `Evaluator::eval_standalone`, using the same evaluation result already
//! computed for the rest of the program. `run` block interpolations
//! (`${...}`) are different: a bare reference to a task parameter is left
//! as a neutral `TemplatePart::Parameter` slot (its value isn't known
//! until the CLI binds arguments), while anything else is pre-evaluated
//! the same way metadata is. An expression that mixes a task parameter
//! with other values in one `${...}` can't be represented by either case,
//! so it's rejected with a precise diagnostic instead of silently doing
//! the wrong thing.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use crate::ast::{Expr, Program, ShellTemplatePart, SparType, StringPart, TaskDecl, TopLevelItem};
use crate::error::{Span, SparError};
use crate::evaluator::{ConfigValue, EvalResult, Evaluator};
use crate::resolver::SymbolTable;
use crate::runner::{
    CommandTemplate, ScalarKind, Task, TaskCommand, TaskParameter, TaskSet, TemplatePart,
};

/// Lowers every `task [...]  { ... }` declaration in `program` into a
/// `runner::TaskSet`. Returns `Ok(None)` when the program declares no
/// tasks at all — callers shouldn't attach an empty task catalog to
/// `Compilation`. `eval_result` must be the program's already-computed
/// evaluation result (global/section values), since task metadata and
/// non-parameter interpolations are pre-evaluated against it.
pub fn lower_tasks(
    program: &Program,
    symbols: &SymbolTable,
    eval_result: &EvalResult,
) -> Result<Option<TaskSet>, Vec<SparError>> {
    let decls: Vec<&TaskDecl> = program
        .items
        .iter()
        .filter_map(|item| match item {
            TopLevelItem::Task(t) => Some(t.as_ref()),
            _ => None,
        })
        .collect();

    if decls.is_empty() {
        return Ok(None);
    }

    let mut errors: Vec<SparError> = Vec::new();
    let mut tasks: Vec<Task> = Vec::new();

    for decl in &decls {
        match lower_one_task(decl, program, symbols, eval_result) {
            Ok(task) => tasks.push(task),
            Err(mut errs) => errors.append(&mut errs),
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    let default_names: Vec<String> = tasks
        .iter()
        .filter(|t| t.default)
        .map(|t| t.name.clone())
        .collect();
    if default_names.len() > 1 {
        return Err(vec![SparError::EvalError {
            message: format!(
                "multiple default tasks are configured: {} — only one task may set 'default: true;'",
                default_names.join(", ")
            ),
            span: Span::dummy(),
        }]);
    }

    match TaskSet::new(tasks) {
        Ok(set) => Ok(Some(set)),
        Err(e) => Err(vec![SparError::EvalError {
            message: e.to_string(),
            span: Span::dummy(),
        }]),
    }
}

fn lower_one_task(
    decl: &TaskDecl,
    program: &Program,
    symbols: &SymbolTable,
    eval_result: &EvalResult,
) -> Result<Task, Vec<SparError>> {
    let mut errors: Vec<SparError> = Vec::new();

    let description = decl
        .description
        .as_ref()
        .and_then(|e| eval_str(program, symbols, eval_result, e, &mut errors));
    let default = decl
        .default
        .as_ref()
        .and_then(|e| eval_bool(program, symbols, eval_result, e, &mut errors))
        .unwrap_or(false);
    let quiet = decl
        .quiet
        .as_ref()
        .and_then(|e| eval_bool(program, symbols, eval_result, e, &mut errors))
        .unwrap_or(false);
    let cwd = decl
        .cwd
        .as_ref()
        .and_then(|e| eval_str(program, symbols, eval_result, e, &mut errors))
        .map(PathBuf::from);

    let mut environment: BTreeMap<String, String> = BTreeMap::new();
    for (key, value_expr) in &decl.env {
        if let Some(v) = eval_str(program, symbols, eval_result, value_expr, &mut errors) {
            environment.insert(key.clone(), v);
        }
    }

    let param_names: HashSet<String> = decl.params.iter().map(|p| p.name.clone()).collect();
    let parameters: Vec<TaskParameter> = decl
        .params
        .iter()
        .map(|p| TaskParameter {
            name: p.name.clone(),
            kind: scalar_kind(&p.ty),
        })
        .collect();

    let mut commands: Vec<TaskCommand> = Vec::new();
    for command in &decl.run {
        let mut parts: Vec<TemplatePart> = Vec::new();
        for part in &command.parts {
            match part {
                ShellTemplatePart::Literal(s) => parts.push(TemplatePart::Literal(s.clone())),
                ShellTemplatePart::Expr(expr) => match bare_param_ref(expr, &param_names) {
                    Some(name) => parts.push(TemplatePart::Parameter(name)),
                    None => {
                        if expr_mentions_any(expr, &param_names) {
                            errors.push(SparError::EvalError {
                                message: format!(
                                    "task '{}': a '${{...}}' interpolation cannot combine a task \
                                     parameter with other values — reference the parameter alone \
                                     (e.g. '${{{}}}'), or use a literal / global value instead",
                                    decl.name,
                                    param_names.iter().next().cloned().unwrap_or_default()
                                ),
                                span: command.span.clone(),
                            });
                            continue;
                        }
                        match eval_any(program, symbols, eval_result, expr) {
                            Ok(v) => parts.push(TemplatePart::Literal(v.coerce_to_str())),
                            Err(e) => errors.push(e),
                        }
                    }
                },
            }
        }
        commands.push(TaskCommand {
            template: CommandTemplate { parts },
        });
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(Task {
        name: decl.name.clone(),
        description,
        default,
        quiet,
        dependencies: decl.depends_on.iter().map(|d| d.name.clone()).collect(),
        parameters,
        environment,
        cwd,
        commands,
    })
}

fn scalar_kind(ty: &SparType) -> ScalarKind {
    match ty {
        SparType::Str => ScalarKind::Str,
        SparType::Int => ScalarKind::Int,
        SparType::Float => ScalarKind::Float,
        SparType::Bool => ScalarKind::Bool,
        // The typechecker rejects `list`/`section` task parameters before
        // lowering ever runs — this arm is unreachable in practice, but a
        // safe fallback beats a panic if that invariant ever slips.
        SparType::List(_) | SparType::Section | SparType::Named(_) => ScalarKind::Str,
    }
}

/// `expr` is exactly a bare reference to one of `param_names` — the only
/// shape `task_lowering` can turn into a neutral `TemplatePart::Parameter`
/// slot instead of pre-evaluating.
fn bare_param_ref(expr: &Expr, param_names: &HashSet<String>) -> Option<String> {
    match expr {
        Expr::NamespaceRef(nr)
            if nr.segments.len() == 1 && param_names.contains(&nr.segments[0]) =>
        {
            Some(nr.segments[0].clone())
        }
        _ => None,
    }
}

/// Does `expr` reference any of `param_names` anywhere within it? Used to
/// detect the unsupported case of a task parameter combined with other
/// values inside one `${...}` (e.g. `${environment + "-x"}`).
fn expr_mentions_any(expr: &Expr, param_names: &HashSet<String>) -> bool {
    match expr {
        Expr::NamespaceRef(nr) => nr.segments.len() == 1 && param_names.contains(&nr.segments[0]),
        Expr::Literal(_) => false,
        Expr::String(s) => s.parts.iter().any(|p| match p {
            StringPart::Literal(_) => false,
            StringPart::Expr(e) => expr_mentions_any(e, param_names),
        }),
        Expr::FieldAccess { base, .. } => expr_mentions_any(base, param_names),
        Expr::FnCall(fc) => fc.args.iter().any(|a| expr_mentions_any(a, param_names)),
        Expr::BinaryOp(op) => {
            expr_mentions_any(&op.lhs, param_names) || expr_mentions_any(&op.rhs, param_names)
        }
        Expr::List(items, _) => items.iter().any(|i| expr_mentions_any(i, param_names)),
        Expr::Grouped(inner, _) => expr_mentions_any(inner, param_names),
        Expr::Call { args, .. } => args
            .iter()
            .any(|a| expr_mentions_any(&a.value, param_names)),
        Expr::Unary { operand, .. } => expr_mentions_any(operand, param_names),
        Expr::Index { source, index, .. } => {
            expr_mentions_any(source, param_names) || expr_mentions_any(index, param_names)
        }
        Expr::Comprehension { source, body, .. } => {
            expr_mentions_any(source, param_names) || expr_mentions_any(body, param_names)
        }
        Expr::Object(items, _) => items.iter().any(|item| match item {
            crate::ast::SectionItem::Field(f) => match &f.value {
                Some(crate::ast::FieldValue::Expr(e)) => expr_mentions_any(e, param_names),
                _ => false,
            },
            crate::ast::SectionItem::Spread(s) => expr_mentions_any(&s.expr, param_names),
        }),
    }
}

fn eval_any(
    program: &Program,
    symbols: &SymbolTable,
    eval_result: &EvalResult,
    expr: &Expr,
) -> Result<ConfigValue, SparError> {
    Evaluator::eval_standalone(program, symbols, eval_result, expr, &HashMap::new())
}

fn eval_str(
    program: &Program,
    symbols: &SymbolTable,
    eval_result: &EvalResult,
    expr: &Expr,
    errors: &mut Vec<SparError>,
) -> Option<String> {
    match eval_any(program, symbols, eval_result, expr) {
        Ok(v) => Some(v.coerce_to_str()),
        Err(e) => {
            errors.push(e);
            None
        }
    }
}

fn eval_bool(
    program: &Program,
    symbols: &SymbolTable,
    eval_result: &EvalResult,
    expr: &Expr,
    errors: &mut Vec<SparError>,
) -> Option<bool> {
    match eval_any(program, symbols, eval_result, expr) {
        Ok(ConfigValue::Bool(b)) => Some(b),
        Ok(_) => None, // the typechecker already guarantees `bool` here
        Err(e) => {
            errors.push(e);
            None
        }
    }
}
