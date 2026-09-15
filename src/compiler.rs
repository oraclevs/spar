//! Stable, embeddable façade for the Spar compilation pipeline.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast::Program;
use crate::error::SparError;
use crate::evaluator::{EvalResult, Evaluator};
use crate::loader::{self, ImportLoader, LoadedImport};
use crate::resolver::{Resolver, SymbolTable};
use crate::runner::TaskSet;
use crate::task_lowering::TaskExprEntry;
use crate::typechecker::TypeChecker;
use crate::{Lexer, Parser};

#[derive(Clone, Debug)]
pub struct CompileOptions {
    pub base_dir: PathBuf,
    pub evaluate: bool,
    pub allow_schema_file: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            base_dir: PathBuf::from("."),
            evaluate: true,
            allow_schema_file: true,
        }
    }
}

impl CompileOptions {
    pub fn for_path(path: impl AsRef<Path>) -> Self {
        let base_dir = path
            .as_ref()
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Self {
            base_dir,
            ..Self::default()
        }
    }
}

#[derive(Debug)]
pub struct Compilation {
    pub program: Option<Program>,
    pub symbols: Option<SymbolTable>,
    pub imports: HashMap<String, LoadedImport>,
    pub result: Option<EvalResult>,
    /// The program's lowered task catalog, if it declares any `task [...]`
    /// items. Only ever `Some` once every other stage — lex through
    /// evaluate — has succeeded; task declarations never appear in
    /// `result`/emitted JSON.
    pub tasks: Option<TaskSet>,
    /// Task `run` block expressions that mix a parameter with other
    /// values (function calls, concatenation, ...) and so can't be
    /// pre-evaluated — indexed by the `id` on each `TemplatePart::Expr`
    /// in `tasks`. Evaluate one with `Evaluator::eval_standalone`, using
    /// `program`/`symbols`/`result` and the task's bound parameter
    /// values, to render that command at run time.
    pub task_exprs: Vec<TaskExprEntry>,
    pub errors: Vec<SparError>,
}

impl Compilation {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn into_result(self) -> Result<Self, Vec<SparError>> {
        if self.errors.is_empty() {
            Ok(self)
        } else {
            Err(self.errors)
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Compiler {
    options: CompileOptions,
}

impl Compiler {
    pub fn new(options: CompileOptions) -> Self {
        Self { options }
    }

    pub fn options(&self) -> &CompileOptions {
        &self.options
    }

    pub fn compile(&self, source: &str) -> Compilation {
        let mut compilation = Compilation {
            program: None,
            symbols: None,
            imports: HashMap::new(),
            result: None,
            tasks: None,
            task_exprs: Vec::new(),
            errors: Vec::new(),
        };

        let lexer = Lexer::new(source);
        let shebang = lexer.shebang().map(str::to_owned);
        let tokens = match lexer.tokenize() {
            Ok(tokens) => tokens,
            Err(error) => {
                compilation.errors.push(error);
                return compilation;
            }
        };
        let mut program = match Parser::new(tokens).parse() {
            Ok(program) => program,
            Err(error) => {
                compilation.errors.push(error);
                return compilation;
            }
        };
        program.shebang = shebang;

        if program.is_schema_file && !self.options.allow_schema_file {
            compilation.errors.push(SparError::SchemaError {
                message:
                    "this is a schema file and cannot be emitted — schema files declare shape only"
                        .into(),
                span: crate::Span::new(0, 0, 1, 1),
            });
            compilation.program = Some(program);
            return compilation;
        }

        let mut expand_loader = ImportLoader::new(&self.options.base_dir);
        if let Err(errors) = loader::expand_imports(&mut program, &mut expand_loader) {
            compilation.errors.extend(errors);
        }

        let mut import_loader = ImportLoader::new(&self.options.base_dir);
        match loader::collect_imports(&program, &mut import_loader) {
            Ok(imports) => compilation.imports = imports,
            Err(errors) => compilation.errors.extend(errors),
        }

        let symbols = match Resolver::resolve_with_imports(&program, &compilation.imports) {
            Ok(symbols) => symbols,
            Err(errors) => {
                compilation.errors.extend(errors);
                compilation.program = Some(program);
                return compilation;
            }
        };

        let schema_bindings =
            match loader::validate_schema_imports(&program, &self.options.base_dir) {
                Ok(bindings) => bindings,
                Err(errors) => {
                    compilation.errors.extend(errors);
                    HashMap::new()
                }
            };
        if let Err(errors) = TypeChecker::check_with_schema(&program, &symbols, schema_bindings) {
            compilation.errors.extend(errors);
        }

        if self.options.evaluate && compilation.errors.is_empty() {
            match Evaluator::evaluate_with_imports_and_base(
                &program,
                &symbols,
                &compilation.imports,
                &self.options.base_dir,
            ) {
                Ok(result) => {
                    match crate::task_lowering::lower_tasks(
                        &program,
                        &symbols,
                        &result,
                        &self.options.base_dir,
                        &mut compilation.task_exprs,
                    ) {
                        Ok(tasks) => compilation.tasks = tasks,
                        Err(errors) => compilation.errors.extend(errors),
                    }
                    compilation.result = Some(result);
                }
                Err(errors) => compilation.errors.extend(errors),
            }
        }

        compilation.program = Some(program);
        compilation.symbols = Some(symbols);
        compilation
    }
}

/// Validates a declared `main` function's signature: no parameters, a
/// return type of `int` or `void`, and not `private`. A program with no
/// `main` at all is `Ok(())` here — this only checks the shape of a
/// declaration that exists; deciding whether Execute mode *requires* one
/// present is that mode's job (Task 9), not this structural check's.
pub fn validate_entry_signature(program: &Program) -> Result<(), SparError> {
    let Some(main) = program.items.iter().find_map(|item| match item {
        crate::ast::TopLevelItem::Function(f) if f.name == "main" => Some(f),
        _ => None,
    }) else {
        return Ok(());
    };

    if main.is_private {
        return Err(SparError::ResolveError {
            message: "'main' cannot be declared 'private' — it must be callable as the \
                       application entry point"
                .into(),
            hint: None,
            span: main.span.clone(),
        });
    }
    if !main.params.is_empty() {
        return Err(SparError::ResolveError {
            message: "'main' must not declare parameters".into(),
            hint: None,
            span: main.span.clone(),
        });
    }
    if !matches!(
        main.ret,
        crate::ast::SparType::Int | crate::ast::SparType::Void
    ) {
        return Err(SparError::TypeError {
            message: format!(
                "'main' must return 'int' or 'void', found '{}'",
                crate::typechecker::display_type(&main.ret)
            ),
            hint: None,
            span: main.ret_span.clone(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Program {
        let tokens = crate::Lexer::new(src).tokenize().expect("lex");
        crate::Parser::new(tokens).parse().expect("parse")
    }

    fn assert_entry_ok(src: &str) {
        validate_entry_signature(&parse(src)).expect("expected a valid entry signature");
    }

    fn assert_entry_error(src: &str, expected_substring: &str) {
        let error = validate_entry_signature(&parse(src)).expect_err("expected an entry error");
        let message = format!("{error:?}");
        assert!(
            message.contains(expected_substring),
            "expected error containing {expected_substring:?}, got: {message}"
        );
    }

    #[test]
    fn execute_entry_accepts_only_zero_argument_int_or_void_main() {
        assert_entry_ok("function main() -> int { return 7; };");
        assert_entry_ok("function main() -> void {};");
        assert_entry_error(
            "function main(x: int) -> int { return x; };",
            "must not declare parameters",
        );
    }

    #[test]
    fn execute_entry_rejects_private_main() {
        assert_entry_error(
            "private function main() -> int { return 0; };",
            "cannot be declared 'private'",
        );
    }

    #[test]
    fn execute_entry_rejects_a_non_int_non_void_return_type() {
        assert_entry_error(
            "function main() -> str { return \"ok\"; };",
            "must return 'int' or 'void'",
        );
    }

    #[test]
    fn program_without_a_main_declaration_is_a_valid_entry_signature() {
        assert_entry_ok("var x: int = 1;");
    }
}
