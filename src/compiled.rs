use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast::{Program, TopLevelItem};
use crate::compiler::{Compilation, CompileOptions};
use crate::error::{Span, SparError};
use crate::loader::LoadedImport;
use crate::resolver::SymbolTable;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModuleId(pub(crate) u32);

#[allow(dead_code)] // Read by lowering/runtime tasks added in this phase.
pub(crate) struct CheckedProgram {
    pub program: Program,
    pub symbols: SymbolTable,
    pub imports: HashMap<String, LoadedImport>,
}

#[allow(dead_code)] // Expanded and executed by subsequent phase tasks.
pub(crate) struct CompiledFunction {
    pub name: String,
}

#[allow(dead_code)] // Graph fields become active when imports are lowered.
pub(crate) struct CompiledModule {
    pub id: ModuleId,
    pub identity: PathBuf,
    pub checked: CheckedProgram,
    pub functions: Vec<CompiledFunction>,
}

pub struct CompiledProgram {
    #[allow(dead_code)] // Used by entry dispatch once Runtime is installed.
    pub(crate) entry: ModuleId,
    pub(crate) modules: Vec<CompiledModule>,
    pub(crate) options: CompileOptions,
}

impl CompiledProgram {
    pub(crate) fn from_compilation(
        compilation: Compilation,
        options: CompileOptions,
    ) -> Result<Self, Vec<SparError>> {
        if !compilation.errors.is_empty() {
            return Err(compilation.errors);
        }
        let program = compilation.program.ok_or_else(|| {
            vec![SparError::EvalError {
                message: "internal lowering error: successful compilation has no program".into(),
                span: Span::dummy(),
            }]
        })?;
        let symbols = compilation.symbols.ok_or_else(|| {
            vec![SparError::EvalError {
                message: "internal lowering error: successful compilation has no symbols".into(),
                span: Span::dummy(),
            }]
        })?;
        let functions = program
            .items
            .iter()
            .flat_map(|item| match item {
                TopLevelItem::Function(function) => vec![CompiledFunction {
                    name: function.name.clone(),
                }],
                TopLevelItem::FunctionGroup(group) => group
                    .functions
                    .iter()
                    .map(|function| CompiledFunction {
                        name: function.name.clone(),
                    })
                    .collect(),
                _ => Vec::new(),
            })
            .collect();
        let identity = options
            .source_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("<source>"));

        Ok(Self {
            entry: ModuleId(0),
            modules: vec![CompiledModule {
                id: ModuleId(0),
                identity,
                checked: CheckedProgram {
                    program,
                    symbols,
                    imports: compilation.imports,
                },
                functions,
            }],
            options,
        })
    }

    pub fn source_path(&self) -> Option<&Path> {
        self.options.source_path.as_deref()
    }

    pub fn function_count(&self) -> usize {
        self.modules
            .iter()
            .map(|module| module.functions.len())
            .sum()
    }
}
