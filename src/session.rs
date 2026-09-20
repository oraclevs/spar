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
    crate::parser::Parser::new(tokens).parse().map(|_| ())
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

pub struct Session {
    options: CompileOptions,
    committed_source: String,
    globals: HashMap<String, ConfigValue>,
    sections: HashMap<Vec<String>, HashMap<String, ConfigValue>>,
    identifiers: BTreeSet<String>,
    functions: BTreeSet<String>,
    function_params: BTreeMap<String, Vec<String>>,
}

struct EvaluatedCandidate {
    source: String,
    globals: HashMap<String, ConfigValue>,
    sections: HashMap<Vec<String>, HashMap<String, ConfigValue>>,
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
    pub fn eval_transient(
        &self,
        fragment: &str,
    ) -> Result<InteractiveEvalResult, Vec<SparError>> {
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
        let fragment = format!(
            "var {name}: shell = shell {{\n{terminated}\n}};"
        );
        let mut evaluated =
            self.evaluate_candidate(&fragment, Some(runtime_context(cwd, environment)))?;
        match evaluated.globals.remove(&name) {
            Some(ConfigValue::Shell(plan)) => Ok(plan),
            _ => Err(vec![SparError::EvalError {
                message: "interactive command did not evaluate to a shell plan".into(),
                span: crate::Span::dummy(),
            }]),
        }
    }

    fn evaluate_candidate(
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
        let compilation = Compiler::new(options.clone()).compile(&candidate).into_result()?;
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
            Some(context) => crate::evaluator::Evaluator::evaluate_with_imports_base_effects_natives_and_context(
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
            )?,
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
    pub fn section(&self, name: &str) -> Option<ConfigValue> {
        self.sections
            .get(&vec![name.to_string()])
            .cloned()
            .map(ConfigValue::Section)
    }


    /// Names currently visible to an interactive client for completion.
    pub fn identifiers(&self) -> impl Iterator<Item = &str> {
        self.identifiers.iter().map(String::as_str)
    }

    pub fn has_function(&self, name: &str) -> bool {
        self.functions.contains(name)
    }

    /// Function names visible to interactive embedders such as Sparsh.
    pub fn function_names(&self) -> impl Iterator<Item = &str> {
        self.functions.iter().map(String::as_str)
    }

    /// Named parameter labels for an interactive function call.
    pub fn function_parameters(&self, name: &str) -> Option<&[String]> {
        self.function_params.get(name).map(Vec::as_slice)
    }
    /// Every fragment committed so far, concatenated — mainly useful for
    /// diagnostics/debugging a session, not for driving further logic.
    pub fn committed_source(&self) -> &str {
        &self.committed_source
    }
}

fn runtime_context(cwd: &Path, environment: &[(OsString, OsString)]) -> crate::runtime::RuntimeContext {
    let mut context = crate::runtime::RuntimeContext::new(cwd.to_path_buf());
    context.replace_environment(environment.iter().filter_map(|(key, value)| {
        Some((key.to_str()?.to_string(), value.to_str()?.to_string()))
    }));
    context
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::host::{HostFunction, HostRegistry};

    #[test]
    fn interactive_eval_accepts_missing_final_statement_terminators() {
        let mut session = crate::Engine::default().session();

        session.eval_interactive(r#"var name: str = "OCC""#).unwrap();
        assert_eq!(session.value("name"), Some(&ConfigValue::Str("OCC".into())));

        session
            .eval_interactive("function greet(name: str) -> str { return name; }")
            .unwrap();
        assert!(session.has_function("greet"));
        assert_eq!(
            session.eval_interactive(r#"greet(name: "Sparsh")"#).unwrap(),
            InteractiveEvalResult::Value(ConfigValue::Str("Sparsh".into()))
        );
    }

    #[test]
    fn session_exposes_function_names_and_parameter_names_for_interactive_clients() {
        let mut session = crate::Engine::default().session();
        session
            .eval_interactive(
                "function create(name: str, path: str) -> str { return name; }",
            )
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
            .eval(
                r#"import pkg { answer } from "sparsh/config"; var result: int = answer();"#,
            )
            .unwrap();

        assert_eq!(session.value("result"), Some(&ConfigValue::Int(42)));
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
