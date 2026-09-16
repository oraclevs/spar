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
//! as a neutral `TemplatePart::Parameter` slot, and anything that doesn't
//! mention a task parameter at all is pre-evaluated the same way metadata
//! is — both have known values before the CLI ever binds arguments. An
//! expression that *does* mention a parameter but isn't a bare reference
//! (a function call taking a parameter, string concatenation, field
//! access, ...) can't be pre-evaluated, since the parameter's value isn't
//! known until the CLI binds it; it's instead lowered to
//! `TemplatePart::Expr` and evaluated at task-run time against the bound
//! parameter values, via `Evaluator::eval_standalone` again — see
//! `lower_run_block` and `Compilation::task_exprs`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::ast::{Expr, Program, ShellTemplatePart, SparType, StringPart, TaskDecl, TopLevelItem};
use crate::error::{Span, SparError};
use crate::evaluator::{ConfigValue, EvalResult, Evaluator};
use crate::resolver::SymbolTable;
use crate::runner::{
    CommandTemplate, ScalarKind, Task, TaskCommand, TaskParameter, TaskSet, TemplatePart,
};

/// A `${...}` interpolation lowered to `TemplatePart::Expr` because it
/// mentions a task parameter without being a bare reference to one.
/// Carried on `Compilation::task_exprs`, indexed by `TemplatePart::Expr`'s
/// `id`, so the `spar` binary can evaluate it at task-run time via
/// `Evaluator::eval_standalone` once parameters are bound — see the module
/// doc comment above.
#[derive(Debug, Clone)]
pub struct TaskExprEntry {
    pub expr: Expr,
    /// Declared scalar type of every task parameter this task exposes,
    /// so a caller evaluating `expr` can coerce each bound (string) CLI
    /// argument to the right `ConfigValue` before binding it as a local.
    pub param_kinds: HashMap<String, ScalarKind>,
}

/// Lowers every `task [...]  { ... }` declaration in `program` into a
/// `runner::TaskSet`. Returns `Ok(None)` when the program declares no
/// tasks at all — callers shouldn't attach an empty task catalog to
/// `Compilation`. `eval_result` must be the program's already-computed
/// evaluation result (global/section values), since task metadata and
/// non-parameter interpolations are pre-evaluated against it. Any
/// parameter-dependent `run` block expression that can't be pre-evaluated
/// is appended to `expr_table` instead; its index there becomes the `id`
/// on the corresponding `TemplatePart::Expr`.
pub fn lower_tasks(
    program: &Program,
    symbols: &SymbolTable,
    eval_result: &EvalResult,
    base_dir: &Path,
    expr_table: &mut Vec<TaskExprEntry>,
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
        match lower_one_task(decl, program, symbols, eval_result, base_dir, expr_table) {
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
    base_dir: &Path,
    expr_table: &mut Vec<TaskExprEntry>,
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
        .unwrap_or(true);
    let private = decl
        .private
        .as_ref()
        .and_then(|e| eval_bool(program, symbols, eval_result, e, &mut errors))
        .unwrap_or(false);
    let group = decl
        .group
        .as_ref()
        .and_then(|e| eval_str(program, symbols, eval_result, e, &mut errors));
    let confirm = decl
        .confirm
        .as_ref()
        .and_then(|e| eval_str(program, symbols, eval_result, e, &mut errors));
    let shell = decl
        .shell
        .as_ref()
        .and_then(|e| eval_string_list(program, symbols, eval_result, e, &mut errors));
    let cwd = decl
        .cwd
        .as_ref()
        .and_then(|e| eval_str(program, symbols, eval_result, e, &mut errors))
        .map(PathBuf::from);

    let mut environment = if let Some(path) = &program.load_env {
        match crate::dotenv::load(&base_dir.join(path)) {
            Ok(values) => values
                .into_iter()
                .filter(|(key, _)| std::env::var_os(key).is_none())
                .collect(),
            Err(error) => {
                errors.push(error);
                BTreeMap::new()
            }
        }
    } else {
        BTreeMap::new()
    };
    for (key, value_expr) in &decl.env {
        if let Some(v) = eval_str(program, symbols, eval_result, value_expr, &mut errors) {
            environment.insert(key.clone(), v);
        }
    }

    let param_names: HashSet<String> = decl.params.iter().map(|p| p.name.clone()).collect();
    let param_kinds: HashMap<String, ScalarKind> = decl
        .params
        .iter()
        .map(|p| (p.name.clone(), scalar_kind(&p.ty)))
        .collect();
    let parameters: Vec<TaskParameter> = decl
        .params
        .iter()
        .map(|p| TaskParameter {
            name: p.name.clone(),
            kind: scalar_kind(&p.ty),
            default: p
                .default
                .as_ref()
                .and_then(|e| eval_str(program, symbols, eval_result, e, &mut errors)),
            variadic: p.variadic,
        })
        .collect();

    let current_os = std::env::consts::OS;
    let selected_block = decl
        .run_blocks
        .iter()
        .find(|block| block.os.as_deref() == Some(current_os))
        .or_else(|| decl.run_blocks.iter().find(|block| block.os.is_none()));

    let mut commands: Vec<TaskCommand> = Vec::new();
    match selected_block {
        Some(block) => {
            for command in &block.commands {
                let mut parts: Vec<TemplatePart> = Vec::new();
                for part in &command.parts {
                    match part {
                        ShellTemplatePart::Literal(s) => {
                            parts.push(TemplatePart::Literal(s.replace("#{", "${")))
                        }
                        ShellTemplatePart::Expr(expr) => match bare_param_ref(expr, &param_names) {
                            Some(name) => parts.push(TemplatePart::Parameter(name)),
                            None if expr_mentions_any(expr, &param_names) => {
                                let id = expr_table.len();
                                let source = format_expr_source(expr);
                                expr_table.push(TaskExprEntry {
                                    expr: expr.clone(),
                                    param_kinds: param_kinds.clone(),
                                });
                                parts.push(TemplatePart::Expr { id, source });
                            }
                            None => match eval_any(program, symbols, eval_result, expr) {
                                Ok(v) => parts.push(TemplatePart::Literal(v.coerce_to_str())),
                                Err(e) => errors.push(e),
                            },
                        },
                    }
                }
                commands.push(if command.is_shebang {
                    TaskCommand::Script(CommandTemplate { parts })
                } else {
                    TaskCommand::Shell(CommandTemplate { parts })
                });
            }
        }
        None => {
            let labels: Vec<&str> = decl
                .run_blocks
                .iter()
                .filter_map(|block| block.os.as_deref())
                .collect();
            errors.push(SparError::EvalError {
                message: format!(
                    "task '{}' has no run block for `{current_os}` (defined: {}) and no default 'run {{}}' block",
                    decl.name,
                    labels.join(", ")
                ),
                span: decl.span.clone(),
            });
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(Task {
        name: decl.name.clone(),
        source_line: Some(decl.span.line),
        description,
        default,
        quiet,
        private,
        group,
        confirm,
        dependencies: decl.depends_on.iter().map(|d| d.name.clone()).collect(),
        parameters,
        environment,
        cwd,
        shell,
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
        SparType::List(_)
        | SparType::Section
        | SparType::Named(_)
        | SparType::TypeParameter(_)
        | SparType::Applied { .. }
        | SparType::Void
        | SparType::Shell => ScalarKind::Str,
    }
}

/// Renders `expr` back to Spar source text, for display inside an
/// unbound `${...}` template (`CommandTemplate::render_unbound`, used by
/// `spar show`/`spar dump` before parameters are ever bound).
fn format_expr_source(expr: &Expr) -> String {
    let mut out = String::new();
    crate::formatter::format_expr(
        expr,
        0,
        0,
        &crate::formatter::FormatConfig::default(),
        &mut out,
    );
    out
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
/// detect the deferred case of a task parameter combined with other
/// values inside one `${...}` (e.g. `${environment + "-x"}`, or a function
/// call taking a parameter as an argument) — lowered to `TemplatePart::Expr`
/// and evaluated at task-run time instead of here.
fn expr_mentions_any(expr: &Expr, param_names: &HashSet<String>) -> bool {
    match expr {
        Expr::NamespaceRef(nr) => nr.segments.len() == 1 && param_names.contains(&nr.segments[0]),
        Expr::Literal(_) | Expr::Shell(_) | Expr::ExecShell(_) => false,
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

fn eval_string_list(
    program: &Program,
    symbols: &SymbolTable,
    result: &EvalResult,
    expr: &Expr,
    errors: &mut Vec<SparError>,
) -> Option<Vec<String>> {
    match eval_any(program, symbols, result, expr) {
        Ok(ConfigValue::List(values)) => Some(
            values
                .into_iter()
                .map(|value| value.coerce_to_str())
                .collect(),
        ),
        Ok(_) => None,
        Err(error) => {
            errors.push(error);
            None
        }
    }
}
