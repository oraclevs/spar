//! Stable, embeddable façade for the Spar compilation pipeline.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast::{Program, SparType};
use crate::error::SparError;
use crate::evaluator::{EvalResult, Evaluator};
use crate::loader::{self, ImportLoader, LoadedImport};
use crate::resolver::{GlobalEntry, Resolver, SymbolTable};
use crate::runner::TaskSet;
use crate::task_lowering::TaskExprEntry;
use crate::typechecker::TypeChecker;
use crate::{Lexer, Parser};

#[derive(Clone, Debug, Default)]
pub struct BundledPackageRoots {
    roots: HashMap<String, PathBuf>,
}

impl BundledPackageRoots {
    pub fn register(
        &mut self,
        name: impl Into<String>,
        root: impl Into<PathBuf>,
    ) -> Result<(), String> {
        let name = name.into();
        if name.is_empty()
            || name.contains('/')
            || name.contains('\\')
            || name == crate::stdlib::STD_PACKAGE_NAME
        {
            return Err(format!("invalid bundled package name '{name}'"));
        }
        self.roots.insert(name, root.into());
        Ok(())
    }

    pub fn resolve(&self, request: &str) -> Option<PathBuf> {
        let (name, module) = match request.split_once('/') {
            Some((name, module)) => (name, Some(module)),
            None => (request, None),
        };
        let root = self.roots.get(name)?;
        let relative = match module {
            None => PathBuf::from("lib.spar"),
            Some(module) if !module.is_empty() => {
                let path = Path::new(module);
                if path.is_absolute()
                    || path.components().any(|part| {
                        matches!(
                            part,
                            std::path::Component::ParentDir
                                | std::path::Component::RootDir
                                | std::path::Component::Prefix(_)
                        )
                    })
                {
                    return None;
                }
                let mut path = path.to_path_buf();
                if path.extension().is_none() {
                    path.set_extension("spar");
                }
                path
            }
            Some(_) => return None,
        };
        Some(root.join(relative))
    }

    pub fn contains_package(&self, request: &str) -> bool {
        let name = request.split('/').next().unwrap_or(request);
        self.roots.contains_key(name)
    }
}

#[derive(Clone, Debug)]
pub struct CompileOptions {
    pub base_dir: PathBuf,
    /// Original source identity. Reserved metadata basenames use this to
    /// preload their compiler-owned schemas and validation rules.
    pub source_path: Option<PathBuf>,
    pub evaluate: bool,
    pub allow_schema_file: bool,
    /// Native functions `ns::fn(...)` calls may dispatch to — empty by
    /// default, so every existing caller behaves exactly as before.
    pub hosts: crate::host::HostRegistry,
    /// Internal runtime-native capabilities. The default registry contains the
    /// capabilities required by the bundled standard library.
    pub natives: crate::runtime::NativeRegistry,
    /// Resolves explicit `import pkg` requests through a project's package
    /// lock/store. Ordinary `import` statements remain local modules only.
    pub locator: Option<crate::package::ModuleLocator>,
    /// The command package-import hints tell users to run (`spar` for the
    /// CLI; embedders such as Sparsh set their own, e.g. `pkg`).
    pub package_command: String,
    /// Source-backed first-party packages registered by an embedder.
    /// `std` remains reserved and is resolved separately before these roots.
    pub bundled_packages: BundledPackageRoots,
    /// Optional replay guard for process effects. Ordinary one-shot
    /// compilation leaves this unset; persistent sessions install one
    /// shared ledger across every replay.
    pub effect_ledger: Option<crate::session::EffectLedger>,
    /// Make the `std/data` functions (`where`, `map`, `take`, ...) available
    /// without an `import`, for every name the program does not declare or
    /// import itself. Off by default; interactive shells turn it on.
    pub data_prelude: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            base_dir: PathBuf::from("."),
            source_path: None,
            evaluate: true,
            allow_schema_file: true,
            hosts: crate::host::HostRegistry::default(),
            natives: crate::stdlib::native_registry(),
            locator: None,
            package_command: "spar".to_string(),
            bundled_packages: BundledPackageRoots::default(),
            effect_ledger: None,
            data_prelude: false,
        }
    }
}

impl CompileOptions {
    pub fn for_path(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let base_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Self {
            base_dir,
            source_path: Some(path.to_path_buf()),
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
    interactive_previous_type: Option<SparType>,
    interactive_expressions: bool,
}

impl Compiler {
    pub fn new(mut options: CompileOptions) -> Self {
        // The bundled std package (implicitly preloaded into every program)
        // is backed by the stdlib natives, so they must stay resolvable even
        // when an embedder supplies its own registry.
        options
            .natives
            .extend_missing(&crate::stdlib::native_registry());
        Self {
            options,
            interactive_previous_type: None,
            interactive_expressions: false,
        }
    }

    pub(crate) fn with_interactive_expressions(mut self) -> Self {
        self.interactive_expressions = true;
        self
    }

    pub(crate) fn with_interactive_previous_type(mut self, ty: Option<SparType>) -> Self {
        self.interactive_previous_type = ty;
        self
    }

    pub fn options(&self) -> &CompileOptions {
        &self.options
    }

    fn import_loader(&self) -> ImportLoader {
        let loader = ImportLoader::new(&self.options.base_dir)
            .with_bundled_packages(self.options.bundled_packages.clone())
            .with_data_prelude(self.options.data_prelude)
            .with_package_command(self.options.package_command.clone());
        match &self.options.locator {
            Some(locator) => loader.with_locator(locator.clone()),
            None => loader,
        }
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
        let parser = if self.interactive_expressions {
            Parser::new(tokens).interactive()
        } else {
            Parser::new(tokens)
        };
        let mut program = match parser.parse() {
            Ok(program) => program,
            Err(error) => {
                compilation.errors.push(error);
                return compilation;
            }
        };
        program.shebang = shebang;

        let compiling_bundled_std = self
            .options
            .source_path
            .as_deref()
            .is_some_and(crate::stdlib::is_bundled_std_path);
        if !compiling_bundled_std {
            if let Err(errors) = crate::stdlib::inject_prelude(&mut program) {
                compilation.errors.extend(errors);
            }
        }

        if self
            .options
            .source_path
            .as_deref()
            .is_some_and(crate::stdlib::is_bundled_std_path)
        {
            crate::loader::mark_program_trusted_native(&mut program);
        }

        inject_exec_result_type(&mut program);

        if let Some((path, kind)) = self
            .options
            .source_path
            .as_deref()
            .and_then(|path| crate::package::metadata_kind(path).map(|kind| (path, kind)))
        {
            if let Err(error) = crate::package::metadata::validate_source(source, path, kind) {
                compilation.errors.push(error);
            }
            crate::package::metadata::inject_builtin_types(&mut program, kind);
        }

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

        let mut expand_loader = self.import_loader();
        if let Err(errors) = loader::expand_imports(&mut program, &mut expand_loader) {
            compilation.errors.extend(errors);
        }

        let mut import_loader = self.import_loader();
        match loader::collect_imports(&program, &mut import_loader) {
            Ok(imports) => compilation.imports = imports,
            Err(errors) => compilation.errors.extend(errors),
        }

        let mut symbols = match Resolver::resolve_with_imports_hosts_and_natives(
            &program,
            &compilation.imports,
            self.options.hosts.clone(),
            self.options.natives.clone(),
        ) {
            Ok(symbols) => symbols,
            Err(errors) => {
                compilation.errors.extend(errors);
                compilation.program = Some(program);
                return compilation;
            }
        };

        symbols.top_level_await = self.interactive_expressions;
        if let Some(ty) = self.interactive_previous_type.clone() {
            symbols.globals.insert(
                "_".into(),
                GlobalEntry::Var {
                    ty,
                    optional: false,
                    exported: false,
                    mutable: false,
                    emit: false,
                    span: crate::Span::dummy(),
                },
            );
        }

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
            match Evaluator::evaluate_with_imports_base_effects_and_natives(
                &program,
                &symbols,
                &compilation.imports,
                &self.options.base_dir,
                self.options.hosts.clone(),
                self.options.natives.clone(),
                self.options.effect_ledger.clone(),
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

pub(crate) fn inject_exec_result_type(program: &mut Program) {
    use crate::ast::{SparType, TopLevelItem, TypeDecl, TypeField, TypeFieldShape};

    let span = crate::Span::new(0, 0, 1, 1);
    program.items.push(TopLevelItem::Type(TypeDecl {
        name: "ExecResult".to_string(),
        name_span: span.clone(),
        type_parameters: Vec::new(),
        exported: false,
        fields: vec![
            TypeField {
                name: "success".to_string(),
                optional: false,
                shape: TypeFieldShape::Primitive(SparType::Bool),
                default: None,
                span: span.clone(),
            },
            TypeField {
                name: "exitCode".to_string(),
                optional: false,
                shape: TypeFieldShape::Primitive(SparType::Int),
                default: None,
                span: span.clone(),
            },
            TypeField {
                name: "stdout".to_string(),
                optional: false,
                shape: TypeFieldShape::Primitive(SparType::List(Box::new(SparType::Int))),
                default: None,
                span: span.clone(),
            },
            TypeField {
                name: "stderr".to_string(),
                optional: false,
                shape: TypeFieldShape::Primitive(SparType::List(Box::new(SparType::Int))),
                default: None,
                span: span.clone(),
            },
        ],
        span,
        end_line: 0,
    }));
    let span = crate::Span::new(0, 0, 1, 1);
    program.items.push(TopLevelItem::Type(TypeDecl {
        name: "ProcessStatus".to_string(),
        name_span: span.clone(),
        type_parameters: Vec::new(),
        exported: false,
        fields: vec![
            TypeField {
                name: "code".to_string(),
                optional: false,
                shape: TypeFieldShape::Primitive(SparType::Int),
                default: None,
                span: span.clone(),
            },
            TypeField {
                name: "success".to_string(),
                optional: false,
                shape: TypeFieldShape::Primitive(SparType::Bool),
                default: None,
                span: span.clone(),
            },
            TypeField {
                name: "signal".to_string(),
                optional: true,
                shape: TypeFieldShape::Primitive(SparType::Int),
                default: None,
                span: span.clone(),
            },
            TypeField {
                name: "pid".to_string(),
                optional: false,
                shape: TypeFieldShape::Primitive(SparType::Int),
                default: None,
                span: span.clone(),
            },
            TypeField {
                name: "pipeline".to_string(),
                optional: false,
                shape: TypeFieldShape::Primitive(SparType::List(Box::new(SparType::Named(
                    "ProcessStatus".into(),
                )))),
                default: None,
                span: span.clone(),
            },
        ],
        span: span.clone(),
        end_line: 0,
    }));

    for (name, fields) in [
        (
            "Bytes",
            vec![TypeField {
                name: "values".into(),
                optional: false,
                shape: TypeFieldShape::Primitive(SparType::List(Box::new(SparType::Int))),
                default: None,
                span: span.clone(),
            }],
        ),
        (
            "PipelineStatus",
            vec![
                TypeField {
                    name: "code".into(),
                    optional: false,
                    shape: TypeFieldShape::Primitive(SparType::Int),
                    default: None,
                    span: span.clone(),
                },
                TypeField {
                    name: "success".into(),
                    optional: false,
                    shape: TypeFieldShape::Primitive(SparType::Bool),
                    default: None,
                    span: span.clone(),
                },
                TypeField {
                    name: "processes".into(),
                    optional: false,
                    shape: TypeFieldShape::Primitive(SparType::List(Box::new(SparType::Named(
                        "ProcessStatus".into(),
                    )))),
                    default: None,
                    span: span.clone(),
                },
            ],
        ),
        (
            "Command",
            vec![
                TypeField {
                    name: "program".into(),
                    optional: false,
                    shape: TypeFieldShape::Primitive(SparType::Str),
                    default: None,
                    span: span.clone(),
                },
                TypeField {
                    name: "args".into(),
                    optional: false,
                    shape: TypeFieldShape::Primitive(SparType::List(Box::new(SparType::Str))),
                    default: None,
                    span: span.clone(),
                },
            ],
        ),
        (
            "ProcessChunk",
            vec![
                TypeField {
                    name: "source".into(),
                    optional: false,
                    shape: TypeFieldShape::Primitive(SparType::Str),
                    default: None,
                    span: span.clone(),
                },
                TypeField {
                    name: "bytes".into(),
                    optional: false,
                    shape: TypeFieldShape::Named("Bytes".into()),
                    default: None,
                    span: span.clone(),
                },
            ],
        ),
        ("ProcessStream", vec![]),
        (
            "ProcessResult",
            vec![
                TypeField {
                    name: "success".into(),
                    optional: false,
                    shape: TypeFieldShape::Primitive(SparType::Bool),
                    default: None,
                    span: span.clone(),
                },
                TypeField {
                    name: "exitCode".into(),
                    optional: false,
                    shape: TypeFieldShape::Primitive(SparType::Int),
                    default: None,
                    span: span.clone(),
                },
                TypeField {
                    name: "status".into(),
                    optional: false,
                    shape: TypeFieldShape::Named("PipelineStatus".into()),
                    default: None,
                    span: span.clone(),
                },
                TypeField {
                    name: "stdout".into(),
                    optional: false,
                    shape: TypeFieldShape::Named("Bytes".into()),
                    default: None,
                    span: span.clone(),
                },
                TypeField {
                    name: "stderr".into(),
                    optional: false,
                    shape: TypeFieldShape::Named("Bytes".into()),
                    default: None,
                    span: span.clone(),
                },
            ],
        ),
        (
            "Job",
            vec![
                TypeField {
                    name: "id".into(),
                    optional: false,
                    shape: TypeFieldShape::Primitive(SparType::Int),
                    default: None,
                    span: span.clone(),
                },
                TypeField {
                    name: "pid".into(),
                    optional: false,
                    shape: TypeFieldShape::Primitive(SparType::Int),
                    default: None,
                    span: span.clone(),
                },
                TypeField {
                    name: "processGroup".into(),
                    optional: false,
                    shape: TypeFieldShape::Primitive(SparType::Int),
                    default: None,
                    span: span.clone(),
                },
                TypeField {
                    name: "state".into(),
                    optional: false,
                    shape: TypeFieldShape::Primitive(SparType::Str),
                    default: None,
                    span: span.clone(),
                },
            ],
        ),
    ] {
        program.items.push(TopLevelItem::Type(TypeDecl {
            name: name.into(),
            name_span: span.clone(),
            type_parameters: Vec::new(),
            exported: false,
            fields,
            span: span.clone(),
            end_line: 0,
        }));
    }
}

/// Validates a declared `main` function's signature: no parameters, a
/// return type of `int`, `void`, or `shell`, and not `private`. A program with no
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
        crate::ast::SparType::Int | crate::ast::SparType::Void | crate::ast::SparType::Shell
    ) {
        return Err(SparError::TypeError {
            message: format!(
                "'main' must return 'int', 'void', or 'shell', found '{}'",
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
    fn execute_entry_accepts_zero_argument_int_void_or_shell_main() {
        assert_entry_ok("function main() -> int { return 7; };");
        assert_entry_ok("function main() -> void {};");
        assert_entry_ok("function main() -> shell { return shell { echo ok; }; };");
        assert_entry_error(
            "function main(x: int) -> int { return x; };",
            "must not declare parameters",
        );
    }

    #[test]
    fn execute_entry_accepts_main_returning_shell() {
        assert_entry_ok("function main() -> shell { return shell { true; }; };");
    }

    #[test]
    fn execute_entry_rejects_private_main() {
        assert_entry_error(
            "private function main() -> int { return 0; };",
            "cannot be declared 'private'",
        );
    }

    #[test]
    fn execute_entry_rejects_a_non_int_non_void_non_shell_return_type() {
        assert_entry_error(
            "function main() -> str { return \"ok\"; };",
            "must return 'int', 'void', or 'shell'",
        );
    }

    #[test]
    fn program_without_a_main_declaration_is_a_valid_entry_signature() {
        assert_entry_ok("var x: int = 1;");
    }
}
