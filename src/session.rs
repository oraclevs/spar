//! A minimal persistent evaluation session — the foundation `spar repl`
//! (Task 11) is built on, and usable directly by an embedder that wants
//! to evaluate Spar fragments one at a time while keeping prior state.
//!
//! Implementation note: each `eval()` re-compiles and re-evaluates the
//! *entire* accumulated source (all previously committed fragments plus
//! the new one), not just the new fragment. That's what makes rollback
//! trivial and correct — a failing fragment simply never gets appended,
//! so the session's committed source (and the globals last read off of
//! it) are untouched. Executed shell expressions are memoized by their
//! stable source spans so their process effects are not repeated during
//! replay. Other observable effects, such as host calls, still replay.
//! Phase 0 doesn't attempt true incremental (parse-once, extend-in-place)
//! evaluation — that's a materially bigger project than a session
//! foundation needs to be to unblock `spar repl`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::compiler::{CompileOptions, Compiler};
use crate::error::SparError;
use crate::evaluator::ConfigValue;

#[derive(Clone, Default, Debug)]
pub struct EffectLedger(Arc<Mutex<HashMap<(usize, usize), ConfigValue>>>);

impl EffectLedger {
    pub(crate) fn get_or_try_run<E>(
        &self,
        span: (usize, usize),
        run: impl FnOnce() -> Result<ConfigValue, E>,
    ) -> Result<ConfigValue, E> {
        let mut guard = self.0.lock().unwrap();
        if let Some(value) = guard.get(&span) {
            return Ok(value.clone());
        }
        let value = run()?;
        guard.insert(span, value.clone());
        Ok(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputCompleteness {
    Complete,
    Incomplete,
}

/// Classifies an interactive Spar fragment for line-editor continuation.
///
/// Complete-but-invalid syntax intentionally returns `Complete`: Reedline
/// should submit it so the normal Spar parser can render the real diagnostic.
/// Only structural end-of-input conditions (open delimiters, unfinished
/// declarations, unterminated strings/blocks, and similar cases) return
/// `Incomplete`. A missing statement terminator is treated as complete for
/// interactive use when appending a synthetic terminator makes the fragment
/// parse successfully.
pub fn input_completeness(source: &str) -> InputCompleteness {
    if source.trim().is_empty() {
        return InputCompleteness::Complete;
    }

    match parse_fragment(source) {
        Ok(()) => InputCompleteness::Complete,
        Err(SparError::LexError { message, .. }) if message.starts_with("unterminated") => {
            InputCompleteness::Incomplete
        }
        Err(SparError::ParseError { span, .. }) if span.start >= source.len() => {
            // Source files require explicit statement terminators, but one-line
            // interactive input does not. Put the synthetic terminator on a new
            // line so a trailing `//` comment cannot swallow it.
            let terminated = format!("{source}\n;");
            if parse_fragment(&terminated).is_ok() {
                InputCompleteness::Complete
            } else {
                InputCompleteness::Incomplete
            }
        }
        Err(_) => InputCompleteness::Complete,
    }
}

fn parse_fragment(source: &str) -> Result<(), SparError> {
    let tokens = crate::lexer::Lexer::new(source).tokenize()?;
    crate::parser::Parser::new(tokens)
        .interactive()
        .parse()
        .map(|_| ())
}

fn normalize_interactive_fragment(source: &str) -> String {
    if source.trim().is_empty() || parse_fragment(source).is_ok() {
        return source.to_string();
    }

    // Put the synthetic terminator on a new line so a trailing `//` comment
    // cannot swallow it. Only keep it when it genuinely fixes the syntax;
    // malformed finished input is left untouched for the normal diagnostic.
    let terminated = format!("{source}\n;");
    if parse_fragment(&terminated).is_ok() {
        terminated
    } else {
        source.to_string()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum InteractiveEvalResult {
    Empty,
    Value(ConfigValue),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InteractivePresentation {
    #[default]
    Value,
    Schema,
    Inspect,
    /// Result of a mixed `cmd | from FMT |> ...` pipeline that named no `to`.
    Pipeline,
    /// Result of a mixed pipeline ending in `|> to FORMAT` with nothing
    /// consuming the bytes, so the terminal renders the value in that format.
    Encoded(&'static str),
}

/// Materialized interactive value returned by the compiled runtime path.
/// Live Stream resources never escape the runtime: Spar materializes only a
/// bounded preview and marks whether additional rows were available.
#[derive(Clone, Debug, PartialEq)]
pub struct InteractiveRuntimeValue {
    pub value: crate::runtime::Value,
    pub stream_preview: bool,
    pub truncated: bool,
    pub presentation: InteractivePresentation,
}

#[derive(Clone, Debug)]
pub enum InteractivePreviewResult {
    Empty,
    Value(ConfigValue),
    RuntimeValue(InteractiveRuntimeValue),
    Process(crate::evaluator::ShellPlanOutcome),
}

pub struct Session {
    options: CompileOptions,
    committed_source: String,
    globals: HashMap<String, ConfigValue>,
    sections: HashMap<Vec<String>, indexmap::IndexMap<String, ConfigValue>>,
    identifiers: BTreeSet<String>,
    functions: BTreeSet<String>,
    function_params: BTreeMap<String, Vec<String>>,
    structured_terminal: bool,
    /// Parameter names of the prelude's `std/data` functions, so interactive
    /// clients can label arguments before anything has been compiled.
    prelude_params: BTreeMap<&'static str, Vec<String>>,
}

struct EvaluatedCandidate {
    source: String,
    globals: HashMap<String, ConfigValue>,
    sections: HashMap<Vec<String>, indexmap::IndexMap<String, ConfigValue>>,
    identifiers: BTreeSet<String>,
    functions: BTreeSet<String>,
    function_params: BTreeMap<String, Vec<String>>,
    interactive: InteractiveEvalResult,
}

impl Session {
    pub(crate) fn new(mut options: CompileOptions) -> Self {
        options.effect_ledger = Some(EffectLedger::default());
        Self {
            options,
            committed_source: String::new(),
            globals: HashMap::new(),
            sections: HashMap::new(),
            identifiers: BTreeSet::new(),
            functions: BTreeSet::new(),
            function_params: BTreeMap::new(),
            structured_terminal: false,
            prelude_params: BTreeMap::new(),
        }
    }

    /// Evaluates `fragment` as if appended to everything previously
    /// committed. On success, commits it — later `eval` calls and
    /// `value` lookups see its declarations and any mutations it made.
    /// On failure, the session is left exactly as it was before this
    /// call; nothing partially commits.
    pub fn eval(&mut self, fragment: &str) -> Result<(), Vec<SparError>> {
        self.eval_interactive(fragment).map(|_| ())
    }

    /// Evaluates one interactive fragment and returns the value of the final
    /// direct module-level expression, if the fragment ends in one. The
    /// session is committed only after successful parse, typecheck, and
    /// evaluation, preserving the same transactional behavior as `eval`.
    pub fn eval_interactive(
        &mut self,
        fragment: &str,
    ) -> Result<InteractiveEvalResult, Vec<SparError>> {
        let evaluated = self.evaluate_candidate(fragment, None)?;
        self.commit_evaluated(evaluated)
    }

    /// Evaluates an interactive fragment using caller-owned cwd/environment
    /// without mutating process-global state. This is the embedding surface
    /// Sparsh uses so `$NAME`, shell-returning functions, and command
    /// substitution see the same session state as external commands.
    pub fn eval_interactive_with_context(
        &mut self,
        fragment: &str,
        cwd: &Path,
        environment: &[(OsString, OsString)],
    ) -> Result<InteractiveEvalResult, Vec<SparError>> {
        let context = runtime_context(cwd, environment);
        let evaluated = self.evaluate_candidate(fragment, Some(context))?;
        self.commit_evaluated(evaluated)
    }

    /// Sparsh-facing interactive evaluation path for runtime-only values.
    ///
    /// Ordinary declaration-only fragments keep the established evaluator
    /// path. When a fragment ends in a direct expression, that expression is
    /// moved into a temporary zero-argument function and executed by the
    /// compiled runtime. This lets `Table<T>`, structured `|>` expressions,
    /// methods, and lazy `Stream<T>` values reach the interactive shell without
    /// forcing them through `ConfigValue` serialization.
    /// The type `_` has for `value`. A record shaped like `std/http`'s
    /// `HttpResponse` is typed as one (when that type is in scope) so its
    /// methods, such as `_.json()`, resolve.
    fn previous_value_type(&self, value: &crate::runtime::Value) -> Option<crate::ast::SparType> {
        if let crate::runtime::Value::Object(fields) = value {
            let http_shaped = fields.len() == 3
                && matches!(fields.get("status"), Some(crate::runtime::Value::Int(_)))
                && matches!(fields.get("body"), Some(crate::runtime::Value::String(_)))
                && matches!(
                    fields.get("contentType"),
                    Some(crate::runtime::Value::String(_))
                );
            if http_shaped && self.identifiers.contains("HttpResponse") {
                return Some(crate::ast::SparType::Named("HttpResponse".into()));
            }
        }
        interactive_value_type(value)
    }

    /// Makes every `std/data` function (`where`, `map`, `take`, ...) usable
    /// without an `import`, as an interactive prompt expects. Anything the
    /// session declares or imports itself, before or after this call, takes
    /// precedence over the prelude.
    pub fn enable_data_prelude(&mut self) {
        self.options.data_prelude = true;
        self.prelude_params = crate::stdlib::DATA_FUNCTIONS
            .iter()
            .filter_map(|name| {
                let signature = self.options.natives.signature("nativeData", name)?;
                let params = signature.params.iter().map(|(param, _)| param.clone());
                Some((*name, params.collect()))
            })
            .collect();
    }

    /// The prelude functions this session actually provides: none unless the
    /// prelude is on, and never a name the session declares or imports itself.
    fn prelude_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        let enabled = self.options.data_prelude;
        crate::stdlib::DATA_FUNCTIONS
            .iter()
            .copied()
            .filter(move |name| enabled && !self.identifiers.contains(*name))
    }

    /// Tells the session that results are shown on an interactive terminal that
    /// renders structured values itself. A mixed pipeline with no downstream
    /// consumer then returns its value instead of writing serialized bytes.
    pub fn set_structured_terminal(&mut self, enabled: bool) {
        self.structured_terminal = enabled;
    }

    /// Execute one raw interactive shell body without first wrapping it as a
    /// module-level `shell { ... }` expression. The body is compiled inside a
    /// temporary declaration-safe function so command-first mixed pipelines can
    /// use the normal shell parser while still returning structured terminal
    /// captures to Sparsh. The temporary function is never committed.
    pub fn eval_interactive_shell_preview_with_context(
        &mut self,
        body: &str,
        cwd: &Path,
        environment: &[(OsString, OsString)],
        previous_value: Option<crate::runtime::Value>,
        preview_limit: usize,
    ) -> Result<InteractivePreviewResult, Vec<SparError>> {
        let trimmed = body.trim().trim_end_matches(';').trim();
        let previous_type = previous_value
            .as_ref()
            .and_then(|value| self.previous_value_type(value));

        let mut function_name = "sparshInteractiveShellPreview".to_string();
        let mut suffix = 0_u64;
        while self.identifiers.contains(&function_name)
            || self
                .committed_source
                .contains(&format!("function {function_name}"))
        {
            suffix += 1;
            function_name = format!("sparshInteractiveShellPreview{suffix}");
        }

        let function_source =
            format!("function {function_name}() -> shell {{ return shell {{\n{trimmed};\n}}; }};");
        let runtime_source = join_committed_source(&self.committed_source, &function_source);
        let options = CompileOptions {
            evaluate: false,
            ..self.options.clone()
        };
        let (origin, origin_line) = fragment_origin(&self.committed_source);
        let prefix = format!("function {function_name}() -> shell {{ return shell {{\n");
        let relocate_body = |errors: Vec<SparError>| {
            relocate_errors(
                errors,
                origin + prefix.len(),
                origin_line + 1,
                0,
                trimmed.len(),
            )
        };
        let compilation = Compiler::new(options.clone())
            .with_interactive_expressions()
            .with_interactive_previous_type(previous_type)
            .compile(&runtime_source)
            .into_result()
            .map_err(&relocate_body)?;
        let program = crate::compiled::CompiledProgram::from_compilation(compilation, options)
            .map_err(&relocate_body)?;

        let mut context = runtime_context(cwd, environment);
        context.set_previous_value(previous_value);
        context.set_structured_terminal(self.structured_terminal);
        let (execution, _) = crate::runtime::execute_interactive_preview_with_context(
            &program,
            &function_name,
            context,
            preview_limit,
            false,
        )
        .map_err(&relocate_body)?;

        Ok(match execution {
            crate::runtime::InteractiveRuntimeExecution::Value(value) => {
                InteractivePreviewResult::RuntimeValue(value)
            }
            crate::runtime::InteractiveRuntimeExecution::Process(outcome) => {
                InteractivePreviewResult::Process(outcome)
            }
        })
    }

    pub fn eval_interactive_preview_with_context(
        &mut self,
        fragment: &str,
        cwd: &Path,
        environment: &[(OsString, OsString)],
        previous_value: Option<crate::runtime::Value>,
        preview_limit: usize,
    ) -> Result<InteractivePreviewResult, Vec<SparError>> {
        let normalized = normalize_interactive_fragment(fragment);
        let Some((prefix, expression)) = split_final_expression(&normalized)? else {
            return self
                .eval_interactive_with_context(fragment, cwd, environment)
                .map(|result| match result {
                    InteractiveEvalResult::Empty => InteractivePreviewResult::Empty,
                    InteractiveEvalResult::Value(value) => InteractivePreviewResult::Value(value),
                });
        };

        let committed = join_committed_source(&self.committed_source, &prefix);
        let options = CompileOptions {
            evaluate: false,
            ..self.options.clone()
        };
        let previous_type = previous_value
            .as_ref()
            .and_then(|value| self.previous_value_type(value));

        // Type-check the submitted expression without evaluating it first.
        // Spar functions require an explicit return type, so the temporary
        // runtime wrapper uses the type already inferred for the expression
        // instead of inventing an "any" type that Spar does not have.
        let candidate_source = join_committed_source(&self.committed_source, &normalized);
        let (origin, origin_line) = fragment_origin(&self.committed_source);
        let candidate = Compiler::new(options.clone())
            .with_interactive_expressions()
            .with_interactive_previous_type(previous_type.clone())
            .compile(&candidate_source)
            .into_result()
            .map_err(|errors| relocate_errors(errors, origin, origin_line, 0, normalized.len()))?;
        let candidate_program = candidate
            .program
            .as_ref()
            .expect("successful compilation always provides a program");
        let candidate_symbols = candidate
            .symbols
            .as_ref()
            .expect("successful compilation always provides symbols");
        let expression_ast = candidate_program
            .items
            .iter()
            .rev()
            .find_map(|item| match item {
                crate::ast::TopLevelItem::Statement(crate::ast::Statement::Expression(expr, _)) => {
                    Some(expr)
                }
                _ => None,
            })
            .ok_or_else(|| {
                vec![SparError::EvalError {
                    message: "interactive expression disappeared during compilation".into(),
                    span: crate::Span::dummy(),
                }]
            })?;
        let presentation = interactive_presentation(expression_ast);
        let expression_type = crate::typechecker::infer_expression_with_locals(
            expression_ast,
            candidate_symbols,
            &HashMap::new(),
        )
        .ok_or_else(|| {
            relocate_errors(
                vec![SparError::TypeError {
                    message: "could not determine the type of this expression".into(),
                    hint: Some(
                        "check that every function and variable it uses is defined or imported"
                            .into(),
                    ),
                    span: expression_ast
                        .span()
                        .cloned()
                        .unwrap_or_else(crate::Span::dummy),
                }],
                origin,
                origin_line,
                0,
                normalized.len(),
            )
        })?;
        let return_type = crate::typechecker::display_type(&expression_type);

        let mut function_name = "sparshInteractivePreview".to_string();
        let mut suffix = 0_u64;
        while self.identifiers.contains(&function_name)
            || committed.contains(&format!("function {function_name}"))
        {
            suffix += 1;
            function_name = format!("sparshInteractivePreview{suffix}");
        }
        // A final expression that awaits runs inside an async wrapper, and the
        // runtime waits for that wrapper's promise before the value is shown.
        let uses_await = expression_uses_await(&expression);
        let async_keyword = if uses_await { "async " } else { "" };
        let function_source = if expression_type == crate::ast::SparType::Void {
            format!(
                "{async_keyword}function {function_name}() -> void {{ {expression}; return; }};"
            )
        } else {
            format!(
                "{async_keyword}function {function_name}() -> {return_type} {{ return {expression}; }};"
            )
        };
        let runtime_source = if committed.trim().is_empty() {
            function_source
        } else {
            format!("{committed}\n{function_source}")
        };

        // Errors from here on point into the wrapper function around the final
        // expression; put them back at that expression's place in the fragment.
        let (committed_origin, committed_line) = fragment_origin(&committed);
        let wrapper_prefix = if expression_type == crate::ast::SparType::Void {
            format!("{async_keyword}function {function_name}() -> void {{ ")
        } else {
            format!("{async_keyword}function {function_name}() -> {return_type} {{ return ")
        };
        let expression_offset = normalized.rfind(expression.as_str());
        let to_fragment = |errors: Vec<SparError>| {
            let errors = relocate_errors(
                errors,
                committed_origin + wrapper_prefix.len(),
                committed_line,
                0,
                expression.len(),
            );
            shift_into_fragment(errors, &normalized, expression_offset)
        };

        let compilation = Compiler::new(options.clone())
            .with_interactive_expressions()
            .with_interactive_previous_type(previous_type)
            .compile(&runtime_source)
            .into_result()
            .map_err(to_fragment)?;
        let symbols = compilation
            .symbols
            .as_ref()
            .expect("successful compilation always provides symbols");
        let mut identifiers = BTreeSet::new();
        identifiers.extend(symbols.globals.keys().cloned());
        identifiers.extend(symbols.functions.keys().cloned());
        identifiers.extend(symbols.imported_functions.keys().cloned());
        identifiers.extend(symbols.types.keys().cloned());
        identifiers.extend(symbols.enums.keys().cloned());
        identifiers.extend(symbols.function_groups.keys().cloned());
        identifiers.extend(symbols.imports.keys().cloned());
        identifiers.extend(symbols.tasks.keys().cloned());
        identifiers.remove(&function_name);
        identifiers.remove("_");
        let mut function_params = BTreeMap::new();
        for (name, entry) in &symbols.imported_functions {
            function_params.insert(
                name.clone(),
                entry.params.iter().map(|(name, _)| name.clone()).collect(),
            );
        }
        for (name, entry) in &symbols.functions {
            if name == &function_name {
                continue;
            }
            function_params.insert(
                name.clone(),
                entry.params.iter().map(|(name, _)| name.clone()).collect(),
            );
        }
        let functions = function_params.keys().cloned().collect();
        let program = crate::compiled::CompiledProgram::from_compilation(compilation, options)
            .map_err(to_fragment)?;
        let mut context = runtime_context(cwd, environment);
        context.set_previous_value(previous_value);
        context.set_structured_terminal(self.structured_terminal);
        let (execution, result) = crate::runtime::execute_interactive_preview_with_context(
            &program,
            &function_name,
            context,
            preview_limit,
            uses_await,
        )
        .map_err(to_fragment)?;

        self.committed_source = committed;
        self.globals = result.globals;
        self.sections = result.sections;
        self.identifiers = identifiers;
        self.functions = functions;
        self.function_params = function_params;

        match execution {
            crate::runtime::InteractiveRuntimeExecution::Process(outcome) => {
                Ok(InteractivePreviewResult::Process(outcome))
            }
            crate::runtime::InteractiveRuntimeExecution::Value(mut preview) => {
                if matches!(
                    preview.presentation,
                    InteractivePresentation::Pipeline | InteractivePresentation::Encoded(_)
                ) {
                    return Ok(InteractivePreviewResult::RuntimeValue(preview));
                }
                preview.presentation = presentation;
                // Configuration values have no null (it would become `0`), so a
                // result with nulls inside stays a runtime value.
                if presentation == InteractivePresentation::Value
                    && !preview.stream_preview
                    && !contains_nested_null(&preview.value)
                {
                    if let Ok(config) = preview.value.clone().try_into_config(&crate::Span::dummy())
                    {
                        return Ok(InteractivePreviewResult::Value(config));
                    }
                }
                Ok(InteractivePreviewResult::RuntimeValue(preview))
            }
        }
    }

    fn commit_evaluated(
        &mut self,
        evaluated: EvaluatedCandidate,
    ) -> Result<InteractiveEvalResult, Vec<SparError>> {
        self.committed_source = evaluated.source;
        self.globals = evaluated.globals;
        self.sections = evaluated.sections;
        self.identifiers = evaluated.identifiers;
        self.functions = evaluated.functions;
        self.function_params = evaluated.function_params;
        Ok(evaluated.interactive)
    }

    /// Evaluates a fragment against the current persistent session without
    /// committing it. This is used by embedders such as Sparsh for startup
    /// hooks and command-line shell plans that need access to current Spar
    /// variables/functions but must not become permanent session source.
    pub fn eval_transient(&self, fragment: &str) -> Result<InteractiveEvalResult, Vec<SparError>> {
        self.evaluate_candidate(fragment, None)
            .map(|evaluated| evaluated.interactive)
    }

    pub fn eval_transient_with_context(
        &self,
        fragment: &str,
        cwd: &Path,
        environment: &[(OsString, OsString)],
    ) -> Result<InteractiveEvalResult, Vec<SparError>> {
        self.evaluate_candidate(fragment, Some(runtime_context(cwd, environment)))
            .map(|evaluated| evaluated.interactive)
    }

    /// Parses and evaluates one native shell command/plan against the current
    /// Spar session without committing it. `${expr}` therefore sees persistent
    /// Spar variables and named function arguments while the resulting plan
    /// remains deferred data for the caller to execute.
    pub fn eval_shell_plan(&self, source: &str) -> Result<spar_command::ShellPlan, Vec<SparError>> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let environment = std::env::vars_os().collect::<Vec<_>>();
        self.eval_shell_plan_with_context(source, &cwd, &environment)
    }

    pub fn eval_shell_plan_with_context(
        &self,
        source: &str,
        cwd: &Path,
        environment: &[(OsString, OsString)],
    ) -> Result<spar_command::ShellPlan, Vec<SparError>> {
        let trimmed = source.trim();
        let terminated = if trimmed.ends_with(';') {
            trimmed.to_string()
        } else {
            format!("{trimmed};")
        };
        // This entry point only receives native command lines. A leading
        // `command` is the POSIX wrapper builtin here, not Spar's `command`
        // sugar, so keep it as the program name.
        let terminated = if terminated == "command;"
            || terminated
                .strip_prefix("command")
                .is_some_and(|rest| rest.starts_with(char::is_whitespace))
        {
            format!("command {terminated}")
        } else {
            terminated
        };

        // The ordinary Spar parser accepts module declarations at the top
        // level, not a bare `shell { ... }` expression.  Evaluate the native
        // shell expression as a temporary typed declaration instead.  This
        // preserves access to the persistent Spar session (including
        // `${expr}` and functions), caller-owned cwd/environment, and command
        // substitution without committing the interactive command itself.
        let mut name = "sparshInteractiveShellPlan".to_string();
        let mut suffix = 0_u64;
        while self.identifiers.contains(&name) {
            suffix += 1;
            name = format!("sparshInteractiveShellPlan{suffix}");
        }
        let wrapper_prefix = format!("var {name}: shell = shell {{\n");
        let fragment = format!("{wrapper_prefix}{terminated}\n}};");
        // `command` may have been inserted before the user's text on its first
        // line; errors are reported in the coordinates of what was typed.
        let inserted = terminated
            .len()
            .saturating_sub(trimmed.len() + usize::from(!trimmed.ends_with(';')));
        let mut evaluated = self
            .evaluate_candidate(&fragment, Some(runtime_context(cwd, environment)))
            .map_err(|errors| {
                relocate_errors(
                    errors,
                    wrapper_prefix.len() + inserted,
                    1,
                    inserted as u32,
                    terminated.len() - inserted,
                )
            })?;
        match evaluated.globals.remove(&name) {
            Some(ConfigValue::Shell(plan)) => Ok(plan),
            _ => Err(vec![SparError::EvalError {
                message: "interactive command did not evaluate to a shell plan".into(),
                span: crate::Span::dummy(),
            }]),
        }
    }

    /// Compiles and evaluates `fragment` against the session. Errors carry
    /// spans in the fragment's own coordinates, not in the hidden session
    /// source it was joined onto.
    fn evaluate_candidate(
        &self,
        fragment: &str,
        runtime_context: Option<crate::runtime::RuntimeContext>,
    ) -> Result<EvaluatedCandidate, Vec<SparError>> {
        let (origin, origin_line) = fragment_origin(&self.committed_source);
        self.evaluate_candidate_in_session(fragment, runtime_context)
            .map_err(|errors| relocate_errors(errors, origin, origin_line, 0, fragment.len()))
    }

    fn evaluate_candidate_in_session(
        &self,
        fragment: &str,
        runtime_context: Option<crate::runtime::RuntimeContext>,
    ) -> Result<EvaluatedCandidate, Vec<SparError>> {
        // Source files retain explicit statement terminators, but interactive
        // input may omit the final `;`. Normalize any fragment for which a
        // single synthetic terminator turns otherwise-incomplete syntax into
        // valid syntax. This applies uniformly to variable declarations,
        // function calls, declarations, and other interactive statements.
        let normalized = normalize_interactive_fragment(fragment);
        let candidate = if self.committed_source.is_empty() {
            normalized
        } else {
            format!("{}\n{}", self.committed_source, normalized)
        };

        let mut options = self.options.clone();
        options.evaluate = runtime_context.is_none();
        let compilation = Compiler::new(options.clone())
            .with_interactive_expressions()
            .compile(&candidate)
            .into_result()?;
        let symbols = compilation
            .symbols
            .as_ref()
            .expect("successful compilation always provides symbols");
        let mut identifiers = BTreeSet::new();
        identifiers.extend(symbols.globals.keys().cloned());
        identifiers.extend(symbols.functions.keys().cloned());
        identifiers.extend(symbols.imported_functions.keys().cloned());
        identifiers.extend(symbols.types.keys().cloned());
        identifiers.extend(symbols.enums.keys().cloned());
        identifiers.extend(symbols.function_groups.keys().cloned());
        identifiers.extend(symbols.imports.keys().cloned());
        identifiers.extend(symbols.tasks.keys().cloned());
        let mut function_params = BTreeMap::new();
        for (name, entry) in &symbols.imported_functions {
            function_params.insert(
                name.clone(),
                entry.params.iter().map(|(name, _)| name.clone()).collect(),
            );
        }
        for (name, entry) in &symbols.functions {
            function_params.insert(
                name.clone(),
                entry.params.iter().map(|(name, _)| name.clone()).collect(),
            );
        }
        let functions = function_params.keys().cloned().collect();

        let result = match runtime_context {
            Some(context) => {
                crate::evaluator::Evaluator::evaluate_with_imports_base_effects_natives_and_context(
                    compilation
                        .program
                        .as_ref()
                        .expect("successful compilation always provides a program"),
                    symbols,
                    &compilation.imports,
                    &options.base_dir,
                    options.hosts.clone(),
                    options.natives.clone(),
                    options.effect_ledger.clone(),
                    context,
                )?
            }
            None => compilation
                .result
                .expect("a successful evaluate:true compile always sets `result`"),
        };
        let interactive = result
            .interactive_value
            .clone()
            .map_or(InteractiveEvalResult::Empty, InteractiveEvalResult::Value);

        Ok(EvaluatedCandidate {
            source: candidate,
            globals: result.globals,
            sections: result.sections,
            identifiers,
            functions,
            function_params,
            interactive,
        })
    }

    /// The current value of a module-scope variable, as of the last
    /// successful `eval`.
    pub fn value(&self, name: &str) -> Option<&ConfigValue> {
        self.globals.get(name)
    }

    /// The evaluated value of a top-level `struct`/section declaration.
    /// Nested sections are included as `ConfigValue::Section` values under
    /// their own names (the evaluator stores them under separate path keys).
    pub fn section(&self, name: &str) -> Option<ConfigValue> {
        let path = vec![name.to_string()];
        self.sections
            .contains_key(&path)
            .then(|| self.assemble_section(&path))
    }

    fn assemble_section(&self, path: &[String]) -> ConfigValue {
        let mut fields = self.sections.get(path).cloned().unwrap_or_default();
        for nested in self
            .sections
            .keys()
            .filter(|candidate| candidate.len() == path.len() + 1 && candidate.starts_with(path))
        {
            if let Some(name) = nested.last() {
                fields.insert(name.clone(), self.assemble_section(nested));
            }
        }
        ConfigValue::Section(fields)
    }

    /// Names currently visible to an interactive client for completion.
    pub fn identifiers<'a>(&'a self) -> impl Iterator<Item = &'a str> + 'a {
        self.identifiers
            .iter()
            .map(String::as_str)
            .chain(self.prelude_names().map(|name| -> &'a str { name }))
    }

    pub fn has_function(&self, name: &str) -> bool {
        self.functions.contains(name) || self.prelude_names().any(|prelude| prelude == name)
    }

    /// Function names visible to interactive embedders such as Sparsh.
    pub fn function_names<'a>(&'a self) -> impl Iterator<Item = &'a str> + 'a {
        self.functions
            .iter()
            .map(String::as_str)
            .chain(self.prelude_names().map(|name| -> &'a str { name }))
    }

    /// Named parameter labels for an interactive function call.
    pub fn function_parameters(&self, name: &str) -> Option<&[String]> {
        self.function_params
            .get(name)
            .or_else(|| {
                self.prelude_names()
                    .any(|prelude| prelude == name)
                    .then(|| self.prelude_params.get(name))
                    .flatten()
            })
            .map(Vec::as_slice)
    }
    /// Every fragment committed so far, concatenated — mainly useful for
    /// diagnostics/debugging a session, not for driving further logic.
    pub fn committed_source(&self) -> &str {
        &self.committed_source
    }
}

fn interactive_value_type(value: &crate::runtime::Value) -> Option<crate::ast::SparType> {
    use crate::ast::SparType;
    use crate::runtime::Value;

    fn merged(values: impl Iterator<Item = Option<SparType>>) -> Option<SparType> {
        let mut values = values;
        let first = values.next()??;
        values.try_fold(first, |current, next| {
            let next = next?;
            (current == next).then_some(current)
        })
    }

    match value {
        Value::Void => Some(SparType::Void),
        Value::Int(_) => Some(SparType::Int),
        Value::Float(_) => Some(SparType::Float),
        Value::Bool(_) => Some(SparType::Bool),
        Value::String(_) => Some(SparType::Str),
        Value::Bytes(_) => Some(SparType::Named("Bytes".into())),
        Value::List(values) => Some(SparType::List(Box::new(merged(
            values.iter().map(interactive_value_type),
        )?))),
        Value::Object(_) => Some(SparType::Named("Record".into())),
        Value::Map(entries) => Some(SparType::Applied {
            name: "Map".into(),
            arguments: vec![
                merged(entries.iter().map(|(key, _)| interactive_value_type(key)))?,
                merged(
                    entries
                        .iter()
                        .map(|(_, value)| interactive_value_type(value)),
                )?,
            ],
        }),
        Value::Option(Some(value)) => Some(SparType::Applied {
            name: "Option".into(),
            arguments: vec![interactive_value_type(value)?],
        }),
        Value::Result(Ok(_)) | Value::Result(Err(_)) => None,
        Value::Table(_) => Some(SparType::Applied {
            name: "Table".into(),
            arguments: vec![SparType::Named("Record".into())],
        }),
        Value::Schema(_) => Some(SparType::Named("Schema".into())),
        Value::Error { .. } => Some(SparType::Error),
        Value::Shell(_) | Value::MixedShell(_) | Value::ShellProgram(_) => Some(SparType::Shell),
        Value::Option(None)
        | Value::Promise(_)
        | Value::Resource(_)
        | Value::Closure(_)
        | Value::Function(_) => None,
    }
}

fn interactive_presentation(expr: &crate::ast::Expr) -> InteractivePresentation {
    use crate::ast::Expr;

    match expr {
        Expr::Call { name, .. } | Expr::FnCall(crate::ast::FnCall { name, .. }) => {
            match name.rsplit("::").next().unwrap_or(name.as_str()) {
                "schema" => InteractivePresentation::Schema,
                "inspect" => InteractivePresentation::Inspect,
                _ => InteractivePresentation::Value,
            }
        }
        Expr::MethodCall { method, .. } => match method.as_str() {
            "schema" => InteractivePresentation::Schema,
            "inspect" => InteractivePresentation::Inspect,
            _ => InteractivePresentation::Value,
        },
        Expr::StructuredPipe { stage, .. } | Expr::Grouped(stage, _) => {
            interactive_presentation(stage)
        }
        _ => InteractivePresentation::Value,
    }
}

fn split_final_expression(source: &str) -> Result<Option<(String, String)>, Vec<SparError>> {
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .map_err(|error| vec![error])?;
    let program = crate::parser::Parser::new(tokens)
        .interactive()
        .parse()
        .map_err(|error| vec![error])?;
    let Some(crate::ast::TopLevelItem::Statement(crate::ast::Statement::Expression(_, span))) =
        program.items.last()
    else {
        return Ok(None);
    };
    // An expression statement's span is only its first token, and this is the
    // last item, so the expression runs from there to the end of the source.
    let Some(raw) = source.get(span.start..) else {
        return Err(vec![SparError::EvalError {
            message: "interactive expression span is outside the submitted source".into(),
            span: span.clone(),
        }]);
    };
    let expression = raw.trim().trim_end_matches(';').trim().to_string();
    let prefix = source
        .get(..span.start)
        .unwrap_or_default()
        .trim_end()
        .to_string();
    Ok(Some((prefix, expression)))
}

/// Whether a record or list has a `null` (void) anywhere inside it.
fn contains_nested_null(value: &crate::runtime::Value) -> bool {
    use crate::runtime::Value;
    let is_null = |value: &Value| matches!(value, Value::Void | Value::Option(None));
    match value {
        Value::Object(fields) => fields
            .values()
            .any(|field| is_null(field) || contains_nested_null(field)),
        Value::List(items) => items
            .iter()
            .any(|item| is_null(item) || contains_nested_null(item)),
        _ => false,
    }
}

/// Whether the expression contains an `await` (string contents don't count).
fn expression_uses_await(expression: &str) -> bool {
    crate::lexer::Lexer::new(expression)
        .tokenize()
        .map(|tokens| {
            tokens
                .iter()
                .any(|token| token.token == crate::token::Token::KwAwait)
        })
        .unwrap_or(false)
}

/// Where a fragment starts inside `join_committed_source(committed, fragment)`:
/// its byte offset and how many lines come before it.
fn fragment_origin(committed: &str) -> (usize, u32) {
    if committed.trim().is_empty() {
        (0, 0)
    } else {
        (
            committed.len() + 1,
            committed.matches('\n').count() as u32 + 1,
        )
    }
}

/// Errors from compiling session source plus a fragment point into that whole
/// text. Re-express them in the fragment's own coordinates (byte offset, line,
/// column) so a prompt can underline what was typed instead of a line number
/// deep inside the hidden session source. `origin` is the byte offset where
/// the user's text starts, `origin_line` the number of lines before it, and
/// `col_shift` the width of synthetic text on the user's first line, and `len`
/// the byte length of the user's text. An error
/// outside the user's text has no place in it and is pinned to its start.
fn relocate_errors(
    mut errors: Vec<SparError>,
    origin: usize,
    origin_line: u32,
    col_shift: u32,
    len: usize,
) -> Vec<SparError> {
    for error in &mut errors {
        let span = error.span_mut();
        // `len` is the length of the user's text: a span past it belongs to
        // some other source (a bundled module, say) and is not in the input.
        if span.start >= origin && span.end <= origin + len && span.line > origin_line {
            span.start -= origin;
            span.end = span.end.saturating_sub(origin).max(span.start);
            span.line -= origin_line;
            if span.line == 1 {
                span.col = span.col.saturating_sub(col_shift).max(1);
            }
        } else {
            *span = crate::Span::new(0, 0, 1, 1);
        }
    }
    errors
}

/// Moves errors whose spans are relative to an expression into the fragment
/// that contains it, given the expression's byte offset there. Without a known
/// offset the errors are pinned to the start of the fragment.
fn shift_into_fragment(
    mut errors: Vec<SparError>,
    fragment: &str,
    expression_offset: Option<usize>,
) -> Vec<SparError> {
    let Some(offset) = expression_offset else {
        return relocate_errors(errors, usize::MAX, u32::MAX, 0, 0);
    };
    let before = &fragment[..offset];
    let extra_lines = before.matches('\n').count() as u32;
    let extra_cols = (offset - before.rfind('\n').map_or(0, |newline| newline + 1)) as u32;
    for error in &mut errors {
        let span = error.span_mut();
        if span.line == 1 {
            span.col += extra_cols;
        }
        span.start += offset;
        span.end += offset;
        span.line += extra_lines;
    }
    errors
}

fn join_committed_source(committed: &str, fragment: &str) -> String {
    match (committed.trim().is_empty(), fragment.trim().is_empty()) {
        (true, true) => String::new(),
        (false, true) => committed.to_string(),
        (true, false) => fragment.to_string(),
        (false, false) => format!("{committed}\n{fragment}"),
    }
}

fn runtime_context(
    cwd: &Path,
    environment: &[(OsString, OsString)],
) -> crate::runtime::RuntimeContext {
    let mut context = crate::runtime::RuntimeContext::new(cwd.to_path_buf());
    context.replace_environment(
        environment.iter().filter_map(|(key, value)| {
            Some((key.to_str()?.to_string(), value.to_str()?.to_string()))
        }),
    );
    context
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::host::{HostFunction, HostRegistry};
    use crate::runtime::Value;

    #[test]
    fn section_includes_nested_sections_as_values() {
        let mut session = crate::Engine::default().session();
        session
            .eval(
                r#"struct Config {
    name: str = "top";
    prompt: section = {
        depth: int = 1;
        inner: section = { flag: bool = true; };
    };
};"#,
            )
            .unwrap();

        let ConfigValue::Section(config) = session.section("Config").expect("Config") else {
            panic!("expected a section");
        };
        assert_eq!(config.get("name"), Some(&ConfigValue::Str("top".into())));
        let Some(ConfigValue::Section(prompt)) = config.get("prompt") else {
            panic!("nested `prompt` section missing: {config:?}");
        };
        assert_eq!(prompt.get("depth"), Some(&ConfigValue::Int(1)));
        let Some(ConfigValue::Section(inner)) = prompt.get("inner") else {
            panic!("doubly nested section missing: {prompt:?}");
        };
        assert_eq!(inner.get("flag"), Some(&ConfigValue::Bool(true)));
        assert!(session.section("Missing").is_none());
    }

    #[test]
    fn nested_object_literals_stay_inside_their_typed_field() {
        let mut session = crate::Engine::default().session();
        session
            .eval(
                r#"type Deep { z?: int; };
type Sub { x?: int; deep?: Deep; };
type P { a?: int; sub?: Sub; };
struct Config { prompt: P = { a: 1; sub: { x: 2; deep: { z: 3; }; }; }; };"#,
            )
            .unwrap();

        let ConfigValue::Section(config) = session.section("Config").expect("Config") else {
            panic!("expected a section");
        };
        let Some(ConfigValue::Section(prompt)) = config.get("prompt") else {
            panic!("prompt missing: {config:?}");
        };
        assert_eq!(prompt.get("a"), Some(&ConfigValue::Int(1)));
        let Some(ConfigValue::Section(sub)) = prompt.get("sub") else {
            panic!("nested object literal `sub` missing from prompt: {prompt:?}");
        };
        assert_eq!(sub.get("x"), Some(&ConfigValue::Int(2)));
        let Some(ConfigValue::Section(deep)) = sub.get("deep") else {
            panic!("doubly nested literal missing: {sub:?}");
        };
        assert_eq!(deep.get("z"), Some(&ConfigValue::Int(3)));
        assert!(
            session.section("sub").is_none() && session.section("deep").is_none(),
            "nested object literals must not leak out as root sections"
        );
    }

    #[test]
    fn interactive_eval_accepts_missing_final_statement_terminators() {
        let mut session = crate::Engine::default().session();

        session
            .eval_interactive(r#"var name: str = "OCC""#)
            .unwrap();
        assert_eq!(session.value("name"), Some(&ConfigValue::Str("OCC".into())));

        session
            .eval_interactive("function greet(name: str) -> str { return name; }")
            .unwrap();
        assert!(session.has_function("greet"));
        assert_eq!(
            session
                .eval_interactive(r#"greet(name: "Sparsh")"#)
                .unwrap(),
            InteractiveEvalResult::Value(ConfigValue::Str("Sparsh".into()))
        );
    }

    #[test]
    fn session_exposes_function_names_and_parameter_names_for_interactive_clients() {
        let mut session = crate::Engine::default().session();
        session
            .eval_interactive("function create(name: str, path: str) -> str { return name; }")
            .unwrap();

        assert!(session.function_names().any(|name| name == "create"));
        assert_eq!(
            session.function_parameters("create"),
            Some(&["name".to_string(), "path".to_string()][..])
        );
    }

    #[test]
    fn input_completeness_distinguishes_finished_input_from_structural_eof() {
        assert_eq!(input_completeness("build()"), InputCompleteness::Complete);
        assert_eq!(
            input_completeness("var name: str = \"OCC\""),
            InputCompleteness::Complete
        );
        assert_eq!(
            input_completeness("function build() -> int { return 1; }"),
            InputCompleteness::Complete
        );

        assert_eq!(input_completeness("build("), InputCompleteness::Incomplete);
        assert_eq!(
            input_completeness("function build() -> int {"),
            InputCompleteness::Incomplete
        );
        assert_eq!(
            input_completeness("var files: [str] = ["),
            InputCompleteness::Incomplete
        );
        assert_eq!(
            input_completeness("var name: str = \"OCC"),
            InputCompleteness::Incomplete
        );
    }

    #[test]
    fn input_completeness_submits_finished_syntax_errors_for_normal_diagnostics() {
        assert_eq!(
            input_completeness("var broken: = 1;"),
            InputCompleteness::Complete
        );
    }

    #[test]
    fn session_keeps_mutation_and_rejects_failed_fragment_atomically() {
        let mut session = Engine::default().session();
        session.eval("var mut count: int = 1;").unwrap();
        session.eval("count = count + 1;").unwrap();
        assert!(session.eval("count = \"bad\";").is_err());
        assert_eq!(session.value("count"), Some(&ConfigValue::Int(2)));
    }

    #[test]
    fn session_sees_declarations_from_earlier_fragments() {
        let mut session = Engine::default().session();
        session
            .eval("function double(x: int) -> int { return x * 2; };")
            .unwrap();
        session.eval("var y: int = double(x: 21);").unwrap();
        assert_eq!(session.value("y"), Some(&ConfigValue::Int(42)));
    }

    #[test]
    fn session_carries_registered_hosts_across_fragments() {
        let mut hosts = HostRegistry::new();
        hosts
            .register(HostFunction::new(
                "math",
                "square",
                vec![("n", crate::ast::SparType::Int)],
                crate::ast::SparType::Int,
                |args| match &args[0] {
                    ConfigValue::Int(n) => Ok(ConfigValue::Int(n * n)),
                    _ => Err("expected int".to_string()),
                },
            ))
            .unwrap();
        let mut session = Engine::default().with_hosts(hosts).session();
        session.eval("var x: int = math::square(n: 6);").unwrap();
        assert_eq!(session.value("x"), Some(&ConfigValue::Int(36)));
    }

    #[test]
    fn interactive_eval_returns_direct_expression_value() {
        let mut session = Engine::default().session();
        session
            .eval("function answer() -> int { return 42; };")
            .unwrap();
        assert_eq!(
            session.eval_interactive("answer();").unwrap(),
            InteractiveEvalResult::Value(ConfigValue::Int(42))
        );
    }

    #[test]
    fn interactive_assignment_returns_empty_and_persists_value() {
        let mut session = Engine::default().session();
        assert_eq!(
            session.eval_interactive("var value: int = 7;").unwrap(),
            InteractiveEvalResult::Empty
        );
        assert_eq!(session.value("value"), Some(&ConfigValue::Int(7)));
    }

    #[test]
    fn exec_shell_only_runs_once_across_session_replays() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("effect.txt");
        let mut session = Engine::default().session();
        let src = format!(
            r#"function bump() -> int {{
                var r = exec shell {{ printf x >> {:?}; }};
                return 0;
            }};
            var mut triggered: int = bump();"#,
            path.to_string_lossy()
        );

        session.eval(&src).unwrap();
        session.eval("var mut count: int = 1;").unwrap();
        session.eval("count = count + 1;").unwrap();

        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "x",
            "exec shell must only actually run once, not once per replay"
        );
    }

    #[test]
    fn interactive_external_command_is_evaluated_without_module_parse_error() {
        let session = Engine::default().session();

        let plan = session.eval_shell_plan("ls").unwrap();

        let spar_command::Step::Command(command) = &plan.steps[0].1 else {
            panic!("expected command")
        };
        assert_eq!(command.program, "ls");
        assert!(command.args.is_empty());
    }

    #[test]
    fn transient_shell_plan_uses_legal_collision_safe_internal_name() {
        let mut session = Engine::default().session();
        session
            .eval("var sparshInteractiveShellPlan: int = 7;")
            .unwrap();

        let plan = session.eval_shell_plan("ls").unwrap();

        let spar_command::Step::Command(command) = &plan.steps[0].1 else {
            panic!("expected command")
        };
        assert_eq!(command.program, "ls");
        assert_eq!(
            session.value("sparshInteractiveShellPlan"),
            Some(&ConfigValue::Int(7))
        );
    }

    #[test]
    fn transient_shell_plan_sees_persistent_spar_values_without_committing_command_source() {
        let mut session = Engine::default().session();
        session.eval("var name: str = \"OCC\";").unwrap();
        let before = session.committed_source().to_string();

        let plan = session.eval_shell_plan("echo ${name}").unwrap();

        let spar_command::Step::Command(command) = &plan.steps[0].1 else {
            panic!("expected command")
        };
        assert_eq!(command.args, ["OCC"]);
        assert_eq!(session.committed_source(), before);
    }

    #[test]
    fn contextual_shell_plan_uses_caller_environment_and_cwd() {
        let session = Engine::default().session();
        let temp = tempfile::tempdir().unwrap();
        let mut environment = std::env::vars_os().collect::<Vec<_>>();
        environment.retain(|(key, _)| key != "SPARSH_TEST_NAME");
        environment.push((
            OsString::from("SPARSH_TEST_NAME"),
            OsString::from("session-value"),
        ));

        let plan = session
            .eval_shell_plan_with_context(
                "echo $SPARSH_TEST_NAME $(pwd)",
                temp.path(),
                &environment,
            )
            .unwrap();

        let spar_command::Step::Command(command) = &plan.steps[0].1 else {
            panic!("expected command")
        };
        assert_eq!(command.args[0], "session-value");
        assert_eq!(command.args[1], temp.path().to_string_lossy().as_ref());
    }

    #[test]
    fn session_can_import_from_embedding_registered_package() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sparsh");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("config.spar"),
            "function answer() -> int { return 42; };\n",
        )
        .unwrap();

        let engine = Engine::default()
            .with_bundled_package_root("sparsh", root)
            .unwrap();
        let mut session = engine.session();
        session
            .eval(r#"import pkg { answer } from "sparsh/config"; var result: int = answer();"#)
            .unwrap();

        assert_eq!(session.value("result"), Some(&ConfigValue::Int(42)));
    }

    #[test]
    fn interactive_runtime_preview_returns_tables_without_forcing_config_serialization() {
        let mut session = Engine::default().session();
        let cwd = std::env::current_dir().unwrap();
        let environment = std::env::vars_os().collect::<Vec<_>>();
        let result = session
            .eval_interactive_preview_with_context(
                r#"
                import pkg { collectTable } from "std/data";
                struct User { name: str = ""; age: int = 0; };
                var users: [User] = [User(name: "Obi", age: 24), User(name: "Ada", age: 31)];
                users |> collectTable()
                "#,
                &cwd,
                &environment,
                None,
                20,
            )
            .expect("table preview should execute through the compiled runtime");

        let InteractivePreviewResult::RuntimeValue(preview) = result else {
            panic!("expected runtime preview value");
        };
        let Value::Table(table) = preview.value else {
            panic!("expected Table value");
        };
        assert_eq!(table.len(), 2);
        assert!(!preview.stream_preview);
        assert!(!preview.truncated);
    }

    fn mixed_preview(
        session: &mut Session,
        source: &str,
    ) -> Result<InteractivePreviewResult, Vec<SparError>> {
        let cwd = std::env::current_dir().unwrap();
        let environment = std::env::vars_os().collect::<Vec<_>>();
        session.eval_interactive_preview_with_context(source, &cwd, &environment, None, 20)
    }

    #[test]
    fn interactive_shell_preview_accepts_raw_mixed_pipeline_without_to() {
        let mut session = Engine::default().session();
        session.set_structured_terminal(true);
        session
            .eval(r#"import pkg { where } from "std/data";"#)
            .unwrap();
        let cwd = std::env::current_dir().unwrap();
        let environment = std::env::vars_os().collect::<Vec<_>>();

        let result = session
            .eval_interactive_shell_preview_with_context(
                "printf 'name,age,team\\nObi,24,core\\nAda,31,ops\\n' | from csv |> where(fn(row) => row.age > 20)",
                &cwd,
                &environment,
                None,
                20,
            )
            .expect("raw mixed pipeline should execute");

        let InteractivePreviewResult::RuntimeValue(preview) = result else {
            panic!("expected structured runtime value, got {result:?}");
        };
        assert_eq!(preview.presentation, InteractivePresentation::Pipeline);
        let Value::Table(table) = preview.value else {
            panic!("expected table");
        };
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn interactive_shell_preview_preserves_terminal_encoder_presentation() {
        let mut session = Engine::default().session();
        session.set_structured_terminal(true);
        let cwd = std::env::current_dir().unwrap();
        let environment = std::env::vars_os().collect::<Vec<_>>();

        let result = session
            .eval_interactive_shell_preview_with_context(
                "printf 'name,age\\nObi,24\\nAda,31\\n' | from csv |> to json",
                &cwd,
                &environment,
                None,
                20,
            )
            .unwrap();

        let InteractivePreviewResult::RuntimeValue(preview) = result else {
            panic!("expected encoded structured result, got {result:?}");
        };
        assert_eq!(
            preview.presentation,
            InteractivePresentation::Encoded("json")
        );
        assert!(matches!(preview.value, Value::Table(_)));
    }

    #[test]
    fn interactive_shell_preview_keeps_downstream_and_redirected_encoders_as_processes() {
        let mut session = Engine::default().session();
        session.set_structured_terminal(true);
        let cwd = std::env::current_dir().unwrap();
        let environment = std::env::vars_os().collect::<Vec<_>>();

        for body in [
            "printf 'n\\n1\\n' | from csv |> to json | cat > /dev/null",
            "printf 'n\\n1\\n' | from csv |> to json > /dev/null",
        ] {
            let result = session
                .eval_interactive_shell_preview_with_context(body, &cwd, &environment, None, 20)
                .unwrap();
            assert!(
                matches!(result, InteractivePreviewResult::Process(_)),
                "{body}: {result:?}"
            );
        }
    }

    #[test]
    fn interactive_shell_error_reports_the_inner_failure() {
        let mut session = Engine::default().session();
        session.set_structured_terminal(true);
        session
            .eval(r#"import pkg { where } from "std/data";"#)
            .unwrap();
        let cwd = std::env::current_dir().unwrap();
        let environment = std::env::vars_os().collect::<Vec<_>>();

        let errors = session
            .eval_interactive_shell_preview_with_context(
                "printf 'age\\n24\\n' | from csv |> where(fn(row) => row.age > )",
                &cwd,
                &environment,
                None,
                20,
            )
            .unwrap_err();
        let rendered = errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!rendered.contains("start a declaration"), "{rendered}");
    }

    const WHERE_IMPORT: &str = "import pkg { where } from \"std/data\";";
    const CSV_SOURCE: &str = "printf 'name,age\\nObi,24\\nAda,31\\nZed,12\\n'";

    #[test]
    fn terminal_mixed_pipeline_without_to_returns_the_whole_table() {
        let mut session = Engine::default().session();
        session.set_structured_terminal(true);
        session.eval(WHERE_IMPORT).unwrap();
        let result = mixed_preview(
            &mut session,
            &format!("shell {{ {CSV_SOURCE} | from csv |> where(fn(r) => r.age > 20); }}"),
        )
        .expect("mixed pipeline without `to` should run");

        let InteractivePreviewResult::RuntimeValue(preview) = result else {
            panic!("expected a structured value, got {result:?}");
        };
        assert_eq!(preview.presentation, InteractivePresentation::Pipeline);
        assert!(!preview.truncated);
        let Value::Table(table) = preview.value else {
            panic!("expected a Table");
        };
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn terminal_mixed_pipeline_with_to_keeps_the_format_and_the_full_value() {
        let mut session = Engine::default().session();
        session.set_structured_terminal(true);
        let result = mixed_preview(
            &mut session,
            &format!("shell {{ {CSV_SOURCE} | from csv |> to json; }}"),
        )
        .unwrap();

        let InteractivePreviewResult::RuntimeValue(preview) = result else {
            panic!("expected a structured value, got {result:?}");
        };
        assert_eq!(
            preview.presentation,
            InteractivePresentation::Encoded("json")
        );
        let Value::Table(table) = preview.value else {
            panic!("expected a Table");
        };
        assert_eq!(table.len(), 3);
    }

    #[test]
    fn mixed_pipeline_still_writes_bytes_when_something_consumes_them_or_no_terminal() {
        let mut terminal = Engine::default().session();
        terminal.set_structured_terminal(true);
        for source in [
            format!("shell {{ {CSV_SOURCE} | from csv |> to json | cat > /dev/null; }}"),
            format!("shell {{ {CSV_SOURCE} | from csv |> to json > /dev/null; }}"),
        ] {
            let result = mixed_preview(&mut terminal, &source).unwrap();
            assert!(
                matches!(result, InteractivePreviewResult::Process(_)),
                "{source}: {result:?}"
            );
        }

        let mut plain = Engine::default().session();
        plain.eval(WHERE_IMPORT).unwrap();
        let result = mixed_preview(
            &mut plain,
            &format!("shell {{ {CSV_SOURCE} | from csv |> where(fn(r) => r.age > 20); }}"),
        );
        // No terminal and no `to`: falls back to JSON Lines bytes.
        assert!(
            matches!(result, Ok(InteractivePreviewResult::Process(_))),
            "{result:?}"
        );
    }

    #[test]
    fn mixed_pipeline_rejects_a_unix_pipe_without_to() {
        let mut session = Engine::default().session();
        session.eval(WHERE_IMPORT).unwrap();
        let result = mixed_preview(
            &mut session,
            &format!("shell {{ {CSV_SOURCE} | from csv |> where(fn(r) => r.age > 20) | cat; }}"),
        );
        assert!(result.is_err(), "{result:?}");
    }

    #[test]
    fn data_prelude_makes_bare_where_work_without_an_import() {
        let mut session = Engine::default().session();
        session.set_structured_terminal(true);
        session.enable_data_prelude();
        let result = mixed_preview(
            &mut session,
            &format!("shell {{ {CSV_SOURCE} | from csv |> where(fn(r) => r.age > 20); }}"),
        )
        .expect("`where` should resolve through the prelude");
        assert!(
            matches!(result, InteractivePreviewResult::RuntimeValue(_)),
            "{result:?}"
        );
    }

    #[test]
    fn data_prelude_skips_names_the_session_already_defines() {
        let mut session = Engine::default().session();
        session
            .eval("function count() -> int { return 7; };")
            .unwrap();
        session.enable_data_prelude();
        // The user's own `count` survives; the rest are imported.
        session.eval("var n: int = count();").unwrap();
        assert_eq!(session.value("n"), Some(&ConfigValue::Int(7)));
        session
            .eval(r#"import pkg { take } from "std/data";"#)
            .expect("an explicit import after the prelude must not collide");
        // Running it twice is harmless.
        session.enable_data_prelude();
    }

    #[test]
    fn user_declaration_may_reuse_a_prelude_name() {
        let mut session = Engine::default().session();
        session.enable_data_prelude();
        session
            .eval("var mut count: int = 0;")
            .expect("declaring `count` after the prelude must still work");
        session.eval("count = count + 1;").unwrap();
        assert_eq!(session.value("count"), Some(&ConfigValue::Int(1)));
        session
            .eval("function first() -> int { return 9; };")
            .expect("so must defining `first`");
    }

    /// The text an error's span covers in `source`.
    fn covered<'a>(error: &SparError, source: &'a str) -> &'a str {
        let span = error.span();
        &source[span.start..span.end]
    }

    fn session_with_history() -> Session {
        let mut session = Engine::default().session();
        session
            .eval("var first: int = 1;\nvar second: int = 2;\nvar third: int = 3;")
            .unwrap();
        session
    }

    #[test]
    fn declaration_errors_point_into_the_typed_fragment_not_the_session() {
        let mut session = session_with_history();
        let fragment = "var ok: int = 1;\nvar bad: int = nowhere;";
        let errors = session.eval_interactive(fragment).unwrap_err();
        assert_eq!(covered(&errors[0], fragment), "nowhere", "{errors:?}");
        assert_eq!((errors[0].span().line, errors[0].span().col), (2, 16));
    }

    #[test]
    fn expression_errors_point_at_the_typed_expression() {
        let mut session = session_with_history();
        let cwd = std::env::current_dir().unwrap();
        let fragment = "var ok: int = 1;\n  nowhere(1)";
        let errors = session
            .eval_interactive_preview_with_context(fragment, &cwd, &[], None, 20)
            .unwrap_err();
        assert_eq!(covered(&errors[0], fragment), "nowhere", "{errors:?}");
        assert_eq!((errors[0].span().line, errors[0].span().col), (2, 3));
    }

    #[test]
    fn mixed_pipeline_errors_point_at_the_typed_stage() {
        let mut session = session_with_history();
        let cwd = std::env::current_dir().unwrap();
        let body = "  printf x | from csv |> where(fn(r) => r.age > 20)";
        let errors = session
            .eval_interactive_shell_preview_with_context(body, &cwd, &[], None, 20)
            .unwrap_err();
        // Spans are relative to the trimmed body.
        assert_eq!(covered(&errors[0], body.trim()), "where", "{errors:?}");
        assert_eq!(errors[0].span().line, 1);
    }

    #[test]
    fn command_line_errors_point_at_the_typed_command() {
        let session = session_with_history();
        let cwd = std::env::current_dir().unwrap();
        let line = "echo ${nowhere}";
        let errors = session
            .eval_shell_plan_with_context(line, &cwd, &[])
            .unwrap_err();
        assert_eq!(covered(&errors[0], line), "nowhere", "{errors:?}");
        assert_eq!(errors[0].span().line, 1);
    }

    #[test]
    fn prelude_functions_are_reported_to_interactive_clients_until_shadowed() {
        let mut session = Engine::default().session();
        assert!(!session.has_function("where"));

        session.enable_data_prelude();
        assert!(session.has_function("where"));
        assert!(session.function_names().any(|name| name == "where"));
        assert!(session.identifiers().any(|name| name == "where"));
        assert_eq!(
            session.function_parameters("where"),
            Some(&["source".to_string(), "predicate".to_string()][..])
        );

        // Once the user declares the name themselves, the prelude steps aside.
        session.eval("var mut count: int = 0;").unwrap();
        assert!(!session.has_function("count"));
        assert!(session.has_function("take"));
    }

    fn async_session() -> Session {
        let mut session = Engine::default().session();
        session
            .eval(
                "async function double(value: int) -> int { return value * 2; };\n\
                 async function boom() -> int { return 1 / 0; };",
            )
            .unwrap();
        session
    }

    fn preview_value(session: &mut Session, source: &str) -> Result<ConfigValue, Vec<SparError>> {
        let cwd = std::env::current_dir().unwrap();
        match session.eval_interactive_preview_with_context(source, &cwd, &[], None, 20)? {
            InteractivePreviewResult::Value(value) => Ok(value),
            InteractivePreviewResult::RuntimeValue(preview) => Ok(preview
                .value
                .try_into_config(&crate::Span::dummy())
                .unwrap()),
            other => panic!("unexpected result {other:?}"),
        }
    }

    #[test]
    fn an_async_call_without_await_still_shows_a_promise() {
        let mut session = async_session();
        let value = preview_value(&mut session, "double(value: 21)").unwrap();
        assert!(matches!(value, ConfigValue::Promise(_)), "{value:?}");
    }

    #[test]
    fn await_at_the_prompt_waits_for_the_promise() {
        let mut session = async_session();
        assert_eq!(
            preview_value(&mut session, "await double(value: 21)").unwrap(),
            ConfigValue::Int(42)
        );
        assert_eq!(
            preview_value(&mut session, "(await double(value: 20)) + 2").unwrap(),
            ConfigValue::Int(42)
        );
        assert_eq!(
            preview_value(
                &mut session,
                "await double(value: 1) + await double(value: 2)"
            )
            .unwrap(),
            ConfigValue::Int(6)
        );
        // Declarations before the awaited expression still commit.
        assert_eq!(
            preview_value(
                &mut session,
                "var base: int = 5;\nawait double(value: base)"
            )
            .unwrap(),
            ConfigValue::Int(10)
        );
        assert_eq!(session.value("base"), Some(&ConfigValue::Int(5)));
    }

    #[test]
    fn await_reports_failures_and_non_promises_at_the_awaited_text() {
        let mut session = async_session();

        let source = "await boom()";
        let errors = preview_value(&mut session, source).unwrap_err();
        assert!(errors[0].to_string().contains("division"), "{errors:?}");

        let source = "await 5";
        let errors = preview_value(&mut session, source).unwrap_err();
        assert!(
            errors[0].to_string().contains("cannot await `int`"),
            "{errors:?}"
        );
        assert_eq!(covered(&errors[0], source), "await");
    }

    #[test]
    fn an_undeclared_name_in_a_command_is_undefined_not_cyclic() {
        let session = session_with_history();
        let cwd = std::env::current_dir().unwrap();
        let errors = session
            .eval_shell_plan_with_context("echo ${nowhere}", &cwd, &[])
            .unwrap_err();
        let message = errors[0].to_string();
        assert!(
            message.contains("undefined reference: `nowhere`"),
            "{message}"
        );
        assert!(!message.contains("cyclic"), "{message}");
    }

    #[test]
    fn a_real_cycle_is_still_reported_as_cyclic() {
        let mut session = Engine::default().session();
        let errors = session
            .eval("var a: int = b;\nvar b: int = a;")
            .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.to_string().contains("cyclic")),
            "{errors:?}"
        );
    }

    #[test]
    fn piping_a_record_into_a_sequence_stage_explains_what_is_needed() {
        let mut session = Engine::default().session();
        session.enable_data_prelude();
        let cwd = std::env::current_dir().unwrap();
        let record = crate::runtime::Value::Object(
            [("id".to_string(), crate::runtime::Value::Int(1))]
                .into_iter()
                .collect(),
        );
        let source = "_ |> take(1)";
        let errors = session
            .eval_interactive_preview_with_context(source, &cwd, &[], Some(record), 20)
            .unwrap_err();
        let message = errors[0].to_string();
        assert!(
            message.contains("`take` needs a list, table or stream, but got a record"),
            "{message}"
        );
        assert!(!message.contains("internal runtime error"), "{message}");
        // The error comes from inside the std module, so it is pinned to the
        // start of the typed line instead of pointing into that module.
        assert_eq!(errors[0].span().line, 1);
        assert!(errors[0].span().end <= source.len(), "{errors:?}");
    }

    #[test]
    fn missing_data_import_error_says_what_to_import() {
        let mut session = Engine::default().session();
        let error = mixed_preview(
            &mut session,
            &format!("shell {{ {CSV_SOURCE} | from csv |> where(fn(r) => r.age > 20); }}"),
        )
        .unwrap_err();
        let SparError::TypeError { hint, .. } = &error[0] else {
            panic!("expected a type error, got {:?}", error[0]);
        };
        assert_eq!(
            hint.as_deref(),
            Some("import it first: import pkg { where } from \"std/data\";")
        );
    }

    #[test]
    fn interactive_runtime_preview_can_pipe_the_previous_value_through_underscore() {
        let mut session = Engine::default().session();
        let cwd = std::env::current_dir().unwrap();
        let environment = std::env::vars_os().collect::<Vec<_>>();
        let first = session
            .eval_interactive_preview_with_context(
                r#"import pkg { take } from "std/data"; var numbers: [int] = [1, 2, 3]; numbers |> take(3)"#,
                &cwd,
                &environment,
                None,
                20,
            )
            .unwrap();
        let previous = match first {
            InteractivePreviewResult::Value(value) => Value::from_config(value),
            InteractivePreviewResult::RuntimeValue(value) => value.value,
            other => panic!("expected a value, found {other:?}"),
        };
        let second = session
            .eval_interactive_preview_with_context(
                "_ |> take(2)",
                &cwd,
                &environment,
                Some(previous),
                20,
            )
            .unwrap();
        match second {
            InteractivePreviewResult::Value(ConfigValue::List(values)) => {
                assert_eq!(values, vec![ConfigValue::Int(1), ConfigValue::Int(2)]);
            }
            InteractivePreviewResult::RuntimeValue(value) => {
                assert_eq!(value.value, Value::List(vec![Value::Int(1), Value::Int(2)]));
            }
            other => panic!("expected piped previous value, found {other:?}"),
        }
    }

    #[test]
    fn interactive_schema_and_inspect_requests_preserve_presentation_intent() {
        let mut session = Engine::default().session();
        let cwd = std::env::current_dir().unwrap();
        let environment = std::env::vars_os().collect::<Vec<_>>();
        let prefix = r#"
            import pkg { schema, inspect, collectTable } from "std/data";
            struct User { name: str = ""; age: int = 0; };
            var users: [User] = [User(name: "Obi", age: 24), User(name: "Ada", age: 31)];
        "#;
        session.eval(prefix).unwrap();

        let schema = session
            .eval_interactive_preview_with_context(
                "users |> collectTable() |> schema()",
                &cwd,
                &environment,
                None,
                20,
            )
            .unwrap();
        let InteractivePreviewResult::RuntimeValue(schema) = schema else {
            panic!("schema should stay a runtime value for dedicated presentation");
        };
        assert_eq!(schema.presentation, InteractivePresentation::Schema);
        assert!(matches!(schema.value, Value::Schema(_)));

        let inspect = session
            .eval_interactive_preview_with_context(
                "users |> inspect()",
                &cwd,
                &environment,
                None,
                20,
            )
            .unwrap();
        let InteractivePreviewResult::RuntimeValue(inspect) = inspect else {
            panic!("inspect should stay a runtime value for dedicated presentation");
        };
        assert_eq!(inspect.presentation, InteractivePresentation::Inspect);
        assert_eq!(inspect.value.type_name(), "list");
    }

    #[test]
    fn structured_pipe_completeness_remains_parser_driven() {
        assert_eq!(
            input_completeness("users |>"),
            InputCompleteness::Incomplete
        );
        assert_eq!(
            input_completeness("users |> take(2)"),
            InputCompleteness::Complete
        );
    }

    #[test]
    fn identifier_snapshot_tracks_functions_and_globals() {
        let mut session = Engine::default().session();
        session
            .eval("var project: str = \"spar\"; function build(name: str) -> str { return name; };")
            .unwrap();

        let names = session.identifiers().collect::<Vec<_>>();
        assert!(names.contains(&"project"));
        assert!(names.contains(&"build"));
        assert!(session.has_function("build"));
    }
}
