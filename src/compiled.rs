use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast::{Program, ShellExpr, SparType, TopLevelItem};
use crate::compiler::Compiler;
use crate::compiler::{Compilation, CompileOptions};
use crate::error::{Span, SparError};
use crate::loader::LoadedImport;
use crate::lowerer::{allocate_local_layout, lower_function, LoweringContext};
use crate::resolver::SymbolTable;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModuleId(pub(crate) u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FunctionId(pub(crate) u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalSlot(pub(crate) u32);

pub(crate) struct LocalLayout {
    pub parameter_slots: Vec<LocalSlot>,
    pub names: Vec<String>,
    #[allow(dead_code)] // Consumed by typed expression lowering in the next task.
    pub types: Vec<crate::ast::SparType>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TypedOperation {
    IntAdd,
    FloatAdd,
    StringConcat,
    ShellConcat,
    IntSub,
    FloatSub,
    IntMul,
    FloatMul,
    IntDiv,
    FloatDiv,
    IntEq,
    FloatEq,
    StringEq,
    BoolEq,
    IntNotEq,
    FloatNotEq,
    StringNotEq,
    BoolNotEq,
    IntLt,
    FloatLt,
    IntGt,
    FloatGt,
    IntLtEq,
    FloatLtEq,
    IntGtEq,
    FloatGtEq,
    BoolAnd,
    BoolOr,
    BoolNot,
    IntNeg,
    FloatNeg,
    Fallback,
}

#[allow(dead_code)] // Fully consumed by the compiled runtime in Task 5.
#[derive(Clone)]
pub(crate) enum CompiledExpression {
    Constant(crate::ConfigValue, Span),
    Local(LocalSlot, Span),
    Global(String, Span),
    DirectCall {
        function: FunctionId,
        arguments: Vec<CompiledExpression>,
        span: Span,
    },
    HostCall {
        namespace: String,
        name: String,
        arguments: Vec<CompiledExpression>,
        span: Span,
    },
    Panic {
        message: Box<CompiledExpression>,
        span: Span,
    },
    ImportedValue {
        module: ModuleId,
        path: Vec<String>,
        span: Span,
    },
    List(Vec<CompiledExpression>, Span),
    Object(Vec<CompiledObjectItem>, Span),
    Operation {
        operation: TypedOperation,
        operands: Vec<CompiledExpression>,
        span: Span,
    },
    Await {
        promise: Box<CompiledExpression>,
        span: Span,
    },
    Index {
        source: Box<CompiledExpression>,
        index: Box<CompiledExpression>,
        span: Span,
    },
    Field {
        base: Box<CompiledExpression>,
        field: String,
        span: Span,
    },
    Interpolation(Vec<CompiledStringPart>, Span),
    Comprehension {
        binding: LocalSlot,
        source: Box<CompiledExpression>,
        body: Box<CompiledExpression>,
        span: Span,
    },
    Shell(ShellExpr),
    ExecShell(ShellExpr),
}

#[allow(dead_code)] // Fully consumed by the compiled runtime in Task 5.
#[derive(Clone)]
pub(crate) enum CompiledObjectItem {
    Field {
        name: String,
        value: CompiledExpression,
    },
    Spread(CompiledExpression),
}

#[allow(dead_code)] // Fully consumed by the compiled runtime in Task 5.
#[derive(Clone)]
pub(crate) enum CompiledStringPart {
    Literal(String),
    Expression(CompiledExpression),
}

#[allow(dead_code)] // Fully consumed by the compiled runtime in Task 5.
#[derive(Clone)]
pub(crate) enum CompiledStatement {
    StoreLocal {
        slot: LocalSlot,
        value: CompiledExpression,
        span: Span,
    },
    StoreGlobal {
        name: String,
        value: CompiledExpression,
        span: Span,
    },
    Expression(CompiledExpression, Span),
    If {
        condition: CompiledExpression,
        then_body: Vec<CompiledStatement>,
        else_body: Vec<CompiledStatement>,
        span: Span,
    },
    For {
        index_slot: Option<LocalSlot>,
        value_slot: LocalSlot,
        iterable: CompiledExpression,
        body: Vec<CompiledStatement>,
        span: Span,
    },
    Return(Option<CompiledExpression>, Span),
    Break(Span),
    Continue(Span),
    Try {
        body: Vec<CompiledStatement>,
        catch_slot: Option<LocalSlot>,
        handler: Vec<CompiledStatement>,
        span: Span,
    },
}

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
    pub parameter_slots: Vec<LocalSlot>,
    pub slot_count: usize,
    pub local_layout: LocalLayout,
    pub default_values: Vec<Option<CompiledExpression>>,
    pub return_type: SparType,
    pub is_async: bool,
    pub body: Vec<CompiledStatement>,
    pub span: Span,
}

impl CompiledFunction {
    #[cfg(test)]
    pub(crate) fn debug_slot_names(&self) -> Vec<&str> {
        self.local_layout.names.iter().map(String::as_str).collect()
    }
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
        builder.lower_functions().map_err(|error| vec![error])?;
        let entry_main = builder
            .modules
            .first()
            .ok_or_else(|| {
                vec![SparError::EvalError {
                    message: "internal lowering error: entry module is unavailable".into(),
                    span: Span::dummy(),
                }]
            })?
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

    pub(crate) fn compilation_with_errors(&self, errors: Vec<SparError>) -> Compilation {
        let entry = self.modules.get(self.entry.0 as usize);
        Compilation {
            program: entry.map(|module| module.checked.program.clone()),
            symbols: entry.map(|module| module.checked.symbols.clone()),
            imports: entry
                .map(|module| module.checked.imports.clone())
                .unwrap_or_default(),
            result: None,
            tasks: None,
            task_exprs: Vec::new(),
            errors,
        }
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

    #[cfg(test)]
    pub(crate) fn debug_operations(&self) -> Vec<TypedOperation> {
        let mut operations = Vec::new();
        self.visit_expressions(|expression| {
            if let CompiledExpression::Operation { operation, .. } = expression {
                operations.push(*operation);
            }
        });
        operations
    }

    #[cfg(test)]
    pub(crate) fn debug_direct_call_ids(&self) -> Vec<FunctionId> {
        let mut calls = Vec::new();
        self.visit_expressions(|expression| {
            if let CompiledExpression::DirectCall { function, .. } = expression {
                calls.push(*function);
            }
        });
        calls
    }

    #[cfg(test)]
    pub(crate) fn debug_local_read_slots(&self) -> Vec<LocalSlot> {
        let mut slots = Vec::new();
        self.visit_expressions(|expression| {
            if let CompiledExpression::Local(slot, _) = expression {
                slots.push(*slot);
            }
        });
        slots
    }

    #[cfg(test)]
    fn visit_expressions(&self, mut visit: impl FnMut(&CompiledExpression)) {
        for function in self
            .modules
            .iter()
            .flat_map(|module| module.functions.iter())
        {
            for default in function.default_values.iter().flatten() {
                visit_expression(default, &mut visit);
            }
            visit_statements(&function.body, &mut visit);
        }
    }
}

#[cfg(test)]
fn visit_statements(statements: &[CompiledStatement], visit: &mut impl FnMut(&CompiledExpression)) {
    for statement in statements {
        match statement {
            CompiledStatement::StoreLocal { value, .. }
            | CompiledStatement::StoreGlobal { value, .. }
            | CompiledStatement::Expression(value, _) => visit_expression(value, visit),
            CompiledStatement::If {
                condition,
                then_body,
                else_body,
                ..
            } => {
                visit_expression(condition, visit);
                visit_statements(then_body, visit);
                visit_statements(else_body, visit);
            }
            CompiledStatement::For { iterable, body, .. } => {
                visit_expression(iterable, visit);
                visit_statements(body, visit);
            }
            CompiledStatement::Return(value, _) => {
                if let Some(value) = value {
                    visit_expression(value, visit);
                }
            }
            CompiledStatement::Break(_) | CompiledStatement::Continue(_) => {}
            CompiledStatement::Try { .. } => {}
        }
    }
}

#[cfg(test)]
fn visit_expression(expression: &CompiledExpression, visit: &mut impl FnMut(&CompiledExpression)) {
    visit(expression);
    match expression {
        CompiledExpression::DirectCall { arguments, .. }
        | CompiledExpression::HostCall { arguments, .. }
        | CompiledExpression::List(arguments, _)
        | CompiledExpression::Operation {
            operands: arguments,
            ..
        } => {
            for argument in arguments {
                visit_expression(argument, visit);
            }
        }
        CompiledExpression::Object(items, _) => {
            for item in items {
                match item {
                    CompiledObjectItem::Field { value, .. } | CompiledObjectItem::Spread(value) => {
                        visit_expression(value, visit)
                    }
                }
            }
        }
        CompiledExpression::Await { promise, .. } => visit_expression(promise, visit),
        CompiledExpression::Panic { message, .. } => visit_expression(message, visit),
        CompiledExpression::Index { source, index, .. } => {
            visit_expression(source, visit);
            visit_expression(index, visit);
        }
        CompiledExpression::Field { base, .. } => visit_expression(base, visit),
        CompiledExpression::Interpolation(parts, _) => {
            for part in parts {
                if let CompiledStringPart::Expression(value) = part {
                    visit_expression(value, visit);
                }
            }
        }
        CompiledExpression::Comprehension { source, body, .. } => {
            visit_expression(source, visit);
            visit_expression(body, visit);
        }
        CompiledExpression::Constant(_, _)
        | CompiledExpression::Local(_, _)
        | CompiledExpression::Global(_, _)
        | CompiledExpression::ImportedValue { .. }
        | CompiledExpression::Shell(_)
        | CompiledExpression::ExecShell(_) => {}
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
        let functions = self.compile_function_headers(id, &checked.program, &checked.symbols);
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
            self.modules
                .get_mut(id.0 as usize)
                .ok_or_else(|| {
                    vec![SparError::EvalError {
                        message: format!("internal lowering error: unknown module ID {}", id.0),
                        span: Span::dummy(),
                    }]
                })?
                .import_modules
                .insert(alias, module_id);
        }
        Ok(id)
    }

    fn lower_functions(&mut self) -> Result<(), SparError> {
        let function_ids: HashMap<FunctionKey, FunctionId> = self
            .modules
            .iter()
            .flat_map(|module| {
                module
                    .functions
                    .iter()
                    .map(|function| (function.key.clone(), function.id))
            })
            .collect();
        let parameters: HashMap<FunctionId, Vec<String>> = self
            .modules
            .iter()
            .flat_map(|module| {
                function_declarations(&module.checked.program)
                    .into_iter()
                    .zip(module.functions.iter())
                    .map(|(declaration, function)| {
                        (
                            function.id,
                            declaration
                                .params
                                .iter()
                                .map(|parameter| parameter.name.clone())
                                .collect(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let lowered = self
            .modules
            .iter()
            .map(|module| {
                function_declarations(&module.checked.program)
                    .into_iter()
                    .map(|declaration| {
                        lower_function(
                            declaration,
                            &module.checked.symbols,
                            LoweringContext {
                                module: module.id,
                                imports: &module.import_modules,
                                functions: &function_ids,
                                parameters: &parameters,
                            },
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        for (module, lowered_functions) in self.modules.iter_mut().zip(lowered) {
            for (function, lowered) in module.functions.iter_mut().zip(lowered_functions) {
                function.parameter_slots = lowered.layout.parameter_slots.clone();
                function.slot_count = lowered.layout.names.len();
                function.local_layout = lowered.layout;
                function.default_values = lowered.defaults;
                function.body = lowered.body;
            }
        }
        Ok(())
    }

    fn compile_function_headers(
        &mut self,
        module: ModuleId,
        program: &Program,
        symbols: &SymbolTable,
    ) -> Vec<CompiledFunction> {
        let mut functions = Vec::new();
        for item in &program.items {
            match item {
                TopLevelItem::Function(function) => {
                    functions.push(self.function_header(module, None, function, symbols));
                }
                TopLevelItem::FunctionGroup(group) => {
                    for function in &group.functions {
                        functions.push(self.function_header(
                            module,
                            Some(group.name.clone()),
                            function,
                            symbols,
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
        function: &crate::ast::FunctionDecl,
        symbols: &SymbolTable,
    ) -> CompiledFunction {
        let id = FunctionId(self.next_function);
        self.next_function += 1;
        let local_layout = allocate_local_layout(function, symbols);
        CompiledFunction {
            id,
            key: FunctionKey {
                module,
                group,
                name: function.name.clone(),
            },
            name: function.name.clone(),
            parameter_slots: local_layout.parameter_slots.clone(),
            slot_count: local_layout.names.len(),
            local_layout,
            default_values: Vec::new(),
            return_type: function.ret.clone(),
            is_async: function.is_async,
            body: Vec::new(),
            span: function.span.clone(),
        }
    }
}

fn function_declarations(program: &Program) -> Vec<&crate::ast::FunctionDecl> {
    program
        .items
        .iter()
        .flat_map(|item| match item {
            TopLevelItem::Function(function) => vec![function],
            TopLevelItem::FunctionGroup(group) => group.functions.iter().collect(),
            _ => Vec::new(),
        })
        .collect()
}
