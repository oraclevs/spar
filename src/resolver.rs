use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::depgraph::DeclId;
use crate::error::{Span, SparError};
use crate::loader::LoadedImport;
use crate::naming;

// ── Exhaustiveness / scope-exit analysis (pure; no resolver state needed) ─────

/// If `stmts` always returns on every path, returns `None`.
/// Otherwise returns `Some(scope)` — names in scope after the sequence falls through.
pub(crate) fn sequence_exit_scope(stmts: &[FuncStmt]) -> Option<HashMap<String, SparType>> {
    let mut scope: HashMap<String, SparType> = HashMap::new();
    for stmt in stmts {
        match stmt {
            FuncStmt::Return(_, _) => return None,
            FuncStmt::LocalVar(local) => {
                scope.insert(local.name.clone(), local.ty.clone());
            }
            FuncStmt::If(if_stmt) => {
                let then_exit = sequence_exit_scope(&if_stmt.then_stmts);
                let else_exit = sequence_exit_scope(&if_stmt.else_stmts);
                match (then_exit, else_exit) {
                    (None, None) => return None,
                    (Some(names), None) | (None, Some(names)) => scope.extend(names),
                    (Some(then_names), Some(else_names)) => {
                        for (name, ty) in &then_names {
                            if else_names.contains_key(name) {
                                scope.insert(name.clone(), ty.clone());
                            }
                        }
                    }
                }
            }
            // A for-loop never guarantees execution (iterable may be empty).
            FuncStmt::For { .. } => {}
        }
    }
    Some(scope)
}

pub(crate) fn stmts_always_return(stmts: &[FuncStmt]) -> bool {
    sequence_exit_scope(stmts).is_none()
}

fn func_stmt_span(stmt: &FuncStmt) -> Span {
    match stmt {
        FuncStmt::LocalVar(l) => l.span.clone(),
        FuncStmt::If(i) => i.span.clone(),
        FuncStmt::Return(_, s) => s.clone(),
        FuncStmt::For { span, .. } => span.clone(),
    }
}

#[derive(Debug, Clone)]
pub enum GlobalEntry {
    Var {
        ty: SparType,
        optional: bool,
        exported: bool,
        span: Span,
    },
    Dynamic {
        optional: bool,
        span: Span,
    },
}

#[derive(Debug, Clone)]
pub struct SectionEntry {
    pub fields: HashMap<String, FieldEntry>,
    pub exported: bool,
    pub private: bool,
    /// The section's `-> TypeName` binding, if any. Only ever set for a
    /// top-level section (nested sections can't declare their own binding —
    /// their shape comes from the enclosing binding's `TypeFieldShape`).
    /// Lets the typechecker resolve a spread source's declared shape
    /// without needing to walk back through the raw `Program` AST.
    pub type_binding: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FieldEntry {
    /// `None` when the field's type is inferred from a `-> TypeName`
    /// binding rather than declared explicitly (see `FieldDecl.ty`).
    pub ty: Option<SparType>,
    pub optional: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ImportEntry {
    pub path: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FunctionEntry {
    pub params: Vec<(String, SparType)>,
    pub ret: SparType,
    pub span: Span,
    pub closure_deps: HashSet<DeclId>,
    pub is_private: bool,
}

#[derive(Debug, Clone)]
pub struct TypeEntry {
    pub fields: Vec<TypeField>,
    pub exported: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FunctionGroupEntry {
    pub is_private: bool,
    pub functions: HashMap<String, FunctionEntry>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct EnumEntry {
    pub variants: Vec<String>,
    pub exported: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct TaskEntry {
    pub params: Vec<(String, SparType)>,
    pub depends_on: Vec<String>,
    pub span: Span,
    pub name_span: Span,
}

#[derive(Debug, Clone)]
pub struct SymbolTable {
    pub globals: HashMap<String, GlobalEntry>,
    pub sections: HashMap<Vec<String>, SectionEntry>,
    pub imports: HashMap<String, ImportEntry>,
    pub functions: HashMap<String, FunctionEntry>,
    pub types: HashMap<String, TypeEntry>,
    pub enums: HashMap<String, EnumEntry>,
    pub function_groups: HashMap<String, FunctionGroupEntry>,
    pub tasks: HashMap<String, TaskEntry>,
}

impl SymbolTable {
    pub fn lookup_task(&self, name: &str) -> Option<&TaskEntry> {
        self.tasks.get(name)
    }
    pub fn lookup_global(&self, name: &str) -> Option<&GlobalEntry> {
        self.globals.get(name)
    }

    pub fn lookup_section(&self, path: &[String]) -> Option<&SectionEntry> {
        self.sections.get(path)
    }

    pub fn lookup_import(&self, alias: &str) -> Option<&ImportEntry> {
        self.imports.get(alias)
    }

    pub fn lookup_function(&self, name: &str) -> Option<&FunctionEntry> {
        self.functions.get(name)
    }

    pub fn lookup_function_group(&self, name: &str) -> Option<&FunctionGroupEntry> {
        self.function_groups.get(name)
    }

    pub fn lookup_type(&self, name: &str) -> Option<&TypeEntry> {
        self.types.get(name)
    }
}

// ── Levenshtein + suggestion helpers ─────────────────────────────────────────

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = a.len();
    let n = b.len();
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, val) in dp[0].iter_mut().enumerate() {
        *val = j;
    }
    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = if a[i - 1] == b[j - 1] {
                dp[i - 1][j - 1]
            } else {
                1 + dp[i - 1][j].min(dp[i][j - 1]).min(dp[i - 1][j - 1])
            };
        }
    }
    dp[m][n]
}

pub(crate) fn suggest(
    name: &str,
    candidates: impl Iterator<Item = impl AsRef<str>>,
) -> Option<String> {
    candidates
        .map(|c| {
            let s = c.as_ref().to_string();
            let d = levenshtein(name, &s);
            (s, d)
        })
        .filter(|(_, d)| *d > 0 && *d <= 2)
        .min_by_key(|(_, d)| *d)
        .map(|(s, _)| format!("did you mean `{s}`?"))
}

// ── Resolver ──────────────────────────────────────────────────────────────────

pub struct Resolver {
    globals: HashMap<String, GlobalEntry>,
    sections: HashMap<Vec<String>, SectionEntry>,
    imports: HashMap<String, ImportEntry>,
    functions: HashMap<String, FunctionEntry>,
    types: HashMap<String, TypeEntry>,
    enums: HashMap<String, EnumEntry>,
    function_groups: HashMap<String, FunctionGroupEntry>,
    tasks: HashMap<String, TaskEntry>,
    loaded_exports: HashMap<String, HashSet<String>>, // alias → exported names
    errors: Vec<SparError>,
    current_section: Option<Vec<String>>,
}

impl Resolver {
    pub fn new() -> Self {
        Self {
            globals: HashMap::new(),
            sections: HashMap::new(),
            imports: HashMap::new(),
            functions: HashMap::new(),
            types: HashMap::new(),
            enums: HashMap::new(),
            function_groups: HashMap::new(),
            tasks: HashMap::new(),
            loaded_exports: HashMap::new(),
            errors: Vec::new(),
            current_section: None,
        }
    }

    fn with_loaded(exports: HashMap<String, HashSet<String>>) -> Self {
        Self {
            globals: HashMap::new(),
            sections: HashMap::new(),
            imports: HashMap::new(),
            functions: HashMap::new(),
            types: HashMap::new(),
            enums: HashMap::new(),
            function_groups: HashMap::new(),
            tasks: HashMap::new(),
            loaded_exports: exports,
            errors: Vec::new(),
            current_section: None,
        }
    }

    /// Resolve a program with no imports (or with a pre-built slice of imports).
    /// The instance-method form enables `Resolver::new().resolve(&prog, &[])`.
    pub fn resolve(
        mut self,
        program: &Program,
        loaded_imports: &[LoadedImport],
    ) -> Result<SymbolTable, Vec<SparError>> {
        // Populate loaded_exports from the slice (use path stem as alias)
        for li in loaded_imports {
            let alias = li
                .path
                .rsplit('/')
                .next()
                .unwrap_or(&li.path)
                .trim_end_matches(".spar")
                .to_string();
            self.loaded_exports.insert(alias, li.exports.clone());
        }
        self.register(program);
        self.check_function_group_import_collisions();
        self.resolve_program(program);
        self.resolve_function_bodies(program);
        if self.errors.is_empty() {
            Ok(SymbolTable {
                globals: self.globals,
                sections: self.sections,
                imports: self.imports,
                functions: self.functions,
                types: self.types,
                enums: self.enums,
                function_groups: self.function_groups,
                tasks: self.tasks,
            })
        } else {
            Err(self.errors)
        }
    }

    pub fn resolve_with_imports(
        program: &Program,
        loaded: &HashMap<String, LoadedImport>,
    ) -> Result<SymbolTable, Vec<SparError>> {
        // Build alias → exported names map
        let exports: HashMap<String, HashSet<String>> = loaded
            .iter()
            .map(|(alias, li)| (alias.clone(), li.exports.clone()))
            .collect();

        let mut r = Resolver::with_loaded(exports);
        r.register(program);
        r.check_function_group_import_collisions();
        r.resolve_program(program);
        r.resolve_function_bodies(program);
        if r.errors.is_empty() {
            Ok(SymbolTable {
                globals: r.globals,
                sections: r.sections,
                imports: r.imports,
                functions: r.functions,
                types: r.types,
                enums: r.enums,
                function_groups: r.function_groups,
                tasks: r.tasks,
            })
        } else {
            Err(r.errors)
        }
    }

    fn push_error(&mut self, message: impl Into<String>, span: Span) {
        self.errors.push(SparError::ResolveError {
            message: message.into(),
            hint: None,
            span,
        });
    }

    fn push_error_hint(&mut self, message: impl Into<String>, hint: Option<String>, span: Span) {
        self.errors.push(SparError::ResolveError {
            message: message.into(),
            hint,
            span,
        });
    }

    fn check_function_group_import_collisions(&mut self) {
        let colliding: Vec<(String, Span)> = self
            .function_groups
            .iter()
            .filter(|(name, _)| self.imports.contains_key(name.as_str()))
            .map(|(name, entry)| (name.clone(), entry.span.clone()))
            .collect();
        for (name, span) in colliding {
            self.push_error(
                format!(
                    "functionGroup '{name}' has the same name as an import alias — \
                     rename one of them to avoid ambiguous '{name}::...' calls"
                ),
                span,
            );
        }
    }
}

// ── Pass 1: Registration ──────────────────────────────────────────────────────

impl Resolver {
    fn register(&mut self, program: &Program) {
        for item in &program.items {
            match item {
                TopLevelItem::Import(decl) => self.register_import(decl),
                TopLevelItem::Var(decl) => self.register_var(decl),
                TopLevelItem::Dynamic(decl) => self.register_dynamic(decl),
                TopLevelItem::Section(decl) => self.register_section(decl),
                TopLevelItem::Function(decl) => self.register_function(decl),
                TopLevelItem::SchemaSection(_) => {}
                TopLevelItem::Type(decl) => self.register_type(decl),
                TopLevelItem::Enum(decl) => self.register_enum(decl),
                TopLevelItem::FunctionGroup(decl) => self.register_function_group(decl),
                TopLevelItem::SchemaFrom(_) => {} // never reaches the resolver — schema files aren't resolved (loader.rs handles them out-of-band)
                TopLevelItem::Task(decl) => self.register_task(decl),
            }
        }
    }

    fn register_type(&mut self, decl: &TypeDecl) {
        if decl.name == "Schema" {
            self.push_error(
                "'Schema' is reserved and cannot be used as a type name — \
                 it already means a `Schema [Name]{...}` file-level contract",
                decl.name_span.clone(),
            );
            return;
        }

        if !naming::is_pascal_case(&decl.name) {
            self.push_error_hint(
                format!(
                    "type name '{}' must be PascalCase (start with an uppercase letter, no underscores)",
                    decl.name
                ),
                Some(naming::pascal_case_hint(&decl.name)),
                decl.name_span.clone(),
            );
            // Do NOT return — continue registering so other errors can be found
        }

        if self.types.contains_key(&decl.name) {
            self.push_error(
                format!("type '{}' is already defined", decl.name),
                decl.name_span.clone(),
            );
            return;
        }

        if self.enums.contains_key(&decl.name) {
            self.push_error(
                format!(
                    "'{}' is already declared as an enum — a type can't share a name with an enum",
                    decl.name
                ),
                decl.name_span.clone(),
            );
            return;
        }

        self.types.insert(
            decl.name.clone(),
            TypeEntry {
                fields: decl.fields.clone(),
                exported: decl.exported,
                span: decl.span.clone(),
            },
        );
    }

    fn register_enum(&mut self, decl: &EnumDecl) {
        if !naming::is_pascal_case(&decl.name) {
            self.push_error_hint(
                format!(
                    "enum name '{}' must be PascalCase (start with an uppercase letter, no underscores)",
                    decl.name
                ),
                Some(naming::pascal_case_hint(&decl.name)),
                decl.name_span.clone(),
            );
            // Do NOT return — continue registering so other errors can be found
        }

        if self.types.contains_key(&decl.name) {
            self.push_error(
                format!(
                    "'{}' is already declared as a type — an enum can't share a name with a type",
                    decl.name
                ),
                decl.name_span.clone(),
            );
            return;
        }

        if self.enums.contains_key(&decl.name) {
            self.push_error(
                format!("enum '{}' is already defined", decl.name),
                decl.name_span.clone(),
            );
            return;
        }

        self.enums.insert(
            decl.name.clone(),
            EnumEntry {
                variants: decl.variants.clone(),
                exported: decl.exported,
                span: decl.span.clone(),
            },
        );
    }

    fn register_function(&mut self, decl: &FunctionDecl) {
        if self.functions.contains_key(&decl.name) {
            self.push_error(
                format!("function '{}' is already defined", decl.name),
                decl.name_span.clone(),
            );
            return;
        }
        let entry = self.build_function_entry(decl);
        self.functions.insert(decl.name.clone(), entry);
    }

    /// Builds a `FunctionEntry` for a single function declaration — naming
    /// convention checks, param validation, and signature capture. Does NOT
    /// check for duplicate names (top-level and functionGroup callers use
    /// different maps and different duplicate-detection scopes) and does NOT
    /// insert into any map — callers own that.
    fn build_function_entry(&mut self, decl: &FunctionDecl) -> FunctionEntry {
        if !naming::is_camel_case(&decl.name) {
            self.push_error_hint(
                format!(
                    "function name '{}' must be camelCase (start with a lowercase letter, no underscores)",
                    decl.name
                ),
                Some(naming::camel_case_hint(&decl.name)),
                decl.name_span.clone(),
            );
            // continue — still register so subsequent errors can be found
        }
        let mut params: Vec<(String, SparType)> = Vec::new();
        for param in &decl.params {
            if matches!(param.ty, SparType::Section) {
                self.push_error(
                    format!(
                        "param '{}': section type is not allowed for function parameters",
                        param.name
                    ),
                    param.span.clone(),
                );
                // Do NOT return — collect further errors
            }
            if !naming::is_camel_case(&param.name) {
                self.push_error_hint(
                    format!(
                        "param '{}' must be camelCase (start with a lowercase letter, no underscores)",
                        param.name
                    ),
                    Some(naming::camel_case_hint(&param.name)),
                    param.span.clone(),
                );
            }
            params.push((param.name.clone(), param.ty.clone()));
        }
        FunctionEntry {
            params,
            ret: decl.ret.clone(),
            span: decl.name_span.clone(),
            closure_deps: HashSet::new(), // computed in Pass 3
            is_private: decl.is_private,
        }
    }

    fn register_function_group(&mut self, decl: &FunctionGroupDecl) {
        if self.function_groups.contains_key(&decl.name) {
            self.push_error(
                format!("functionGroup '{}' is already defined", decl.name),
                decl.name_span.clone(),
            );
            return;
        }
        if !naming::is_pascal_case(&decl.name) {
            self.push_error_hint(
                format!(
                    "functionGroup name '{}' must be PascalCase (start with an uppercase letter, no underscores)",
                    decl.name
                ),
                Some(naming::pascal_case_hint(&decl.name)),
                decl.name_span.clone(),
            );
        }

        let mut functions: HashMap<String, FunctionEntry> = HashMap::new();
        for f in &decl.functions {
            if functions.contains_key(&f.name) {
                self.push_error(
                    format!(
                        "function '{}' is already defined in functionGroup '{}'",
                        f.name, decl.name
                    ),
                    f.name_span.clone(),
                );
                continue;
            }
            let entry = self.build_function_entry(f);
            functions.insert(f.name.clone(), entry);
        }

        self.function_groups.insert(
            decl.name.clone(),
            FunctionGroupEntry {
                is_private: decl.is_private,
                functions,
                span: decl.span.clone(),
            },
        );
    }

    fn register_task(&mut self, decl: &TaskDecl) {
        if self.tasks.contains_key(&decl.name) {
            self.push_error(
                format!("task '{}' is already defined", decl.name),
                decl.name_span.clone(),
            );
            return;
        }
        if !naming::is_pascal_case(&decl.name) {
            self.push_error_hint(
                format!(
                    "task name '{}' must be PascalCase (start with an uppercase letter, no underscores)",
                    decl.name
                ),
                Some(naming::pascal_case_hint(&decl.name)),
                decl.name_span.clone(),
            );
        }
        let params = decl
            .params
            .iter()
            .map(|p| (p.name.clone(), p.ty.clone()))
            .collect();
        let depends_on = decl.depends_on.iter().map(|d| d.name.clone()).collect();
        self.tasks.insert(
            decl.name.clone(),
            TaskEntry {
                params,
                depends_on,
                span: decl.span.clone(),
                name_span: decl.name_span.clone(),
            },
        );
    }

    fn register_import(&mut self, decl: &ImportDecl) {
        // Schema imports are consumed by the validation pass; Selective /
        // TypeSelective / AsPartOf imports are already spliced away by
        // loader::expand_imports before resolve ever runs — only a plain
        // aliased import reaches this function.
        let ImportKind::Aliased(alias) = &decl.kind else {
            return;
        };

        let namespace = alias.clone().unwrap_or_else(|| {
            decl.path
                .rsplit('/')
                .next()
                .unwrap_or(&decl.path)
                .trim_end_matches(".spar")
                .to_string()
        });

        if !naming::is_camel_case(&namespace) {
            self.push_error_hint(
                format!(
                    "import alias '{}' must be camelCase (start with a lowercase letter, no underscores)",
                    namespace
                ),
                Some(naming::camel_case_hint(&namespace)),
                decl.span.clone(),
            );
        }

        if self.imports.contains_key(&namespace) {
            self.push_error(
                format!("duplicate import namespace `{namespace}` — use `as` to give one an alias"),
                decl.span.clone(),
            );
            return;
        }

        self.imports.insert(
            namespace,
            ImportEntry {
                path: decl.path.clone(),
                span: decl.span.clone(),
            },
        );
    }

    fn register_var(&mut self, decl: &VarDecl) {
        if self.globals.contains_key(&decl.name) {
            self.push_error(
                format!(
                    "duplicate declaration: `{}` is already declared in the global scope",
                    decl.name
                ),
                decl.span.clone(),
            );
            return;
        }
        if !naming::is_camel_case(&decl.name) {
            self.push_error_hint(
                format!(
                    "variable '{}' must be camelCase (start with a lowercase letter, no underscores)",
                    decl.name
                ),
                Some(naming::camel_case_hint(&decl.name)),
                decl.span.clone(),
            );
        }
        self.globals.insert(
            decl.name.clone(),
            GlobalEntry::Var {
                ty: decl.ty.clone(),
                optional: decl.optional,
                exported: decl.exported,
                span: decl.span.clone(),
            },
        );
    }

    fn register_dynamic(&mut self, decl: &DynamicDecl) {
        if self.globals.contains_key(&decl.name) {
            self.push_error(
                format!(
                    "duplicate declaration: `{}` is already declared in the global scope",
                    decl.name
                ),
                decl.span.clone(),
            );
            return;
        }
        self.globals.insert(
            decl.name.clone(),
            GlobalEntry::Dynamic {
                optional: decl.optional,
                span: decl.span.clone(),
            },
        );
    }

    fn register_section(&mut self, decl: &SectionDecl) {
        if decl.path.first().is_some_and(|s| s == "global") {
            self.push_error(
                "`global` is a reserved namespace and cannot be used as a section name",
                decl.span.clone(),
            );
            return;
        }

        // Naming: section names must be PascalCase
        if let Some(name) = decl.path.first() {
            if !naming::is_pascal_case(name) {
                self.push_error_hint(
                    format!(
                        "section name '{}' must be PascalCase (start with an uppercase letter, no underscores)",
                        name
                    ),
                    Some(naming::pascal_case_hint(name)),
                    decl.span.clone(),
                );
                // Do NOT return — continue registering the section so other errors can be found
            }
        }

        if self.sections.contains_key(&decl.path) {
            self.push_error(
                format!(
                    "duplicate section `[{}]` — each section path must be unique",
                    decl.path.join(".")
                ),
                decl.span.clone(),
            );
            return;
        }

        let mut fields = HashMap::new();
        for item in &decl.items {
            if let SectionItem::Field(f) = item {
                if fields.contains_key(&f.name) {
                    self.push_error(
                        format!(
                            "duplicate field `{}` in section `[{}]`",
                            f.name,
                            decl.path.join(".")
                        ),
                        f.span.clone(),
                    );
                } else {
                    if !naming::is_camel_case(&f.name) {
                        self.push_error_hint(
                            format!(
                                "field '{}' must be camelCase (start with a lowercase letter, no underscores)",
                                f.name
                            ),
                            Some(naming::camel_case_hint(&f.name)),
                            f.span.clone(),
                        );
                    }
                    fields.insert(
                        f.name.clone(),
                        FieldEntry {
                            ty: f.ty.clone(),
                            optional: f.optional,
                            span: f.span.clone(),
                        },
                    );
                }
            }
        }

        self.sections.insert(
            decl.path.clone(),
            SectionEntry {
                fields,
                type_binding: decl.type_binding.as_ref().map(|b| b.name.clone()),
                exported: decl.exported,
                private: decl.private,
                span: decl.span.clone(),
            },
        );

        // Register nested section-type fields recursively. A field is a
        // nested section if its value is FieldValue::Nested, regardless
        // of whether its type is explicit (Some(Section)) or inferred
        // (None, from a `-> TypeName` binding).
        for item in &decl.items {
            if let SectionItem::Field(f) = item {
                if let Some(FieldValue::Nested(sub_fields)) = &f.value {
                    let nested_path = [decl.path.as_slice(), &[f.name.clone()]].concat();
                    self.register_nested_section(nested_path, sub_fields);
                }
            }
        }
    }

    fn register_nested_section(&mut self, path: Vec<String>, items: &[SectionItem]) {
        if self.sections.contains_key(&path) {
            return; // already registered (e.g. via a second spread of the same path)
        }
        let mut field_map = HashMap::new();
        for item in items {
            // A spread contributes fields only known at eval time — can't
            // statically know their names, so nothing to register here.
            let SectionItem::Field(field) = item else {
                continue;
            };

            if matches!(field.value, Some(FieldValue::Nested(_))) {
                if field_map.contains_key(&field.name) {
                    self.push_error(
                        format!(
                            "duplicate field `{}` in section `[{}]`",
                            field.name,
                            path.join(".")
                        ),
                        field.span.clone(),
                    );
                } else {
                    // Recurse for deeper nesting
                    if let Some(FieldValue::Nested(sub)) = &field.value {
                        let nested_path = [path.as_slice(), &[field.name.clone()]].concat();
                        self.register_nested_section(nested_path, sub);
                    }
                    if !naming::is_camel_case(&field.name) {
                        self.push_error_hint(
                            format!(
                                "field '{}' must be camelCase (start with a lowercase letter, no underscores)",
                                field.name
                            ),
                            Some(naming::camel_case_hint(&field.name)),
                            field.span.clone(),
                        );
                    }
                    field_map.insert(
                        field.name.clone(),
                        FieldEntry {
                            ty: field.ty.clone(),
                            optional: field.optional,
                            span: field.span.clone(),
                        },
                    );
                }
            } else {
                if field_map.contains_key(&field.name) {
                    self.push_error(
                        format!(
                            "duplicate field `{}` in section `[{}]`",
                            field.name,
                            path.join(".")
                        ),
                        field.span.clone(),
                    );
                } else {
                    if !naming::is_camel_case(&field.name) {
                        self.push_error_hint(
                            format!(
                                "field '{}' must be camelCase (start with a lowercase letter, no underscores)",
                                field.name
                            ),
                            Some(naming::camel_case_hint(&field.name)),
                            field.span.clone(),
                        );
                    }
                    field_map.insert(
                        field.name.clone(),
                        FieldEntry {
                            ty: field.ty.clone(),
                            optional: field.optional,
                            span: field.span.clone(),
                        },
                    );
                }
            }
        }
        let span = items
            .first()
            .map(|it| match it {
                SectionItem::Field(f) => f.span.clone(),
                SectionItem::Spread(s) => s.span.clone(),
            })
            .unwrap_or_else(Span::dummy);
        self.sections.insert(
            path.clone(),
            SectionEntry {
                fields: field_map,
                type_binding: None, // a nested section can't declare its own `-> Type` binding
                exported: false,
                private: false, // nested sections inherit parent privacy at emit time only
                span,
            },
        );
    }
}

// ── Pass 2: Resolution ────────────────────────────────────────────────────────

impl Resolver {
    fn resolve_program(&mut self, program: &Program) {
        for item in &program.items {
            match item {
                TopLevelItem::Import(_) => {}
                TopLevelItem::Var(decl) => {
                    self.check_named_type_exists(&decl.ty, &decl.span);
                    if let Some(val) = &decl.value {
                        self.resolve_expr(val);
                    }
                }
                TopLevelItem::Dynamic(decl) => {
                    if let Some(val) = &decl.value {
                        self.resolve_expr(val);
                    }
                }
                TopLevelItem::Section(decl) => self.resolve_section(decl),
                TopLevelItem::Function(f) => {
                    for p in &f.params {
                        self.check_named_type_exists(&p.ty, &p.span);
                    }
                    self.check_named_type_exists(&f.ret, &f.ret_span);
                } // function BODIES still handled in resolve_function_bodies
                TopLevelItem::SchemaSection(_) => {}
                TopLevelItem::Type(decl) => self.resolve_type(decl),
                TopLevelItem::Enum(_) => {} // nothing to resolve — no field expressions, registration already validated it
                TopLevelItem::FunctionGroup(g) => {
                    for f in &g.functions {
                        for p in &f.params {
                            self.check_named_type_exists(&p.ty, &p.span);
                        }
                        self.check_named_type_exists(&f.ret, &f.ret_span);
                    }
                }
                TopLevelItem::SchemaFrom(_) => {} // never reaches the resolver — schema files aren't resolved (loader.rs handles them out-of-band)
                TopLevelItem::Task(decl) => self.resolve_task(decl),
            }
        }
    }

    /// Pass 3: resolve function bodies and compute closure dependencies.
    fn resolve_function_bodies(&mut self, program: &Program) {
        for item in &program.items {
            match item {
                TopLevelItem::Function(f) => {
                    let deps = self.resolve_one_function_body(f);
                    if let Some(entry) = self.functions.get_mut(&f.name) {
                        entry.closure_deps = deps;
                    }
                }
                TopLevelItem::FunctionGroup(g) => {
                    for f in &g.functions {
                        let deps = self.resolve_one_function_body(f);
                        if let Some(entry) = self
                            .function_groups
                            .get_mut(&g.name)
                            .and_then(|ge| ge.functions.get_mut(&f.name))
                        {
                            entry.closure_deps = deps;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Resolves a single function's body (statement resolution, exhaustive-
    /// return check, unreachable-code check, closure-dependency collection),
    /// used for both top-level functions and functions nested in a
    /// functionGroup. Returns the computed closure deps; the caller decides
    /// which map to store them in.
    fn resolve_one_function_body(&mut self, f: &FunctionDecl) -> HashSet<DeclId> {
        let param_names: HashSet<String> = f.params.iter().map(|p| p.name.clone()).collect();
        let mut local_names = param_names.clone();

        self.resolve_func_stmts(&f.body.stmts, &mut local_names);

        if !stmts_always_return(&f.body.stmts) {
            self.errors.push(SparError::ResolveError {
                message: format!(
                    "function '{}' does not guarantee a value is returned on every \
                     possible path — add a 'return' that covers the remaining case(s)",
                    f.name
                ),
                hint: None,
                span: f.span.clone(),
            });
        }

        let stmts = f.body.stmts.clone();
        self.check_unreachable(&stmts);

        let mut deps: HashSet<DeclId> = HashSet::new();
        self.collect_closure_deps_stmts(&f.body.stmts, &param_names, &mut deps);
        deps
    }

    fn check_unreachable(&mut self, stmts: &[FuncStmt]) {
        let mut terminated = false;
        for stmt in stmts {
            if terminated {
                self.errors.push(SparError::ResolveError {
                    message: "unreachable code: every path before this statement \
                               already returns, so it can never execute"
                        .to_string(),
                    hint: None,
                    span: func_stmt_span(stmt),
                });
            }
            match stmt {
                FuncStmt::Return(_, _) => {
                    terminated = true;
                }
                FuncStmt::LocalVar(_) => {}
                FuncStmt::If(if_stmt) => {
                    let then_stmts = if_stmt.then_stmts.clone();
                    let else_stmts = if_stmt.else_stmts.clone();
                    self.check_unreachable(&then_stmts);
                    self.check_unreachable(&else_stmts);
                    if stmts_always_return(&then_stmts) && stmts_always_return(&else_stmts) {
                        terminated = true;
                    }
                }
                FuncStmt::For { body, .. } => {
                    let body = body.clone();
                    self.check_unreachable(&body);
                    // A for-loop never sets terminated — iterable may be empty.
                }
            }
        }
    }

    /// Resolves a task's expressions. Metadata fields (`description`,
    /// `default`, `quiet`, `cwd`, `env` values) may only reference ordinary
    /// global/section names — they're pre-evaluated by `task_lowering`
    /// before any task runs, so a task parameter (whose value isn't known
    /// until the CLI binds it) can't appear there. Only the `run` block's
    /// `${...}` interpolations may reference task parameters, via the same
    /// locals-aware resolution function bodies use.
    fn resolve_task(&mut self, decl: &TaskDecl) {
        for dep in &decl.depends_on {
            if !self.tasks.contains_key(&dep.name) {
                let candidates: Vec<String> = self.tasks.keys().cloned().collect();
                let hint = suggest(&dep.name, candidates.iter().map(|s| s.as_str()));
                self.push_error_hint(
                    format!(
                        "task '{}' depends on unknown task '{}'",
                        decl.name, dep.name
                    ),
                    hint,
                    dep.span.clone(),
                );
            }
        }

        if let Some(expr) = &decl.description {
            self.resolve_expr(expr);
        }
        if let Some(expr) = &decl.default {
            self.resolve_expr(expr);
        }
        if let Some(expr) = &decl.quiet {
            self.resolve_expr(expr);
        }
        if let Some(expr) = &decl.cwd {
            self.resolve_expr(expr);
        }
        for (_, value) in &decl.env {
            self.resolve_expr(value);
        }

        let locals: HashSet<String> = decl.params.iter().map(|p| p.name.clone()).collect();
        for command in &decl.run {
            for part in &command.parts {
                if let ShellTemplatePart::Expr(expr) = part {
                    if let Err(e) = self.resolve_expr_with_locals(expr, &locals) {
                        self.errors.push(e);
                    }
                }
            }
        }
    }

    fn resolve_section(&mut self, decl: &SectionDecl) {
        let prev_section = self.current_section.replace(decl.path.clone());
        if let Some(binding) = &decl.type_binding {
            if !self.types.contains_key(&binding.name) {
                let candidates: Vec<String> = self.types.keys().cloned().collect();
                let hint = suggest(&binding.name, candidates.iter().map(|s| s.as_str()));
                self.push_error_hint(
                    format!("undefined type: `{}` is not declared", binding.name),
                    hint,
                    binding.span.clone(),
                );
            }
        }
        for item in &decl.items {
            match item {
                SectionItem::Field(f) => {
                    if let Some(ty) = &f.ty {
                        self.check_named_type_exists(ty, &f.span);
                    }
                    match &f.value {
                        Some(FieldValue::Expr(val)) => self.resolve_expr(val),
                        Some(FieldValue::Nested(sub_items)) => {
                            self.resolve_nested_fields(sub_items);
                        }
                        None => {}
                    }
                }
                SectionItem::Spread(s) => self.resolve_spread(s),
            }
        }
        self.current_section = prev_section;
    }

    fn resolve_nested_fields(&mut self, items: &[SectionItem]) {
        for item in items {
            match item {
                SectionItem::Field(field) => {
                    if let Some(ty) = &field.ty {
                        self.check_named_type_exists(ty, &field.span);
                    }
                    match &field.value {
                        Some(FieldValue::Expr(val)) => self.resolve_expr(val),
                        Some(FieldValue::Nested(sub)) => self.resolve_nested_fields(sub),
                        None => {}
                    }
                }
                SectionItem::Spread(s) => self.resolve_spread(s),
            }
        }
    }

    fn resolve_type(&mut self, decl: &TypeDecl) {
        self.resolve_type_fields(&decl.fields);
    }

    fn resolve_type_fields(&mut self, fields: &[TypeField]) {
        for field in fields {
            match &field.shape {
                TypeFieldShape::Primitive(_) => {}
                TypeFieldShape::Named(name) => {
                    if !self.types.contains_key(name) && !self.enums.contains_key(name) {
                        let candidates: Vec<String> = self
                            .types
                            .keys()
                            .chain(self.enums.keys())
                            .cloned()
                            .collect();
                        let hint = suggest(name, candidates.iter().map(|s| s.as_str()));
                        self.push_error_hint(
                            format!("undefined type: `{name}` is not declared"),
                            hint,
                            field.span.clone(),
                        );
                    }
                }
                TypeFieldShape::Section(nested) => self.resolve_type_fields(nested),
            }
        }
    }

    /// Checks that a `SparType::Named(name)` — including nested inside
    /// `List(...)` — refers to a declared type. No-op for every other
    /// `SparType` variant. Mirrors the "undefined type" error
    /// `resolve_type_fields` already raises for `TypeFieldShape::Named`.
    fn check_named_type_exists(&mut self, ty: &SparType, span: &Span) {
        match ty {
            SparType::Named(name) => {
                if !self.types.contains_key(name) && !self.enums.contains_key(name) {
                    let candidates: Vec<String> = self
                        .types
                        .keys()
                        .chain(self.enums.keys())
                        .cloned()
                        .collect();
                    let hint = suggest(name, candidates.iter().map(|s| s.as_str()));
                    self.push_error_hint(
                        format!("undefined type: `{}` is not declared", name),
                        hint,
                        span.clone(),
                    );
                }
            }
            SparType::List(inner) => self.check_named_type_exists(inner, span),
            _ => {}
        }
    }

    fn resolve_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Object(items, _) => self.resolve_nested_fields(items),
            Expr::Literal(_) => {}
            Expr::NamespaceRef(nr) => self.resolve_namespace_ref(nr),
            Expr::FieldAccess {
                base, field, span, ..
            } => self.resolve_field_access(base, field, span),
            Expr::FnCall(fc) => {
                for arg in &fc.args {
                    self.resolve_expr(arg);
                }
            }
            Expr::BinaryOp(op) => {
                self.resolve_expr(&op.lhs);
                self.resolve_expr(&op.rhs);
            }
            Expr::String(s) => {
                for part in &s.parts {
                    if let StringPart::Expr(e) = part {
                        self.resolve_expr(e);
                    }
                }
            }
            Expr::List(items, _) => {
                for item in items {
                    self.resolve_expr(item);
                }
            }
            Expr::Grouped(inner, _) => self.resolve_expr(inner),
            Expr::Call {
                name,
                name_span,
                args,
                ..
            } => {
                let segments: Vec<&str> = name.split("::").collect();
                match segments.len() {
                    3 => {
                        // Cross-file functionGroup call: alias::Group::fn(...)
                        let alias = segments[0];
                        let group = segments[1];
                        if self.imports.contains_key(alias)
                            || self.loaded_exports.contains_key(alias)
                        {
                            if let Some(exports) = self.loaded_exports.get(alias) {
                                if !exports.contains(group) {
                                    self.push_error(
                                        format!("'{group}' is not exported by import '{alias}'"),
                                        name_span.clone(),
                                    );
                                }
                            }
                            // `fn_name` existence within the group is deferred:
                            // `loaded_exports` is a flat name-only set, it
                            // doesn't carry a group's inner function names.
                        } else {
                            self.push_error(
                                format!("undefined function '{name}'"),
                                name_span.clone(),
                            );
                        }
                        for arg in args {
                            self.resolve_expr(&arg.value);
                        }
                    }
                    2 => {
                        let ns = segments[0];
                        let fn_name = segments[1];
                        if let Some(group) = self.function_groups.get(ns) {
                            if !group.functions.contains_key(fn_name) {
                                self.push_error(
                                    format!(
                                        "function '{fn_name}' not found in functionGroup '{ns}'"
                                    ),
                                    name_span.clone(),
                                );
                            }
                            for arg in args {
                                self.resolve_expr(&arg.value);
                            }
                        } else if self.imports.contains_key(ns)
                            || self.loaded_exports.contains_key(ns)
                        {
                            if let Some(exports) = self.loaded_exports.get(ns) {
                                if !exports.contains(fn_name) {
                                    self.push_error(
                                        format!("function '{fn_name}' not exported from '{ns}'"),
                                        name_span.clone(),
                                    );
                                }
                            }
                            for arg in args {
                                self.resolve_expr(&arg.value);
                            }
                        } else {
                            self.push_error(
                                format!("undefined function '{name}'"),
                                name_span.clone(),
                            );
                            for arg in args {
                                self.resolve_expr(&arg.value);
                            }
                        }
                    }
                    _ => {
                        // 1 segment: local plain function call.
                        if let Some(entry) = self.functions.get(name.as_str()).cloned() {
                            let param_names: HashSet<String> =
                                entry.params.iter().map(|(n, _)| n.clone()).collect();
                            let mut seen: HashSet<String> = HashSet::new();
                            for arg in args {
                                if !param_names.contains(&arg.param_name) {
                                    self.push_error(
                                        format!(
                                            "function '{}' has no param '{}'",
                                            name, arg.param_name
                                        ),
                                        arg.param_name_span.clone(),
                                    );
                                } else if !seen.insert(arg.param_name.clone()) {
                                    self.push_error(
                                        format!("duplicate argument '{}'", arg.param_name),
                                        arg.param_name_span.clone(),
                                    );
                                }
                                self.resolve_expr(&arg.value);
                            }
                            let missing: Vec<_> = param_names
                                .iter()
                                .filter(|p| !seen.contains(p.as_str()))
                                .collect();
                            if !missing.is_empty() {
                                self.push_error(
                                    format!(
                                        "missing arguments for function '{}': {:?}",
                                        name, missing
                                    ),
                                    name_span.clone(),
                                );
                            }
                        } else {
                            self.push_error(
                                format!("undefined function '{name}'"),
                                name_span.clone(),
                            );
                            for arg in args {
                                self.resolve_expr(&arg.value);
                            }
                        }
                    }
                }
            }
            Expr::Unary { operand, .. } => self.resolve_expr(operand),
            Expr::Index { source, index, .. } => {
                self.resolve_expr(source);
                self.resolve_expr(index);
            }
            Expr::Comprehension {
                var_name,
                source,
                body,
                ..
            } => {
                self.resolve_expr(source);
                // The body can reference the comprehension variable
                let mut locals = HashSet::new();
                locals.insert(var_name.clone());
                if let Err(e) = self.resolve_expr_with_locals(body, &locals) {
                    self.errors.push(e);
                }
            }
        }
    }

    fn resolve_namespace_ref(&mut self, nr: &NamespaceRef) {
        match nr.segments.as_slice() {
            // ── 1 segment ────────────────────────────────────────────────────
            [name] => {
                if name == "self" || name == "global" {
                    // Bare self/global (not followed by `.field`) is never
                    // a value on its own — FieldAccess resolution handles
                    // the `self.x`/`global.x` case before ever calling
                    // this function on a bare self/global NamespaceRef.
                    self.push_error(
                        format!(
                            "`{name}` must be followed by `.field` — bare `{name}` is not a value"
                        ),
                        nr.span.clone(),
                    );
                    return;
                }
                if !self.globals.contains_key(name.as_str()) {
                    let candidates: Vec<String> = self.globals.keys().cloned().collect();
                    let hint = suggest(name, candidates.iter().map(|s| s.as_str()));
                    self.push_error_hint(
                        format!(
                            "undefined reference: `{name}` is not declared in the global scope"
                        ),
                        hint,
                        nr.span.clone(),
                    );
                }
            }

            // ── 2+ segments — enum variant or import-alias item ONLY ────────
            [ns, name] => {
                if let Some(entry) = self.enums.get(ns.as_str()) {
                    if !entry.variants.iter().any(|v| v == name) {
                        let hint = suggest(name, entry.variants.iter().map(|s| s.as_str()));
                        self.push_error_hint(
                            format!("`{name}` is not a variant of enum `{ns}`"),
                            hint,
                            nr.span.clone(),
                        );
                    }
                } else if self.imports.contains_key(ns.as_str()) {
                    if let Some(exports) = self.loaded_exports.get(ns.as_str()) {
                        if !exports.contains(name.as_str()) {
                            self.push_error(
                                format!("'{}' is not exported by import '{ns}'", name),
                                nr.span.clone(),
                            );
                        }
                    }
                } else if self.migration_hint_target(ns) {
                    self.push_error(
                        format!("field access via '::' is no longer supported — use '.' instead (e.g. '{ns}.{name}')"),
                        nr.span.clone(),
                    );
                } else {
                    let hint = suggest(
                        ns,
                        self.imports
                            .keys()
                            .chain(self.enums.keys())
                            .map(|s| s.as_str()),
                    );
                    self.push_error_hint(
                        format!("undefined namespace: `{ns}` is not an import alias or enum"),
                        hint,
                        nr.span.clone(),
                    );
                }
            }

            [] => {}
            // 3+ segments: alias::EnumName::Variant or similarly nested
            // static lookups — deferred, same "no error unless the alias
            // itself is unknown" policy as the 2-segment import-alias case.
            [first, ..] => {
                if !self.imports.contains_key(first.as_str())
                    && !self.enums.contains_key(first.as_str())
                {
                    if self.migration_hint_target(first) {
                        self.push_error(
                            format!(
                                "field access via '::' is no longer supported — use '.' instead"
                            ),
                            nr.span.clone(),
                        );
                    } else {
                        self.push_error(
                            format!(
                                "undefined namespace: `{first}` is not an import alias or enum"
                            ),
                            nr.span.clone(),
                        );
                    }
                }
            }
        }
    }

    /// True if `name` is something that used to be a valid `::` field-
    /// access prefix before this session's dot-notation change — a
    /// section, `self`, `global`, or a known var — used only to decide
    /// whether an unresolvable `::` reference gets the specific migration
    /// hint or the generic "undefined namespace" message.
    fn migration_hint_target(&self, name: &str) -> bool {
        name == "self"
            || name == "global"
            || self.sections.contains_key(&vec![name.to_string()])
            || self.globals.contains_key(name)
    }

    fn resolve_field_access(&mut self, base: &Expr, field: &str, span: &Span) {
        if let Expr::NamespaceRef(nr) = base {
            // An import alias is a valid dot-access base (`shared.port`).
            // Export validation already happened while collecting imports.
            if nr.segments.len() == 1 && self.imports.contains_key(&nr.segments[0]) {
                return;
            }
            if nr.segments == ["self"] {
                let Some(section_path) = self.current_section.clone() else {
                    self.push_error(
                        "`self` can only be used inside a section's own field values".to_string(),
                        span.clone(),
                    );
                    return;
                };
                if let Some(entry) = self.sections.get(&section_path) {
                    if !entry.fields.contains_key(field) {
                        let hint = suggest(field, entry.fields.keys().map(|s| s.as_str()));
                        self.push_error_hint(
                            format!(
                                "undefined reference: `{field}` is not a field in section `[{}]`",
                                section_path.join(".")
                            ),
                            hint,
                            span.clone(),
                        );
                    }
                }
                return;
            }
            if nr.segments == ["global"] {
                // global.x IS a direct name lookup (not a value's field) —
                // same check today's `ns == "global"` branches already did.
                if !self.globals.contains_key(field) {
                    let candidates: Vec<String> = self.globals.keys().cloned().collect();
                    let hint = suggest(field, candidates.iter().map(|s| s.as_str()));
                    self.push_error_hint(
                        format!(
                            "undefined reference: `{field}` is not declared in the global scope"
                        ),
                        hint,
                        span.clone(),
                    );
                }
                return;
            }
            // A bare name naming a top-level section (e.g. `Database.pool`)
            // is a section-field access — check field existence directly,
            // same as the old `Section::field` 2-segment check did.
            let key = vec![nr.segments.first().cloned().unwrap_or_default()];
            if nr.segments.len() == 1 && self.sections.contains_key(&key) {
                let entry = &self.sections[&key];
                if !entry.fields.contains_key(field) {
                    let hint = suggest(field, entry.fields.keys().map(|s| s.as_str()));
                    self.push_error_hint(
                        format!(
                            "undefined reference: `{field}` is not a field in section `[{}]`",
                            nr.segments[0]
                        ),
                        hint,
                        span.clone(),
                    );
                }
                return;
            }
        }
        // General case: resolve `base` like any other expression (var
        // lookup, index, call, nested FieldAccess, ...) and defer field-
        // existence entirely to the typechecker.
        self.resolve_expr(base);
    }

    fn check_field_access_with_locals(
        &self,
        base: &Expr,
        field: &str,
        span: &Span,
        locals: &HashSet<String>,
    ) -> Result<(), SparError> {
        if let Expr::NamespaceRef(nr) = base {
            if nr.segments.len() == 1 && self.imports.contains_key(&nr.segments[0]) {
                return Ok(());
            }
            if nr.segments == ["self"] {
                let Some(section_path) = self.current_section.clone() else {
                    return Err(SparError::ResolveError {
                        message: "`self` can only be used inside a section's own field values"
                            .to_string(),
                        hint: None,
                        span: span.clone(),
                    });
                };
                if let Some(entry) = self.sections.get(&section_path) {
                    if !entry.fields.contains_key(field) {
                        let hint = suggest(field, entry.fields.keys().map(|s| s.as_str()));
                        return Err(SparError::ResolveError {
                            message: format!(
                                "undefined reference: `{field}` is not a field in section `[{}]`",
                                section_path.join(".")
                            ),
                            hint,
                            span: span.clone(),
                        });
                    }
                }
                return Ok(());
            }
            if nr.segments == ["global"] {
                if self.globals.contains_key(field) {
                    return Ok(());
                }
                let candidates: Vec<String> = self.globals.keys().cloned().collect();
                let hint = suggest(field, candidates.iter().map(|s| s.as_str()));
                return Err(SparError::ResolveError {
                    message: format!(
                        "undefined reference: `{field}` is not declared in the global scope"
                    ),
                    hint,
                    span: span.clone(),
                });
            }
            if nr.segments.len() == 1 && !locals.contains(&nr.segments[0]) {
                let key = vec![nr.segments[0].clone()];
                if let Some(entry) = self.sections.get(&key) {
                    if !entry.fields.contains_key(field) {
                        let hint = suggest(field, entry.fields.keys().map(|s| s.as_str()));
                        return Err(SparError::ResolveError {
                            message: format!(
                                "undefined reference: `{field}` is not a field in section `[{}]`",
                                nr.segments[0]
                            ),
                            hint,
                            span: span.clone(),
                        });
                    }
                    return Ok(());
                }
            }
        }
        self.resolve_expr_with_locals(base, locals)
    }

    // ── Function body helpers ─────────────────────────────────────────────────

    fn resolve_func_stmts(&mut self, stmts: &[FuncStmt], local_names: &mut HashSet<String>) {
        for stmt in stmts {
            match stmt {
                FuncStmt::LocalVar(lv) => {
                    self.check_named_type_exists(&lv.ty, &lv.span);
                    if let Err(e) = self.resolve_expr_with_locals(&lv.value, local_names) {
                        self.errors.push(e);
                    }
                    if !naming::is_camel_case(&lv.name) {
                        self.errors.push(SparError::ResolveError {
                            message: format!("local variable '{}' must be camelCase", lv.name),
                            hint: Some(naming::camel_case_hint(&lv.name)),
                            span: lv.span.clone(),
                        });
                    }
                    local_names.insert(lv.name.clone());
                }
                FuncStmt::Return(ret_value, _) => match ret_value {
                    ReturnValue::Expr(e) => {
                        if let Err(err) = self.resolve_expr_with_locals(e, local_names) {
                            self.errors.push(err);
                        }
                    }
                    ReturnValue::SectionBlock(fields) => {
                        for rf in fields {
                            if let Some(ty) = &rf.ty {
                                self.check_named_type_exists(ty, &rf.span);
                            }
                            if let Err(err) = self.resolve_expr_with_locals(&rf.value, local_names)
                            {
                                self.errors.push(err);
                            }
                        }
                    }
                },
                FuncStmt::For {
                    var_name,
                    iterable,
                    body,
                    ..
                } => {
                    if let Err(e) = self.resolve_expr_with_locals(iterable, local_names) {
                        self.errors.push(e);
                    }
                    let mut loop_scope = local_names.clone();
                    loop_scope.insert(var_name.clone());
                    let body = body.clone();
                    self.resolve_func_stmts(&body, &mut loop_scope);
                }
                FuncStmt::If(if_stmt) => {
                    if let Err(e) = self.resolve_expr_with_locals(&if_stmt.condition, local_names) {
                        self.errors.push(e);
                    }
                    let mut then_scope = local_names.clone();
                    let then_stmts = if_stmt.then_stmts.clone();
                    let else_stmts = if_stmt.else_stmts.clone();
                    self.resolve_func_stmts(&then_stmts, &mut then_scope);
                    let mut else_scope = local_names.clone();
                    self.resolve_func_stmts(&else_stmts, &mut else_scope);

                    // Merge via sequence_exit_scope (handles terminal-branch exemption)
                    let then_exit = sequence_exit_scope(&then_stmts);
                    let else_exit = sequence_exit_scope(&else_stmts);
                    match (then_exit, else_exit) {
                        (None, None) => {}
                        (Some(names), None) | (None, Some(names)) => {
                            for name in names.keys() {
                                local_names.insert(name.clone());
                            }
                        }
                        (Some(then_names), Some(else_names)) => {
                            for name in then_names.keys() {
                                if else_names.contains_key(name) {
                                    local_names.insert(name.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Locals-aware sibling of `resolve_nested_fields`, for an object
    /// literal appearing inside a function body (a local var's value, a
    /// return value, ...) where names may refer to locals/params instead
    /// of globals. A spread's own expression is resolved the same way any
    /// other locals-aware expression is — via `resolve_expr_with_locals`
    /// itself, not the (non-locals-aware, `&mut self`) `resolve_spread`.
    fn resolve_nested_fields_with_locals(
        &self,
        items: &[SectionItem],
        locals: &HashSet<String>,
    ) -> Result<(), SparError> {
        for item in items {
            match item {
                SectionItem::Field(f) => match &f.value {
                    Some(FieldValue::Expr(e)) => self.resolve_expr_with_locals(e, locals)?,
                    Some(FieldValue::Nested(sub)) => {
                        self.resolve_nested_fields_with_locals(sub, locals)?
                    }
                    None => {}
                },
                SectionItem::Spread(sp) => self.resolve_expr_with_locals(&sp.expr, locals)?,
            }
        }
        Ok(())
    }

    /// Validate an expression inside a function body, allowing locals to shadow globals.
    fn resolve_expr_with_locals(
        &self,
        expr: &Expr,
        locals: &HashSet<String>,
    ) -> Result<(), SparError> {
        match expr {
            Expr::Object(items, _) => self.resolve_nested_fields_with_locals(items, locals),
            Expr::Literal(_) => Ok(()),
            Expr::String(s) => {
                for part in &s.parts {
                    if let StringPart::Expr(e) = part {
                        self.resolve_expr_with_locals(e, locals)?;
                    }
                }
                Ok(())
            }
            Expr::NamespaceRef(nr) => self.check_ns_ref_with_locals(nr, locals),
            Expr::FieldAccess {
                base, field, span, ..
            } => self.check_field_access_with_locals(base, field, span, locals),
            Expr::FnCall(fc) => {
                for arg in &fc.args {
                    self.resolve_expr_with_locals(arg, locals)?;
                }
                Ok(())
            }
            Expr::BinaryOp(b) => {
                self.resolve_expr_with_locals(&b.lhs, locals)?;
                self.resolve_expr_with_locals(&b.rhs, locals)
            }
            Expr::List(items, _) => {
                for item in items {
                    self.resolve_expr_with_locals(item, locals)?;
                }
                Ok(())
            }
            Expr::Grouped(inner, _) => self.resolve_expr_with_locals(inner, locals),
            Expr::Call {
                name,
                name_span,
                args,
                ..
            } => {
                let segments: Vec<&str> = name.split("::").collect();
                match segments.len() {
                    3 => {
                        let alias = segments[0];
                        let group = segments[1];
                        if self.imports.contains_key(alias)
                            || self.loaded_exports.contains_key(alias)
                        {
                            if let Some(exports) = self.loaded_exports.get(alias) {
                                if !exports.contains(group) {
                                    return Err(SparError::ResolveError {
                                        message: format!(
                                            "'{group}' is not exported by import '{alias}'"
                                        ),
                                        hint: None,
                                        span: name_span.clone(),
                                    });
                                }
                            }
                            for arg in args {
                                self.resolve_expr_with_locals(&arg.value, locals)?;
                            }
                            return Ok(());
                        }
                        Err(SparError::ResolveError {
                            message: format!("undefined function '{name}'"),
                            hint: None,
                            span: name_span.clone(),
                        })
                    }
                    2 => {
                        let ns = segments[0];
                        let fn_name = segments[1];
                        if let Some(group) = self.function_groups.get(ns) {
                            if !group.functions.contains_key(fn_name) {
                                return Err(SparError::ResolveError {
                                    message: format!(
                                        "function '{fn_name}' not found in functionGroup '{ns}'"
                                    ),
                                    hint: None,
                                    span: name_span.clone(),
                                });
                            }
                            for arg in args {
                                self.resolve_expr_with_locals(&arg.value, locals)?;
                            }
                            return Ok(());
                        }
                        if self.imports.contains_key(ns) || self.loaded_exports.contains_key(ns) {
                            if let Some(exports) = self.loaded_exports.get(ns) {
                                if !exports.contains(fn_name) {
                                    return Err(SparError::ResolveError {
                                        message: format!(
                                            "function '{fn_name}' not exported from '{ns}'"
                                        ),
                                        hint: None,
                                        span: name_span.clone(),
                                    });
                                }
                            }
                            for arg in args {
                                self.resolve_expr_with_locals(&arg.value, locals)?;
                            }
                            return Ok(());
                        }
                        Err(SparError::ResolveError {
                            message: format!("undefined function '{name}'"),
                            hint: None,
                            span: name_span.clone(),
                        })
                    }
                    _ => {
                        let entry = self.functions.get(name.as_str()).ok_or_else(|| {
                            SparError::ResolveError {
                                message: format!("undefined function '{name}'"),
                                hint: None,
                                span: name_span.clone(),
                            }
                        })?;
                        let param_names: HashSet<String> =
                            entry.params.iter().map(|(n, _)| n.clone()).collect();
                        let mut seen: HashSet<String> = HashSet::new();
                        for arg in args {
                            if !param_names.contains(&arg.param_name) {
                                return Err(SparError::ResolveError {
                                    message: format!(
                                        "function '{name}' has no param '{}'",
                                        arg.param_name
                                    ),
                                    hint: None,
                                    span: arg.param_name_span.clone(),
                                });
                            }
                            if !seen.insert(arg.param_name.clone()) {
                                return Err(SparError::ResolveError {
                                    message: format!("duplicate argument '{}'", arg.param_name),
                                    hint: None,
                                    span: arg.param_name_span.clone(),
                                });
                            }
                            self.resolve_expr_with_locals(&arg.value, locals)?;
                        }
                        let missing: Vec<_> = param_names
                            .iter()
                            .filter(|p| !seen.contains(p.as_str()))
                            .collect();
                        if !missing.is_empty() {
                            return Err(SparError::ResolveError {
                                message: format!(
                                    "missing arguments for function '{name}': {:?}",
                                    missing
                                ),
                                hint: None,
                                span: name_span.clone(),
                            });
                        }
                        Ok(())
                    }
                }
            }
            Expr::Unary { operand, .. } => self.resolve_expr_with_locals(operand, locals),
            Expr::Index { source, index, .. } => {
                self.resolve_expr_with_locals(source, locals)?;
                self.resolve_expr_with_locals(index, locals)
            }
            Expr::Comprehension {
                var_name,
                source,
                body,
                ..
            } => {
                self.resolve_expr_with_locals(source, locals)?;
                let mut inner_locals = locals.clone();
                inner_locals.insert(var_name.clone());
                self.resolve_expr_with_locals(body, &inner_locals)
            }
        }
    }

    /// Namespace-ref validation that returns Result (used in function body context).
    fn check_ns_ref_with_locals(
        &self,
        nr: &NamespaceRef,
        locals: &HashSet<String>,
    ) -> Result<(), SparError> {
        match nr.segments.as_slice() {
            [name] => {
                if locals.contains(name) {
                    return Ok(());
                }
                if name == "self" || name == "global" {
                    return Err(SparError::ResolveError {
                        message: format!(
                            "`{name}` must be followed by `.field` — bare `{name}` is not a value"
                        ),
                        hint: None,
                        span: nr.span.clone(),
                    });
                }
                if self.globals.contains_key(name.as_str()) {
                    return Ok(());
                }
                let candidates: Vec<String> = self.globals.keys().cloned().collect();
                let hint = suggest(name, candidates.iter().map(|s| s.as_str()));
                Err(SparError::ResolveError {
                    message: format!(
                        "undefined reference: `{name}` is not declared in the global scope"
                    ),
                    hint,
                    span: nr.span.clone(),
                })
            }
            [ns, name] => {
                if let Some(entry) = self.enums.get(ns.as_str()) {
                    if entry.variants.iter().any(|v| v == name) {
                        return Ok(());
                    }
                    let hint = suggest(name, entry.variants.iter().map(|s| s.as_str()));
                    return Err(SparError::ResolveError {
                        message: format!("`{name}` is not a variant of enum `{ns}`"),
                        hint,
                        span: nr.span.clone(),
                    });
                }
                if self.imports.contains_key(ns.as_str()) {
                    return Ok(()); // defer import ref validation
                }
                if self.migration_hint_target(ns) {
                    return Err(SparError::ResolveError {
                        message: format!("field access via '::' is no longer supported — use '.' instead (e.g. '{ns}.{name}')"),
                        hint: None,
                        span: nr.span.clone(),
                    });
                }
                Err(SparError::ResolveError {
                    message: format!("undefined namespace: `{ns}` is not an import alias or enum"),
                    hint: None,
                    span: nr.span.clone(),
                })
            }
            [] => Ok(()),
            // 3+ segments: alias::EnumName::Variant or similarly nested
            // static lookups — deferred, same policy as the 2-segment
            // import-alias case.
            [first, ..] => {
                if self.imports.contains_key(first.as_str())
                    || self.enums.contains_key(first.as_str())
                {
                    Ok(())
                } else if self.migration_hint_target(first) {
                    Err(SparError::ResolveError {
                        message: "field access via '::' is no longer supported — use '.' instead"
                            .to_string(),
                        hint: None,
                        span: nr.span.clone(),
                    })
                } else {
                    Err(SparError::ResolveError {
                        message: format!(
                            "undefined namespace: `{first}` is not an import alias or enum"
                        ),
                        hint: None,
                        span: nr.span.clone(),
                    })
                }
            }
        }
    }

    // ── Closure dependency analysis ───────────────────────────────────────────

    fn collect_closure_deps_expr(
        &self,
        expr: &Expr,
        local_names: &HashSet<String>,
        deps: &mut HashSet<DeclId>,
    ) {
        match expr {
            Expr::NamespaceRef(nr) => {
                match nr.segments.as_slice() {
                    [name] => {
                        if !local_names.contains(name) {
                            if self.globals.contains_key(name.as_str()) {
                                deps.insert(DeclId::Global(name.clone()));
                            }
                            // Check if name is a top-level section name
                            if self
                                .sections
                                .keys()
                                .any(|k| k.first().map(|s| s.as_str()) == Some(name.as_str()))
                            {
                                deps.insert(DeclId::Section(name.clone()));
                            }
                        }
                    }
                    [top, ..] => {
                        // e.g. Server::host — top-level section name is segments[0]
                        if self
                            .sections
                            .keys()
                            .any(|k| k.first().map(|s| s.as_str()) == Some(top.as_str()))
                        {
                            deps.insert(DeclId::Section(top.clone()));
                        }
                    }
                    [] => {}
                }
            }
            Expr::FieldAccess { base, .. } => {
                self.collect_closure_deps_expr(base, local_names, deps);
            }
            Expr::Call { name, args, .. } => {
                for arg in args {
                    self.collect_closure_deps_expr(&arg.value, local_names, deps);
                }
                // Transitively include the called function's closure deps
                if let Some(fe) = self.functions.get(name) {
                    for d in &fe.closure_deps {
                        deps.insert(d.clone());
                    }
                }
            }
            Expr::BinaryOp(b) => {
                self.collect_closure_deps_expr(&b.lhs, local_names, deps);
                self.collect_closure_deps_expr(&b.rhs, local_names, deps);
            }
            Expr::Unary { operand, .. } => {
                self.collect_closure_deps_expr(operand, local_names, deps);
            }
            Expr::Comprehension {
                var_name,
                source,
                body,
                ..
            } => {
                self.collect_closure_deps_expr(source, local_names, deps);
                let mut inner = local_names.clone();
                inner.insert(var_name.clone());
                self.collect_closure_deps_expr(body, &inner, deps);
            }
            Expr::List(items, _) => {
                for item in items {
                    self.collect_closure_deps_expr(item, local_names, deps);
                }
            }
            Expr::Grouped(inner, _) => {
                self.collect_closure_deps_expr(inner, local_names, deps);
            }
            Expr::FnCall(fc) => {
                for arg in &fc.args {
                    self.collect_closure_deps_expr(arg, local_names, deps);
                }
            }
            Expr::String(s) => {
                for part in &s.parts {
                    if let StringPart::Expr(e) = part {
                        self.collect_closure_deps_expr(e, local_names, deps);
                    }
                }
            }
            Expr::Index { source, index, .. } => {
                self.collect_closure_deps_expr(source, local_names, deps);
                self.collect_closure_deps_expr(index, local_names, deps);
            }
            Expr::Object(items, _) => {
                for item in items {
                    match item {
                        SectionItem::Field(f) => {
                            if let Some(FieldValue::Expr(e)) = &f.value {
                                self.collect_closure_deps_expr(e, local_names, deps);
                            }
                        }
                        SectionItem::Spread(sp) => {
                            self.collect_closure_deps_expr(&sp.expr, local_names, deps);
                        }
                    }
                }
            }
            Expr::Literal(_) => {}
        }
    }

    fn collect_closure_deps_stmts(
        &self,
        stmts: &[FuncStmt],
        local_names: &HashSet<String>,
        deps: &mut HashSet<DeclId>,
    ) {
        let mut locals = local_names.clone();
        for stmt in stmts {
            match stmt {
                FuncStmt::LocalVar(lv) => {
                    self.collect_closure_deps_expr(&lv.value, &locals, deps);
                    locals.insert(lv.name.clone());
                }
                FuncStmt::Return(ret_value, _) => match ret_value {
                    ReturnValue::Expr(e) => self.collect_closure_deps_expr(e, &locals, deps),
                    ReturnValue::SectionBlock(fields) => {
                        for rf in fields {
                            self.collect_closure_deps_expr(&rf.value, &locals, deps);
                        }
                    }
                },
                FuncStmt::For {
                    var_name,
                    iterable,
                    body,
                    ..
                } => {
                    self.collect_closure_deps_expr(iterable, &locals, deps);
                    let mut loop_locals = locals.clone();
                    loop_locals.insert(var_name.clone());
                    self.collect_closure_deps_stmts(body, &loop_locals, deps);
                }
                FuncStmt::If(if_stmt) => {
                    self.collect_closure_deps_expr(&if_stmt.condition, &locals, deps);
                    self.collect_closure_deps_stmts(&if_stmt.then_stmts, &locals, deps);
                    self.collect_closure_deps_stmts(&if_stmt.else_stmts, &locals, deps);
                }
            }
        }
    }

    fn resolve_spread(&mut self, spread: &SpreadStmt) {
        match &spread.expr {
            Expr::NamespaceRef(nr) => match nr.segments.as_slice() {
                [name] => {
                    if !self.sections.contains_key(&vec![name.clone()]) {
                        self.push_error(
                            format!("undefined spread target: `{name}` is not a declared section"),
                            spread.span.clone(),
                        );
                    }
                }
                [alias, _] => {
                    if !self.imports.contains_key(alias.as_str()) {
                        self.push_error(
                            format!("undefined import namespace `{alias}` in spread"),
                            spread.span.clone(),
                        );
                    }
                }
                _ => {}
            },
            Expr::FieldAccess { base, field, .. } if matches!(base.as_ref(), Expr::NamespaceRef(nr) if nr.segments == ["global"]) => {
                if !self.sections.contains_key(&vec![field.clone()]) {
                    self.push_error(
                        format!("undefined spread target: `{field}` is not a declared section"),
                        spread.span.clone(),
                    );
                }
            }
            Expr::Call {
                name, name_span, ..
            } => {
                if !self.functions.contains_key(name.as_str()) {
                    self.push_error(
                        format!("undefined function `{name}` in spread"),
                        name_span.clone(),
                    );
                }
            }
            _ => self.resolve_expr(&spread.expr),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve_ok(src: &str) -> SymbolTable {
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        Resolver::new()
            .resolve(&program, &[])
            .expect("resolve failed")
    }

    fn resolve_err(src: &str) -> Vec<String> {
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        Resolver::new()
            .resolve(&program, &[])
            .unwrap_err()
            .into_iter()
            .map(|e| e.to_string())
            .collect()
    }

    fn has_error(src: &str, fragment: &str) -> bool {
        resolve_err(src).iter().any(|e| e.contains(fragment))
    }

    #[test]
    fn test_clean_program() {
        let src = r#"
            var port: int = 3000;
            var name: str = "keel";
            [Server]{ bind: str = "0.0.0.0"; };
        "#;
        let table = resolve_ok(src);
        assert!(table.globals.contains_key("port"));
        assert!(table.sections.contains_key(&vec!["Server".to_string()]));
    }

    #[test]
    fn test_valid_global_ref() {
        let src = r#"
            var port: int = 3000;
            var bind: str = "test";
        "#;
        resolve_ok(src);
    }

    #[test]
    fn test_undefined_single_segment() {
        assert!(has_error("var bind: str = port;", "undefined reference"));
        assert!(has_error("var bind: str = port;", "port"));
    }

    #[test]
    fn test_valid_global_ns_ref() {
        let src = r#"
            var port: int = 3000;
            var bind: str = global.port;
        "#;
        resolve_ok(src);
    }

    #[test]
    fn test_undefined_global_ns_ref() {
        assert!(has_error("var bind: str = global.missing;", "missing"));
        assert!(has_error("var bind: str = global.missing;", "not declared"));
    }

    #[test]
    fn test_valid_section_field_ref() {
        let src = r#"
            [Database]{ pool: int = 5; };
            var p: int = Database.pool;
        "#;
        resolve_ok(src);
    }

    #[test]
    fn test_undefined_section_namespace() {
        assert!(has_error("var x: str = cache::host;", "cache"));
        assert!(has_error(
            "var x: str = cache::host;",
            "undefined namespace"
        ));
    }

    #[test]
    fn test_undefined_field_in_known_section() {
        let src = r#"
            [Database]{ pool: int = 5; };
            var x: int = Database.timeout;
        "#;
        assert!(has_error(src, "timeout"));
        assert!(has_error(src, "not a field in section"));
    }

    #[test]
    fn test_valid_three_segment_ref() {
        let src = r#"
            [Server]{ port: int = 3000; };
            var p: int = Server.port;
        "#;
        resolve_ok(src);
    }

    #[test]
    fn test_undefined_section_in_three_segment() {
        assert!(has_error("var x: int = ghost.port;", "ghost"));
        assert!(has_error("var x: int = ghost.port;", "not declared"));
    }

    #[test]
    fn test_undefined_field_in_three_segment() {
        let src = r#"
            [Server]{ port: int = 3000; };
            var x: str = Server.host;
        "#;
        assert!(has_error(src, "host"));
        assert!(has_error(src, "not a field"));
    }

    #[test]
    fn test_import_alias_ref_deferred() {
        let src = r#"
            import "base.spar" as config;
            var v: str = config::version;
        "#;
        resolve_ok(src);
    }

    #[test]
    fn test_import_three_segment_deferred() {
        let src = r#"
            import "base.spar" as config;
            var v: str = config::db::pool;
        "#;
        resolve_ok(src);
    }

    #[test]
    fn test_unknown_namespace_error() {
        assert!(has_error("var x: str = unknown::value;", "unknown"));
        assert!(has_error(
            "var x: str = unknown::value;",
            "undefined namespace"
        ));
    }

    #[test]
    fn test_local_spread_valid() {
        let src = r#"
            [Defaults]{ workers: int = 4; };
            [Server]{ ...Defaults; };
        "#;
        resolve_ok(src);
    }

    #[test]
    fn test_local_spread_undefined() {
        assert!(has_error(
            "[Server]{ ...missing_defaults; };",
            "missing_defaults"
        ));
        assert!(has_error(
            "[Server]{ ...missing_defaults; };",
            "not a declared section"
        ));
    }

    #[test]
    fn test_global_spread_valid() {
        let src = r#"
            [Defaults]{ workers: int = 4; };
            [Server]{ ...global.Defaults; };
        "#;
        resolve_ok(src);
    }

    #[test]
    fn test_import_alias_spread_deferred() {
        let src = r#"
            import "base.spar" as config;
            [Server]{ ...config::defaults; };
        "#;
        resolve_ok(src);
    }

    #[test]
    fn test_duplicate_global_var() {
        let src = r#"
            var port: int = 3000;
            var port: int = 8080;
        "#;
        assert!(has_error(src, "duplicate"));
        assert!(has_error(src, "port"));
    }

    #[test]
    fn test_duplicate_section() {
        let src = r#"
            [Server]{ port: int = 3000; };
            [Server]{ host: str = "localhost"; };
        "#;
        assert!(has_error(src, "duplicate section"));
        assert!(has_error(src, "Server"));
    }

    #[test]
    fn test_duplicate_import_namespace() {
        let src = r#"
            import "a.spar" as config;
            import "b.spar" as config;
        "#;
        assert!(has_error(src, "duplicate import namespace"));
        assert!(has_error(src, "config"));
    }

    #[test]
    fn test_global_reserved_section_name() {
        assert!(has_error("[global]{ port: int = 3000; };", "global"));
        assert!(has_error("[global]{ port: int = 3000; };", "reserved"));
    }

    #[test]
    fn test_duplicate_field_in_section() {
        let src = "[Server]{ port: int = 3000; port: int = 8080; };";
        assert!(has_error(src, "duplicate field"));
        assert!(has_error(src, "port"));
    }

    #[test]
    fn test_multiple_errors_collected() {
        let src = r#"
            var x: str = missing_a;
            var y: str = missing_b;
        "#;
        assert_eq!(resolve_err(src).len(), 2);
    }

    #[test]
    fn test_forward_reference() {
        let src = r#"
            var bind: str = global.port;
            var port: int = 3000;
        "#;
        resolve_ok(src);
    }

    #[test]
    fn test_interp_string_valid_ref() {
        let src = r#"
            var host: str = "localhost";
            var url: str = "http://${global.host}";
        "#;
        resolve_ok(src);
    }

    #[test]
    fn test_interp_string_invalid_ref() {
        let src = r#"var url: str = "http://${global::missing}";"#;
        assert!(has_error(src, "missing"));
    }

    #[test]
    fn test_symbol_table_import_entry() {
        let src = r#"import "config/base.spar" as cfg;"#;
        let table = resolve_ok(src);
        assert!(table.imports.contains_key("cfg"));
        assert_eq!(table.imports["cfg"].path, "config/base.spar");
    }

    #[test]
    fn test_import_stem_namespace() {
        let src = r#"import "base.spar";"#;
        let table = resolve_ok(src);
        assert!(table.imports.contains_key("base"));
    }

    #[test]
    fn section_lowercase_start_is_naming_error() {
        let src = "[metaData]{ port: int = 8080; };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let errs = Resolver::new().resolve(&program, &[]).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(e, SparError::ResolveError { message, hint, .. }
                if message.contains("PascalCase") && hint.as_deref() == Some("rename to 'MetaData'")
            )),
            "expected PascalCase error with hint 'MetaData', got: {:?}", errs
        );
    }

    #[test]
    fn variable_uppercase_start_is_naming_error() {
        let src = r#"var BaseUrl: str = "x";"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let errs = Resolver::new().resolve(&program, &[]).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, SparError::ResolveError { message, .. }
                    if message.contains("camelCase")
                )),
            "expected camelCase error for 'BaseUrl', got: {:?}",
            errs
        );
    }

    #[test]
    fn snake_case_field_is_naming_error() {
        let src = "[Server]{ pool_size: int = 5; };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let errs = Resolver::new().resolve(&program, &[]).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(e, SparError::ResolveError { message, hint, .. }
                if message.contains("camelCase") && hint.as_deref() == Some("rename to 'poolSize'")
            )),
            "expected camelCase error with hint 'poolSize', got: {:?}", errs
        );
    }

    #[test]
    fn valid_pascal_section_name_passes() {
        let src = "[Server]{ port: int = 8080; };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let result = Resolver::new().resolve(&program, &[]);
        let has_naming_err = result
            .as_ref()
            .err()
            .map(|es| {
                es.iter().any(|e| {
                    matches!(e, SparError::ResolveError { message, .. }
                if message.contains("PascalCase") || message.contains("camelCase"))
                })
            })
            .unwrap_or(false);
        assert!(
            !has_naming_err,
            "'Server' is valid PascalCase — must not produce a naming error"
        );
    }

    #[test]
    fn valid_camel_variable_passes() {
        let src = r#"var baseUrl: str = "x";"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let result = Resolver::new().resolve(&program, &[]);
        let has_naming_err = result
            .as_ref()
            .err()
            .map(|es| {
                es.iter().any(|e| {
                    matches!(e, SparError::ResolveError { message, .. }
                if message.contains("camelCase"))
                })
            })
            .unwrap_or(false);
        assert!(
            !has_naming_err,
            "'baseUrl' is valid camelCase — must not produce a naming error"
        );
    }

    #[test]
    fn nested_section_appears_in_symbol_table() {
        let src = r#"[Outer]{ inner: section = { key: str = "v"; }; };"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let symbols = Resolver::new().resolve(&program, &[]).unwrap();
        assert!(
            symbols
                .sections
                .contains_key(&vec!["Outer".to_string(), "inner".to_string()]),
            "nested section must be registered in symbol table"
        );
    }

    #[test]
    fn private_section_resolves_and_is_referenceable() {
        use crate::typechecker::TypeChecker;
        use crate::{Lexer, Parser};
        let src = r#"
private [Defaults]{ timeout: int = 30; };
[Server]{ timeout: int = Defaults.timeout; };
"#;
        let tokens = Lexer::new(src).tokenize().unwrap();
        let program = Parser::new(tokens).parse().unwrap();
        let symbols = Resolver::new().resolve(&program, &[]).unwrap();
        assert!(TypeChecker::check(&program, &symbols).is_ok());
        let path = vec!["Defaults".to_string()];
        assert!(
            symbols.sections.contains_key(&path),
            "Defaults must be in symbol table"
        );
        assert!(
            symbols.sections[&path].private,
            "Defaults must be marked private"
        );
    }

    #[test]
    fn cross_file_function_call_resolves_ok() {
        use std::fs;
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("base.spar"),
            r#"function main() -> int { return 42; }"#,
        )
        .unwrap();

        let src = r#"import "base.spar" as base; var port: int = base::main();"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let mut loader = crate::loader::ImportLoader::new(dir.path());
        let imports = crate::loader::collect_imports(&program, &mut loader).unwrap();
        assert!(
            Resolver::resolve_with_imports(&program, &imports).is_ok(),
            "cross-file function call via alias must not produce undefined-function error"
        );
    }

    #[test]
    fn cross_file_function_call_inside_section_resolves_ok() {
        use std::fs;
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("base.spar"),
            r#"function main() -> int { return 42; }"#,
        )
        .unwrap();

        let src = r#"import "base.spar" as base; [Server]{ port: int = base::main(); };"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let mut loader = crate::loader::ImportLoader::new(dir.path());
        let imports = crate::loader::collect_imports(&program, &mut loader).unwrap();
        let result = Resolver::resolve_with_imports(&program, &imports);
        assert!(
            result.is_ok(),
            "cross-file function call inside section must resolve ok, got: {:?}",
            result.unwrap_err()
        );
    }

    #[test]
    fn cross_file_function_call_unexported_errors() {
        use std::fs;
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("base.spar"),
            r#"private function secret() -> int { return 1; }"#,
        )
        .unwrap();

        let src = r#"import "base.spar" as base; var x: int = base::secret();"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let mut loader = crate::loader::ImportLoader::new(dir.path());
        let imports = crate::loader::collect_imports(&program, &mut loader).unwrap();
        let result = Resolver::resolve_with_imports(&program, &imports);
        assert!(
            result.is_err(),
            "calling private cross-file function must error"
        );
        let errs = result.unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, SparError::ResolveError { message, .. }
                    if message.contains("secret") || message.contains("not exported")
                )),
            "error must mention the unexported function, got: {:?}",
            errs
        );
    }

    #[test]
    fn resolve_with_imports_validates_exported_field() {
        use std::fs;
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("db.spar"),
            r#"export var host: str = "localhost";"#,
        )
        .unwrap();

        let src = r#"import "db.spar" as db; var endpoint: str = db::host;"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let mut loader = crate::loader::ImportLoader::new(dir.path());
        let imports = crate::loader::collect_imports(&program, &mut loader).unwrap();
        assert!(Resolver::resolve_with_imports(&program, &imports).is_ok());
    }

    #[test]
    fn resolve_with_imports_errors_on_nonexported_field() {
        use std::fs;
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        // db.keel exports 'host' but NOT 'port'
        fs::write(
            dir.path().join("db.spar"),
            r#"export var host: str = "localhost";"#,
        )
        .unwrap();

        let src = r#"import "db.spar" as db; var x: str = db::port;"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let mut loader = crate::loader::ImportLoader::new(dir.path());
        let imports = crate::loader::collect_imports(&program, &mut loader).unwrap();
        let result = Resolver::resolve_with_imports(&program, &imports);
        assert!(
            result.is_err(),
            "referencing non-exported field must be an error"
        );
        let errs = result.unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, SparError::ResolveError { message, .. }
                    if message.contains("port") || message.contains("not exported")
                )),
            "error must mention the missing field, got: {:?}",
            errs
        );
    }

    #[test]
    fn resolve_single_file_still_defers_import_refs() {
        // Normal resolve() must still defer (no error) for import alias refs
        let src = r#"import "base.spar" as config; var v: str = config::version;"#;
        resolve_ok(src); // existing helper that calls Resolver::resolve
    }

    #[test]
    fn function_with_cascading_early_returns_is_exhaustive() {
        let src = r#"
function classify(score: int) -> str {
    if score >= 90 { return "A"; }
    if score >= 80 { return "B"; }
    return "C";
}
"#;
        resolve_ok(src);
    }

    #[test]
    fn function_missing_fallback_return_is_not_exhaustive() {
        let src = r#"
function classify(score: int) -> str {
    if score >= 90 { return "A"; }
    if score >= 80 { return "B"; }
}
"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        let errs = Resolver::new().resolve(&program, &[]).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, SparError::ResolveError { message, .. }
            if message.contains("does not guarantee a value is returned"))),
            "got: {:?}",
            errs
        );
    }

    #[test]
    fn if_else_both_returning_is_exhaustive() {
        let src = r#"
function pick(debug: bool) -> int {
    if debug { return 9000; } else { return 8080; }
}
"#;
        resolve_ok(src);
    }

    #[test]
    fn mixed_terminal_and_nonterminal_branch_resolves_correctly() {
        let src = r#"
function f(useDefault: bool) -> str {
    if useDefault {
        return "default-value";
    } else {
        var computed: str = "computed-value";
    }
    return computed;
}
"#;
        resolve_ok(src);
    }

    #[test]
    fn unreachable_code_after_exhaustive_if_is_flagged() {
        let src = r#"
function f(debug: bool) -> int {
    if debug { return 1; } else { return 2; }
    var dead: int = 5;
    return dead;
}
"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        let errs = Resolver::new().resolve(&program, &[]).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, SparError::ResolveError { message, .. }
            if message.contains("unreachable"))),
            "got: {:?}",
            errs
        );
    }

    #[test]
    fn bare_if_with_unused_local_and_no_else_resolves_fine() {
        let src = r#"
function f(flag: bool) -> int {
    if flag {
        var unused: int = 1;
    }
    return 0;
}
"#;
        resolve_ok(src);
    }

    #[test]
    fn bare_if_referencing_its_own_local_afterward_is_undefined() {
        let src = r#"
function f(flag: bool) -> int {
    if flag {
        var x: int = 1;
    }
    return x;
}
"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        assert!(
            Resolver::new().resolve(&program, &[]).is_err(),
            "x must be undefined outside the bare if"
        );
    }

    #[test]
    fn nested_if_inside_nonterminal_branch_merges_correctly() {
        let src = r#"
function f(a: bool, b: bool) -> int {
    if a {
        if b { var x: int = 1; } else { var x: int = 2; }
    } else {
        var x: int = 3;
    }
    return x;
}
"#;
        resolve_ok(src);
    }

    #[test]
    fn closure_deps_do_not_include_local_shadow() {
        // var appName shadows a global; return appName should NOT add appName to closure_deps
        let src = r#"
            var appName: str = "global";
            function f(x: str) -> str {
                var appName: str = "local";
                return appName;
            }
        "#;
        let sym = resolve_ok(src);
        let f = &sym.functions["f"];
        // appName is locally declared inside f; it should NOT appear as a closure dep
        assert!(!f
            .closure_deps
            .iter()
            .any(|d| matches!(d, DeclId::Global(n) if n == "appName")));
    }
}
