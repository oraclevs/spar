use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::depgraph::DeclId;
use crate::error::{Span, SparError};
use crate::resolver::SymbolTable;

const MAX_CALL_DEPTH: usize = 20;

// ── Output types ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PromiseHandle(u64);

#[allow(dead_code)] // Constructed by compiled async runtime in Phase 5 Task 4.
impl PromiseHandle {
    pub(crate) fn new(id: u64) -> Self {
        Self(id)
    }

    pub(crate) fn id(self) -> u64 {
        self.0
    }
}

impl std::fmt::Debug for PromiseHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PromiseHandle(..)")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigValue {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    List(Vec<ConfigValue>),
    Section(indexmap::IndexMap<String, ConfigValue>),
    Shell(spar_command::ShellPlan),
    ShellProgram(crate::runtime::ShellProgramValue),
    Promise(PromiseHandle),
    Error {
        message: String,
        kind: String,
        code: i64,
        cause: Option<Box<ConfigValue>>,
    },
}

impl ConfigValue {
    pub fn coerce_to_str(&self) -> String {
        match self {
            ConfigValue::Str(s) => s.clone(),
            ConfigValue::Int(n) => n.to_string(),
            ConfigValue::Float(f) => f.to_string(),
            ConfigValue::Bool(b) => b.to_string(),
            ConfigValue::List(_) => unreachable!("lists cannot appear in string interpolation"),
            ConfigValue::Section(_) => {
                unreachable!("sections cannot appear in string interpolation")
            }
            ConfigValue::Shell(_) => {
                unreachable!("shell plans cannot appear in string interpolation")
            }
            ConfigValue::ShellProgram(_) => {
                unreachable!("shell programs cannot appear in string interpolation")
            }
            ConfigValue::Promise(_) => "<promise>".into(),
            ConfigValue::Error { message, .. } => message.clone(),
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            ConfigValue::Str(_) => "str",
            ConfigValue::Int(_) => "int",
            ConfigValue::Float(_) => "float",
            ConfigValue::Bool(_) => "bool",
            ConfigValue::List(_) => "list",
            ConfigValue::Section(_) => "section",
            ConfigValue::Shell(_) => "shell",
            ConfigValue::ShellProgram(_) => "shell",
            ConfigValue::Promise(_) => "Promise",
            ConfigValue::Error { .. } => "error",
        }
    }
}

#[derive(Debug)]
pub struct EvalResult {
    pub globals: HashMap<String, ConfigValue>,
    pub sections: HashMap<Vec<String>, indexmap::IndexMap<String, ConfigValue>>,
    pub warnings: Vec<String>,
    pub(crate) interactive_value: Option<ConfigValue>,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingPromise {
    pub(crate) handle: PromiseHandle,
    pub(crate) import_alias: Option<String>,
    pub(crate) group: Option<String>,
    pub(crate) function: String,
    pub(crate) arguments: Vec<ConfigValue>,
}

// ── Internal error type ───────────────────────────────────────────────────────

#[derive(Debug)]
enum EvalErr {
    Fatal {
        message: String,
        span: Span,
    },
    EnvVarMissing(String, Span),
    CyclicRef {
        name: String,
        span: Span,
    },
    DivisionByZero(Span),
    ImportRef {
        alias: String,
        symbol: String,
    },
    NotScalar {
        name: String,
        span: Span,
    },
    TypeMismatch {
        expected: &'static str,
        got: &'static str,
    },
    MaxCallDepth {
        name: String,
    },
    PathNotFound {
        path: String,
        span: Span,
    },
    Host {
        message: String,
    },
}

enum StatementFlow {
    Normal,
    Break,
    Continue,
    Return(ConfigValue),
}

impl StatementFlow {
    fn into_return(self) -> Option<ConfigValue> {
        match self {
            Self::Return(value) => Some(value),
            Self::Normal | Self::Break | Self::Continue => None,
        }
    }
}

impl EvalErr {
    /// Gives an error that has no source location the location of `span`.
    /// Errors that already point somewhere are left alone.
    fn located_at(self, span: &Span) -> EvalErr {
        let unlocated = match &self {
            EvalErr::Fatal { span, .. }
            | EvalErr::CyclicRef { span, .. }
            | EvalErr::DivisionByZero(span)
            | EvalErr::NotScalar { span, .. }
            | EvalErr::PathNotFound { span, .. } => span.line == 0,
            EvalErr::TypeMismatch { .. } | EvalErr::MaxCallDepth { .. } | EvalErr::Host { .. } => {
                true
            }
            // Caught by the `??` fallback, and downgraded to a warning by
            // `push_eval_error`; both must keep seeing the original variant.
            EvalErr::ImportRef { .. } => false,
            EvalErr::EnvVarMissing(..) => false,
        };
        if !unlocated {
            return self;
        }
        match self {
            EvalErr::Fatal { message, .. } => EvalErr::Fatal {
                message,
                span: span.clone(),
            },
            EvalErr::CyclicRef { name, .. } => EvalErr::CyclicRef {
                name,
                span: span.clone(),
            },
            EvalErr::DivisionByZero(_) => EvalErr::DivisionByZero(span.clone()),
            EvalErr::NotScalar { name, .. } => EvalErr::NotScalar {
                name,
                span: span.clone(),
            },
            EvalErr::PathNotFound { path, .. } => EvalErr::PathNotFound {
                path,
                span: span.clone(),
            },
            other => {
                let SparError::EvalError { message, .. } = other.into_kl_error() else {
                    unreachable!("into_kl_error always yields an EvalError");
                };
                EvalErr::Fatal {
                    message,
                    span: span.clone(),
                }
            }
        }
    }

    fn into_kl_error(self) -> SparError {
        match self {
            EvalErr::Fatal { message, span } => SparError::EvalError { message, span },
            EvalErr::CyclicRef { name, span } => SparError::EvalError {
                message: format!(
                    "cyclic reference: `{name}` depends on itself — \
                     check for circular var references"
                ),
                span,
            },
            EvalErr::DivisionByZero(span) => SparError::EvalError {
                message: "division by zero".into(),
                span,
            },
            EvalErr::EnvVarMissing(name, span) => SparError::EvalError {
                message: format!("env var `{name}` is not set and has no `??` fallback"),
                span,
            },
            EvalErr::ImportRef { alias, symbol } => SparError::EvalError {
                message: format!(
                    "cannot evaluate cross-file reference `{alias}::{symbol}` \
                     without loading the imported file — handled by the CLI"
                ),
                span: Span::dummy(),
            },
            EvalErr::NotScalar { name, span } => SparError::EvalError {
                message: format!("'{}' is a nested section, not a scalar value", name),
                span,
            },
            EvalErr::PathNotFound { path, span } => SparError::EvalError {
                message: format!("undefined path: `{path}` does not refer to any known field"),
                span,
            },
            EvalErr::TypeMismatch { expected, got } => SparError::EvalError {
                message: format!("type mismatch: expected {expected}, got {got}"),
                span: Span::dummy(),
            },
            EvalErr::MaxCallDepth { name } => SparError::EvalError {
                message: format!(
                    "maximum call depth ({MAX_CALL_DEPTH}) exceeded in function '{name}'"
                ),
                span: Span::dummy(),
            },
            EvalErr::Host { message } => SparError::EvalError {
                message,
                span: Span::dummy(),
            },
        }
    }
}

type EvalResult_ = Result<ConfigValue, EvalErr>;

// ── Evaluator ─────────────────────────────────────────────────────────────────

/// Tracks the fields of the top-level section currently being built, keyed
/// by full absolute path, as they're computed — so a reference to an
/// already-computed sibling (via its own name, or via `self::`) can be
/// resolved without re-entering `eval_section_by_path` for a section that's
/// already mid-evaluation (which would trip its cyclic-reference guard).
struct SelfFrame {
    top_name: String,
    fields: HashMap<Vec<String>, ConfigValue>,
}

#[derive(Clone)]
struct ImportedProgram {
    program: Program,
    symbols: SymbolTable,
    imports: HashMap<String, ImportedProgram>,
    base_dir: std::path::PathBuf,
}

pub struct Evaluator {
    program: Program,
    symbols: SymbolTable,
    call_depth: usize,
    global_cache: HashMap<String, ConfigValue>,
    section_cache: HashMap<Vec<String>, indexmap::IndexMap<String, ConfigValue>>,
    evaluating: HashSet<String>,
    evaluating_sects: HashSet<Vec<String>>,
    self_stack: Vec<SelfFrame>,
    errors: Vec<SparError>,
    warnings: Vec<String>,
    imported_programs: HashMap<String, ImportedProgram>,
    hosts: crate::host::HostRegistry,
    natives: crate::runtime::NativeRegistry,
    runtime_context: crate::runtime::RuntimeContext,
    effect_ledger: Option<crate::session::EffectLedger>,
    next_promise_id: u64,
    pending_promises: Vec<PendingPromise>,
}

fn build_imported_programs(
    loaded: &HashMap<String, crate::loader::LoadedImport>,
    _base_dir: &std::path::Path,
    hosts: &crate::host::HostRegistry,
    natives: &crate::runtime::NativeRegistry,
) -> Result<HashMap<String, ImportedProgram>, Vec<SparError>> {
    let mut visiting = Vec::new();
    let mut cache = HashMap::new();
    loaded
        .iter()
        .map(|(alias, import)| {
            // `resolved_path` is already the right file — a plain
            // filesystem join for an ordinary import, or a package
            // store/live path for an explicit package import — so
            // this never re-derives it from `base_dir`/`import.path`.
            load_imported_program(
                &import.resolved_path,
                import.locator.as_ref(),
                &mut visiting,
                &mut cache,
                hosts,
                natives,
            )
            .map(|program| (alias.clone(), program))
        })
        .collect()
}

fn load_imported_program(
    path: &std::path::Path,
    locator: Option<&crate::package::ModuleLocator>,
    visiting: &mut Vec<std::path::PathBuf>,
    cache: &mut HashMap<std::path::PathBuf, ImportedProgram>,
    hosts: &crate::host::HostRegistry,
    natives: &crate::runtime::NativeRegistry,
) -> Result<ImportedProgram, Vec<SparError>> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if let Some(program) = cache.get(&canonical) {
        return Ok(program.clone());
    }
    if let Some(cycle) = crate::depgraph::find_cycle_in_stack(visiting, &canonical) {
        return Err(vec![SparError::ResolveError {
            message: format!(
                "import cycle detected during evaluation: {}",
                cycle
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
            hint: None,
            span: Span::dummy(),
        }]);
    }

    let source = std::fs::read_to_string(path).map_err(|error| {
        vec![SparError::ResolveError {
            message: format!("cannot read import file '{}': {error}", path.display()),
            hint: None,
            span: Span::dummy(),
        }]
    })?;
    let tokens = crate::lexer::Lexer::new(&source)
        .tokenize()
        .map_err(|error| {
            vec![SparError::ResolveError {
                message: format!("import file '{}' has a lex error: {error}", path.display()),
                hint: None,
                span: Span::dummy(),
            }]
        })?;
    let mut program = crate::parser::Parser::new(tokens)
        .parse()
        .map_err(|error| {
            vec![SparError::ResolveError {
                message: format!(
                    "import file '{}' has a parse error: {error}",
                    path.display()
                ),
                hint: None,
                span: Span::dummy(),
            }]
        })?;
    if crate::stdlib::is_bundled_std_path(path) {
        crate::loader::mark_program_trusted_native(&mut program);
    }
    // Same builtin types the compiler gives every module (`Bytes`, ...), so
    // imported modules that mention them resolve on their own.
    crate::compiler::inject_exec_result_type(&mut program);
    let base_dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let mut expand_loader = crate::loader::ImportLoader::new(base_dir);
    let mut import_loader = crate::loader::ImportLoader::new(base_dir);
    if let Some(locator) = locator {
        expand_loader = expand_loader.with_locator(locator.clone());
        import_loader = import_loader.with_locator(locator.clone());
    }
    crate::loader::expand_imports(&mut program, &mut expand_loader)?;
    let loaded = crate::loader::collect_imports(&program, &mut import_loader)?;

    visiting.push(canonical.clone());
    let imports = loaded
        .iter()
        .map(|(alias, import)| {
            load_imported_program(
                &import.resolved_path,
                import.locator.as_ref(),
                visiting,
                cache,
                hosts,
                natives,
            )
            .map(|program| (alias.clone(), program))
        })
        .collect::<Result<HashMap<_, _>, _>>();
    visiting.pop();
    let imports = imports?;
    let symbols = crate::resolver::Resolver::resolve_with_imports_hosts_and_natives(
        &program,
        &loaded,
        hosts.clone(),
        natives.clone(),
    )?;
    let imported = ImportedProgram {
        program,
        symbols,
        imports,
        base_dir: base_dir.to_path_buf(),
    };
    cache.insert(canonical, imported.clone());
    Ok(imported)
}

impl Evaluator {
    pub fn new(symbols: SymbolTable, program: Program) -> Self {
        Evaluator {
            program,
            symbols,
            call_depth: 0,
            global_cache: HashMap::new(),
            section_cache: HashMap::new(),
            evaluating: HashSet::new(),
            evaluating_sects: HashSet::new(),
            self_stack: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            imported_programs: HashMap::new(),
            hosts: crate::host::HostRegistry::default(),
            natives: crate::stdlib::native_registry(),
            runtime_context: crate::runtime::RuntimeContext::for_base_dir(std::path::Path::new(
                ".",
            )),
            effect_ledger: None,
            next_promise_id: 1,
            pending_promises: Vec::new(),
        }
    }

    /// Registers native functions this evaluator's `ns::fn(...)` calls may
    /// dispatch to — chainable so existing `Evaluator::new(...)` callers
    /// are unaffected.
    pub fn with_hosts(mut self, hosts: crate::host::HostRegistry) -> Self {
        self.hosts = hosts;
        self
    }

    pub fn with_natives(mut self, natives: crate::runtime::NativeRegistry) -> Self {
        self.natives = natives;
        self
    }

    pub fn with_runtime_context(mut self, context: crate::runtime::RuntimeContext) -> Self {
        self.runtime_context = context;
        self
    }

    fn with_effect_ledger(mut self, effect_ledger: Option<crate::session::EffectLedger>) -> Self {
        self.effect_ledger = effect_ledger;
        self
    }

    pub fn evaluate_with_imports(
        program: &Program,
        symbols: &SymbolTable,
        loaded: &std::collections::HashMap<String, crate::loader::LoadedImport>,
    ) -> Result<EvalResult, Vec<SparError>> {
        Self::evaluate_with_imports_and_base(
            program,
            symbols,
            loaded,
            std::path::Path::new("."),
            crate::host::HostRegistry::default(),
        )
    }

    pub fn evaluate_with_imports_and_base(
        program: &Program,
        symbols: &SymbolTable,
        loaded: &std::collections::HashMap<String, crate::loader::LoadedImport>,
        base_dir: &std::path::Path,
        hosts: crate::host::HostRegistry,
    ) -> Result<EvalResult, Vec<SparError>> {
        Self::evaluate_with_imports_base_and_effects(
            program, symbols, loaded, base_dir, hosts, None,
        )
    }

    pub(crate) fn evaluate_with_imports_base_and_effects(
        program: &Program,
        symbols: &SymbolTable,
        loaded: &std::collections::HashMap<String, crate::loader::LoadedImport>,
        base_dir: &std::path::Path,
        hosts: crate::host::HostRegistry,
        effect_ledger: Option<crate::session::EffectLedger>,
    ) -> Result<EvalResult, Vec<SparError>> {
        Self::evaluate_with_imports_base_effects_and_natives(
            program,
            symbols,
            loaded,
            base_dir,
            hosts,
            crate::stdlib::native_registry(),
            effect_ledger,
        )
    }

    pub(crate) fn evaluate_with_imports_base_effects_and_natives(
        program: &Program,
        symbols: &SymbolTable,
        loaded: &std::collections::HashMap<String, crate::loader::LoadedImport>,
        base_dir: &std::path::Path,
        hosts: crate::host::HostRegistry,
        natives: crate::runtime::NativeRegistry,
        effect_ledger: Option<crate::session::EffectLedger>,
    ) -> Result<EvalResult, Vec<SparError>> {
        Self::evaluate_with_imports_base_effects_natives_and_context(
            program,
            symbols,
            loaded,
            base_dir,
            hosts,
            natives,
            effect_ledger,
            Self::runtime_context_for_program(program, base_dir)?,
        )
    }

    /// Runtime context for the main program: starts from the host
    /// environment and overlays the file's `@LoadEnv` dotenv values. A
    /// variable already set in the host environment is never overridden.
    fn runtime_context_for_program(
        program: &Program,
        base_dir: &std::path::Path,
    ) -> Result<crate::runtime::RuntimeContext, Vec<SparError>> {
        let mut context = crate::runtime::RuntimeContext::for_base_dir(base_dir);
        if let Some(path) = &program.load_env {
            let values = crate::dotenv::load(&base_dir.join(path)).map_err(|error| vec![error])?;
            for (key, value) in values {
                if std::env::var_os(&key).is_none() {
                    context.env_set(key, value);
                }
            }
        }
        Ok(context)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn evaluate_with_imports_base_effects_natives_and_context(
        program: &Program,
        symbols: &SymbolTable,
        loaded: &std::collections::HashMap<String, crate::loader::LoadedImport>,
        base_dir: &std::path::Path,
        hosts: crate::host::HostRegistry,
        natives: crate::runtime::NativeRegistry,
        effect_ledger: Option<crate::session::EffectLedger>,
        runtime_context: crate::runtime::RuntimeContext,
    ) -> Result<EvalResult, Vec<SparError>> {
        let imported = build_imported_programs(loaded, base_dir, &hosts, &natives)?;
        let mut ev = Evaluator::new(symbols.clone(), program.clone())
            .with_hosts(hosts)
            .with_natives(natives)
            .with_runtime_context(runtime_context)
            .with_effect_ledger(effect_ledger);
        ev.imported_programs = imported;
        let result = ev.run();
        match result {
            Ok(r) => Ok(r),
            Err(first_err) => {
                let mut errs = vec![first_err];
                errs.extend(std::mem::take(&mut ev.errors));
                Err(errs)
            }
        }
    }

    pub(crate) fn evaluate_for_runtime(
        program: &Program,
        symbols: &SymbolTable,
        loaded: &std::collections::HashMap<String, crate::loader::LoadedImport>,
        base_dir: &std::path::Path,
        hosts: crate::host::HostRegistry,
        natives: crate::runtime::NativeRegistry,
        effect_ledger: Option<crate::session::EffectLedger>,
    ) -> Result<(EvalResult, Vec<PendingPromise>), Vec<SparError>> {
        let imported = build_imported_programs(loaded, base_dir, &hosts, &natives)?;
        let mut evaluator = Evaluator::new(symbols.clone(), program.clone())
            .with_hosts(hosts)
            .with_natives(natives)
            .with_runtime_context(Self::runtime_context_for_program(program, base_dir)?)
            .with_effect_ledger(effect_ledger);
        evaluator.imported_programs = imported;
        match evaluator.run() {
            Ok(result) => Ok((result, evaluator.pending_promises)),
            Err(first_error) => {
                let mut errors = vec![first_error];
                errors.extend(std::mem::take(&mut evaluator.errors));
                Err(errors)
            }
        }
    }

    pub fn evaluate(
        program: &Program,
        symbols: &SymbolTable,
    ) -> Result<EvalResult, Vec<SparError>> {
        let mut ev = Evaluator::new(symbols.clone(), program.clone());
        let result = ev.run();
        match result {
            Ok(r) => Ok(r),
            Err(first_err) => {
                let mut errs = vec![first_err];
                errs.extend(std::mem::take(&mut ev.errors));
                Err(errs)
            }
        }
    }

    /// Evaluate a single expression against an already-computed evaluation
    /// result (globals + sections) — used by `task_lowering` to resolve
    /// ordinary Spar values referenced from task metadata and shell
    /// interpolation, and by the `spar` binary at task-run time to
    /// evaluate a `TemplatePart::Expr` (a `${...}` interpolation that
    /// mixes a task parameter with other values, e.g. a function call).
    /// Reuses `eval_expr` so scalar coercion, `env()`/`str()` calls, and
    /// field access behave identically to normal Spar value evaluation.
    /// `local_scope` lets a caller supply already-known local bindings —
    /// empty for plain `task_lowering` metadata, or the task's bound
    /// parameter values (coerced to `ConfigValue`) when rendering a
    /// `TemplatePart::Expr`.
    pub fn eval_standalone(
        program: &Program,
        symbols: &SymbolTable,
        result: &EvalResult,
        expr: &Expr,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> Result<ConfigValue, SparError> {
        Self::eval_standalone_with_environment(
            program,
            symbols,
            result,
            expr,
            local_scope,
            &std::collections::BTreeMap::new(),
        )
    }

    /// Like `eval_standalone`, but overlays `environment` (a task's
    /// `@LoadEnv` values and `env:` entries) on the host environment the
    /// evaluator's runtime context starts with, so `std/env` functions and
    /// `$NAME` expansion see the same variables the task's commands do.
    pub fn eval_standalone_with_environment(
        program: &Program,
        symbols: &SymbolTable,
        result: &EvalResult,
        expr: &Expr,
        local_scope: &HashMap<String, ConfigValue>,
        environment: &std::collections::BTreeMap<String, String>,
    ) -> Result<ConfigValue, SparError> {
        Self::eval_task_block(program, symbols, result, expr, local_scope, environment)
            .map(|(value, _)| value)
    }

    /// Evaluates a task's native `run { }` block to its shell plan, also
    /// returning the exit code the block requested through `exit(code: N)`,
    /// if it did.
    pub fn eval_task_block(
        program: &Program,
        symbols: &SymbolTable,
        result: &EvalResult,
        expr: &Expr,
        local_scope: &HashMap<String, ConfigValue>,
        environment: &std::collections::BTreeMap<String, String>,
    ) -> Result<(ConfigValue, Option<i32>), SparError> {
        let mut ev = Evaluator::new(symbols.clone(), program.clone());
        ev.global_cache = result.globals.clone();
        ev.section_cache = result.sections.clone();
        for (key, value) in environment {
            ev.runtime_context.env_set(key.clone(), value.clone());
        }
        let value = ev
            .eval_expr(expr, local_scope)
            .map_err(EvalErr::into_kl_error)?;
        Ok((value, ev.runtime_context.requested_exit()))
    }

    pub fn run(&mut self) -> Result<EvalResult, SparError> {
        let graph = self.build_dep_graph();
        let order = crate::depgraph::topological_sort(&graph).map_err(|cycle| {
            let names: Vec<_> = cycle
                .iter()
                .map(|d| match d {
                    DeclId::Global(n) | DeclId::Section(n) => n.clone(),
                })
                .collect();
            SparError::EvalError {
                message: format!("cyclic dependency detected: {:?}", names),
                span: Span::dummy(),
            }
        })?;

        for decl_id in &order {
            match decl_id {
                DeclId::Global(name) => {
                    self.eval_global(name);
                }
                DeclId::Section(name) => {
                    self.eval_section_by_top_name(name);
                }
            }
        }

        let statements: Vec<Statement> = self
            .program
            .items
            .iter()
            .filter_map(|item| match item {
                TopLevelItem::Statement(statement) => Some(statement.clone()),
                _ => None,
            })
            .collect();
        let mut interactive_value = None;
        if !statements.is_empty() {
            let mut module_scope = HashMap::new();
            for statement in &statements {
                match statement {
                    Statement::Expression(expression, _) => {
                        match self.eval_expr(expression, &module_scope) {
                            Ok(value) => interactive_value = Some(value),
                            Err(error) => {
                                self.push_eval_error(error);
                                break;
                            }
                        }
                    }
                    other => {
                        interactive_value = None;
                        if let Err(error) =
                            self.eval_func_stmts(std::slice::from_ref(other), &mut module_scope)
                        {
                            self.push_eval_error(error);
                            break;
                        }
                    }
                }
            }
        }

        if self.errors.is_empty() {
            Ok(EvalResult {
                globals: self.global_cache.clone(),
                sections: self.section_cache.clone(),
                warnings: self.warnings.clone(),
                interactive_value,
            })
        } else {
            Err(self.errors.remove(0))
        }
    }

    /// Calls a declared, zero-argument top-level function by name and
    /// returns its result. Used by Execute mode to invoke `main` after
    /// `run()` has already performed module initialization — never call
    /// this before `run()`, or `main` would see an uninitialized module.
    pub fn call_entry(&mut self, name: &str) -> Result<ConfigValue, SparError> {
        let func_decl = self
            .program
            .items
            .iter()
            .find_map(|item| match item {
                TopLevelItem::Function(f) if f.name == name => Some(f.clone()),
                _ => None,
            })
            .ok_or_else(|| SparError::EvalError {
                message: format!("no zero-argument function named '{name}' to execute"),
                span: Span::dummy(),
            })?;
        let mut local_scope = HashMap::new();
        self.eval_func_stmts(&func_decl.body.stmts.clone(), &mut local_scope)
            .map(|flow| flow.into_return().unwrap_or(ConfigValue::Int(0)))
            .map_err(EvalErr::into_kl_error)
    }

    /// Runs module initialization exactly once, then calls `entry_name` (a
    /// declared zero-argument top-level function — Execute mode's `main`).
    /// Returns the module's `EvalResult` alongside the entry call's value.
    pub fn evaluate_and_call_entry_with_imports_and_base(
        program: &Program,
        symbols: &SymbolTable,
        loaded: &std::collections::HashMap<String, crate::loader::LoadedImport>,
        base_dir: &std::path::Path,
        entry_name: &str,
        hosts: crate::host::HostRegistry,
    ) -> Result<(EvalResult, ConfigValue), Vec<SparError>> {
        Self::evaluate_and_call_entry_with_imports_base_and_effects(
            program, symbols, loaded, base_dir, entry_name, hosts, None,
        )
    }

    pub(crate) fn evaluate_and_call_entry_with_imports_base_and_effects(
        program: &Program,
        symbols: &SymbolTable,
        loaded: &std::collections::HashMap<String, crate::loader::LoadedImport>,
        base_dir: &std::path::Path,
        entry_name: &str,
        hosts: crate::host::HostRegistry,
        effect_ledger: Option<crate::session::EffectLedger>,
    ) -> Result<(EvalResult, ConfigValue), Vec<SparError>> {
        let natives = crate::stdlib::native_registry();
        let imported = build_imported_programs(loaded, base_dir, &hosts, &natives)?;
        let mut ev = Evaluator::new(symbols.clone(), program.clone())
            .with_hosts(hosts)
            .with_natives(natives)
            .with_runtime_context(Self::runtime_context_for_program(program, base_dir)?)
            .with_effect_ledger(effect_ledger);
        ev.imported_programs = imported;
        match ev.run() {
            Ok(eval_result) => {
                let entry_result = ev.call_entry(entry_name).map_err(|e| vec![e])?;
                Ok((eval_result, entry_result))
            }
            Err(first_err) => {
                let mut errs = vec![first_err];
                errs.extend(std::mem::take(&mut ev.errors));
                Err(errs)
            }
        }
    }

    fn push_eval_error(&mut self, e: EvalErr) {
        match e {
            EvalErr::ImportRef { alias, symbol } => {
                self.warnings.push(format!(
                    "cross-file reference `{alias}::{symbol}` was not evaluated — \
                     run via `spar` CLI for full multi-file evaluation"
                ));
            }
            other => self.errors.push(other.into_kl_error()),
        }
    }

    fn absorb_diagnostics(&mut self, sub: &mut Evaluator) {
        self.errors.append(&mut sub.errors);
        self.warnings.append(&mut sub.warnings);
    }
}

// ── Dependency graph ──────────────────────────────────────────────────────────

impl Evaluator {
    fn build_dep_graph(&self) -> crate::depgraph::DepGraph {
        use crate::depgraph::DepGraph;
        let mut graph: DepGraph = HashMap::new();

        for item in &self.program.items {
            match item {
                TopLevelItem::Var(v) => {
                    let node = DeclId::Global(v.name.clone());
                    let mut deps = HashSet::new();
                    if let Some(expr) = &v.value {
                        self.collect_expr_deps(expr, &mut deps);
                    }
                    graph.insert(node, deps);
                }
                TopLevelItem::Section(s) => {
                    let top_name = s.path[0].clone();
                    let node = DeclId::Section(top_name.clone());
                    let mut deps = HashSet::new();
                    self.collect_items_deps(&s.items, &mut deps);
                    // A section referencing its own name is not a real
                    // cross-decl dependency — intra-section ordering is
                    // handled during evaluation itself (see Task 2/4),
                    // and topological_sort treats any self-loop as an
                    // unresolvable cycle.
                    deps.remove(&DeclId::Section(top_name));
                    graph.entry(node).or_default().extend(deps);
                }
                TopLevelItem::Dynamic(d) => {
                    let node = DeclId::Global(d.name.clone());
                    graph.insert(node, HashSet::new());
                }
                _ => {}
            }
        }
        graph
    }

    /// Recurses into `FieldValue::Nested` at any depth — a spread or expr
    /// reference nested inside a field's own `{ ... }` body (not just a
    /// section's own top-level items) still needs a dependency edge, same
    /// as a top-level one.
    fn collect_items_deps(&self, items: &[SectionItem], deps: &mut HashSet<DeclId>) {
        for item in items {
            match item {
                SectionItem::Field(f) => match &f.value {
                    Some(FieldValue::Expr(e)) => self.collect_expr_deps(e, deps),
                    Some(FieldValue::Nested(sub)) => self.collect_items_deps(sub, deps),
                    None => {}
                },
                SectionItem::Spread(sp) => {
                    self.collect_expr_deps(&sp.expr, deps);
                }
            }
        }
    }

    fn collect_expr_deps(&self, expr: &Expr, deps: &mut HashSet<DeclId>) {
        match expr {
            Expr::Object(items, _) => self.collect_items_deps(items, deps),
            Expr::Closure { body, .. } => match body {
                ClosureBody::Expr(value) => self.collect_expr_deps(value, deps),
                ClosureBody::Block(body) => {
                    for stmt in &body.stmts {
                        match stmt {
                            Statement::LocalVar(local) => {
                                self.collect_expr_deps(&local.value, deps)
                            }
                            Statement::Assignment { value, .. }
                            | Statement::FieldAssignment { value, .. }
                            | Statement::Expression(value, _) => {
                                self.collect_expr_deps(value, deps)
                            }
                            Statement::Return(ReturnValue::Expr(value), _) => {
                                self.collect_expr_deps(value, deps)
                            }
                            _ => {}
                        }
                    }
                }
            },
            Expr::NamespaceRef(nr) => {
                if let Some(top) = nr.segments.first() {
                    if self.symbols.globals.contains_key(top.as_str()) {
                        deps.insert(DeclId::Global(top.clone()));
                    } else if self.symbols.sections.keys().any(|k| k.first() == Some(top)) {
                        deps.insert(DeclId::Section(top.clone()));
                    }
                }
            }
            Expr::Call { name, args, .. } => {
                for arg in args {
                    self.collect_expr_deps(&arg.value, deps);
                }
                if let Some(fe) = self.symbols.functions.get(name) {
                    deps.extend(fe.closure_deps.clone());
                }
            }
            Expr::BinaryOp(b) => {
                self.collect_expr_deps(&b.lhs, deps);
                self.collect_expr_deps(&b.rhs, deps);
            }
            Expr::Unary { operand, .. } => self.collect_expr_deps(operand, deps),
            Expr::Await { value, .. } => self.collect_expr_deps(value, deps),
            Expr::Comprehension { source, body, .. } => {
                self.collect_expr_deps(source, deps);
                self.collect_expr_deps(body, deps);
            }
            Expr::List(items, _) => {
                for item in items {
                    self.collect_expr_deps(item, deps);
                }
            }
            Expr::Grouped(inner, _) => self.collect_expr_deps(inner, deps),
            Expr::FnCall(fc) => {
                for arg in &fc.args {
                    self.collect_expr_deps(arg, deps);
                }
            }
            Expr::String(s) => {
                for part in &s.parts {
                    if let StringPart::Expr(e) = part {
                        self.collect_expr_deps(e, deps);
                    }
                }
            }
            Expr::Index { source, index, .. } => {
                self.collect_expr_deps(source, deps);
                self.collect_expr_deps(index, deps);
            }
            Expr::MethodCall { receiver, args, .. } => {
                self.collect_expr_deps(receiver, deps);
                for argument in args {
                    self.collect_expr_deps(argument, deps);
                }
            }
            Expr::StructuredPipe { input, stage, .. } => {
                self.collect_expr_deps(input, deps);
                self.collect_expr_deps(stage, deps);
            }
            Expr::FieldAccess { base, .. } => self.collect_expr_deps(base, deps),
            Expr::Shell(_)
            | Expr::ExecShell(_)
            | Expr::CommandSubstitution(_)
            | Expr::Literal(_) => {}
        }
    }
}

// ── Global evaluation ─────────────────────────────────────────────────────────

impl Evaluator {
    /// Why `eval_global` gave nothing for `name`: a real cycle, a name that was
    /// never declared, or a declaration whose own evaluation already failed.
    fn unresolved_global(&self, name: &str, span: &Span) -> EvalErr {
        if self.evaluating.contains(name) {
            return EvalErr::CyclicRef {
                name: name.to_string(),
                span: span.clone(),
            };
        }
        let declared = self.symbols.globals.contains_key(name)
            || self.program.items.iter().any(|item| match item {
                TopLevelItem::Var(decl) => decl.name == name,
                TopLevelItem::Dynamic(decl) => decl.name == name,
                _ => false,
            });
        let message = if declared {
            format!("`{name}` could not be evaluated (see the error above)")
        } else {
            format!("undefined reference: `{name}` is not declared in the global scope")
        };
        EvalErr::Fatal {
            message,
            span: span.clone(),
        }
    }

    fn eval_global(&mut self, name: &str) -> Option<ConfigValue> {
        if let Some(cached) = self.global_cache.get(name) {
            return Some(cached.clone());
        }
        if self.evaluating.contains(name) {
            self.errors.push(SparError::EvalError {
                message: format!(
                    "cyclic reference: `{name}` depends on itself — \
                     check for circular var references"
                ),
                span: Span::dummy(),
            });
            return None;
        }

        let value_expr = self.program.items.iter().find_map(|item| match item {
            TopLevelItem::Var(d) if d.name == name => d.value.clone(),
            TopLevelItem::Dynamic(d) if d.name == name => d.value.clone(),
            _ => None,
        });

        let expr = value_expr?;

        self.evaluating.insert(name.to_string());
        let result = self.eval_expr(&expr, &HashMap::new());
        self.evaluating.remove(name);

        match result {
            Ok(val) => {
                self.global_cache.insert(name.to_string(), val.clone());
                Some(val)
            }
            Err(e) => {
                self.push_eval_error(e);
                None
            }
        }
    }
}

// ── Section evaluation ────────────────────────────────────────────────────────

impl Evaluator {
    fn eval_section_by_top_name(&mut self, top_name: &str) {
        // Collect all section paths with this top name first (avoid borrow conflicts)
        let paths: Vec<Vec<String>> = self
            .program
            .items
            .iter()
            .filter_map(|item| {
                if let TopLevelItem::Section(s) = item {
                    if s.path.first().map(|s| s.as_str()) == Some(top_name) {
                        return Some(s.path.clone());
                    }
                }
                None
            })
            .collect();
        for path in paths {
            self.eval_section_by_path(&path);
        }
    }

    fn eval_section_by_path(
        &mut self,
        path: &[String],
    ) -> Option<indexmap::IndexMap<String, ConfigValue>> {
        let path_vec = path.to_vec();
        if let Some(cached) = self.section_cache.get(&path_vec) {
            return Some(cached.clone());
        }
        if self.evaluating_sects.contains(&path_vec) {
            self.errors.push(SparError::EvalError {
                message: format!(
                    "cyclic section reference: `[{}]` spreads into itself",
                    path_vec.join(".")
                ),
                span: Span::dummy(),
            });
            return None;
        }

        let decl = self.program.items.iter().find_map(|item| {
            if let TopLevelItem::Section(d) = item {
                if d.path == path_vec {
                    Some(d.clone())
                } else {
                    None
                }
            } else {
                None
            }
        });

        let decl = decl?;

        self.evaluating_sects.insert(path_vec.clone());
        self.self_stack.push(SelfFrame {
            top_name: path_vec[0].clone(),
            fields: HashMap::new(),
        });
        let fields = self.eval_section_decl(&decl);
        self.self_stack.pop();
        self.evaluating_sects.remove(&path_vec);

        self.section_cache.insert(path_vec, fields.clone());
        Some(fields)
    }

    fn eval_section_decl(&mut self, decl: &SectionDecl) -> indexmap::IndexMap<String, ConfigValue> {
        let path = decl.path.clone();
        let mut items = decl.items.clone();
        if let Some(binding) = &decl.type_binding {
            if let Some((_, fields)) = self.type_fields_for_binding(&binding.ty) {
                for field in fields {
                    if field.default.is_some()
                        && !items.iter().any(|item| {
                            matches!(item, SectionItem::Field(existing) if existing.name == field.name)
                        })
                    {
                        items.push(SectionItem::Field(FieldDecl {
                            name: field.name,
                            optional: field.optional,
                            ty: None,
                            value: field.default.map(FieldValue::Expr),
                            end_line: field.span.line,
                            span: field.span,
                        }));
                    }
                }
            }
        }
        self.eval_section_fields(&items, &path, &HashMap::new())
    }

    fn type_fields_for_binding(
        &self,
        ty: &SparType,
    ) -> Option<(String, Vec<crate::ast::TypeField>)> {
        let (name, arguments) = match ty {
            SparType::Named(name) => (name, None),
            SparType::Applied { name, arguments } => (name, Some(arguments)),
            _ => return None,
        };
        let entry = self.symbols.types.get(name)?;
        let fields = match arguments {
            Some(arguments) => {
                let substitution: std::collections::HashMap<_, _> = entry
                    .type_parameters
                    .iter()
                    .zip(arguments.iter())
                    .map(|(parameter, argument)| (parameter.name.clone(), argument.clone()))
                    .collect();
                entry
                    .fields
                    .iter()
                    .map(|field| crate::typechecker::substitute_type_field(field, &substitution))
                    .collect()
            }
            None => entry.fields.clone(),
        };
        Some((crate::typechecker::display_type(ty), fields))
    }

    fn eval_section_fields(
        &mut self,
        items: &[SectionItem],
        parent_path: &[String],
        local_scope: &HashMap<String, ConfigValue>,
    ) -> indexmap::IndexMap<String, ConfigValue> {
        let mut result: indexmap::IndexMap<String, ConfigValue> = indexmap::IndexMap::new();

        for item in items {
            match item {
                SectionItem::Spread(spread) => {
                    let target = self.eval_spread(&spread.expr, local_scope);
                    if let Some(fields) = target {
                        for (k, v) in fields {
                            result.entry(k).or_insert(v);
                        }
                    }
                }

                SectionItem::Field(field) => {
                    match &field.value {
                        Some(FieldValue::Expr(val_expr)) => {
                            let val_expr = val_expr.clone();
                            match self.eval_expr(&val_expr, local_scope) {
                                Ok(ConfigValue::Section(map)) => {
                                    // Section-returning function call — register at nested path
                                    let nested_path =
                                        [parent_path, std::slice::from_ref(&field.name)].concat();
                                    if let Some(frame) = self.self_stack.last_mut() {
                                        frame.fields.insert(
                                            nested_path.clone(),
                                            ConfigValue::Section(map.clone()),
                                        );
                                    }
                                    self.section_cache.insert(nested_path, map);
                                }
                                Ok(val) => {
                                    let field_path =
                                        [parent_path, std::slice::from_ref(&field.name)].concat();
                                    if let Some(frame) = self.self_stack.last_mut() {
                                        frame.fields.insert(field_path, val.clone());
                                    }
                                    result.insert(field.name.clone(), val);
                                }
                                Err(e) => {
                                    self.push_eval_error(e);
                                }
                            }
                        }
                        Some(FieldValue::Nested(sub_items)) => {
                            let nested_path =
                                [parent_path, std::slice::from_ref(&field.name)].concat();
                            let nested_map =
                                self.eval_section_fields(sub_items, &nested_path, &HashMap::new());
                            if let Some(frame) = self.self_stack.last_mut() {
                                frame.fields.insert(
                                    nested_path.clone(),
                                    ConfigValue::Section(nested_map.clone()),
                                );
                            }
                            self.section_cache.insert(nested_path, nested_map);
                            // Do NOT insert into result — nested sections aren't scalar values
                        }
                        None => {}
                    }
                }
            }
        }

        result
    }

    /// Moves the sections registered directly under `path` in `section_cache`
    /// into `map` as `ConfigValue::Section` values (recursively), removing
    /// them from the cache.
    fn inline_nested_sections(
        &mut self,
        path: &[String],
        map: &mut indexmap::IndexMap<String, ConfigValue>,
    ) {
        let children: Vec<Vec<String>> = self
            .section_cache
            .keys()
            .filter(|key| key.len() == path.len() + 1 && key.starts_with(path))
            .cloned()
            .collect();
        for child_path in children {
            let Some(mut child) = self.section_cache.remove(&child_path) else {
                continue;
            };
            self.inline_nested_sections(&child_path, &mut child);
            if let Some(name) = child_path.last() {
                map.insert(name.clone(), ConfigValue::Section(child));
            }
        }
    }

    fn eval_spread(
        &mut self,
        expr: &Expr,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> Option<indexmap::IndexMap<String, ConfigValue>> {
        match expr {
            Expr::NamespaceRef(nr) => match nr.segments.as_slice() {
                [name] => self.eval_section_by_path(std::slice::from_ref(name)),
                [alias, name] => {
                    self.warnings.push(format!(
                        "spread `...{alias}::{name}` skipped — \
                         cross-file spreads are resolved by the CLI"
                    ));
                    None
                }
                segs => {
                    self.warnings.push(format!(
                        "spread `...{}` skipped — cross-file spreads are resolved by the CLI",
                        segs.join("::")
                    ));
                    None
                }
            },
            Expr::FieldAccess { base, field, .. } if matches!(base.as_ref(), Expr::NamespaceRef(nr) if nr.segments == ["global"]) => {
                self.eval_section_by_path(std::slice::from_ref(field))
            }
            other => {
                let span = match other {
                    Expr::Call { span, .. } => span.clone(),
                    Expr::FnCall(fc) => fc.span.clone(),
                    _ => Span::dummy(),
                };
                match self.eval_expr(other, local_scope) {
                    Ok(ConfigValue::Section(map)) => Some(map),
                    Ok(v) => {
                        self.push_eval_error(EvalErr::TypeMismatch {
                            expected: "section",
                            got: v.type_name(),
                        });
                        None
                    }
                    Err(EvalErr::CyclicRef { name, .. }) => {
                        self.push_eval_error(EvalErr::CyclicRef { name, span });
                        None
                    }
                    Err(e) => {
                        self.push_eval_error(e);
                        None
                    }
                }
            }
        }
    }

    fn eval_index(
        &mut self,
        source: &Expr,
        index: &Expr,
        span: &Span,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> EvalResult_ {
        let source_val = self.eval_expr(source, local_scope)?;
        let index_val = self.eval_expr(index, local_scope)?;
        match (source_val, index_val) {
            (ConfigValue::List(items), ConfigValue::Int(i)) => {
                if i < 0 || i as usize >= items.len() {
                    Err(EvalErr::CyclicRef {
                        name: format!(
                            "index {} out of bounds for list of length {}",
                            i,
                            items.len()
                        ),
                        span: span.clone(),
                    })
                } else {
                    Ok(items[i as usize].clone())
                }
            }
            (ConfigValue::Section(mut fields), ConfigValue::Int(i)) => {
                let Some(ConfigValue::List(items)) = fields.shift_remove("values") else {
                    return Err(EvalErr::TypeMismatch {
                        expected: "list or Bytes",
                        got: "section",
                    });
                };
                if i < 0 || i as usize >= items.len() {
                    Err(EvalErr::CyclicRef {
                        name: format!(
                            "index {} out of bounds for Bytes of length {}",
                            i,
                            items.len()
                        ),
                        span: span.clone(),
                    })
                } else {
                    Ok(items[i as usize].clone())
                }
            }
            (_, iv) => Err(EvalErr::TypeMismatch {
                expected: "list[int]",
                got: iv.type_name(),
            }),
        }
    }
}

// ── Expression evaluation ─────────────────────────────────────────────────────

impl Evaluator {
    fn eval_expr(
        &mut self,
        expr: &Expr,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> EvalResult_ {
        // The innermost expression that fails claims errors that carry no
        // span of their own, so they surface on the right line.
        self.eval_expr_inner(expr, local_scope)
            .map_err(|error| match expr.span() {
                Some(span) => error.located_at(span),
                None => error,
            })
    }

    fn eval_expr_inner(
        &mut self,
        expr: &Expr,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> EvalResult_ {
        match expr {
            Expr::Literal(Literal::Int(n)) => Ok(ConfigValue::Int(*n)),
            Expr::Literal(Literal::Float(f)) => Ok(ConfigValue::Float(*f)),
            Expr::Literal(Literal::Bool(b)) => Ok(ConfigValue::Bool(*b)),
            Expr::String(s) => self.eval_interp_string(s, local_scope),
            Expr::Object(items, _) => {
                // parent_path is empty — an anonymous object literal has no
                // path identity of its own. Nested self-references /
                // section_cache entries inside an object literal are
                // therefore not uniquely path-addressed if multiple object
                // literals exist in the same evaluation scope; deliberate,
                // documented scope limitation — object literals are
                // structural data (JSON-object-like), not full
                // cross-referenceable sections.
                // Nested object literals register their own fields in
                // `section_cache` under the path they are given. Use a private
                // scratch prefix, then move those entries into the returned
                // map so a literal keeps its nested objects inline instead of
                // leaking them out as root-level sections.
                let scratch = vec![format!("\u{0}object@{:p}", items.as_ptr())];
                let mut map = self.eval_section_fields(items, &scratch, local_scope);
                self.inline_nested_sections(&scratch, &mut map);
                Ok(ConfigValue::Section(map))
            }
            Expr::Closure { .. } => Err(EvalErr::Fatal {
                message:
                    "closure runtime evaluation is not available until the closure-runtime phase"
                        .into(),
                span: expr.span().cloned().unwrap_or_else(Span::dummy),
            }),
            Expr::List(items, _) => {
                let mut vals = Vec::with_capacity(items.len());
                for item in items {
                    vals.push(self.eval_expr(item, local_scope)?);
                }
                Ok(ConfigValue::List(vals))
            }
            Expr::Grouped(inner, _) => self.eval_expr(inner, local_scope),
            Expr::NamespaceRef(nr) => self.eval_namespace_ref(nr, local_scope),
            Expr::MethodCall { span, .. } => Err(EvalErr::Fatal {
                message: "method calls require the compiled runtime".into(),
                span: span.clone(),
            }),
            Expr::StructuredPipe { span, .. } => Err(EvalErr::Fatal {
                message: "structured value pipes require the compiled runtime".into(),
                span: span.clone(),
            }),
            Expr::FieldAccess {
                base, field, span, ..
            } => self.eval_field_access(base, field, span, local_scope),
            Expr::FnCall(fc) => {
                let fc = fc.clone();
                self.eval_fn_call(&fc, local_scope)
            }
            Expr::BinaryOp(op) => {
                let op = op.clone();
                self.eval_binop(&op, local_scope)
            }
            Expr::Call {
                name, args, span, ..
            } => {
                let name = name.clone();
                let args = args.clone();
                let span = span.clone();
                self.eval_call(&name, &args, &span, local_scope)
            }
            Expr::Unary { op, operand, .. } => {
                let operand = operand.clone();
                let op = op.clone();
                match (op, self.eval_expr(&operand, local_scope)?) {
                    (UnOp::Not, ConfigValue::Bool(b)) => Ok(ConfigValue::Bool(!b)),
                    (UnOp::Not, v) => Err(EvalErr::TypeMismatch {
                        expected: "bool",
                        got: v.type_name(),
                    }),
                    (UnOp::Neg, ConfigValue::Int(n)) => Ok(ConfigValue::Int(-n)),
                    (UnOp::Neg, ConfigValue::Float(f)) => Ok(ConfigValue::Float(-f)),
                    (UnOp::Neg, v) => Err(EvalErr::TypeMismatch {
                        expected: "int or float",
                        got: v.type_name(),
                    }),
                }
            }
            Expr::Await { .. } => Err(EvalErr::Host {
                message: "`await` requires the compiled async runtime".into(),
            }),
            Expr::Index {
                source,
                index,
                span,
            } => {
                let source = source.clone();
                let index = index.clone();
                let span = span.clone();
                self.eval_index(&source, &index, &span, local_scope)
            }
            Expr::Comprehension {
                var_name,
                source,
                body,
                ..
            } => {
                let var_name = var_name.clone();
                let source = source.clone();
                let body = body.clone();
                let source_val = self.eval_expr(&source, local_scope)?;
                match source_val {
                    ConfigValue::List(items) => {
                        let mut results = Vec::new();
                        for item in items {
                            let mut inner_scope = local_scope.clone();
                            inner_scope.insert(var_name.clone(), item);
                            results.push(self.eval_expr(&body, &inner_scope)?);
                        }
                        Ok(ConfigValue::List(results))
                    }
                    v => Err(EvalErr::TypeMismatch {
                        expected: "list",
                        got: v.type_name(),
                    }),
                }
            }
            Expr::Shell(shell) => Ok(ConfigValue::Shell(
                self.eval_deferred_shell(shell, local_scope)?,
            )),
            Expr::ExecShell(shell) => {
                let plan = self.eval_deferred_shell(shell, local_scope)?;
                // The child sees the runtime's environment (`set()`, a loaded
                // `.env`, a task's `env:`), like `$( )` already does.
                let options = spar_process::ExecutionOptions {
                    environment: Some(self.runtime_context.environment_pairs()),
                    ..spar_process::ExecutionOptions::default()
                };
                let run = || {
                    let outcome =
                        execute_shell_plan_with_options(&plan, &options).map_err(|error| {
                            EvalErr::Host {
                                message: format!("could not execute shell plan: {error}"),
                            }
                        })?;
                    Ok(ConfigValue::Section(indexmap::IndexMap::from([
                        ("success".to_string(), ConfigValue::Bool(outcome.success)),
                        (
                            "exitCode".to_string(),
                            ConfigValue::Int(i64::from(outcome.exit_code)),
                        ),
                    ])))
                };
                let ledger = self.effect_ledger.clone();
                match ledger {
                    Some(ledger) => ledger.get_or_try_run((shell.span.start, shell.span.end), run),
                    None => run(),
                }
            }
            Expr::CommandSubstitution(shell) => Ok(ConfigValue::Str(
                self.eval_deferred_command_substitution(shell, local_scope)?,
            )),
        }
    }

    fn eval_deferred_shell(
        &mut self,
        shell: &ShellExpr,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> Result<spar_command::ShellPlan, EvalErr> {
        if !shell.statements.is_empty() {
            let mut mixed_scope = local_scope.clone();
            let (plan, _) =
                self.eval_deferred_shell_statements(&shell.statements, &mut mixed_scope)?;
            return Ok(plan);
        }

        let mut steps = Vec::with_capacity(shell.steps.len());
        for (join, step) in &shell.steps {
            let join = match join {
                ShellJoin::Always => spar_command::Join::Always,
                ShellJoin::OnSuccess => spar_command::Join::OnSuccess,
                ShellJoin::OnFailure => spar_command::Join::OnFailure,
            };
            let step = match step {
                ShellStep::Command(command) => spar_command::Step::Command(
                    self.eval_deferred_shell_command(command, local_scope)?,
                ),
                ShellStep::Pipeline(commands) => {
                    let mut lowered = Vec::with_capacity(commands.len());
                    for command in commands {
                        lowered.push(self.eval_deferred_shell_command(command, local_scope)?);
                    }
                    spar_command::Step::Pipeline(spar_command::PipelinePlan { commands: lowered })
                }
                ShellStep::MixedPipeline(pipeline) => {
                    return Err(EvalErr::Fatal {
                        message: "mixed structured pipelines require the compiled runtime".into(),
                        span: pipeline.span.clone(),
                    });
                }
            };
            steps.push((join, step));
        }
        Ok(spar_command::ShellPlan { steps })
    }

    fn eval_deferred_shell_statements(
        &mut self,
        statements: &[Statement],
        local_scope: &mut HashMap<String, ConfigValue>,
    ) -> Result<(spar_command::ShellPlan, StatementFlow), EvalErr> {
        let mut steps = Vec::new();

        for statement in statements {
            // `exit(code: N)` ends the block: nothing after it contributes to
            // the plan (commands queued before it still run).
            if let Some(code) = self.runtime_context.requested_exit() {
                return Ok((
                    spar_command::ShellPlan { steps },
                    StatementFlow::Return(ConfigValue::Int(i64::from(code))),
                ));
            }
            match statement {
                Statement::LocalVar(declaration) => {
                    let value = self.eval_expr(&declaration.value, local_scope)?;
                    local_scope.insert(declaration.name.clone(), value);
                }
                Statement::Assignment { name, value, .. } => {
                    let value = self.eval_expr(value, local_scope)?;
                    if local_scope.contains_key(name) {
                        local_scope.insert(name.clone(), value);
                    } else {
                        self.global_cache.insert(name.clone(), value);
                    }
                }
                Statement::FieldAssignment {
                    base,
                    fields,
                    value,
                    span,
                } => {
                    let value = self.eval_expr(value, local_scope)?;
                    if let Some(target) = local_scope.get_mut(base) {
                        assign_config_field_path(target, fields, value, span)?;
                    } else if let Some(target) = self.global_cache.get_mut(base) {
                        assign_config_field_path(target, fields, value, span)?;
                    } else {
                        return Err(EvalErr::PathNotFound {
                            path: base.clone(),
                            span: span.clone(),
                        });
                    }
                }
                Statement::Expression(expression, _) => {
                    if let ConfigValue::Shell(plan) = self.eval_expr(expression, local_scope)? {
                        steps.extend(plan.steps);
                    }
                }
                Statement::Return(value, _) => {
                    let value = match value {
                        ReturnValue::Void => ConfigValue::Int(0),
                        ReturnValue::Expr(expression) => self.eval_expr(expression, local_scope)?,
                        ReturnValue::SectionBlock(fields) => {
                            let mut section = indexmap::IndexMap::new();
                            for field in fields {
                                section.insert(
                                    field.name.clone(),
                                    self.eval_expr(&field.value, local_scope)?,
                                );
                            }
                            ConfigValue::Section(section)
                        }
                    };
                    return Ok((
                        spar_command::ShellPlan { steps },
                        StatementFlow::Return(value),
                    ));
                }
                Statement::Break(_) => {
                    return Ok((spar_command::ShellPlan { steps }, StatementFlow::Break));
                }
                Statement::Continue(_) => {
                    return Ok((spar_command::ShellPlan { steps }, StatementFlow::Continue));
                }
                Statement::If(if_statement) => {
                    let condition = self.eval_expr(&if_statement.condition, local_scope)?;
                    let branch = match condition {
                        ConfigValue::Bool(true) => &if_statement.then_stmts,
                        ConfigValue::Bool(false) => &if_statement.else_stmts,
                        _ => unreachable!("typechecker ensures bool condition"),
                    };
                    let snapshot = local_scope.clone();
                    let (branch_plan, flow) =
                        self.eval_deferred_shell_statements(branch, local_scope)?;
                    restore_block_scope(local_scope, &snapshot, branch, None);
                    steps.extend(branch_plan.steps);
                    if !matches!(flow, StatementFlow::Normal) {
                        return Ok((spar_command::ShellPlan { steps }, flow));
                    }
                }
                Statement::For(for_statement) => {
                    let items = match self.eval_expr(&for_statement.iterable, local_scope)? {
                        ConfigValue::List(items) => items,
                        _ => unreachable!("typechecker ensures for-loop iterable is a list"),
                    };
                    for (index, item) in items.into_iter().enumerate() {
                        let snapshot = local_scope.clone();
                        match &for_statement.binding {
                            ForBinding::Value { name, .. } => {
                                local_scope.insert(name.clone(), item);
                            }
                            ForBinding::Indexed {
                                index_name,
                                value_name,
                                ..
                            } => {
                                local_scope
                                    .insert(index_name.clone(), ConfigValue::Int(index as i64));
                                local_scope.insert(value_name.clone(), item);
                            }
                        }
                        let (iteration_plan, flow) =
                            self.eval_deferred_shell_statements(&for_statement.body, local_scope)?;
                        restore_block_scope(
                            local_scope,
                            &snapshot,
                            &for_statement.body,
                            Some(&for_statement.binding),
                        );
                        steps.extend(iteration_plan.steps);
                        match flow {
                            StatementFlow::Normal | StatementFlow::Continue => {}
                            StatementFlow::Break => break,
                            flow @ StatementFlow::Return(_) => {
                                return Ok((spar_command::ShellPlan { steps }, flow));
                            }
                        }
                    }
                }
                Statement::Try(try_statement) => {
                    let snapshot = local_scope.clone();
                    match self.eval_deferred_shell_statements(&try_statement.body, local_scope) {
                        Ok((body_plan, flow)) => {
                            restore_block_scope(local_scope, &snapshot, &try_statement.body, None);
                            steps.extend(body_plan.steps);
                            if !matches!(flow, StatementFlow::Normal) {
                                return Ok((spar_command::ShellPlan { steps }, flow));
                            }
                        }
                        Err(error @ EvalErr::Fatal { .. }) => return Err(error),
                        Err(error) => {
                            restore_block_scope(local_scope, &snapshot, &try_statement.body, None);
                            let handler_snapshot = local_scope.clone();
                            if let Some(name) = &try_statement.catch_name {
                                local_scope.insert(
                                    name.clone(),
                                    ConfigValue::Error {
                                        message: error.into_kl_error().to_string(),
                                        kind: "runtime".into(),
                                        code: 1,
                                        cause: None,
                                    },
                                );
                            }
                            let (handler_plan, flow) = self.eval_deferred_shell_statements(
                                &try_statement.handler,
                                local_scope,
                            )?;
                            restore_block_scope(
                                local_scope,
                                &handler_snapshot,
                                &try_statement.handler,
                                None,
                            );
                            if let Some(name) = &try_statement.catch_name {
                                match handler_snapshot.get(name) {
                                    Some(value) => {
                                        local_scope.insert(name.clone(), value.clone());
                                    }
                                    None => {
                                        local_scope.remove(name);
                                    }
                                }
                            }
                            steps.extend(handler_plan.steps);
                            if !matches!(flow, StatementFlow::Normal) {
                                return Ok((spar_command::ShellPlan { steps }, flow));
                            }
                        }
                    }
                }
            }
        }

        Ok((spar_command::ShellPlan { steps }, StatementFlow::Normal))
    }

    fn eval_deferred_shell_command(
        &mut self,
        command: &ShellCommandExpr,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> Result<spar_command::CommandPlan, EvalErr> {
        let mut args = Vec::with_capacity(command.args.len());
        for argument in &command.args {
            let spread = match argument.parts.as_slice() {
                [ShellWordPart::Literal(prefix), ShellWordPart::Expr(expression)]
                    if prefix == "..." =>
                {
                    Some(expression)
                }
                _ => None,
            };
            if let Some(expression) = spread {
                let value = self.eval_expr(expression, local_scope)?;
                let values = match value {
                    ConfigValue::List(values) => values,
                    other => {
                        return Err(EvalErr::TypeMismatch {
                            expected: "list",
                            got: other.type_name(),
                        })
                    }
                };
                for value in values {
                    args.push(shell_scalar_to_string(value)?);
                }
            } else {
                args.push(self.eval_deferred_shell_word(argument, local_scope)?);
            }
        }

        let mut env = Vec::with_capacity(command.environment.len());
        for entry in &command.environment {
            env.push(spar_command::EnvironmentOverride {
                key: entry.name.clone(),
                value: self.eval_deferred_shell_word(&entry.value, local_scope)?,
            });
        }
        Ok(spar_command::CommandPlan {
            program: self.eval_deferred_shell_word(&command.program, local_scope)?,
            args,
            env,
            cwd: None,
            stdin: self.eval_deferred_shell_redirect(command.stdin.as_ref(), local_scope)?,
            stdout: self.eval_deferred_shell_redirect(command.stdout.as_ref(), local_scope)?,
            stderr: self.eval_deferred_shell_redirect(command.stderr.as_ref(), local_scope)?,
            redirections: command
                .redirections
                .iter()
                .map(|redirect| {
                    Ok(spar_command::OrderedRedirection {
                        fd: redirect.fd,
                        target: match &redirect.target {
                            ShellFdRedirectTarget::File(file) => spar_command::Redirection::File {
                                path: self.eval_deferred_shell_word(&file.target, local_scope)?,
                                mode: file.mode.clone(),
                            },
                            ShellFdRedirectTarget::Duplicate(fd) => {
                                spar_command::Redirection::DuplicateFd(*fd)
                            }
                        },
                    })
                })
                .collect::<Result<Vec<_>, EvalErr>>()?,
            background: command.background,
        })
    }

    fn eval_deferred_shell_redirect(
        &mut self,
        redirect: Option<&ShellRedirect>,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> Result<Option<spar_command::Redirection>, EvalErr> {
        redirect
            .map(|redirect| {
                Ok(spar_command::Redirection::File {
                    path: self.eval_deferred_shell_word(&redirect.target, local_scope)?,
                    mode: redirect.mode.clone(),
                })
            })
            .transpose()
    }

    fn eval_deferred_shell_word(
        &mut self,
        word: &ShellWord,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> Result<String, EvalErr> {
        let mut output = String::new();
        for part in &word.parts {
            match part {
                ShellWordPart::Literal(value) => output.push_str(value),
                ShellWordPart::Expr(expression) => {
                    output.push_str(&shell_scalar_to_string(
                        self.eval_expr(expression, local_scope)?,
                    )?);
                }
                ShellWordPart::Environment(name) => {
                    if name == "?" || name == "!" {
                        return Err(EvalErr::Host {
                            message: format!("${name} requires an active shell execution context"),
                        });
                    }
                    output.push_str(self.runtime_context.env_get(name).unwrap_or_default());
                }
                ShellWordPart::CommandSubstitution(shell) => {
                    output.push_str(&self.eval_deferred_command_substitution(shell, local_scope)?);
                }
            }
        }
        Ok(output)
    }

    fn eval_deferred_command_substitution(
        &mut self,
        shell: &ShellExpr,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> Result<String, EvalErr> {
        let mut plan = self.eval_deferred_shell(shell, local_scope)?;
        if plan.steps.is_empty() {
            return Err(EvalErr::Host {
                message: "empty command substitution".into(),
            });
        }

        let cwd = spar_command::WorkingDirectory::Path(
            self.runtime_context.cwd().to_string_lossy().into_owned(),
        );
        let options = spar_process::ExecutionOptions {
            capture_stdout: true,
            capture_stderr: false,
            environment: Some(self.runtime_context.environment_pairs()),
        };
        let mut success = true;
        let mut exit_code = 0;
        let mut captured = Vec::new();
        let mut executed = false;

        for (join, step) in &mut plan.steps {
            let should_run = match join {
                spar_command::Join::Always => true,
                spar_command::Join::OnSuccess => success,
                spar_command::Join::OnFailure => !success,
            };
            if !should_run {
                continue;
            }

            match step {
                spar_command::Step::Command(command) => {
                    if command.background {
                        return Err(EvalErr::Host {
                            message:
                                "background commands are not allowed inside command substitution"
                                    .into(),
                        });
                    }
                    if command.program == "cd" {
                        return Err(EvalErr::Host {
                            message: "'cd' inside command substitution is not supported".into(),
                        });
                    }
                    if command.cwd.is_none() {
                        command.cwd = Some(cwd.clone());
                    }
                }
                spar_command::Step::Pipeline(pipeline) => {
                    if pipeline.commands.iter().any(|command| command.background) {
                        return Err(EvalErr::Host {
                            message:
                                "background commands are not allowed inside command substitution"
                                    .into(),
                        });
                    }
                    if pipeline
                        .commands
                        .iter()
                        .any(|command| command.program == "cd")
                    {
                        return Err(EvalErr::Host {
                            message: "'cd' inside command substitution is not supported".into(),
                        });
                    }
                    for command in &mut pipeline.commands {
                        if command.cwd.is_none() {
                            command.cwd = Some(cwd.clone());
                        }
                    }
                }
            }

            let output = match step {
                spar_command::Step::Command(command) => {
                    spar_process::run_command(command, &options)
                }
                spar_command::Step::Pipeline(pipeline) => {
                    spar_process::run_pipeline(pipeline, &options)
                }
            }
            .map_err(|error| EvalErr::Host {
                message: format!("command substitution failed to start: {error}"),
            })?;
            success = output.status.success;
            exit_code = output.status.code.unwrap_or(if success { 0 } else { 1 });
            captured.extend_from_slice(&output.stdout.unwrap_or_default());
            executed = true;
        }

        if !executed {
            return Err(EvalErr::Host {
                message: "empty command substitution".into(),
            });
        }
        if !success {
            return Err(EvalErr::Host {
                message: format!("command substitution exited with status {exit_code}"),
            });
        }
        let mut text = String::from_utf8(captured).map_err(|_| EvalErr::Host {
            message: "command substitution output is not valid UTF-8".into(),
        })?;
        while text.ends_with('\n') || text.ends_with('\r') {
            text.pop();
        }
        Ok(text)
    }

    fn eval_interp_string(
        &mut self,
        s: &InterpolString,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> EvalResult_ {
        let mut result = String::new();
        for part in &s.parts {
            match part {
                StringPart::Literal(text) => result.push_str(text),
                StringPart::Expr(expr) => {
                    let val = self.eval_expr(expr, local_scope)?;
                    result.push_str(&val.coerce_to_str());
                }
            }
        }
        Ok(ConfigValue::Str(result))
    }

    fn eval_section_field_direct(
        &mut self,
        section_path: &[String],
        field_name: &str,
        span: &Span,
    ) -> EvalResult_ {
        if let Some(cached) = self.section_cache.get(section_path) {
            if let Some(val) = cached.get(field_name) {
                return Ok(val.clone());
            }
        }

        // If the owning top-level section is the one currently being
        // built, section_cache won't have it yet by definition — check
        // the in-progress accumulator instead of recursing into
        // eval_section_by_path, which would trip its own reentrancy guard
        // and report a false cycle for what is really just a reference to
        // an already-computed sibling.
        if let Some(top) = section_path.first() {
            if let Some(frame) = self.self_stack.last() {
                if &frame.top_name == top {
                    let mut key = section_path.to_vec();
                    key.push(field_name.to_string());
                    return frame
                        .fields
                        .get(&key)
                        .cloned()
                        .ok_or_else(|| EvalErr::PathNotFound {
                            path: format!("{}::{field_name}", section_path.join("::")),
                            span: span.clone(),
                        });
                }
            }
        }

        // Not cached yet — fully evaluate the owning top-level section.
        // eval_section_by_path recursively walks every FieldValue::Nested
        // under it and populates section_cache at every resulting path
        // (see eval_section_fields), so this works for any depth, not
        // just a direct top-level field. Its own `evaluating_sects` guard
        // reports a clear cyclic-section error and returns None if we're
        // already in the middle of evaluating this same top-level path.
        if let Some(top) = section_path.first() {
            self.eval_section_by_path(std::slice::from_ref(top));
        }

        if let Some(cached) = self.section_cache.get(section_path) {
            if let Some(val) = cached.get(field_name) {
                return Ok(val.clone());
            }
        }

        // Still missing: either the path names a nested section (not a
        // scalar) at `field_name`, or the path is simply wrong.
        let mut deeper = section_path.to_vec();
        deeper.push(field_name.to_string());
        if self.section_cache.contains_key(&deeper) {
            Err(EvalErr::NotScalar {
                name: format!("{}::{field_name}", section_path.join(".")),
                span: span.clone(),
            })
        } else {
            Err(EvalErr::PathNotFound {
                path: format!("{}::{field_name}", section_path.join("::")),
                span: span.clone(),
            })
        }
    }

    #[allow(dead_code)]
    fn eval_self_ref(&mut self, nr: &NamespaceRef) -> EvalResult_ {
        let frame = self
            .self_stack
            .last()
            .ok_or_else(|| EvalErr::PathNotFound {
                path: "self".to_string(),
                span: nr.span.clone(),
            })?;
        let mut key = vec![frame.top_name.clone()];
        key.extend(nr.segments[1..].iter().cloned());
        frame
            .fields
            .get(&key)
            .cloned()
            .ok_or_else(|| EvalErr::PathNotFound {
                path: format!("self::{}", nr.segments[1..].join("::")),
                span: nr.span.clone(),
            })
    }

    fn eval_namespace_ref(
        &mut self,
        nr: &NamespaceRef,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> EvalResult_ {
        match nr.segments.as_slice() {
            [name] => {
                if name == "_" {
                    let value =
                        self.runtime_context
                            .previous_value()
                            .cloned()
                            .ok_or_else(|| EvalErr::PathNotFound {
                                path: "_".into(),
                                span: nr.span.clone(),
                            })?;
                    return value
                        .try_into_config(&nr.span)
                        .map_err(|error| EvalErr::Host {
                            message: error.to_string(),
                        });
                }
                // Check local scope first
                if let Some(val) = local_scope.get(name.as_str()) {
                    return Ok(val.clone());
                }
                self.eval_global(name)
                    .ok_or_else(|| self.unresolved_global(name, &nr.span))
            }

            // ── 2 segments — enum variant or import-alias item ONLY ────────
            [ns, name] => {
                if self.symbols.enums.contains_key(ns.as_str()) {
                    return Ok(ConfigValue::Str(name.clone()));
                }
                if let Some(imported) = self.imported_programs.get(ns.as_str()).cloned() {
                    let mut sub = Evaluator::new(imported.symbols.clone(), imported.program);
                    sub.imported_programs = imported.imports;
                    sub.hosts = self.hosts.clone();
                    sub.natives = self.natives.clone();
                    sub.runtime_context =
                        crate::runtime::RuntimeContext::for_base_dir(&imported.base_dir);
                    sub.effect_ledger = self.effect_ledger.clone();
                    let result = if imported
                        .symbols
                        .lookup_section(std::slice::from_ref(name))
                        .is_some()
                    {
                        sub.eval_section_by_path(std::slice::from_ref(name))
                            .map(ConfigValue::Section)
                            .ok_or_else(|| EvalErr::ImportRef {
                                alias: ns.to_string(),
                                symbol: name.to_string(),
                            })
                    } else {
                        sub.eval_global(name).ok_or_else(|| EvalErr::ImportRef {
                            alias: ns.to_string(),
                            symbol: name.to_string(),
                        })
                    };
                    self.absorb_diagnostics(&mut sub);
                    return result;
                }
                Err(EvalErr::CyclicRef {
                    name: format!("{ns}::{name}"),
                    span: nr.span.clone(),
                })
            }

            [] => Err(EvalErr::CyclicRef {
                name: String::new(),
                span: nr.span.clone(),
            }),
            // 3+ segments: either `alias::EnumName::Variant` (rest[0] names
            // an enum in the imported file — recurse, same deferred policy
            // as the resolver/typechecker) or `alias::var::field[::field…]`
            // (rest[0] names a plain var/section — evaluate it whole in the
            // imported file's own evaluator, then walk the remaining
            // segments as ordinary field access on that value). These are
            // NOT the same shape: recursing with `rest` unconditionally (as
            // this used to) re-interprets rest[0] as if it were itself an
            // import alias, which it isn't — it has no entry in
            // `imported_programs`, so that path fell through to a bogus
            // "cyclic reference" error for every plain cross-file field
            // access past the first segment.
            [first, rest @ ..] => {
                if self.symbols.enums.contains_key(first.as_str()) {
                    return Ok(ConfigValue::Str(rest.last().cloned().unwrap_or_default()));
                }
                if let Some(imported) = self.imported_programs.get(first.as_str()).cloned() {
                    let mut sub = Evaluator::new(imported.symbols.clone(), imported.program);
                    sub.imported_programs = imported.imports;
                    sub.hosts = self.hosts.clone();
                    sub.natives = self.natives.clone();
                    sub.runtime_context =
                        crate::runtime::RuntimeContext::for_base_dir(&imported.base_dir);
                    sub.effect_ledger = self.effect_ledger.clone();

                    if imported.symbols.enums.contains_key(rest[0].as_str()) {
                        let inner_nr = NamespaceRef {
                            segments: rest.to_vec(),
                            span: nr.span.clone(),
                        };
                        let result = sub.eval_namespace_ref(&inner_nr, &HashMap::new());
                        self.absorb_diagnostics(&mut sub);
                        return result;
                    }

                    let head = &rest[0];
                    let value = if imported
                        .symbols
                        .lookup_section(std::slice::from_ref(head))
                        .is_some()
                    {
                        sub.eval_section_by_path(std::slice::from_ref(head))
                            .map(ConfigValue::Section)
                    } else {
                        sub.eval_global(head)
                    };
                    self.absorb_diagnostics(&mut sub);
                    let mut value = value.ok_or_else(|| EvalErr::ImportRef {
                        alias: first.to_string(),
                        symbol: head.clone(),
                    })?;
                    for field in &rest[1..] {
                        value = match value {
                            ConfigValue::Section(map) => {
                                map.get(field)
                                    .cloned()
                                    .ok_or_else(|| EvalErr::PathNotFound {
                                        path: format!("{first}::{}", nr.segments[1..].join("::")),
                                        span: nr.span.clone(),
                                    })?
                            }
                            _ => {
                                return Err(EvalErr::PathNotFound {
                                    path: format!("{first}::{}", nr.segments[1..].join("::")),
                                    span: nr.span.clone(),
                                });
                            }
                        };
                    }
                    return Ok(value);
                }
                Err(EvalErr::CyclicRef {
                    name: nr.segments.join("::"),
                    span: nr.span.clone(),
                })
            }
        }
    }

    fn eval_field_access(
        &mut self,
        base: &Expr,
        field: &str,
        span: &Span,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> EvalResult_ {
        if let Expr::NamespaceRef(nr) = base {
            if nr.segments.len() == 1 && self.imported_programs.contains_key(&nr.segments[0]) {
                let imported = NamespaceRef {
                    segments: vec![nr.segments[0].clone(), field.to_string()],
                    span: span.clone(),
                };
                return self.eval_namespace_ref(&imported, local_scope);
            }
        }
        // A chain of bare identifiers rooted at `self`/`global` or a known
        // top-level section (`self.a.b`, `global.Section.field`,
        // `Section.nested.deeper.field`) is a static path — nested-section
        // intermediates aren't independently addressable ConfigValues while
        // still being built (see `eval_section_fields`'s "Do NOT insert
        // into result" note, and `self`'s frame.fields is only populated
        // for a nested section *after* that section finishes evaluating),
        // so the whole path must be resolved in one `eval_section_field_direct`
        // call rather than hop-by-hop.
        if let Some((is_self, rest)) = self.flatten_prefixed_path(base) {
            if is_self {
                let top_name = self
                    .self_stack
                    .last()
                    .map(|f| f.top_name.clone())
                    .ok_or_else(|| EvalErr::PathNotFound {
                        path: "self".to_string(),
                        span: span.clone(),
                    })?;
                let mut section_path = vec![top_name];
                section_path.extend(rest);
                return self.eval_section_field_direct(&section_path, field, span);
            }
            if rest.is_empty() {
                return self
                    .eval_global(field)
                    .ok_or_else(|| self.unresolved_global(field, span));
            }
            return self.eval_section_field_direct(&rest, field, span);
        }
        if let Some(section_path) = self.flatten_static_section_path(base, local_scope) {
            return self.eval_section_field_direct(&section_path, field, span);
        }
        let base_val = self.eval_expr(base, local_scope)?;
        match base_val {
            ConfigValue::Section(map) => {
                map.get(field).cloned().ok_or_else(|| EvalErr::CyclicRef {
                    name: field.to_string(),
                    span: span.clone(),
                })
            }
            ConfigValue::Error {
                message,
                kind,
                code,
                cause,
            } => match field {
                "message" => Ok(ConfigValue::Str(message)),
                "kind" => Ok(ConfigValue::Str(kind)),
                "code" => Ok(ConfigValue::Int(code)),
                "cause" => cause
                    .map(|value| *value)
                    .ok_or_else(|| EvalErr::PathNotFound {
                        path: "error.cause".into(),
                        span: span.clone(),
                    }),
                _ => Err(EvalErr::PathNotFound {
                    path: format!("error.{field}"),
                    span: span.clone(),
                }),
            },
            _ => Err(EvalErr::CyclicRef {
                name: field.to_string(),
                span: span.clone(),
            }),
        }
    }

    /// If `expr` is a chain of bare-identifier `FieldAccess`es rooted at
    /// `self` or `global`, returns `(true, path)` for `self` or `(false,
    /// path)` for `global`, where `path` is the segments between the root
    /// and the field being accessed (e.g. `self.a.b` called with base=`a`'s
    /// FieldAccess returns `(true, ["a"])`).
    fn flatten_prefixed_path(&self, expr: &Expr) -> Option<(bool, Vec<String>)> {
        match expr {
            Expr::NamespaceRef(nr) if nr.segments == ["self"] => Some((true, vec![])),
            Expr::NamespaceRef(nr) if nr.segments == ["global"] => Some((false, vec![])),
            Expr::FieldAccess { base, field, .. } => {
                let (is_self, mut path) = self.flatten_prefixed_path(base)?;
                path.push(field.clone());
                Some((is_self, path))
            }
            _ => None,
        }
    }

    /// If `expr` is a chain of bare-identifier `FieldAccess`es rooted at a
    /// known top-level section name (not shadowed by a local), returns the
    /// full section path (e.g. `Section.nested.deeper` → `["Section",
    /// "nested", "deeper"]`). Returns `None` for anything else (a call,
    /// index, self/global base, or a root that's a local/global var) —
    /// those fall through to normal per-hop expression evaluation.
    fn flatten_static_section_path(
        &self,
        expr: &Expr,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> Option<Vec<String>> {
        match expr {
            Expr::NamespaceRef(nr) if nr.segments.len() == 1 => {
                let name = &nr.segments[0];
                if !local_scope.contains_key(name.as_str())
                    && self
                        .symbols
                        .lookup_section(std::slice::from_ref(name))
                        .is_some()
                {
                    Some(vec![name.clone()])
                } else {
                    None
                }
            }
            Expr::FieldAccess { base, field, .. } => {
                let mut path = self.flatten_static_section_path(base, local_scope)?;
                path.push(field.clone());
                Some(path)
            }
            _ => None,
        }
    }

    fn eval_fn_call(
        &mut self,
        fc: &FnCall,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> EvalResult_ {
        // Positional calls are now also used for first-class callables. Preserve
        // compatibility for ordinary Spar functions by binding positional
        // arguments to the declaration's parameter order before falling back to
        // the legacy built-ins below.
        if let Some(function) = self.program.items.iter().find_map(|item| match item {
            TopLevelItem::Function(function) if function.name == fc.name => Some(function.clone()),
            _ => None,
        }) {
            if fc.args.len() > function.params.len() {
                return Err(EvalErr::Fatal {
                    message: format!(
                        "function '{}' expects at most {} argument(s), got {}",
                        fc.name,
                        function.params.len(),
                        fc.args.len()
                    ),
                    span: fc.span.clone(),
                });
            }
            let args = function
                .params
                .iter()
                .zip(fc.args.iter())
                .map(|(parameter, value)| CallArg {
                    param_name: parameter.name.clone(),
                    param_name_span: fc.span.clone(),
                    value: value.clone(),
                    span: fc.span.clone(),
                })
                .collect::<Vec<_>>();
            return self.eval_call(&fc.name, &args, &fc.span, local_scope);
        }

        // Selective imports keep the imported function under its local name. The
        // compatibility evaluator does not carry the resolver's module identity,
        // so locate the unique imported module that actually exports that name.
        if self.symbols.imported_functions.contains_key(&fc.name) {
            let imported = self.imported_programs.values().find_map(|program| {
                let function = program.program.items.iter().find_map(|item| match item {
                    TopLevelItem::Function(function)
                        if function.name == fc.name && !function.is_private =>
                    {
                        Some(function.clone())
                    }
                    _ => None,
                })?;
                Some((program.clone(), function))
            });
            if let Some((imported, function)) = imported {
                if fc.args.len() > function.params.len() {
                    return Err(EvalErr::Fatal {
                        message: format!(
                            "function '{}' expects at most {} argument(s), got {}",
                            fc.name,
                            function.params.len(),
                            fc.args.len()
                        ),
                        span: fc.span.clone(),
                    });
                }
                let mut bound = HashMap::new();
                for (parameter, expression) in function.params.iter().zip(fc.args.iter()) {
                    bound.insert(
                        parameter.name.clone(),
                        self.eval_expr(expression, local_scope)?,
                    );
                }
                if function.is_async {
                    return self.allocate_opaque_promise(
                        None,
                        None,
                        fc.name.clone(),
                        &function,
                        &bound,
                    );
                }
                let mut sub = Evaluator::new(imported.symbols, imported.program);
                sub.imported_programs = imported.imports;
                sub.hosts = self.hosts.clone();
                sub.natives = self.natives.clone();
                sub.effect_ledger = self.effect_ledger.clone();
                sub.call_depth = self.call_depth + 1;
                sub.eval_default_args(&function, &mut bound)?;
                let result = sub
                    .eval_func_stmts(&function.body.stmts, &mut bound)?
                    .into_return()
                    .unwrap_or(ConfigValue::Int(0));
                self.absorb_diagnostics(&mut sub);
                return Ok(result);
            }
        }

        match fc.name.as_str() {
            "env" => {
                let key = match self.eval_expr(&fc.args[0], local_scope)? {
                    ConfigValue::Str(s) => s,
                    other => {
                        return Err(EvalErr::CyclicRef {
                            name: format!("env() arg must be str, got {}", other.type_name()),
                            span: Span::dummy(),
                        })
                    }
                };
                std::env::var(&key)
                    .map(ConfigValue::Str)
                    .map_err(|_| EvalErr::EnvVarMissing(key, fc.span.clone()))
            }
            "str" => {
                let val = self.eval_expr(&fc.args[0], local_scope)?;
                Ok(ConfigValue::Str(val.coerce_to_str()))
            }
            "int" => {
                let val = self.eval_expr(&fc.args[0], local_scope)?;
                match val {
                    ConfigValue::Int(i) => Ok(ConfigValue::Int(i)),
                    ConfigValue::Float(f) => Ok(ConfigValue::Int(f as i64)),
                    ConfigValue::Str(s) => {
                        s.trim().parse::<i64>().map(ConfigValue::Int).map_err(|_| {
                            EvalErr::CyclicRef {
                                name: format!("cannot convert {:?} to int", s),
                                span: fc.span.clone(),
                            }
                        })
                    }
                    v => Err(EvalErr::TypeMismatch {
                        expected: "int, float, or str",
                        got: v.type_name(),
                    }),
                }
            }
            "float" => {
                let val = self.eval_expr(&fc.args[0], local_scope)?;
                match val {
                    ConfigValue::Int(i) => Ok(ConfigValue::Float(i as f64)),
                    ConfigValue::Float(f) => Ok(ConfigValue::Float(f)),
                    ConfigValue::Str(s) => {
                        s.trim()
                            .parse::<f64>()
                            .map(ConfigValue::Float)
                            .map_err(|_| EvalErr::CyclicRef {
                                name: format!("cannot convert {:?} to float", s),
                                span: fc.span.clone(),
                            })
                    }
                    v => Err(EvalErr::TypeMismatch {
                        expected: "int, float, or str",
                        got: v.type_name(),
                    }),
                }
            }
            "bool" => {
                let val = self.eval_expr(&fc.args[0], local_scope)?;
                match val {
                    ConfigValue::Bool(b) => Ok(ConfigValue::Bool(b)),
                    ConfigValue::Str(s) => match s.trim() {
                        "true" => Ok(ConfigValue::Bool(true)),
                        "false" => Ok(ConfigValue::Bool(false)),
                        other => Err(EvalErr::CyclicRef {
                            name: format!(
                                "cannot convert {:?} to bool (expected \"true\" or \"false\")",
                                other
                            ),
                            span: fc.span.clone(),
                        }),
                    },
                    v => Err(EvalErr::TypeMismatch {
                        expected: "str or bool",
                        got: v.type_name(),
                    }),
                }
            }
            other => Err(EvalErr::CyclicRef {
                name: format!("unknown built-in `{other}`"),
                span: Span::dummy(),
            }),
        }
    }

    fn eval_binop(
        &mut self,
        op: &BinaryOp,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> EvalResult_ {
        if op.op == BinOp::Fallback {
            return match self.eval_expr(&op.lhs, local_scope) {
                Ok(val) => Ok(val),
                Err(EvalErr::EnvVarMissing(..)) | Err(EvalErr::ImportRef { .. }) => {
                    self.eval_expr(&op.rhs, local_scope)
                }
                Err(e) => Err(e),
            };
        }

        if matches!(
            op.op,
            BinOp::Eq
                | BinOp::NotEq
                | BinOp::Lt
                | BinOp::Gt
                | BinOp::LtEq
                | BinOp::GtEq
                | BinOp::And
                | BinOp::Or
        ) {
            return self.eval_comparison_or_logical(op, local_scope);
        }

        let lhs = self.eval_expr(&op.lhs, local_scope)?;
        let rhs = self.eval_expr(&op.rhs, local_scope)?;

        match (&op.op, &lhs, &rhs) {
            (BinOp::Add, ConfigValue::Int(a), ConfigValue::Int(b)) => Ok(ConfigValue::Int(a + b)),
            (BinOp::Add, ConfigValue::Float(a), ConfigValue::Float(b)) => {
                Ok(ConfigValue::Float(a + b))
            }
            (BinOp::Add, ConfigValue::Str(a), ConfigValue::Str(b)) => {
                Ok(ConfigValue::Str(format!("{a}{b}")))
            }
            (BinOp::Add, ConfigValue::Shell(a), ConfigValue::Shell(b)) => {
                Ok(ConfigValue::Shell(a.clone().then(b.clone())))
            }
            (BinOp::Sub, ConfigValue::Int(a), ConfigValue::Int(b)) => Ok(ConfigValue::Int(a - b)),
            (BinOp::Sub, ConfigValue::Float(a), ConfigValue::Float(b)) => {
                Ok(ConfigValue::Float(a - b))
            }
            (BinOp::Mul, ConfigValue::Int(a), ConfigValue::Int(b)) => Ok(ConfigValue::Int(a * b)),
            (BinOp::Mul, ConfigValue::Float(a), ConfigValue::Float(b)) => {
                Ok(ConfigValue::Float(a * b))
            }
            (BinOp::Div, ConfigValue::Int(_), ConfigValue::Int(0)) => {
                Err(EvalErr::DivisionByZero(op.span.clone()))
            }
            (BinOp::Div, ConfigValue::Int(a), ConfigValue::Int(b)) => Ok(ConfigValue::Int(a / b)),
            (BinOp::Div, ConfigValue::Float(a), ConfigValue::Float(b)) => {
                if *b == 0.0 {
                    Err(EvalErr::DivisionByZero(op.span.clone()))
                } else {
                    Ok(ConfigValue::Float(a / b))
                }
            }
            (op_kind, l, r) => unreachable!(
                "evaluator reached invalid binop {:?} on {} and {} — \
                 type checker should have caught this",
                op_kind,
                l.type_name(),
                r.type_name()
            ),
        }
    }

    fn eval_comparison_or_logical(
        &mut self,
        b: &BinaryOp,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> EvalResult_ {
        match b.op {
            BinOp::And => {
                let lhs = self.eval_expr(&b.lhs, local_scope)?;
                if let ConfigValue::Bool(false) = &lhs {
                    return Ok(ConfigValue::Bool(false));
                }
                let rhs = self.eval_expr(&b.rhs, local_scope)?;
                match (lhs, rhs) {
                    (ConfigValue::Bool(l), ConfigValue::Bool(r)) => Ok(ConfigValue::Bool(l && r)),
                    _ => unreachable!("typechecker ensures bool operands"),
                }
            }
            BinOp::Or => {
                let lhs = self.eval_expr(&b.lhs, local_scope)?;
                if let ConfigValue::Bool(true) = &lhs {
                    return Ok(ConfigValue::Bool(true));
                }
                let rhs = self.eval_expr(&b.rhs, local_scope)?;
                match (lhs, rhs) {
                    (ConfigValue::Bool(l), ConfigValue::Bool(r)) => Ok(ConfigValue::Bool(l || r)),
                    _ => unreachable!(),
                }
            }
            BinOp::Eq => {
                let lhs = self.eval_expr(&b.lhs, local_scope)?;
                let rhs = self.eval_expr(&b.rhs, local_scope)?;
                Ok(ConfigValue::Bool(lhs == rhs))
            }
            BinOp::NotEq => {
                let lhs = self.eval_expr(&b.lhs, local_scope)?;
                let rhs = self.eval_expr(&b.rhs, local_scope)?;
                Ok(ConfigValue::Bool(lhs != rhs))
            }
            BinOp::Lt => self.eval_numeric_cmp(b, local_scope, |a, b| a < b, |a, b| a < b),
            BinOp::Gt => self.eval_numeric_cmp(b, local_scope, |a, b| a > b, |a, b| a > b),
            BinOp::LtEq => self.eval_numeric_cmp(b, local_scope, |a, b| a <= b, |a, b| a <= b),
            BinOp::GtEq => self.eval_numeric_cmp(b, local_scope, |a, b| a >= b, |a, b| a >= b),
            _ => unreachable!(),
        }
    }

    fn eval_numeric_cmp(
        &mut self,
        b: &BinaryOp,
        local_scope: &HashMap<String, ConfigValue>,
        int_cmp: impl Fn(i64, i64) -> bool,
        float_cmp: impl Fn(f64, f64) -> bool,
    ) -> EvalResult_ {
        let lhs = self.eval_expr(&b.lhs, local_scope)?;
        let rhs = self.eval_expr(&b.rhs, local_scope)?;
        match (lhs, rhs) {
            (ConfigValue::Int(l), ConfigValue::Int(r)) => Ok(ConfigValue::Bool(int_cmp(l, r))),
            (ConfigValue::Float(l), ConfigValue::Float(r)) => {
                Ok(ConfigValue::Bool(float_cmp(l, r)))
            }
            _ => unreachable!("typechecker ensures numeric operands"),
        }
    }
}

fn shell_scalar_to_string(value: ConfigValue) -> Result<String, EvalErr> {
    match value {
        ConfigValue::Str(value) => Ok(value),
        ConfigValue::Int(value) => Ok(value.to_string()),
        ConfigValue::Float(value) => Ok(value.to_string()),
        ConfigValue::Bool(value) => Ok(value.to_string()),
        other => Err(EvalErr::TypeMismatch {
            expected: "primitive",
            got: other.type_name(),
        }),
    }
}

pub(crate) fn lower_shell_expr(expression: &ShellExpr) -> spar_command::ShellPlan {
    spar_command::ShellPlan {
        steps: expression
            .steps
            .iter()
            .map(|(join, step)| {
                let join = match join {
                    ShellJoin::Always => spar_command::Join::Always,
                    ShellJoin::OnSuccess => spar_command::Join::OnSuccess,
                    ShellJoin::OnFailure => spar_command::Join::OnFailure,
                };
                let step = match step {
                    ShellStep::Command(command) => {
                        spar_command::Step::Command(lower_shell_command(command))
                    }
                    ShellStep::Pipeline(commands) => {
                        spar_command::Step::Pipeline(spar_command::PipelinePlan {
                            commands: commands.iter().map(lower_shell_command).collect(),
                        })
                    }
                    ShellStep::MixedPipeline(_) => unreachable!(
                        "mixed structured pipelines must not enter the byte-only shell lowerer"
                    ),
                };
                (join, step)
            })
            .collect(),
    }
}

fn lower_shell_command(command: &ShellCommandExpr) -> spar_command::CommandPlan {
    spar_command::CommandPlan {
        program: command.program.text.clone(),
        args: command.args.iter().map(|word| word.text.clone()).collect(),
        env: command
            .environment
            .iter()
            .map(|entry| spar_command::EnvironmentOverride {
                key: entry.name.clone(),
                value: entry.value.text.clone(),
            })
            .collect(),
        cwd: None,
        stdin: command.stdin.as_ref().map(lower_shell_redirect),
        stdout: command.stdout.as_ref().map(lower_shell_redirect),
        stderr: command.stderr.as_ref().map(lower_shell_redirect),
        redirections: command
            .redirections
            .iter()
            .map(|redirect| spar_command::OrderedRedirection {
                fd: redirect.fd,
                target: match &redirect.target {
                    ShellFdRedirectTarget::File(file) => lower_shell_redirect(file),
                    ShellFdRedirectTarget::Duplicate(fd) => {
                        spar_command::Redirection::DuplicateFd(*fd)
                    }
                },
            })
            .collect(),
        background: command.background,
    }
}

fn lower_shell_redirect(redirect: &ShellRedirect) -> spar_command::Redirection {
    spar_command::Redirection::File {
        path: redirect.target.text.clone(),
        mode: redirect.mode.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellPlanOutcome {
    pub success: bool,
    pub exit_code: i32,
    pub signal: Option<i32>,
    pub pid: u32,
    pub pipeline: Vec<spar_process::ProcessStatus>,
}

pub fn execute_shell_plan(plan: &spar_command::ShellPlan) -> std::io::Result<ShellPlanOutcome> {
    let options = spar_process::ExecutionOptions::default();
    execute_shell_plan_with_options(plan, &options)
}

pub fn execute_shell_plan_with_options(
    plan: &spar_command::ShellPlan,
    options: &spar_process::ExecutionOptions,
) -> std::io::Result<ShellPlanOutcome> {
    struct NativeExecutor<'a> {
        options: &'a spar_process::ExecutionOptions,
        stop: bool,
        last_status: Option<spar_process::PipelineStatus>,
    }

    impl spar_process::StepExecutor for NativeExecutor<'_> {
        type Error = std::io::Error;

        fn run_command(
            &mut self,
            command: &spar_command::CommandPlan,
        ) -> Result<spar_process::ExitStatus, Self::Error> {
            if command.program == "exit" {
                let code = command
                    .args
                    .first()
                    .map(|value| value.parse::<i32>())
                    .transpose()
                    .map_err(|_| std::io::Error::other("exit status must be an integer"))?
                    .unwrap_or(0);
                self.stop = true;
                // The builtin decides the plan's final status, not whatever
                // command ran before it.
                self.last_status = Some(spar_process::PipelineStatus {
                    code,
                    success: code == 0,
                    processes: vec![],
                });
                return Ok(spar_process::ExitStatus {
                    success: code == 0,
                    code: Some(code),
                });
            }
            let output = spar_process::run_command(command, self.options)?;
            self.last_status = output.pipeline_status;
            Ok(output.status)
        }

        fn run_pipeline(
            &mut self,
            pipeline: &spar_command::PipelinePlan,
        ) -> Result<spar_process::ExitStatus, Self::Error> {
            let output = spar_process::run_pipeline(pipeline, self.options)?;
            self.last_status = output.pipeline_status;
            Ok(output.status)
        }

        fn should_stop(&self) -> bool {
            self.stop
        }
    }

    let mut executor = NativeExecutor {
        options,
        stop: false,
        last_status: None,
    };
    spar_process::run_plan(plan, &mut executor).map(|outcome| {
        let status = executor
            .last_status
            .unwrap_or(spar_process::PipelineStatus {
                code: outcome.exit_code,
                success: outcome.success,
                processes: vec![],
            });
        let last = status.processes.last();
        ShellPlanOutcome {
            success: status.success,
            exit_code: status.code,
            signal: last.and_then(|process| process.signal),
            pid: last.map_or(0, |process| process.pid),
            pipeline: status.processes,
        }
    })
}

// ── Function call evaluation ──────────────────────────────────────────────────

impl Evaluator {
    fn allocate_opaque_promise(
        &mut self,
        import_alias: Option<String>,
        group: Option<String>,
        function: String,
        declaration: &FunctionDecl,
        bound_arguments: &HashMap<String, ConfigValue>,
    ) -> EvalResult_ {
        let id = self.next_promise_id;
        self.next_promise_id =
            self.next_promise_id
                .checked_add(1)
                .ok_or_else(|| EvalErr::Host {
                    message: "promise identity space exhausted".into(),
                })?;
        let handle = PromiseHandle::new(id);
        let arguments = declaration
            .params
            .iter()
            .filter_map(|parameter| bound_arguments.get(&parameter.name).cloned())
            .collect();
        self.pending_promises.push(PendingPromise {
            handle,
            import_alias,
            group,
            function,
            arguments,
        });
        Ok(ConfigValue::Promise(handle))
    }

    /// `Struct(field: value, ...)` outside function bodies: clone the struct's
    /// canonical section and apply the named overrides.
    fn eval_struct_constructor(
        &mut self,
        name: &str,
        args: &[CallArg],
        call_span: &Span,
        caller_scope: &HashMap<String, ConfigValue>,
    ) -> EvalResult_ {
        let path = vec![name.to_string()];
        let mut section =
            self.eval_section_by_path(&path)
                .ok_or_else(|| EvalErr::PathNotFound {
                    path: name.to_string(),
                    span: call_span.clone(),
                })?;
        for (field, value) in self.eval_explicit_args(args, caller_scope)? {
            if !section.contains_key(&field) {
                return Err(EvalErr::Fatal {
                    message: format!("struct '{name}' has no field '{field}'"),
                    span: call_span.clone(),
                });
            }
            section.insert(field, value);
        }
        Ok(ConfigValue::Section(section))
    }

    fn eval_call(
        &mut self,
        name: &str,
        args: &[CallArg],
        call_span: &Span,
        caller_scope: &HashMap<String, ConfigValue>,
    ) -> EvalResult_ {
        if name == "panic" {
            let message = args
                .iter()
                .find(|argument| argument.param_name == "message")
                .ok_or_else(|| EvalErr::Fatal {
                    message: "panic message argument is unavailable".into(),
                    span: Span::dummy(),
                })?;
            let span = message.span.clone();
            let value = self.eval_expr(&message.value, caller_scope)?;
            let ConfigValue::Str(message) = value else {
                return Err(EvalErr::Fatal {
                    message: "panic message must be str".into(),
                    span,
                });
            };
            return Err(EvalErr::Fatal { message, span });
        }
        if self.call_depth >= MAX_CALL_DEPTH {
            return Err(EvalErr::MaxCallDepth {
                name: name.to_string(),
            });
        }
        self.call_depth += 1;

        let segments: Vec<&str> = name.split("::").collect();

        if segments.len() == 3 {
            // Cross-file functionGroup call: alias::Group::fn(args)
            let alias = segments[0];
            let group = segments[1];
            let fn_name = segments[2];
            if let Some(imported) = self.imported_programs.get(alias).cloned() {
                let func_decl = imported.program.items.iter().find_map(|item| {
                    if let TopLevelItem::FunctionGroup(g) = item {
                        if g.name == group && !g.is_private {
                            return g
                                .functions
                                .iter()
                                .find(|f| f.name == fn_name && !f.is_private)
                                .cloned();
                        }
                    }
                    None
                });
                if let Some(fd) = func_decl {
                    let mut local_scope = self.eval_explicit_args(args, caller_scope)?;
                    if fd.is_async {
                        self.call_depth -= 1;
                        return self.allocate_opaque_promise(
                            Some(alias.to_string()),
                            Some(group.to_string()),
                            fn_name.to_string(),
                            &fd,
                            &local_scope,
                        );
                    }
                    let mut sub = Evaluator::new(imported.symbols, imported.program);
                    sub.imported_programs = imported.imports;
                    sub.hosts = self.hosts.clone();
                    sub.natives = self.natives.clone();
                    // Imported functions execute in the caller's runtime session. Moving the
                    // context (rather than creating a fresh one rooted at the imported file)
                    // preserves cwd, environment, redirected IO, resources, and cancellation.
                    // Any intentional context mutations performed by the function therefore
                    // remain visible to its caller.
                    let caller_context = std::mem::replace(
                        &mut self.runtime_context,
                        crate::runtime::RuntimeContext::for_base_dir(&imported.base_dir),
                    );
                    sub.runtime_context = caller_context;
                    sub.effect_ledger = self.effect_ledger.clone();
                    sub.call_depth = self.call_depth;
                    let result = (|| {
                        sub.eval_default_args(&fd, &mut local_scope)?;
                        sub.eval_func_stmts(&fd.body.stmts.clone(), &mut local_scope)
                    })();
                    self.runtime_context = std::mem::replace(
                        &mut sub.runtime_context,
                        crate::runtime::RuntimeContext::for_base_dir(&imported.base_dir),
                    );
                    self.absorb_diagnostics(&mut sub);
                    let result = result?.into_return().unwrap_or(ConfigValue::Int(0));
                    self.call_depth -= 1;
                    return Ok(result);
                }
            }
            self.call_depth -= 1;
            return Err(EvalErr::ImportRef {
                alias: alias.to_string(),
                symbol: format!("{group}::{fn_name}"),
            });
        }

        if segments.len() == 2 {
            let ns = segments[0];
            let fn_name = segments[1];

            // Local functionGroup call: Group::fn(args)
            let group_call = self.program.items.iter().find_map(|item| {
                if let TopLevelItem::FunctionGroup(g) = item {
                    if g.name == ns {
                        return g.functions.iter().find(|f| f.name == fn_name).cloned();
                    }
                }
                None
            });
            if let Some(fd) = group_call {
                let mut local_scope = self.eval_explicit_args(args, caller_scope)?;
                if fd.is_async {
                    self.call_depth -= 1;
                    return self.allocate_opaque_promise(
                        None,
                        Some(ns.to_string()),
                        fn_name.to_string(),
                        &fd,
                        &local_scope,
                    );
                }
                self.eval_default_args(&fd, &mut local_scope)?;
                let result = self
                    .eval_func_stmts(&fd.body.stmts.clone(), &mut local_scope)?
                    .into_return()
                    .unwrap_or(ConfigValue::Int(0));
                self.call_depth -= 1;
                return Ok(result);
            }

            // Cross-file plain function call: alias::fn(args)
            if let Some(imported) = self.imported_programs.get(ns).cloned() {
                let func_decl = imported.program.items.iter().find_map(|item| {
                    if let TopLevelItem::Function(f) = item {
                        if f.name == fn_name && !f.is_private {
                            return Some(f.clone());
                        }
                    }
                    None
                });
                if let Some(fd) = func_decl {
                    let mut local_scope = self.eval_explicit_args(args, caller_scope)?;
                    if fd.is_async {
                        self.call_depth -= 1;
                        return self.allocate_opaque_promise(
                            Some(ns.to_string()),
                            None,
                            fn_name.to_string(),
                            &fd,
                            &local_scope,
                        );
                    }
                    let mut sub = Evaluator::new(imported.symbols, imported.program);
                    sub.imported_programs = imported.imports;
                    sub.hosts = self.hosts.clone();
                    sub.natives = self.natives.clone();
                    // Imported functions execute in the caller's runtime session. Moving the
                    // context (rather than creating a fresh one rooted at the imported file)
                    // preserves cwd, environment, redirected IO, resources, and cancellation.
                    // Any intentional context mutations performed by the function therefore
                    // remain visible to its caller.
                    let caller_context = std::mem::replace(
                        &mut self.runtime_context,
                        crate::runtime::RuntimeContext::for_base_dir(&imported.base_dir),
                    );
                    sub.runtime_context = caller_context;
                    sub.effect_ledger = self.effect_ledger.clone();
                    sub.call_depth = self.call_depth;
                    let result = (|| {
                        sub.eval_default_args(&fd, &mut local_scope)?;
                        sub.eval_func_stmts(&fd.body.stmts.clone(), &mut local_scope)
                    })();
                    self.runtime_context = std::mem::replace(
                        &mut sub.runtime_context,
                        crate::runtime::RuntimeContext::for_base_dir(&imported.base_dir),
                    );
                    self.absorb_diagnostics(&mut sub);
                    let result = result?.into_return().unwrap_or(ConfigValue::Int(0));
                    self.call_depth -= 1;
                    return Ok(result);
                }
            }
            // Registered host function: ns::fn(args)
            if let Some(host_fn) = self.hosts.get(ns, fn_name).cloned() {
                let bound = self.eval_explicit_args(args, caller_scope)?;
                let ordered: Vec<ConfigValue> = host_fn
                    .params
                    .iter()
                    .map(|(param_name, _)| bound[param_name].clone())
                    .collect();
                self.call_depth -= 1;
                return host_fn.call(&ordered).map_err(|e| EvalErr::Host {
                    message: e.to_string(),
                });
            }
            if let Some(signature) = self
                .symbols
                .natives
                .get(&(ns.to_string(), fn_name.to_string()))
                .cloned()
            {
                let bound = self.eval_explicit_args(args, caller_scope)?;
                let ordered = signature
                    .params
                    .iter()
                    .map(|(param_name, _)| {
                        bound
                            .get(param_name)
                            .cloned()
                            .map(crate::runtime::Value::from_config)
                            .ok_or_else(|| EvalErr::Host {
                                message: format!("missing native argument '{param_name}'"),
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.call_depth -= 1;
                return self
                    .natives
                    .call(signature.id, &mut self.runtime_context, &ordered, call_span)
                    .and_then(|value| value.try_into_config(call_span))
                    .map_err(|error| EvalErr::Host {
                        message: error.to_string(),
                    });
            }

            self.call_depth -= 1;
            return Err(EvalErr::ImportRef {
                alias: ns.to_string(),
                symbol: fn_name.to_string(),
            });
        }

        // 1 segment: local plain function call.
        let func_decl = self.program.items.iter().find_map(|item| {
            if let TopLevelItem::Function(f) = item {
                if f.name == name {
                    return Some(f.clone());
                }
            }
            None
        });
        let Some(func_decl) = func_decl else {
            // Not a function: a canonical struct called as a constructor.
            // The resolver guarantees one of the two exists.
            self.call_depth -= 1;
            return self.eval_struct_constructor(name, args, call_span, caller_scope);
        };

        let mut local_scope = self.eval_explicit_args(args, caller_scope)?;
        if func_decl.is_async {
            self.call_depth -= 1;
            return self.allocate_opaque_promise(
                None,
                None,
                name.to_string(),
                &func_decl,
                &local_scope,
            );
        }
        self.eval_default_args(&func_decl, &mut local_scope)?;

        let result = self
            .eval_func_stmts(&func_decl.body.stmts.clone(), &mut local_scope)?
            .into_return()
            // resolver ensures every path returns, except a `void`
            // function's implicit fallthrough, whose value a caller can
            // never observe (the typechecker forbids storing it).
            .unwrap_or(ConfigValue::Int(0));

        self.call_depth -= 1;
        Ok(result)
    }

    fn eval_explicit_args(
        &mut self,
        args: &[CallArg],
        caller_scope: &HashMap<String, ConfigValue>,
    ) -> Result<HashMap<String, ConfigValue>, EvalErr> {
        let mut local_scope = HashMap::new();
        for arg in args {
            let value = self.eval_expr(&arg.value, caller_scope)?;
            local_scope.insert(arg.param_name.clone(), value);
        }
        Ok(local_scope)
    }

    fn eval_default_args(
        &mut self,
        function: &FunctionDecl,
        local_scope: &mut HashMap<String, ConfigValue>,
    ) -> Result<(), EvalErr> {
        for param in &function.params {
            if local_scope.contains_key(&param.name) {
                continue;
            }
            if let Some(default) = &param.default {
                let value = self.eval_expr(default, &HashMap::new())?;
                local_scope.insert(param.name.clone(), value);
            }
        }
        Ok(())
    }

    fn eval_func_stmts(
        &mut self,
        stmts: &[FuncStmt],
        local_scope: &mut HashMap<String, ConfigValue>,
    ) -> Result<StatementFlow, EvalErr> {
        for stmt in stmts {
            match stmt {
                FuncStmt::LocalVar(lv) => {
                    let val = self.eval_expr(&lv.value.clone(), local_scope)?;
                    local_scope.insert(lv.name.clone(), val);
                }
                FuncStmt::Expression(expr, _) => {
                    self.eval_expr(expr, local_scope)?;
                }
                FuncStmt::Assignment { name, value, .. } => {
                    let value = self.eval_expr(value, local_scope)?;
                    if local_scope.contains_key(name) {
                        local_scope.insert(name.clone(), value);
                    } else {
                        self.global_cache.insert(name.clone(), value);
                    }
                }
                FuncStmt::FieldAssignment {
                    base,
                    fields,
                    value,
                    span,
                } => {
                    let value = self.eval_expr(value, local_scope)?;
                    if let Some(target) = local_scope.get_mut(base) {
                        assign_config_field_path(target, fields, value, span)?;
                    } else if let Some(target) = self.global_cache.get_mut(base) {
                        assign_config_field_path(target, fields, value, span)?;
                    } else {
                        return Err(EvalErr::PathNotFound {
                            path: base.clone(),
                            span: span.clone(),
                        });
                    }
                }
                FuncStmt::Return(ret_value, _) => {
                    let val = match ret_value {
                        // `void` functions never let this value escape — the
                        // typechecker forbids storing a void result — so a
                        // bare `return;` just needs any placeholder here.
                        ReturnValue::Void => ConfigValue::Int(0),
                        ReturnValue::Expr(e) => self.eval_expr(&e.clone(), local_scope)?,
                        ReturnValue::SectionBlock(fields) => {
                            let fields = fields.clone();
                            let mut map = indexmap::IndexMap::new();
                            for rf in &fields {
                                let v = self.eval_expr(&rf.value, local_scope)?;
                                map.insert(rf.name.clone(), v);
                            }
                            ConfigValue::Section(map)
                        }
                    };
                    return Ok(StatementFlow::Return(val));
                }
                FuncStmt::Break(_) => return Ok(StatementFlow::Break),
                FuncStmt::Continue(_) => return Ok(StatementFlow::Continue),
                FuncStmt::Try(statement) => {
                    let body_snapshot = local_scope.clone();
                    match self.eval_func_stmts(&statement.body, local_scope) {
                        Ok(flow) => {
                            restore_block_scope(local_scope, &body_snapshot, &statement.body, None);
                            if !matches!(flow, StatementFlow::Normal) {
                                return Ok(flow);
                            }
                        }
                        Err(error @ EvalErr::Fatal { .. }) => return Err(error),
                        Err(error) => {
                            restore_block_scope(local_scope, &body_snapshot, &statement.body, None);
                            let handler_snapshot = local_scope.clone();
                            if let Some(name) = &statement.catch_name {
                                local_scope.insert(
                                    name.clone(),
                                    ConfigValue::Error {
                                        message: error.into_kl_error().to_string(),
                                        kind: "runtime".into(),
                                        code: 1,
                                        cause: None,
                                    },
                                );
                            }
                            let flow = self.eval_func_stmts(&statement.handler, local_scope)?;
                            restore_block_scope(
                                local_scope,
                                &handler_snapshot,
                                &statement.handler,
                                None,
                            );
                            if let Some(name) = &statement.catch_name {
                                match handler_snapshot.get(name) {
                                    Some(value) => {
                                        local_scope.insert(name.clone(), value.clone());
                                    }
                                    None => {
                                        local_scope.remove(name);
                                    }
                                }
                            }
                            if !matches!(flow, StatementFlow::Normal) {
                                return Ok(flow);
                            }
                        }
                    }
                }
                FuncStmt::For(statement) => {
                    let items = match self.eval_expr(&statement.iterable, local_scope)? {
                        ConfigValue::List(items) => items,
                        _ => unreachable!("typechecker ensures for-loop iterable is a list"),
                    };
                    for (index, item) in items.into_iter().enumerate() {
                        let snapshot = local_scope.clone();
                        match &statement.binding {
                            ForBinding::Value { name, .. } => {
                                local_scope.insert(name.clone(), item);
                            }
                            ForBinding::Indexed {
                                index_name,
                                value_name,
                                ..
                            } => {
                                local_scope
                                    .insert(index_name.clone(), ConfigValue::Int(index as i64));
                                local_scope.insert(value_name.clone(), item);
                            }
                        }
                        let body = statement.body.clone();
                        let flow = self.eval_func_stmts(&body, local_scope)?;
                        restore_block_scope(
                            local_scope,
                            &snapshot,
                            &body,
                            Some(&statement.binding),
                        );
                        match flow {
                            StatementFlow::Normal | StatementFlow::Continue => {}
                            StatementFlow::Break => break,
                            flow @ StatementFlow::Return(_) => return Ok(flow),
                        }
                    }
                }
                FuncStmt::If(if_stmt) => {
                    let cond = self.eval_expr(&if_stmt.condition.clone(), local_scope)?;
                    let branch = match cond {
                        ConfigValue::Bool(true) => if_stmt.then_stmts.clone(),
                        ConfigValue::Bool(false) => if_stmt.else_stmts.clone(),
                        _ => unreachable!("typechecker ensures bool condition"),
                    };
                    let snapshot = local_scope.clone();
                    let flow = self.eval_func_stmts(&branch, local_scope)?;
                    restore_block_scope(local_scope, &snapshot, &branch, None);
                    match flow {
                        StatementFlow::Normal => {}
                        flow => return Ok(flow),
                    }
                }
            }
        }
        Ok(StatementFlow::Normal)
    }
}

fn assign_config_field_path(
    target: &mut ConfigValue,
    fields: &[String],
    value: ConfigValue,
    span: &Span,
) -> Result<(), EvalErr> {
    let Some((field, rest)) = fields.split_first() else {
        return Err(EvalErr::Fatal {
            message: "field assignment requires a field path".into(),
            span: span.clone(),
        });
    };
    let got = target.type_name();
    let ConfigValue::Section(section) = target else {
        return Err(EvalErr::TypeMismatch {
            expected: "section",
            got,
        });
    };
    if rest.is_empty() {
        if !section.contains_key(field) {
            return Err(EvalErr::PathNotFound {
                path: field.clone(),
                span: span.clone(),
            });
        }
        section.insert(field.clone(), value);
        return Ok(());
    }
    let nested = section
        .get_mut(field)
        .ok_or_else(|| EvalErr::PathNotFound {
            path: field.clone(),
            span: span.clone(),
        })?;
    assign_config_field_path(nested, rest, value, span)
}

fn restore_block_scope(
    scope: &mut HashMap<String, ConfigValue>,
    snapshot: &HashMap<String, ConfigValue>,
    statements: &[FuncStmt],
    loop_binding: Option<&ForBinding>,
) {
    let mut declared: Vec<&str> = statements
        .iter()
        .filter_map(|statement| match statement {
            FuncStmt::LocalVar(declaration) => Some(declaration.name.as_str()),
            _ => None,
        })
        .collect();
    if let Some(binding) = loop_binding {
        match binding {
            ForBinding::Value { name, .. } => declared.push(name),
            ForBinding::Indexed {
                index_name,
                value_name,
                ..
            } => {
                declared.push(index_name);
                declared.push(value_name);
            }
        }
    }
    for name in declared {
        if let Some(previous) = snapshot.get(name) {
            scope.insert(name.to_string(), previous.clone());
        } else {
            scope.remove(name);
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn eval_ok(src: &str) -> EvalResult {
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        let table = crate::resolver::Resolver::new()
            .resolve(&program, &[])
            .expect("resolve");
        crate::typechecker::TypeChecker::check(&program, &table).expect("typecheck");
        Evaluator::evaluate(&program, &table).expect("eval failed")
    }

    fn eval_err(src: &str) -> Vec<String> {
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        let table = crate::resolver::Resolver::new()
            .resolve(&program, &[])
            .expect("resolve");
        Evaluator::evaluate(&program, &table)
            .unwrap_err()
            .into_iter()
            .map(|e| e.to_string())
            .collect()
    }

    fn global(result: &EvalResult, name: &str) -> ConfigValue {
        result.globals[name].clone()
    }

    fn section_field(result: &EvalResult, path: &[&str], field: &str) -> ConfigValue {
        let key: Vec<String> = path.iter().map(|s| s.to_string()).collect();
        result.sections[&key][field].clone()
    }

    #[test]
    fn section_complete_when_var_references_its_field() {
        let src = r#"
[Man]{ aster: int = 6; };
[MetaData]{
    tool:    str = "stackforge";
    version: int = Man.aster;
    flag:    bool = false;
};
var x: str = MetaData.tool;
"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let symbols = crate::resolver::Resolver::new()
            .resolve(&program, &[])
            .unwrap();
        let result = Evaluator::evaluate(&program, &symbols).unwrap();

        let metadata = result
            .sections
            .get(&vec!["MetaData".to_string()])
            .expect("MetaData section must exist in EvalResult");

        assert!(
            metadata.contains_key("tool"),
            "MetaData must contain 'tool'"
        );
        assert!(
            metadata.contains_key("version"),
            "MetaData must contain 'version'"
        );
        assert!(
            metadata.contains_key("flag"),
            "MetaData must contain 'flag'"
        );
    }

    #[test]
    fn variable_can_reference_section_field_correctly() {
        let src = r#"
[Config]{ host: str = "localhost"; };
var endpoint: str = Config.host;
"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let symbols = crate::resolver::Resolver::new()
            .resolve(&program, &[])
            .unwrap();
        let result = Evaluator::evaluate(&program, &symbols).unwrap();

        assert_eq!(
            result.globals.get("endpoint"),
            Some(&ConfigValue::Str("localhost".to_string()))
        );
        assert!(result
            .sections
            .get(&vec!["Config".to_string()])
            .unwrap()
            .contains_key("host"));
    }

    #[test]
    fn test_int_literal() {
        let r = eval_ok("var x: int = 42;");
        assert_eq!(global(&r, "x"), ConfigValue::Int(42));
    }

    #[test]
    #[allow(clippy::approx_constant)]
    fn test_float_literal() {
        let r = eval_ok("var x: float = 3.14;");
        assert_eq!(global(&r, "x"), ConfigValue::Float(3.14));
    }

    #[test]
    fn test_bool_true() {
        let r = eval_ok("var x: bool = true;");
        assert_eq!(global(&r, "x"), ConfigValue::Bool(true));
    }

    #[test]
    fn test_bool_false() {
        let r = eval_ok("var x: bool = false;");
        assert_eq!(global(&r, "x"), ConfigValue::Bool(false));
    }

    #[test]
    fn test_string_literal() {
        let r = eval_ok(r#"var x: str = "keel";"#);
        assert_eq!(global(&r, "x"), ConfigValue::Str("keel".into()));
    }

    #[test]
    fn test_int_addition() {
        let r = eval_ok("var x: int = 10 + 32;");
        assert_eq!(global(&r, "x"), ConfigValue::Int(42));
    }

    #[test]
    fn test_int_multiplication() {
        let r = eval_ok("var x: int = 6 * 7;");
        assert_eq!(global(&r, "x"), ConfigValue::Int(42));
    }

    #[test]
    fn test_int_subtraction() {
        let r = eval_ok("var x: int = 50 - 8;");
        assert_eq!(global(&r, "x"), ConfigValue::Int(42));
    }

    #[test]
    fn test_int_division() {
        let r = eval_ok("var x: int = 84 / 2;");
        assert_eq!(global(&r, "x"), ConfigValue::Int(42));
    }

    #[test]
    fn test_float_arithmetic() {
        let r = eval_ok("var x: float = 3.0 * 1.5;");
        assert_eq!(global(&r, "x"), ConfigValue::Float(4.5));
    }

    #[test]
    fn test_division_by_zero_error() {
        let errs = eval_err("var x: int = 10 / 0;");
        assert!(
            errs.iter().any(|e| e.contains("division by zero")),
            "got: {errs:?}"
        );
    }

    #[test]
    fn test_string_concatenation() {
        let r = eval_ok(r#"var x: str = "hel" + "lo";"#);
        assert_eq!(global(&r, "x"), ConfigValue::Str("hello".into()));
    }

    #[test]
    fn test_string_interpolation_str_ref() {
        let r = eval_ok(
            r#"
            var name: str = "world";
            var msg: str = "hello ${global.name}";
        "#,
        );
        assert_eq!(global(&r, "msg"), ConfigValue::Str("hello world".into()));
    }

    #[test]
    fn test_string_interpolation_int_coerced() {
        let r = eval_ok(
            r#"
            var port: int = 3000;
            var bind: str = "0.0.0.0:${global.port}";
        "#,
        );
        assert_eq!(global(&r, "bind"), ConfigValue::Str("0.0.0.0:3000".into()));
    }

    #[test]
    fn test_str_coercion_of_int() {
        let r = eval_ok("var x: str = str(42);");
        assert_eq!(global(&r, "x"), ConfigValue::Str("42".into()));
    }

    #[test]
    fn test_str_coercion_of_bool() {
        let r = eval_ok("var x: str = str(true);");
        assert_eq!(global(&r, "x"), ConfigValue::Str("true".into()));
    }

    #[test]
    fn test_env_var_set() {
        std::env::set_var("SPAR_TEST_VAR", "hello");
        let r = eval_ok(r#"var x: str = env("SPAR_TEST_VAR");"#);
        std::env::remove_var("SPAR_TEST_VAR");
        assert_eq!(global(&r, "x"), ConfigValue::Str("hello".into()));
    }

    #[test]
    fn test_env_var_missing_error() {
        std::env::remove_var("SPAR_TEST_MISSING_XYZ");
        let errs = eval_err(r#"var x: str = env("SPAR_TEST_MISSING_XYZ");"#);
        assert!(
            errs.iter()
                .any(|e| e.contains("not set") || e.contains("EnvVar")),
            "got: {errs:?}"
        );
    }

    #[test]
    fn test_env_fallback_var_set() {
        std::env::set_var("SPAR_TEST_VAR2", "set");
        let r = eval_ok(r#"var x: str = env("SPAR_TEST_VAR2") ?? "default";"#);
        std::env::remove_var("SPAR_TEST_VAR2");
        assert_eq!(global(&r, "x"), ConfigValue::Str("set".into()));
    }

    #[test]
    fn test_env_fallback_var_missing() {
        std::env::remove_var("SPAR_TEST_MISSING_XYZ");
        let r = eval_ok(r#"var x: str = env("SPAR_TEST_MISSING_XYZ") ?? "default";"#);
        assert_eq!(global(&r, "x"), ConfigValue::Str("default".into()));
    }

    #[test]
    fn test_namespace_ref_single_segment() {
        let r = eval_ok("var port: int = 3000; var copy: int = port;");
        assert_eq!(global(&r, "copy"), ConfigValue::Int(3000));
    }

    #[test]
    fn test_namespace_ref_global_prefix() {
        let r = eval_ok("var port: int = 3000; var copy: int = global.port;");
        assert_eq!(global(&r, "copy"), ConfigValue::Int(3000));
    }

    #[test]
    fn test_namespace_ref_section_field() {
        let r = eval_ok("[Db]{ pool: int = 5; }; var p: int = Db.pool;");
        assert_eq!(global(&r, "p"), ConfigValue::Int(5));
    }

    #[test]
    fn test_namespace_ref_nested_section_field_3seg() {
        let r = eval_ok(
            r#"
            [Server]{ rateLimit: section = { enabled: bool = true; }; };
            var isDone: bool = Server.rateLimit.enabled;
        "#,
        );
        assert_eq!(global(&r, "isDone"), ConfigValue::Bool(true));
    }

    #[test]
    fn test_forward_reference() {
        let r = eval_ok(
            r#"
            var bind: str = "host:${global.port}";
            var port: int = 9000;
        "#,
        );
        assert_eq!(global(&r, "bind"), ConfigValue::Str("host:9000".into()));
    }

    #[test]
    fn test_cycle_detection_error() {
        let errs = eval_err("var a: str = global.b; var b: str = global.a;");
        assert!(errs.iter().any(|e| e.contains("cyclic")), "got: {errs:?}");
    }

    #[test]
    fn test_simple_section_evaluation() {
        let r = eval_ok(r#"[Server]{ port: int = 8080; host: str = "localhost"; };"#);
        assert_eq!(
            section_field(&r, &["Server"], "port"),
            ConfigValue::Int(8080)
        );
        assert_eq!(
            section_field(&r, &["Server"], "host"),
            ConfigValue::Str("localhost".into())
        );
    }

    #[test]
    fn test_section_field_references_global() {
        let r = eval_ok("var timeout: int = 30; [Db]{ timeout: int = global.timeout; };");
        assert_eq!(section_field(&r, &["Db"], "timeout"), ConfigValue::Int(30));
    }

    #[test]
    fn test_spread_merges_fields() {
        let r = eval_ok(
            r#"
            [Defaults]{ workers: int = 4; timeout: int = 30; };
            [Server]{ ...Defaults; port: int = 8080; };
        "#,
        );
        assert_eq!(
            section_field(&r, &["Server"], "workers"),
            ConfigValue::Int(4)
        );
        assert_eq!(
            section_field(&r, &["Server"], "port"),
            ConfigValue::Int(8080)
        );
    }

    #[test]
    fn test_spread_explicit_overrides_spread() {
        let r = eval_ok(
            r#"
            [Defaults]{ workers: int = 4; port: int = 3000; };
            [Server]{ ...Defaults; port: int = 8080; };
        "#,
        );
        assert_eq!(
            section_field(&r, &["Server"], "port"),
            ConfigValue::Int(8080)
        );
        assert_eq!(
            section_field(&r, &["Server"], "workers"),
            ConfigValue::Int(4)
        );
    }

    #[test]
    fn test_dynamic_mixed_list() {
        let r = eval_ok(r#"dynamic var tags = [2026, "prod", true];"#);
        match global(&r, "tags") {
            ConfigValue::List(items) => {
                assert_eq!(items[0], ConfigValue::Int(2026));
                assert_eq!(items[1], ConfigValue::Str("prod".into()));
                assert_eq!(items[2], ConfigValue::Bool(true));
            }
            _ => panic!("expected list"),
        }
    }

    #[test]
    fn nested_section_evaluated_in_sections_map() {
        let r = eval_ok(r#"[Outer]{ inner: section = { key: str = "v"; }; };"#);
        let nested_key = vec!["Outer".to_string(), "inner".to_string()];
        assert!(
            r.sections.contains_key(&nested_key),
            "nested section must appear in EvalResult.sections"
        );
        assert_eq!(r.sections[&nested_key]["key"], ConfigValue::Str("v".into()));
    }

    #[test]
    fn nested_section_twice_deep_evaluated() {
        let r = eval_ok("[A]{ b: section = { c: section = { val: int = 1; }; }; };");
        let inner_key = vec!["A".to_string(), "b".to_string(), "c".to_string()];
        assert!(
            r.sections.contains_key(&inner_key),
            "two-deep nested section must appear in EvalResult.sections"
        );
        assert_eq!(r.sections[&inner_key]["val"], ConfigValue::Int(1));
    }

    // ── Group 5: early-return evaluator ──────────────────────────────────────

    #[test]
    fn early_return_scalar_function() {
        let r = eval_ok(
            r#"
            function double(x: int) -> int {
                return x * 2;
            };
            var n: int = double(x: 5);
        "#,
        );
        assert_eq!(global(&r, "n"), ConfigValue::Int(10));
    }

    #[test]
    fn early_return_from_if_branch() {
        let r = eval_ok(
            r#"
            function absVal(x: int) -> int {
                if x < 0 { return 0 - x; } else { return x; }
            };
            var a: int = absVal(x: 0 - 3);
            var b: int = absVal(x: 7);
        "#,
        );
        assert_eq!(global(&r, "a"), ConfigValue::Int(3));
        assert_eq!(global(&r, "b"), ConfigValue::Int(7));
    }

    #[test]
    fn early_return_in_then_branch_else_falls_through() {
        let r = eval_ok(
            r#"
            function clamp(x: int) -> int {
                if x > 100 { return 100; }
                else { return x; }
            };
            var a: int = clamp(x: 200);
            var b: int = clamp(x: 42);
        "#,
        );
        assert_eq!(global(&r, "a"), ConfigValue::Int(100));
        assert_eq!(global(&r, "b"), ConfigValue::Int(42));
    }

    #[test]
    fn section_returning_function_result_in_section_cache() {
        let r = eval_ok(
            r#"
            function makeDb() -> section {
                return { host: str = "localhost"; port: int = 5432; };
            };
            [App]{ db: section = makeDb(); };
        "#,
        );
        let db_path = vec!["App".to_string(), "db".to_string()];
        assert!(
            r.sections.contains_key(&db_path),
            "section fn result must appear in sections map"
        );
        assert_eq!(
            r.sections[&db_path]["host"],
            ConfigValue::Str("localhost".into())
        );
        assert_eq!(r.sections[&db_path]["port"], ConfigValue::Int(5432));
    }

    #[test]
    fn local_var_used_in_return_after_if() {
        let r = eval_ok(
            r#"
            function choose(flag: bool) -> int {
                if flag { return 1; }
                else { return 99; }
            };
            var x: int = choose(flag: false);
        "#,
        );
        assert_eq!(global(&r, "x"), ConfigValue::Int(99));
    }

    #[test]
    fn compatibility_evaluator_catches_and_exposes_error_fields() {
        let r = eval_ok(
            r#"
            function recover() -> str {
                try { var impossible: int = 1 / 0; }
                catch err { return err.kind; }
                return "missed";
            };
            var kind: str = recover();
        "#,
        );
        assert_eq!(global(&r, "kind"), ConfigValue::Str("runtime".into()));
    }
}
