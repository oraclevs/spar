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
use crate::compiler::{validate_entry_signature, Compilation, CompileOptions, Compiler};
use crate::error::{Span, SparError};
use crate::evaluator::{ConfigValue, Evaluator};

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

    /// A persistent, incrementally-evaluated session over this engine's
    /// hosts and options — see `session::Session`.
    pub fn session(&self) -> crate::session::Session {
        crate::session::Session::new(self.options.clone())
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
        let options = CompileOptions {
            evaluate: true,
            ..self.options.clone()
        };
        Compiler::new(options).compile(source)
    }

    pub fn emit_path(&self, path: &Path) -> Result<Compilation, Vec<SparError>> {
        Ok(self.with_path(path).emit_source(&read_source(path)?))
    }

    /// Execute mode: Check, then require a valid zero-argument `main`
    /// returning `int` or `void`, evaluate the module exactly once, call
    /// `main`, and translate its result into a process exit status.
    pub fn execute_source(&self, source: &str) -> Result<ExecutionOutcome, Vec<SparError>> {
        let checked = self.check_compile(source)?;
        let program = checked
            .program
            .as_ref()
            .expect("a successful check always records the parsed program");
        let symbols = checked
            .symbols
            .as_ref()
            .expect("a successful check always records resolved symbols");

        require_entry_signature(program).map_err(|e| vec![e])?;

        let (_, result) = Evaluator::evaluate_and_call_entry_with_imports_and_base(
            program,
            symbols,
            &checked.imports,
            &self.options.base_dir,
            "main",
            self.options.hosts.clone(),
        )?;

        let exit_status = match result {
            ConfigValue::Int(status) => status as i32,
            // `main() -> void` — the typechecker guarantees `main` never
            // returns anything else.
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
                       'main' returning 'int' or 'void'"
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
}
