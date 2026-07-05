use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::depgraph::DeclId;
use crate::error::{SparError, Span};
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
        FuncStmt::LocalVar(l)          => l.span.clone(),
        FuncStmt::If(i)                => i.span.clone(),
        FuncStmt::Return(_, s)         => s.clone(),
        FuncStmt::For { span, .. }     => span.clone(),
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
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FieldEntry {
    pub ty: SparType,
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
pub struct SymbolTable {
    pub globals:   HashMap<String, GlobalEntry>,
    pub sections:  HashMap<Vec<String>, SectionEntry>,
    pub imports:   HashMap<String, ImportEntry>,
    pub functions: HashMap<String, FunctionEntry>,
}

impl SymbolTable {
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
}

// ── Levenshtein + suggestion helpers ─────────────────────────────────────────

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = a.len();
    let n = b.len();
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for (i, row) in dp.iter_mut().enumerate() { row[0] = i; }
    for (j, val) in dp[0].iter_mut().enumerate() { *val = j; }
    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = if a[i-1] == b[j-1] {
                dp[i-1][j-1]
            } else {
                1 + dp[i-1][j].min(dp[i][j-1]).min(dp[i-1][j-1])
            };
        }
    }
    dp[m][n]
}

fn suggest(name: &str, candidates: impl Iterator<Item = impl AsRef<str>>) -> Option<String> {
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
    globals:        HashMap<String, GlobalEntry>,
    sections:       HashMap<Vec<String>, SectionEntry>,
    imports:        HashMap<String, ImportEntry>,
    functions:      HashMap<String, FunctionEntry>,
    loaded_exports: HashMap<String, HashSet<String>>,  // alias → exported names
    errors:         Vec<SparError>,
}

impl Resolver {
    pub fn new() -> Self {
        Self {
            globals:        HashMap::new(),
            sections:       HashMap::new(),
            imports:        HashMap::new(),
            functions:      HashMap::new(),
            loaded_exports: HashMap::new(),
            errors:         Vec::new(),
        }
    }

    fn with_loaded(exports: HashMap<String, HashSet<String>>) -> Self {
        Self {
            globals:        HashMap::new(),
            sections:       HashMap::new(),
            imports:        HashMap::new(),
            functions:      HashMap::new(),
            loaded_exports: exports,
            errors:         Vec::new(),
        }
    }

    /// Resolve a program with no imports (or with a pre-built slice of imports).
    /// The instance-method form enables `Resolver::new().resolve(&prog, &[])`.
    pub fn resolve(mut self, program: &Program, loaded_imports: &[LoadedImport]) -> Result<SymbolTable, Vec<SparError>> {
        // Populate loaded_exports from the slice (use path stem as alias)
        for li in loaded_imports {
            let alias = li.path
                .rsplit('/')
                .next()
                .unwrap_or(&li.path)
                .trim_end_matches(".spar")
                .to_string();
            self.loaded_exports.insert(alias, li.exports.clone());
        }
        self.register(program);
        self.resolve_program(program);
        self.resolve_function_bodies(program);
        if self.errors.is_empty() {
            Ok(SymbolTable {
                globals:   self.globals,
                sections:  self.sections,
                imports:   self.imports,
                functions: self.functions,
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
        r.resolve_program(program);
        r.resolve_function_bodies(program);
        if r.errors.is_empty() {
            Ok(SymbolTable {
                globals:   r.globals,
                sections:  r.sections,
                imports:   r.imports,
                functions: r.functions,
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
}

// ── Pass 1: Registration ──────────────────────────────────────────────────────

impl Resolver {
    fn register(&mut self, program: &Program) {
        for item in &program.items {
            match item {
                TopLevelItem::Import(decl)    => self.register_import(decl),
                TopLevelItem::Var(decl)       => self.register_var(decl),
                TopLevelItem::Dynamic(decl)   => self.register_dynamic(decl),
                TopLevelItem::Section(decl)   => self.register_section(decl),
                TopLevelItem::Function(decl)  => self.register_function(decl),
                TopLevelItem::SchemaSection(_) => {}
            }
        }
    }

    fn register_function(&mut self, decl: &FunctionDecl) {
        // Duplicate check
        if self.functions.contains_key(&decl.name) {
            self.push_error(
                format!("function '{}' is already defined", decl.name),
                decl.name_span.clone(),
            );
            return;
        }
        // camelCase validation
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
        // Validate params
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
        self.functions.insert(
            decl.name.clone(),
            FunctionEntry {
                params,
                ret: decl.ret.clone(),
                span: decl.name_span.clone(),
                closure_deps: HashSet::new(), // computed in Pass 3
                is_private: decl.is_private,
            },
        );
    }

    fn register_import(&mut self, decl: &ImportDecl) {
        // Schema imports are consumed by the validation pass; skip them here
        if decl.is_schema { return; }

        let namespace = decl.alias.clone().unwrap_or_else(|| {
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

        self.imports.insert(namespace, ImportEntry {
            path: decl.path.clone(),
            span: decl.span.clone(),
        });
    }

    fn register_var(&mut self, decl: &VarDecl) {
        if self.globals.contains_key(&decl.name) {
            self.push_error(
                format!("duplicate declaration: `{}` is already declared in the global scope", decl.name),
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
                format!("duplicate declaration: `{}` is already declared in the global scope", decl.name),
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
                format!("duplicate section `[{}]` — each section path must be unique",
                    decl.path.join(".")),
                decl.span.clone(),
            );
            return;
        }

        let mut fields = HashMap::new();
        for item in &decl.items {
            if let SectionItem::Field(f) = item {
                if fields.contains_key(&f.name) {
                    self.push_error(
                        format!("duplicate field `{}` in section `[{}]`",
                            f.name, decl.path.join(".")),
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
                    fields.insert(f.name.clone(), FieldEntry {
                        ty: f.ty.clone(),
                        optional: f.optional,
                        span: f.span.clone(),
                    });
                }
            }
        }

        self.sections.insert(decl.path.clone(), SectionEntry {
            fields,
            exported: decl.exported,
            private:  decl.private,
            span: decl.span.clone(),
        });

        // Register nested section-type fields recursively
        for item in &decl.items {
            if let SectionItem::Field(f) = item {
                if f.ty == SparType::Section {
                    if let Some(FieldValue::Nested(sub_fields)) = &f.value {
                        let nested_path = [decl.path.as_slice(), &[f.name.clone()]].concat();
                        self.register_nested_section(nested_path, sub_fields);
                    }
                }
            }
        }
    }

    fn register_nested_section(&mut self, path: Vec<String>, fields: &[FieldDecl]) {
        if self.sections.contains_key(&path) {
            return; // already registered (e.g. via a second spread of the same path)
        }
        let mut field_map = HashMap::new();
        for field in fields {
            if field.ty == SparType::Section {
                if field_map.contains_key(&field.name) {
                    self.push_error(
                        format!("duplicate field `{}` in section `[{}]`",
                            field.name, path.join(".")),
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
                    field_map.insert(field.name.clone(), FieldEntry {
                        ty: SparType::Section,
                        optional: field.optional,
                        span: field.span.clone(),
                    });
                }
            } else {
                if field_map.contains_key(&field.name) {
                    self.push_error(
                        format!("duplicate field `{}` in section `[{}]`",
                            field.name, path.join(".")),
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
                    field_map.insert(field.name.clone(), FieldEntry {
                        ty: field.ty.clone(),
                        optional: field.optional,
                        span: field.span.clone(),
                    });
                }
            }
        }
        self.sections.insert(path.clone(), SectionEntry {
            fields: field_map,
            exported: false,
            private:  false,   // nested sections inherit parent privacy at emit time only
            span: fields.first().map(|f| f.span.clone()).unwrap_or_else(Span::dummy),
        });
    }
}

// ── Pass 2: Resolution ────────────────────────────────────────────────────────

impl Resolver {
    fn resolve_program(&mut self, program: &Program) {
        for item in &program.items {
            match item {
                TopLevelItem::Import(_)     => {}
                TopLevelItem::Var(decl)     => {
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
                TopLevelItem::Function(_)   => {} // function bodies handled in resolve_function_bodies
                TopLevelItem::SchemaSection(_) => {}
            }
        }
    }

    /// Pass 3: resolve function bodies and compute closure dependencies.
    fn resolve_function_bodies(&mut self, program: &Program) {
        for item in &program.items {
            let TopLevelItem::Function(f) = item else { continue };
            let param_names: HashSet<String> = f.params.iter().map(|p| p.name.clone()).collect();
            let mut local_names = param_names.clone();

            // Resolve all statements (Return is now just another statement)
            self.resolve_func_stmts(&f.body.stmts, &mut local_names);

            // Exhaustiveness: every path must hit a return
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

            // Unreachable code detection
            let stmts = f.body.stmts.clone();
            self.check_unreachable(&stmts);

            // Compute closure deps
            let mut deps: HashSet<DeclId> = HashSet::new();
            self.collect_closure_deps_stmts(&f.body.stmts, &param_names, &mut deps);

            if let Some(entry) = self.functions.get_mut(&f.name) {
                entry.closure_deps = deps;
            }
        }
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
                FuncStmt::Return(_, _) => { terminated = true; }
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

    fn resolve_section(&mut self, decl: &SectionDecl) {
        for item in &decl.items {
            match item {
                SectionItem::Field(f) => {
                    match &f.value {
                        Some(FieldValue::Expr(val)) => self.resolve_expr(val),
                        Some(FieldValue::Nested(sub_fields)) => {
                            self.resolve_nested_fields(sub_fields);
                        }
                        None => {}
                    }
                }
                SectionItem::Spread(s) => self.resolve_spread(s),
            }
        }
    }

    fn resolve_nested_fields(&mut self, fields: &[FieldDecl]) {
        for field in fields {
            match &field.value {
                Some(FieldValue::Expr(val)) => self.resolve_expr(val),
                Some(FieldValue::Nested(sub)) => self.resolve_nested_fields(sub),
                None => {}
            }
        }
    }

    fn resolve_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Literal(_)        => {}
            Expr::NamespaceRef(nr)  => self.resolve_namespace_ref(nr),
            Expr::FnCall(fc)        => {
                for arg in &fc.args { self.resolve_expr(arg); }
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
                for item in items { self.resolve_expr(item); }
            }
            Expr::Grouped(inner, _) => self.resolve_expr(inner),
            Expr::Call { name, name_span, args, .. } => {
                // Qualified cross-file call: alias::fn_name(...)
                if let Some((alias, fn_name)) = name.split_once("::") {
                    if self.imports.contains_key(alias) || self.loaded_exports.contains_key(alias) {
                        if let Some(exports) = self.loaded_exports.get(alias) {
                            if !exports.contains(fn_name) {
                                self.push_error(
                                    format!("function '{fn_name}' not exported from '{alias}'"),
                                    name_span.clone(),
                                );
                            }
                        }
                        for arg in args { self.resolve_expr(&arg.value); }
                    } else {
                        self.push_error(format!("undefined function '{name}'"), name_span.clone());
                        for arg in args { self.resolve_expr(&arg.value); }
                    }
                } else if let Some(entry) = self.functions.get(name).cloned() {
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
                    for arg in args { self.resolve_expr(&arg.value); }
                }
            }
            Expr::Unary { operand, .. } => self.resolve_expr(operand),
            Expr::Index { source, index, .. } => {
                self.resolve_expr(source);
                self.resolve_expr(index);
            }
            Expr::Comprehension { var_name, source, body, .. } => {
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
                if !self.globals.contains_key(name.as_str()) {
                    let candidates: Vec<String> = self.globals.keys().cloned().collect();
                    let hint = suggest(name, candidates.iter().map(|s| s.as_str()));
                    self.push_error_hint(
                        format!("undefined reference: `{name}` is not declared in the global scope"),
                        hint,
                        nr.span.clone(),
                    );
                }
            }

            // ── 2 segments ────────────────────────────────────────────────────
            [ns, name] => {
                if ns == "global" {
                    if !self.globals.contains_key(name.as_str()) {
                        let candidates: Vec<String> = self.globals.keys().cloned().collect();
                        let hint = suggest(name, candidates.iter().map(|s| s.as_str()));
                        self.push_error_hint(
                            format!("undefined reference: `{name}` is not declared in the global scope"),
                            hint,
                            nr.span.clone(),
                        );
                    }
                } else if self.sections.contains_key(&vec![ns.clone()]) {
                    let key = vec![ns.clone()];
                    let field_exists = self.sections[&key].fields.contains_key(name.as_str());
                    let hint = if !field_exists {
                        let field_keys: Vec<String> =
                            self.sections[&key].fields.keys().cloned().collect();
                        suggest(name, field_keys.iter().map(|s| s.as_str()))
                    } else {
                        None
                    };
                    if !field_exists {
                        self.push_error_hint(
                            format!("undefined reference: `{name}` is not a field in section `[{ns}]`"),
                            hint,
                            nr.span.clone(),
                        );
                    }
                } else if self.imports.contains_key(ns.as_str()) {
                    // Validate against loaded export set if available
                    if let Some(exports) = self.loaded_exports.get(ns.as_str()) {
                        if !exports.contains(name.as_str()) {
                            self.push_error(
                                format!("'{}' is not exported by import '{ns}'", name),
                                nr.span.clone(),
                            );
                        }
                    }
                    // If loaded_exports is empty (single-file mode), defer silently as before
                } else {
                    let section_names: Vec<String> = self.sections.keys()
                        .filter_map(|p| p.first().cloned())
                        .collect();
                    let import_names: Vec<String> = self.imports.keys().cloned().collect();
                    let all_ns: Vec<String> =
                        section_names.into_iter().chain(import_names).collect();
                    let hint = suggest(ns, all_ns.iter().map(|s| s.as_str()));
                    self.push_error_hint(
                        format!("undefined namespace: `{ns}` is not a section, import alias, or `global`"),
                        hint,
                        nr.span.clone(),
                    );
                }
            }

            // ── 3 segments ────────────────────────────────────────────────────
            [ns, section_name, field_name] => {
                if ns == "global" {
                    let key = vec![section_name.clone()];
                    if let Some(entry) = self.sections.get(&key) {
                        if !entry.fields.contains_key(field_name.as_str()) {
                            self.push_error(
                                format!("undefined reference: `{field_name}` is not a field in section `[{section_name}]`"),
                                nr.span.clone(),
                            );
                        }
                    } else {
                        self.push_error(
                            format!("undefined reference: section `[{section_name}]` is not declared"),
                            nr.span.clone(),
                        );
                    }
                } else if self.imports.contains_key(ns.as_str()) {
                    // deferred — no error
                } else if self.sections.keys().any(|k| k.first().map(|s| s.as_str()) == Some(ns.as_str())) {
                    // `Section::nestedSection::field` — defer deep validation to evaluator.
                } else {
                    self.push_error(
                        format!("undefined namespace: `{ns}` is not a section, import alias, or `global`"),
                        nr.span.clone(),
                    );
                }
            }

            [] => {}
            // ── 4+ segments ───────────────────────────────────────────────────
            // Unbounded nesting depth; defer deep-path validation to the evaluator.
            [first, ..] => {
                if !self.sections.keys().any(|k| k.first().map(|s| s.as_str()) == Some(first.as_str()))
                    && !self.imports.contains_key(first.as_str())
                {
                    self.push_error(
                        format!("undefined namespace: `{first}` is not a section or import alias"),
                        nr.span.clone(),
                    );
                }
            }
        }
    }

    // ── Function body helpers ─────────────────────────────────────────────────

    fn resolve_func_stmts(
        &mut self,
        stmts: &[FuncStmt],
        local_names: &mut HashSet<String>,
    ) {
        for stmt in stmts {
            match stmt {
                FuncStmt::LocalVar(lv) => {
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
                FuncStmt::Return(ret_value, _) => {
                    match ret_value {
                        ReturnValue::Expr(e) => {
                            if let Err(err) = self.resolve_expr_with_locals(e, local_names) {
                                self.errors.push(err);
                            }
                        }
                        ReturnValue::SectionBlock(fields) => {
                            for rf in fields {
                                if let Err(err) = self.resolve_expr_with_locals(&rf.value, local_names) {
                                    self.errors.push(err);
                                }
                            }
                        }
                    }
                }
                FuncStmt::For { var_name, iterable, body, .. } => {
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

    /// Validate an expression inside a function body, allowing locals to shadow globals.
    fn resolve_expr_with_locals(
        &self,
        expr: &Expr,
        locals: &HashSet<String>,
    ) -> Result<(), SparError> {
        match expr {
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
            Expr::Call { name, name_span, args, .. } => {
                // Qualified cross-file call: alias::fn_name(...)
                if let Some((alias, fn_name)) = name.split_once("::") {
                    if self.imports.contains_key(alias) || self.loaded_exports.contains_key(alias) {
                        if let Some(exports) = self.loaded_exports.get(alias) {
                            if !exports.contains(fn_name) {
                                return Err(SparError::ResolveError {
                                    message: format!("function '{fn_name}' not exported from '{alias}'"),
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
                    return Err(SparError::ResolveError {
                        message: format!("undefined function '{name}'"),
                        hint: None,
                        span: name_span.clone(),
                    });
                }
                let entry = self.functions.get(name).ok_or_else(|| SparError::ResolveError {
                    message: format!("undefined function '{name}'"),
                    hint: None,
                    span: name_span.clone(),
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
            Expr::Unary { operand, .. } => self.resolve_expr_with_locals(operand, locals),
            Expr::Index { source, index, .. } => {
                self.resolve_expr_with_locals(source, locals)?;
                self.resolve_expr_with_locals(index, locals)
            }
            Expr::Comprehension { var_name, source, body, .. } => {
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
                if ns == "global" {
                    if self.globals.contains_key(name.as_str()) {
                        return Ok(());
                    }
                    return Err(SparError::ResolveError {
                        message: format!(
                            "undefined reference: `{name}` is not declared in the global scope"
                        ),
                        hint: None,
                        span: nr.span.clone(),
                    });
                }
                if self.sections.contains_key(&vec![ns.clone()]) {
                    let key = vec![ns.clone()];
                    if self.sections[&key].fields.contains_key(name.as_str()) {
                        return Ok(());
                    }
                    return Err(SparError::ResolveError {
                        message: format!(
                            "undefined reference: `{name}` is not a field in section `[{ns}]`"
                        ),
                        hint: None,
                        span: nr.span.clone(),
                    });
                }
                if self.imports.contains_key(ns.as_str()) {
                    return Ok(()); // defer import ref validation
                }
                Err(SparError::ResolveError {
                    message: format!(
                        "undefined namespace: `{ns}` is not a section, import alias, or `global`"
                    ),
                    hint: None,
                    span: nr.span.clone(),
                })
            }
            [ns, section_name, field_name] => {
                if ns == "global" {
                    let key = vec![section_name.clone()];
                    if let Some(entry) = self.sections.get(&key) {
                        if entry.fields.contains_key(field_name.as_str()) {
                            return Ok(());
                        }
                        return Err(SparError::ResolveError {
                            message: format!(
                                "undefined reference: `{field_name}` is not a field in section `[{section_name}]`"
                            ),
                            hint: None,
                            span: nr.span.clone(),
                        });
                    }
                    return Err(SparError::ResolveError {
                        message: format!(
                            "undefined reference: section `[{section_name}]` is not declared"
                        ),
                        hint: None,
                        span: nr.span.clone(),
                    });
                }
                if self.imports.contains_key(ns.as_str()) {
                    return Ok(());
                }
                Err(SparError::ResolveError {
                    message: format!(
                        "undefined namespace: `{ns}` is not a section, import alias, or `global`"
                    ),
                    hint: None,
                    span: nr.span.clone(),
                })
            }
            [] => Ok(()),
            // 4+ segments: defer to evaluator
            [first, ..] => {
                if self.sections.keys().any(|k| k.first().map(|s| s.as_str()) == Some(first.as_str()))
                    || self.imports.contains_key(first.as_str())
                {
                    Ok(())
                } else {
                    Err(SparError::ResolveError {
                        message: format!(
                            "undefined namespace: `{first}` is not a section or import alias"
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
                            if self.sections.keys().any(|k| k.first().map(|s| s.as_str()) == Some(name.as_str())) {
                                deps.insert(DeclId::Section(name.clone()));
                            }
                        }
                    }
                    [top, ..] => {
                        // e.g. Server::host — top-level section name is segments[0]
                        if self.sections.keys().any(|k| k.first().map(|s| s.as_str()) == Some(top.as_str())) {
                            deps.insert(DeclId::Section(top.clone()));
                        }
                    }
                    [] => {}
                }
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
            Expr::Comprehension { var_name, source, body, .. } => {
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
                FuncStmt::Return(ret_value, _) => {
                    match ret_value {
                        ReturnValue::Expr(e) => self.collect_closure_deps_expr(e, &locals, deps),
                        ReturnValue::SectionBlock(fields) => {
                            for rf in fields {
                                self.collect_closure_deps_expr(&rf.value, &locals, deps);
                            }
                        }
                    }
                }
                FuncStmt::For { var_name, iterable, body, .. } => {
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
                [ns, name] if ns == "global" => {
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
            Expr::Call { name, name_span, .. } => {
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
        Resolver::new().resolve(&program, &[]).expect("resolve failed")
    }

    fn resolve_err(src: &str) -> Vec<String> {
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        Resolver::new().resolve(&program, &[])
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
            var bind: str = global::port;
        "#;
        resolve_ok(src);
    }

    #[test]
    fn test_undefined_global_ns_ref() {
        assert!(has_error("var bind: str = global::missing;", "missing"));
        assert!(has_error("var bind: str = global::missing;", "not declared"));
    }

    #[test]
    fn test_valid_section_field_ref() {
        let src = r#"
            [Database]{ pool: int = 5; };
            var p: int = Database::pool;
        "#;
        resolve_ok(src);
    }

    #[test]
    fn test_undefined_section_namespace() {
        assert!(has_error("var x: str = cache::host;", "cache"));
        assert!(has_error("var x: str = cache::host;", "undefined namespace"));
    }

    #[test]
    fn test_undefined_field_in_known_section() {
        let src = r#"
            [Database]{ pool: int = 5; };
            var x: int = Database::timeout;
        "#;
        assert!(has_error(src, "timeout"));
        assert!(has_error(src, "not a field in section"));
    }

    #[test]
    fn test_valid_three_segment_ref() {
        let src = r#"
            [Server]{ port: int = 3000; };
            var p: int = global::Server::port;
        "#;
        resolve_ok(src);
    }

    #[test]
    fn test_undefined_section_in_three_segment() {
        assert!(has_error("var x: int = global::ghost::port;", "ghost"));
        assert!(has_error("var x: int = global::ghost::port;", "not declared"));
    }

    #[test]
    fn test_undefined_field_in_three_segment() {
        let src = r#"
            [Server]{ port: int = 3000; };
            var x: str = global::Server::host;
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
        assert!(has_error("var x: str = unknown::value;", "undefined namespace"));
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
        assert!(has_error("[Server]{ ...missing_defaults; };", "missing_defaults"));
        assert!(has_error("[Server]{ ...missing_defaults; };", "not a declared section"));
    }

    #[test]
    fn test_global_spread_valid() {
        let src = r#"
            [Defaults]{ workers: int = 4; };
            [Server]{ ...global::Defaults; };
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
            var bind: str = global::port;
            var port: int = 3000;
        "#;
        resolve_ok(src);
    }

    #[test]
    fn test_interp_string_valid_ref() {
        let src = r#"
            var host: str = "localhost";
            var url: str = "http://${global::host}";
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
            errs.iter().any(|e| matches!(e, SparError::ResolveError { message, .. }
                if message.contains("camelCase")
            )),
            "expected camelCase error for 'BaseUrl', got: {:?}", errs
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
        let has_naming_err = result.as_ref().err().map(|es| {
            es.iter().any(|e| matches!(e, SparError::ResolveError { message, .. }
                if message.contains("PascalCase") || message.contains("camelCase")))
        }).unwrap_or(false);
        assert!(!has_naming_err, "'Server' is valid PascalCase — must not produce a naming error");
    }

    #[test]
    fn valid_camel_variable_passes() {
        let src = r#"var baseUrl: str = "x";"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let result = Resolver::new().resolve(&program, &[]);
        let has_naming_err = result.as_ref().err().map(|es| {
            es.iter().any(|e| matches!(e, SparError::ResolveError { message, .. }
                if message.contains("camelCase")))
        }).unwrap_or(false);
        assert!(!has_naming_err, "'baseUrl' is valid camelCase — must not produce a naming error");
    }

    #[test]
    fn nested_section_appears_in_symbol_table() {
        let src = r#"[Outer]{ inner: section = { key: str = "v"; }; };"#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let symbols = Resolver::new().resolve(&program, &[]).unwrap();
        assert!(
            symbols.sections.contains_key(&vec!["Outer".to_string(), "inner".to_string()]),
            "nested section must be registered in symbol table"
        );
    }

    #[test]
    fn private_section_resolves_and_is_referenceable() {
        use crate::{Lexer, Parser};
        use crate::typechecker::TypeChecker;
        let src = r#"
private [Defaults]{ timeout: int = 30; };
[Server]{ timeout: int = Defaults::timeout; };
"#;
        let tokens  = Lexer::new(src).tokenize().unwrap();
        let program = Parser::new(tokens).parse().unwrap();
        let symbols = Resolver::new().resolve(&program, &[]).unwrap();
        assert!(TypeChecker::check(&program, &symbols).is_ok());
        let path = vec!["Defaults".to_string()];
        assert!(symbols.sections.contains_key(&path), "Defaults must be in symbol table");
        assert!(symbols.sections[&path].private, "Defaults must be marked private");
    }

    #[test]
    fn cross_file_function_call_resolves_ok() {
        use tempfile::tempdir;
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("base.spar"), r#"function main() -> int { return 42; }"#).unwrap();

        let src = r#"import "base.spar" as base; var port: int = base::main();"#;
        let tokens  = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let mut loader = crate::loader::ImportLoader::new(dir.path());
        let imports    = crate::loader::collect_imports(&program, &mut loader).unwrap();
        assert!(Resolver::resolve_with_imports(&program, &imports).is_ok(),
            "cross-file function call via alias must not produce undefined-function error");
    }

    #[test]
    fn cross_file_function_call_inside_section_resolves_ok() {
        use tempfile::tempdir;
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("base.spar"), r#"function main() -> int { return 42; }"#).unwrap();

        let src = r#"import "base.spar" as base; [Server]{ port: int = base::main(); };"#;
        let tokens  = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let mut loader = crate::loader::ImportLoader::new(dir.path());
        let imports    = crate::loader::collect_imports(&program, &mut loader).unwrap();
        let result = Resolver::resolve_with_imports(&program, &imports);
        assert!(result.is_ok(),
            "cross-file function call inside section must resolve ok, got: {:?}", result.unwrap_err());
    }

    #[test]
    fn cross_file_function_call_unexported_errors() {
        use tempfile::tempdir;
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("base.spar"), r#"private function secret() -> int { return 1; }"#).unwrap();

        let src = r#"import "base.spar" as base; var x: int = base::secret();"#;
        let tokens  = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let mut loader = crate::loader::ImportLoader::new(dir.path());
        let imports    = crate::loader::collect_imports(&program, &mut loader).unwrap();
        let result = Resolver::resolve_with_imports(&program, &imports);
        assert!(result.is_err(), "calling private cross-file function must error");
        let errs = result.unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(e, SparError::ResolveError { message, .. }
                if message.contains("secret") || message.contains("not exported")
            )),
            "error must mention the unexported function, got: {:?}", errs
        );
    }

    #[test]
    fn resolve_with_imports_validates_exported_field() {
        use tempfile::tempdir;
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("db.spar"), r#"export var host: str = "localhost";"#).unwrap();

        let src     = r#"import "db.spar" as db; var endpoint: str = db::host;"#;
        let tokens  = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let mut loader = crate::loader::ImportLoader::new(dir.path());
        let imports    = crate::loader::collect_imports(&program, &mut loader).unwrap();
        assert!(Resolver::resolve_with_imports(&program, &imports).is_ok());
    }

    #[test]
    fn resolve_with_imports_errors_on_nonexported_field() {
        use tempfile::tempdir;
        use std::fs;
        let dir = tempdir().unwrap();
        // db.keel exports 'host' but NOT 'port'
        fs::write(dir.path().join("db.spar"), r#"export var host: str = "localhost";"#).unwrap();

        let src     = r#"import "db.spar" as db; var x: str = db::port;"#;
        let tokens  = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let mut loader = crate::loader::ImportLoader::new(dir.path());
        let imports    = crate::loader::collect_imports(&program, &mut loader).unwrap();
        let result     = Resolver::resolve_with_imports(&program, &imports);
        assert!(result.is_err(), "referencing non-exported field must be an error");
        let errs = result.unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(e, SparError::ResolveError { message, .. }
                if message.contains("port") || message.contains("not exported")
            )),
            "error must mention the missing field, got: {:?}", errs
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
        let tokens  = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        let errs = Resolver::new().resolve(&program, &[]).unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, SparError::ResolveError { message, .. }
            if message.contains("does not guarantee a value is returned"))),
            "got: {:?}", errs);
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
        let tokens  = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        let errs = Resolver::new().resolve(&program, &[]).unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, SparError::ResolveError { message, .. }
            if message.contains("unreachable"))),
            "got: {:?}", errs);
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
        let tokens  = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        assert!(Resolver::new().resolve(&program, &[]).is_err(),
            "x must be undefined outside the bare if");
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
        assert!(!f.closure_deps.iter().any(|d| matches!(d, DeclId::Global(n) if n == "appName")));
    }
}
