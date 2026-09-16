use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast::{Program, TopLevelItem};
use crate::compiler::Compiler;
use crate::compiler::{Compilation, CompileOptions};
use crate::error::{Span, SparError};
use crate::loader::LoadedImport;
use crate::resolver::SymbolTable;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModuleId(pub(crate) u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FunctionId(pub(crate) u32);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FunctionKey {
    pub module: ModuleId,
    pub group: Option<String>,
    pub name: String,
}

#[allow(dead_code)] // Read by lowering/runtime tasks added in this phase.
pub(crate) struct CheckedProgram {
    pub program: Program,
    pub symbols: SymbolTable,
    pub imports: HashMap<String, LoadedImport>,
}

#[allow(dead_code)] // Expanded and executed by subsequent phase tasks.
pub(crate) struct CompiledFunction {
    pub id: FunctionId,
    pub key: FunctionKey,
    pub name: String,
}

#[allow(dead_code)] // Graph fields become active when imports are lowered.
pub(crate) struct CompiledModule {
    pub id: ModuleId,
    pub identity: PathBuf,
    pub checked: CheckedProgram,
    pub import_modules: HashMap<String, ModuleId>,
    pub functions: Vec<CompiledFunction>,
}

pub struct CompiledProgram {
    #[allow(dead_code)] // Used by entry dispatch once Runtime is installed.
    pub(crate) entry: ModuleId,
    pub(crate) modules: Vec<CompiledModule>,
    #[allow(dead_code)] // Used by entry dispatch once Runtime is installed.
    pub(crate) entry_main: Option<FunctionId>,
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
        let entry_checked = checked_from_compilation(compilation)?;
        let mut builder = ModuleGraphBuilder::new(options.clone());
        builder.add_entry(entry_checked)?;
        let entry_main = builder.modules[0]
            .functions
            .iter()
            .find(|function| function.key.group.is_none() && function.name == "main")
            .map(|function| function.id);

        Ok(Self {
            entry: ModuleId(0),
            modules: builder.modules,
            entry_main,
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

    #[cfg(test)]
    pub(crate) fn debug_function_keys(&self) -> Vec<String> {
        self.modules
            .iter()
            .flat_map(|module| {
                let module_name = if module.id == self.entry {
                    "<source>".to_string()
                } else {
                    module
                        .identity
                        .file_name()
                        .unwrap_or(module.identity.as_os_str())
                        .to_string_lossy()
                        .into_owned()
                };
                module
                    .functions
                    .iter()
                    .map(move |function| match &function.key.group {
                        Some(group) => format!("{module_name}::{group}::{}", function.name),
                        None => format!("{module_name}::{}", function.name),
                    })
            })
            .collect()
    }
}

fn checked_from_compilation(compilation: Compilation) -> Result<CheckedProgram, Vec<SparError>> {
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
    Ok(CheckedProgram {
        program,
        symbols,
        imports: compilation.imports,
    })
}

struct ModuleGraphBuilder {
    options: CompileOptions,
    modules: Vec<CompiledModule>,
    identities: HashMap<PathBuf, ModuleId>,
    next_function: u32,
}

impl ModuleGraphBuilder {
    fn new(options: CompileOptions) -> Self {
        Self {
            options,
            modules: Vec::new(),
            identities: HashMap::new(),
            next_function: 0,
        }
    }

    fn add_entry(&mut self, checked: CheckedProgram) -> Result<ModuleId, Vec<SparError>> {
        let identity = self
            .options
            .source_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("<source>"));
        self.add_module(identity, checked)
    }

    fn add_module(
        &mut self,
        identity: PathBuf,
        checked: CheckedProgram,
    ) -> Result<ModuleId, Vec<SparError>> {
        let cache_identity = identity.canonicalize().unwrap_or_else(|_| identity.clone());
        if let Some(id) = self.identities.get(&cache_identity) {
            return Ok(*id);
        }

        let id = ModuleId(self.modules.len() as u32);
        self.identities.insert(cache_identity, id);
        let functions = self.compile_function_headers(id, &checked.program);
        let mut imports: Vec<(String, PathBuf)> = checked
            .imports
            .iter()
            .map(|(alias, import)| (alias.clone(), import.resolved_path.clone()))
            .collect();
        imports.sort_by(|left, right| left.0.cmp(&right.0));

        self.modules.push(CompiledModule {
            id,
            identity,
            checked,
            import_modules: HashMap::new(),
            functions,
        });

        for (alias, path) in imports {
            let module_id = if let Some(existing) = self
                .identities
                .get(&path.canonicalize().unwrap_or_else(|_| path.clone()))
            {
                *existing
            } else {
                let source = std::fs::read_to_string(&path).map_err(|error| {
                    vec![SparError::ResolveError {
                        message: format!("cannot read import file '{}': {error}", path.display()),
                        hint: None,
                        span: Span::dummy(),
                    }]
                })?;
                let import_options = CompileOptions {
                    base_dir: path
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .to_path_buf(),
                    source_path: Some(path.clone()),
                    evaluate: false,
                    ..self.options.clone()
                };
                let compilation = Compiler::new(import_options).compile(&source);
                let imported = checked_from_compilation(compilation)?;
                self.add_module(path, imported)?
            };
            self.modules[id.0 as usize]
                .import_modules
                .insert(alias, module_id);
        }
        Ok(id)
    }

    fn compile_function_headers(
        &mut self,
        module: ModuleId,
        program: &Program,
    ) -> Vec<CompiledFunction> {
        let mut functions = Vec::new();
        for item in &program.items {
            match item {
                TopLevelItem::Function(function) => {
                    functions.push(self.function_header(module, None, &function.name));
                }
                TopLevelItem::FunctionGroup(group) => {
                    for function in &group.functions {
                        functions.push(self.function_header(
                            module,
                            Some(group.name.clone()),
                            &function.name,
                        ));
                    }
                }
                _ => {}
            }
        }
        functions
    }

    fn function_header(
        &mut self,
        module: ModuleId,
        group: Option<String>,
        name: &str,
    ) -> CompiledFunction {
        let id = FunctionId(self.next_function);
        self.next_function += 1;
        CompiledFunction {
            id,
            key: FunctionKey {
                module,
                group,
                name: name.to_string(),
            },
            name: name.to_string(),
        }
    }
}
