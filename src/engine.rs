//! Explicit Check / Emit / Execute surfaces over the compiler pipeline.
//!
//! `Compiler::compile` (see `compiler.rs`) already implements Check
//! (`evaluate: false`) and Emit (`evaluate: true`) — this module adds
//! Execute, the one mode that requires a declared `main` and actually
//! calls it, and gives all three a single, documented entry point so a
//! caller doesn't have to know `Compiler`'s internals to pick the right
//! mode.
//!
//! Task remains a separate surface entirely (`task_lowering.rs` /
//! `runner/`), unaffected by any of this.

use std::path::Path;

use crate::ast::{Program, TopLevelItem};
use crate::compiled::CompiledProgram;
use crate::compiler::{validate_entry_signature, Compilation, CompileOptions, Compiler};
use crate::error::{Span, SparError};
use crate::evaluator::Evaluator;
use crate::runtime::Value;

/// The result of running Execute mode's `main` to completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionOutcome {
    /// `main() -> int`'s returned value, or `0` for `main() -> void`
    /// completing without a runtime error.
    pub exit_status: i32,
}

#[derive(Clone, Debug, Default)]
pub struct Engine {
    options: CompileOptions,
}

impl Engine {
    pub fn new(options: CompileOptions) -> Self {
        Self { options }
    }

    /// Registers native functions this engine's `ns::fn(...)` calls may
    /// dispatch to, in every mode (Check for name/type checking, Emit and
    /// Execute for actually calling them).
    pub fn with_hosts(mut self, hosts: crate::host::HostRegistry) -> Self {
        self.options.hosts = hosts;
        self
    }

    /// Sets the source base directory used for local imports in this engine
    /// and sessions created from it. Embedders use this for rc/source files
    /// whose imports must resolve relative to the file rather than process cwd.
    pub fn with_base_dir(mut self, base_dir: impl Into<std::path::PathBuf>) -> Self {
        self.options.base_dir = base_dir.into();
        self
    }

    /// Registers a source-backed package that is available to explicit
    /// `import pkg` requests in every compiler/session mode.
    pub fn with_bundled_package_root(
        mut self,
        name: impl Into<String>,
        root: impl Into<std::path::PathBuf>,
    ) -> Result<Self, String> {
        self.options.bundled_packages.register(name, root)?;
        Ok(self)
    }

    /// Routes explicit `import pkg` requests through `locator`'s lockfile and
    /// store, exactly as the `spar` CLI does for a project directory.
    pub fn with_locator(mut self, locator: crate::package::ModuleLocator) -> Self {
        self.options.locator = Some(locator);
        self
    }

    /// Sets the command name used in package-import hints (default `spar`).
    pub fn with_package_command(mut self, name: impl Into<String>) -> Self {
        self.options.package_command = name.into();
        self
    }

    /// A persistent, incrementally-evaluated session over this engine's
    /// hosts and options — see `session::Session`.
    pub fn session(&self) -> crate::session::Session {
        crate::session::Session::new(self.options.clone())
    }

    /// Compile and type-check source without evaluating it.
    pub fn compile_source(&self, source: &str) -> Result<CompiledProgram, Vec<SparError>> {
        let options = CompileOptions {
            evaluate: false,
            ..self.options.clone()
        };
        let compilation = Compiler::new(options.clone()).compile(source);
        CompiledProgram::from_compilation(compilation, options)
    }

    pub fn compile_path(&self, path: &Path) -> Result<CompiledProgram, Vec<SparError>> {
        self.with_path(path).compile_source(&read_source(path)?)
    }

    /// Check mode: lex, parse, resolve, and type-check only. Never
    /// evaluates the module (so it can never call `main` or run any other
    /// top-level side effect) — safe for LSP/CI use on arbitrary source.
    pub fn check_source(&self, source: &str) -> Result<(), Vec<SparError>> {
        self.check_compile(source)?;
        Ok(())
    }

    pub fn check_path(&self, path: &Path) -> Result<(), Vec<SparError>> {
        self.with_path(path).check_source(&read_source(path)?)
    }

    /// Emit mode: also evaluates the module's configuration surface (globals
    /// and sections) and lowers any tasks, exactly like `spar emit` today.
    /// Still never calls `main` — nothing auto-invokes a top-level function
    /// by name outside of Execute mode.
    pub fn emit_source(&self, source: &str) -> Compilation {
        match self.compile_source(source) {
            Ok(program) => self
                .emit_compiled(&program)
                .unwrap_or_else(|errors| program.compilation_with_errors(errors)),
            Err(_) => Compiler::new(CompileOptions {
                evaluate: true,
                ..self.options.clone()
            })
            .compile(source),
        }
    }

    pub fn emit_path(&self, path: &Path) -> Result<Compilation, Vec<SparError>> {
        Ok(self.with_path(path).emit_source(&read_source(path)?))
    }

    /// Execute mode: Check, then require a valid zero-argument `main`
    /// returning `int`, `void`, or `shell`, evaluate the module exactly once, call
    /// `main`, and translate its result into a process exit status.
    pub fn execute_source(&self, source: &str) -> Result<ExecutionOutcome, Vec<SparError>> {
        let program = self.compile_source(source)?;
        self.execute_compiled(&program)
    }

    pub fn emit_compiled(&self, program: &CompiledProgram) -> Result<Compilation, Vec<SparError>> {
        let entry = program
            .modules
            .get(program.entry.0 as usize)
            .ok_or_else(|| {
                vec![SparError::EvalError {
                    message: "internal runtime error: entry module is unavailable".into(),
                    span: Span::dummy(),
                }]
            })?;
        let result = Evaluator::evaluate_with_imports_base_effects_and_natives(
            &entry.checked.program,
            &entry.checked.symbols,
            &entry.checked.imports,
            &program.options.base_dir,
            program.options.hosts.clone(),
            program.options.natives.clone(),
            program.options.effect_ledger.clone(),
        )?;
        let mut task_exprs = Vec::new();
        let tasks = crate::task_lowering::lower_tasks(
            &entry.checked.program,
            &entry.checked.symbols,
            &result,
            &program.options.base_dir,
            &mut task_exprs,
        )?;
        Ok(Compilation {
            program: Some(entry.checked.program.clone()),
            symbols: Some(entry.checked.symbols.clone()),
            imports: entry.checked.imports.clone(),
            result: Some(result),
            tasks,
            task_exprs,
            errors: Vec::new(),
        })
    }

    pub fn execute_compiled(
        &self,
        program: &CompiledProgram,
    ) -> Result<ExecutionOutcome, Vec<SparError>> {
        self.execute_compiled_with_context(
            program,
            crate::runtime::RuntimeContext::for_base_dir(&program.options.base_dir),
        )
    }

    /// Execute an already-compiled program with caller-owned runtime state.
    /// Embedders and tests use this to isolate cwd, environment, arguments,
    /// and stdio without mutating process-global state.
    pub fn execute_compiled_with_context(
        &self,
        program: &CompiledProgram,
        context: crate::runtime::RuntimeContext,
    ) -> Result<ExecutionOutcome, Vec<SparError>> {
        let entry = program
            .modules
            .get(program.entry.0 as usize)
            .ok_or_else(|| {
                vec![SparError::EvalError {
                    message: "internal runtime error: entry module is unavailable".into(),
                    span: Span::dummy(),
                }]
            })?;
        require_entry_signature(&entry.checked.program).map_err(|error| vec![error])?;
        let result = crate::runtime::execute_program_with_context(program, context)?;

        let exit_status = match result {
            Value::Int(status) => status as i32,
            // `main() -> void` — the typechecker guarantees `main` never
            // returns anything else. Shell-valued entry points are executed
            // inside `runtime::execute_program` so they keep runtime state and
            // source-aware diagnostics instead of falling back to a second
            // process-execution path here.
            _ => 0,
        };
        Ok(ExecutionOutcome { exit_status })
    }

    pub fn execute_path(&self, path: &Path) -> Result<ExecutionOutcome, Vec<SparError>> {
        self.with_path(path).execute_source(&read_source(path)?)
    }

    fn check_compile(&self, source: &str) -> Result<Compilation, Vec<SparError>> {
        let options = CompileOptions {
            evaluate: false,
            ..self.options.clone()
        };
        Compiler::new(options).compile(source).into_result()
    }

    fn with_path(&self, path: &Path) -> Engine {
        Engine::new(CompileOptions {
            base_dir: path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
            source_path: Some(path.to_path_buf()),
            ..self.options.clone()
        })
    }
}

fn read_source(path: &Path) -> Result<String, Vec<SparError>> {
    std::fs::read_to_string(path).map_err(|error| {
        vec![SparError::EvalError {
            message: format!("could not read '{}': {error}", path.display()),
            span: Span::dummy(),
        }]
    })
}

/// `validate_entry_signature` plus presence — Execute mode is the one
/// place a missing `main` is itself an error (Check/Emit are fine without
/// one; a plain config/task file never declares `main` at all).
fn require_entry_signature(program: &Program) -> Result<(), SparError> {
    let has_main = program
        .items
        .iter()
        .any(|item| matches!(item, TopLevelItem::Function(f) if f.name == "main"));
    if !has_main {
        return Err(SparError::ResolveError {
            message: "no 'main' function found — Execute mode requires a zero-argument \
                       'main' returning 'int', 'void', or 'shell'"
                .into(),
            hint: None,
            span: Span::dummy(),
        });
    }
    validate_entry_signature(program)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::ConfigValue;
    use crate::host::{HostFunction, HostRegistry};

    #[test]
    fn registered_namespaced_host_function_is_typechecked_and_called() {
        let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded_in_closure = recorded.clone();
        let mut hosts = HostRegistry::new();
        hosts
            .register(HostFunction::new(
                "log",
                "write",
                vec![("message", crate::ast::SparType::Str)],
                crate::ast::SparType::Void,
                move |args| {
                    let ConfigValue::Str(message) = &args[0] else {
                        return Err("expected str".to_string());
                    };
                    recorded_in_closure.lock().unwrap().push(message.clone());
                    Ok(ConfigValue::Int(0))
                },
            ))
            .unwrap();

        Engine::default()
            .with_hosts(hosts)
            .execute_source("function main() -> void { log::write(message: \"hello\"); };")
            .expect("execute should succeed");

        assert_eq!(recorded.lock().unwrap().as_slice(), ["hello"]);
    }

    #[test]
    fn host_function_return_value_is_usable_by_a_caller() {
        let mut hosts = HostRegistry::new();
        hosts
            .register(HostFunction::new(
                "math",
                "answer",
                vec![],
                crate::ast::SparType::Int,
                |_| Ok(ConfigValue::Int(42)),
            ))
            .unwrap();

        let outcome = Engine::default()
            .with_hosts(hosts)
            .execute_source(
                "var result: int = math::answer(); function main() -> int { return result; };",
            )
            .expect("execute should succeed");
        assert_eq!(outcome.exit_status, 42);
    }

    #[test]
    fn check_never_evaluates_or_requires_main() {
        let engine = Engine::default();
        engine
            .check_source("var mut count: int = 0; count = count + 1;")
            .expect("check should accept a plain script with no main");
    }

    #[test]
    fn execute_returns_main_integer_status() {
        let outcome = Engine::default()
            .execute_source("function main() -> int { return 23; };")
            .expect("execute should succeed");
        assert_eq!(outcome.exit_status, 23);
    }

    #[test]
    fn execute_void_main_exits_zero() {
        let outcome = Engine::default()
            .execute_source("function main() -> void { };")
            .expect("execute should succeed");
        assert_eq!(outcome.exit_status, 0);
    }

    #[test]
    fn execute_runs_module_initialization_exactly_once_before_main() {
        let outcome = Engine::default()
            .execute_source(
                r#"
                var mut count: int = 0;
                count = count + 1;
                function main() -> int { return count; };
                "#,
            )
            .expect("execute should succeed");
        assert_eq!(outcome.exit_status, 1);
    }

    #[test]
    fn execute_without_main_is_a_clear_diagnostic() {
        let error = Engine::default()
            .execute_source("var x: int = 1;")
            .expect_err("execute should fail without main");
        let message = format!("{error:?}");
        assert!(message.contains("no 'main' function"), "{message}");
    }

    #[test]
    fn emit_and_check_never_call_main() {
        // If Emit/Check ever called `main`, this would fail typechecking
        // (`main` is declared with an unrelated return type but nothing
        // ever references it directly), proving neither mode invokes it.
        let src = "function main() -> int { return 1 / 0; };";
        Engine::default()
            .check_source(src)
            .expect("check must not evaluate, so a `main` that would panic at runtime is fine");
        let compilation = Engine::default().emit_source(src);
        assert!(
            compilation.is_ok(),
            "emit must not call main either: {:?}",
            compilation.errors
        );
    }

    #[test]
    fn exec_shell_true_reports_success() {
        let outcome = Engine::default()
            .execute_source(
                r#"
                function main() -> int {
                    var r = exec shell { true; };
                    return r.exitCode;
                };
                "#,
            )
            .expect("exec shell should succeed");
        assert_eq!(outcome.exit_status, 0);
    }

    #[test]
    fn exec_shell_false_can_be_handled_by_spar_logic() {
        let outcome = Engine::default()
            .execute_source(
                r#"
                function main() -> int {
                    var r = exec shell { false; };
                    if r.success { return 1; }
                    return 0;
                };
                "#,
            )
            .expect("a failed child command is data, not an evaluator error");
        assert_eq!(outcome.exit_status, 0);
    }

    #[test]
    fn shell_sequence_continues_after_failure_by_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("created-after-failure");
        let source = format!(
            r#"
            function main() -> shell {{
                return shell {{ false; printf x > "{}"; }};
            }};
            "#,
            marker.display()
        );
        let outcome = Engine::default()
            .execute_source(&source)
            .expect("ordinary shell sequence handles child failure");
        assert_eq!(outcome.exit_status, 0);
        assert!(marker.exists(), "a command after ';' must run");
    }

    #[test]
    fn main_returning_shell_executes_and_maps_its_status() {
        let success = Engine::default()
            .execute_source("function main() -> shell { return shell { true; }; };")
            .expect("shell main should execute");
        let failure = Engine::default()
            .execute_source("function main() -> shell { return shell { false; }; };")
            .expect("child failure should map to an outcome");
        assert_eq!(success.exit_status, 0);
        assert_ne!(failure.exit_status, 0);
    }

    #[test]
    fn deferred_shell_runs_spar_loops_and_composes_shell_returning_calls() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("loop-output");
        let source = format!(
            r#"
            function writeTwice(items: [str]) -> shell {{
                return shell {{
                    for item in items {{
                        printf x >> "{}";
                    }}
                }};
            }};

            function main() -> shell {{
                return shell {{
                    writeTwice(items: ["a", "b"]);
                }};
            }};
            "#,
            marker.display()
        );

        let outcome = Engine::default()
            .execute_source(&source)
            .expect("mixed shell program should execute");
        assert_eq!(outcome.exit_status, 0);
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "xx");
    }

    #[test]
    fn deferred_shell_runs_nested_loops_with_interpolation_and_local_mutation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("nested-loop-output");
        let source = format!(
            r#"
            function writeGrid(items: [str]) -> shell {{
                return shell {{
                    var mut count: int = 0;
                    for outer in items {{
                        for inner in items {{
                            printf "%s:%s;" "${{outer}}" "${{inner}}" >> "{}";
                            count += 1;
                        }}
                    }}
                    printf "%s" "${{count}}" >> "{}";
                }};
            }};

            function main() -> shell {{
                return shell {{
                    writeGrid(items: ["a", "b"]);
                }};
            }};
            "#,
            marker.display(),
            marker.display()
        );

        let outcome = Engine::default()
            .execute_source(&source)
            .expect("nested mixed shell program should execute");
        assert_eq!(outcome.exit_status, 0);
        assert_eq!(
            std::fs::read_to_string(marker).unwrap(),
            "a:a;a:b;b:a;b:b;4"
        );
    }

    #[test]
    fn deferred_shell_interpolation_captures_values_and_preserves_one_argument() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("captured");
        let source = format!(
            r#"
            function main() -> shell {{
                var mut name: str = "Obi Charles";
                var plan: shell = shell {{
                    printf "%s" "${{name}}" > "{}";
                }};
                name = "changed";
                return plan;
            }};
            "#,
            marker.display()
        );

        let outcome = Engine::default().execute_source(&source).unwrap();
        assert_eq!(outcome.exit_status, 0);
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "Obi Charles");
    }

    #[test]
    fn shell_local_plus_equals_uses_normal_spar_mutation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("count");
        let source = format!(
            r#"
            function main() -> shell {{
                return shell {{
                    var mut count: int = 0;
                    for item in ["a", "b"] {{
                        count += 1;
                    }}
                    printf "%s" "${{count}}" > "{}";
                }};
            }};
            "#,
            marker.display()
        );
        Engine::default().execute_source(&source).unwrap();
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "2");
    }

    #[test]
    fn command_substitution_returns_trimmed_utf8_text_at_shell_runtime() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("branch");
        let source = format!(
            r#"
            function main() -> shell {{
                return shell {{
                    var branch: str = $(printf "main\n\n");
                    if branch == "main" {{
                        printf yes > "{}";
                    }}
                }};
            }};
            "#,
            marker.display()
        );
        Engine::default().execute_source(&source).unwrap();
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "yes");
    }

    #[test]
    fn native_status_binding_exposes_last_nonzero_command() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("status");
        let source = format!(
            r#"
            function main() -> shell {{
                return shell {{
                    false;
                    if !status.success {{
                        printf "%s" "${{status.code}}" > "{}";
                    }}
                }};
            }};
            "#,
            marker.display()
        );
        Engine::default().execute_source(&source).unwrap();
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "1");
    }

    #[test]
    fn native_status_exposes_pid_signal_and_pipeline_stages() {
        let outcome = Engine::default()
            .execute_source(
                r#"
                function main() -> shell {
                    return shell {
                        true | sh -c "kill -TERM $$";
                        if status.code == 143 && !status.success && status.signal == 15 && status.pid > 0 && status.pipeline[0].success && !status.pipeline[1].success {
                            exit 0;
                        }
                        exit 1;
                    };
                };
                "#,
            )
            .unwrap();
        assert_eq!(outcome.exit_status, 0);
    }

    #[test]
    fn explicit_bash_block_runs_as_foreign_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("bash");
        let source = format!(
            r#"
            function main() -> shell {{
                return shell bash {{
                    value=foreign
                    printf "%s" "$value" > "{}"
                }};
            }};
            "#,
            marker.display()
        );
        Engine::default().execute_source(&source).unwrap();
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "foreign");
    }

    #[test]
    fn structured_exec_block_captures_raw_stdout_and_stderr_bytes() {
        let outcome = Engine::default()
            .execute_source(
                r#"
                function main() -> int {
                    var result: ExecResult = exec { sh -c "printf A; printf B >&2; exit 7"; };
                    if result.success { return 1; }
                    return result.stdout[0] - 58;
                };
                "#,
            )
            .expect("nonzero structured capture must return data");
        assert_eq!(outcome.exit_status, 7);
    }

    #[test]
    fn process_result_exposes_pipeline_status_and_raw_bytes() {
        let outcome = Engine::default()
            .execute_source(
                r#"
                function main() -> int {
                    var result: ProcessResult = exec { sh -c "printf '\377'; exit 9"; };
                    if result.status.processes[0].code == 9 && result.stdout[0] == 255 {
                        return 0;
                    }
                    return 1;
                };
                "#,
            )
            .expect("structured process result");
        assert_eq!(outcome.exit_status, 0);
    }

    #[test]
    fn background_command_sets_last_job_and_is_owned_until_exit() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("finished");
        let pid_file = temp.path().join("pid");
        let source = format!(
            r#"
            function main() -> shell {{
                return shell {{
                    sh -c "sleep 0.02; printf done > '{}'" &;
                    printf "%s:%s" "$!" "${{lastJob.pid}}" > "{}";
                }};
            }};
            "#,
            marker.display(),
            pid_file.display()
        );
        let outcome = Engine::default().execute_source(&source).unwrap();
        assert_eq!(outcome.exit_status, 0);
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "done");
        let pids = std::fs::read_to_string(pid_file).unwrap();
        let (short, native) = pids.split_once(':').unwrap();
        assert_eq!(short, native);
        assert!(native.parse::<u32>().is_ok());
    }

    #[test]
    fn native_redirections_support_generic_fds_order_and_both_streams() {
        let temp = tempfile::tempdir().unwrap();
        let stdout = temp.path().join("stdout");
        let stderr = temp.path().join("stderr");
        let both = temp.path().join("both");
        let source = format!(
            r#"
            function main() -> shell {{
                return shell {{
                    sh -c "printf out; printf err >&2" 3> "{}" 2>&3 > "{}";
                    sh -c "printf a; printf b >&2" &> "{}";
                    sh -c "printf c; printf d >&2" &>> "{}";
                }};
            }};
            "#,
            stderr.display(),
            stdout.display(),
            both.display(),
            both.display()
        );
        Engine::default().execute_source(&source).unwrap();
        assert_eq!(std::fs::read_to_string(stdout).unwrap(), "out");
        assert_eq!(std::fs::read_to_string(stderr).unwrap(), "err");
        let both = std::fs::read_to_string(both).unwrap();
        assert!(both.contains('a') && both.contains('b'));
        assert!(both.contains('c') && both.contains('d'));
    }

    #[test]
    fn command_substitution_runs_logical_chains_and_pipelines() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing = temp.path().join("missing");
        let marker = temp.path().join("substitution");
        let source = format!(
            r#"
            function main() -> shell {{
                return shell {{
                    var exists: str = $(test -f "{}" && echo true || echo false);
                    var lines: str = $(printf "one\ntwo\n" | wc -l);
                    printf "%s:%s" "${{exists}}" "${{lines}}" > "{}";
                }};
            }};
            "#,
            missing.display(),
            marker.display()
        );
        Engine::default().execute_source(&source).unwrap();
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "false:2");
    }

    #[test]
    fn command_substitution_nonzero_is_user_eval_error_not_internal_runtime_error() {
        let errors = Engine::default()
            .execute_source(
                "function main() -> shell {\n    return shell {\n        var value: str = $(false);\n        echo \"${value}\";\n    };\n};\n",
            )
            .unwrap_err();
        let rendered = errors
            .first()
            .expect("command substitution failure")
            .to_string();
        assert!(
            rendered.contains("command substitution exited with status 1"),
            "{rendered}"
        );
        assert!(!rendered.contains("internal runtime error"), "{rendered}");
    }

    #[test]
    fn command_substitution_inside_a_shell_word_preserves_one_argv_boundary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("inline-substitution");
        let source = format!(
            r#"
            function main() -> shell {{
                return shell {{
                    printf "[%s]" prefix-$(printf "a b")-suffix > "{}";
                }};
            }};
            "#,
            marker.display()
        );
        Engine::default().execute_source(&source).unwrap();
        assert_eq!(
            std::fs::read_to_string(marker).unwrap(),
            "[prefix-a b-suffix]"
        );
    }

    #[test]
    fn native_cd_changes_cwd_for_nested_shell_calls_and_relative_redirects() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nested = temp.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let source = format!(
            r#"
            function enter(path: str) -> shell {{
                return shell {{
                    cd "${{path}}";
                }};
            }};

            function main() -> shell {{
                return shell {{
                    enter(
                        path: "{}"
                    );
                    cd "nested";
                    printf "%s" "$(pwd)" > "cwd.txt";
                }};
            }};
            "#,
            temp.path().display()
        );
        Engine::default().execute_source(&source).unwrap();
        let marker = nested.join("cwd.txt");
        let printed = std::fs::read_to_string(&marker).unwrap();
        assert_eq!(
            std::fs::canonicalize(printed).unwrap(),
            std::fs::canonicalize(nested).unwrap()
        );
    }

    #[test]
    fn advanced_shell_stress_supports_multiline_spar_and_native_commands() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("stress.txt");
        let source = format!(
            r#"
            function writeSummary(path: str, files: List<str>) -> shell {{
                return shell {{
                    var mut count: int = 0;
                    for file in files {{
                        printf "%s\\n" \
                            "${{file}}" \
                            >> "${{path}}";
                        count += 1;
                    }}
                    printf "count=%s" "${{count}}" >> "${{path}}";
                }};
            }};

            function main() -> shell {{
                return shell {{
                    var files: List<str> = [
                        "Cargo.toml",
                        "Cargo.lock",
                        "src/main.rs",
                    ];
                    writeSummary(
                        path: "{}",
                        files: files
                    );
                }};
            }};
            "#,
            marker.display()
        );
        Engine::default().execute_source(&source).unwrap();
        assert_eq!(
            std::fs::read_to_string(marker).unwrap(),
            "Cargo.toml\nCargo.lock\nsrc/main.rs\ncount=3"
        );
    }

    #[test]
    fn pure_returned_shell_runtime_error_keeps_a_real_source_span() {
        let errors = Engine::default()
            .execute_source(
                "function main() -> shell {\n    return shell { definitely-not-a-real-command-xyz; };\n};\n",
            )
            .unwrap_err();
        let Some(SparError::EvalError { span, .. }) = errors.first() else {
            panic!("expected an eval error, got {errors:?}");
        };
        assert!(
            span.line > 0,
            "runtime error must never use the dummy 0:0 span"
        );
    }

    #[test]
    fn native_command_runtime_error_keeps_source_line() {
        let errors = Engine::default()
            .execute_source(
                "function main() -> shell {\n    return shell {\n        var x: int = 1;\n        definitely-not-a-real-command-xyz;\n    };\n};\n",
            )
            .unwrap_err();
        assert!(format!("{errors:?}").contains("line: 4"), "{errors:?}");
    }

    #[test]
    fn deferred_shell_rejects_mutating_a_captured_binding() {
        let errors = Engine::default()
            .check_source(
                r#"
                function main() -> shell {
                    var mut count: int = 0;
                    return shell { count = count + 1; };
                };
                "#,
            )
            .expect_err("captured mutation must be diagnosed");
        assert!(
            format!("{errors:?}").contains("cannot mutate captured binding `count`"),
            "{errors:?}"
        );
    }

    #[test]
    fn single_quoted_shell_word_is_literal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("literal");
        let source = format!(
            r#"
            function main() -> shell {{
                var name: str = "expanded";
                return shell {{ printf "%s" '${{name}}' > "{}"; }};
            }};
            "#,
            marker.display()
        );
        Engine::default().execute_source(&source).unwrap();
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "${name}");
    }

    #[test]
    fn explicit_list_expansion_preserves_each_argv_item() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("args");
        let source = format!(
            r#"
            function main() -> shell {{
                var files: List<str> = ["one two", "three"];
                return shell {{ printf "%s\n" ...${{files}} > "{}"; }};
            }};
            "#,
            marker.display()
        );
        Engine::default().execute_source(&source).unwrap();
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "one two\nthree\n");
    }

    #[test]
    fn shell_exit_sets_status_and_stops_program() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("must-not-exist");
        let source = format!(
            r#"
            function main() -> shell {{
                return shell {{
                    exit 9;
                    printf bad > "{}";
                }};
            }};
            "#,
            marker.display()
        );
        let outcome = Engine::default().execute_source(&source).unwrap();
        assert_eq!(outcome.exit_status, 9);
        assert!(!marker.exists());
    }
}
