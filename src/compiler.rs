//! Stable, embeddable façade for the Spar compilation pipeline.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast::Program;
use crate::error::SparError;
use crate::evaluator::{EvalResult, Evaluator};
use crate::loader::{self, ImportLoader, LoadedImport};
use crate::resolver::{Resolver, SymbolTable};
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
            errors: Vec::new(),
        };

        let tokens = match Lexer::new(source).tokenize() {
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
                Ok(result) => compilation.result = Some(result),
                Err(errors) => compilation.errors.extend(errors),
            }
        }

        compilation.program = Some(program);
        compilation.symbols = Some(symbols);
        compilation
    }
}
