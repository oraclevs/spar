use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::depgraph::DeclId;
use crate::error::{SparError, Span};
use crate::resolver::SymbolTable;

const MAX_CALL_DEPTH: usize = 20;

// ── Output types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigValue {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    List(Vec<ConfigValue>),
    Section(HashMap<String, ConfigValue>),
}

impl ConfigValue {
    pub fn coerce_to_str(&self) -> String {
        match self {
            ConfigValue::Str(s)   => s.clone(),
            ConfigValue::Int(n)   => n.to_string(),
            ConfigValue::Float(f) => f.to_string(),
            ConfigValue::Bool(b)  => b.to_string(),
            ConfigValue::List(_)  => unreachable!("lists cannot appear in string interpolation"),
            ConfigValue::Section(_) => unreachable!("sections cannot appear in string interpolation"),
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            ConfigValue::Str(_)     => "str",
            ConfigValue::Int(_)     => "int",
            ConfigValue::Float(_)   => "float",
            ConfigValue::Bool(_)    => "bool",
            ConfigValue::List(_)    => "list",
            ConfigValue::Section(_) => "section",
        }
    }
}

#[derive(Debug)]
pub struct EvalResult {
    pub globals:  HashMap<String, ConfigValue>,
    pub sections: HashMap<Vec<String>, HashMap<String, ConfigValue>>,
    pub warnings: Vec<String>,
}

// ── Internal error type ───────────────────────────────────────────────────────

#[derive(Debug)]
enum EvalErr {
    EnvVarMissing(String),
    CyclicRef { name: String, span: Span },
    DivisionByZero(Span),
    ImportRef { alias: String, symbol: String },
    NotScalar { name: String, span: Span },
    TypeMismatch { expected: &'static str, got: &'static str },
    MaxCallDepth { name: String },
}

impl EvalErr {
    fn into_kl_error(self) -> SparError {
        match self {
            EvalErr::CyclicRef { name, span } => SparError::EvalError {
                message: format!(
                    "cyclic reference: `{name}` depends on itself — \
                     check for circular var references"
                ),
                span,
            },
            EvalErr::DivisionByZero(span) => SparError::EvalError {
                message: "division by zero".into(),
                span,
            },
            EvalErr::EnvVarMissing(name) => SparError::EvalError {
                message: format!(
                    "env var `{name}` is not set and has no `??` fallback"
                ),
                span: Span::dummy(),
            },
            EvalErr::ImportRef { alias, symbol } => SparError::EvalError {
                message: format!(
                    "cannot evaluate cross-file reference `{alias}::{symbol}` \
                     without loading the imported file — handled by the CLI"
                ),
                span: Span::dummy(),
            },
            EvalErr::NotScalar { name, span } => SparError::EvalError {
                message: format!(
                    "'{}' is a nested section, not a scalar value", name
                ),
                span,
            },
            EvalErr::TypeMismatch { expected, got } => SparError::EvalError {
                message: format!("type mismatch: expected {expected}, got {got}"),
                span: Span::dummy(),
            },
            EvalErr::MaxCallDepth { name } => SparError::EvalError {
                message: format!(
                    "maximum call depth ({MAX_CALL_DEPTH}) exceeded in function '{name}'"
                ),
                span: Span::dummy(),
            },
        }
    }
}

type EvalResult_ = Result<ConfigValue, EvalErr>;

// ── Evaluator ─────────────────────────────────────────────────────────────────

pub struct Evaluator {
    program:           Program,
    symbols:           SymbolTable,
    call_depth:        usize,
    global_cache:      HashMap<String, ConfigValue>,
    section_cache:     HashMap<Vec<String>, HashMap<String, ConfigValue>>,
    evaluating:        HashSet<String>,
    evaluating_sects:  HashSet<Vec<String>>,
    errors:            Vec<SparError>,
    warnings:          Vec<String>,
    imported_programs: HashMap<String, Program>,
}

impl Evaluator {
    pub fn new(symbols: SymbolTable, program: Program) -> Self {
        Evaluator {
            program,
            symbols,
            call_depth:        0,
            global_cache:      HashMap::new(),
            section_cache:     HashMap::new(),
            evaluating:        HashSet::new(),
            evaluating_sects:  HashSet::new(),
            errors:            Vec::new(),
            warnings:          Vec::new(),
            imported_programs: HashMap::new(),
        }
    }

    pub fn evaluate_with_imports(
        program: &Program,
        symbols: &SymbolTable,
        loaded: &std::collections::HashMap<String, crate::loader::LoadedImport>,
    ) -> Result<EvalResult, Vec<SparError>> {
        Self::evaluate_with_imports_and_base(program, symbols, loaded, std::path::Path::new("."))
    }

    pub fn evaluate_with_imports_and_base(
        program: &Program,
        symbols: &SymbolTable,
        loaded: &std::collections::HashMap<String, crate::loader::LoadedImport>,
        base_dir: &std::path::Path,
    ) -> Result<EvalResult, Vec<SparError>> {
        let imported: HashMap<String, Program> = loaded.iter()
            .filter_map(|(alias, li)| {
                let full = base_dir.join(&li.path);
                let src = std::fs::read_to_string(&full).ok()?;
                let tokens = crate::lexer::Lexer::new(&src).tokenize().ok()?;
                let prog = crate::parser::Parser::new(tokens).parse().ok()?;
                Some((alias.clone(), prog))
            })
            .collect();
        let mut ev = Evaluator::new(symbols.clone(), program.clone());
        ev.imported_programs = imported;
        let result = ev.run();
        match result {
            Ok(r) => Ok(r),
            Err(first_err) => {
                let mut errs = vec![first_err];
                errs.extend(std::mem::take(&mut ev.errors));
                Err(errs)
            }
        }
    }

    pub fn evaluate(
        program: &Program,
        symbols: &SymbolTable,
    ) -> Result<EvalResult, Vec<SparError>> {
        let mut ev = Evaluator::new(symbols.clone(), program.clone());
        let result = ev.run();
        match result {
            Ok(r) => Ok(r),
            Err(first_err) => {
                let mut errs = vec![first_err];
                errs.extend(std::mem::take(&mut ev.errors));
                Err(errs)
            }
        }
    }

    pub fn run(&mut self) -> Result<EvalResult, SparError> {
        let graph = self.build_dep_graph();
        let order = crate::depgraph::topological_sort(&graph).map_err(|cycle| {
            let names: Vec<_> = cycle.iter().map(|d| match d {
                DeclId::Global(n) | DeclId::Section(n) => n.clone(),
            }).collect();
            SparError::EvalError {
                message: format!("cyclic dependency detected: {:?}", names),
                span: Span::dummy(),
            }
        })?;

        for decl_id in &order {
            match decl_id {
                DeclId::Global(name) => { self.eval_global(name); }
                DeclId::Section(name) => { self.eval_section_by_top_name(name); }
            }
        }

        if self.errors.is_empty() {
            Ok(EvalResult {
                globals:  self.global_cache.clone(),
                sections: self.section_cache.clone(),
                warnings: self.warnings.clone(),
            })
        } else {
            Err(self.errors.remove(0))
        }
    }

    fn push_eval_error(&mut self, e: EvalErr) {
        match e {
            EvalErr::ImportRef { alias, symbol } => {
                self.warnings.push(format!(
                    "cross-file reference `{alias}::{symbol}` was not evaluated — \
                     run via `spar` CLI for full multi-file evaluation"
                ));
            }
            other => self.errors.push(other.into_kl_error()),
        }
    }
}

// ── Dependency graph ──────────────────────────────────────────────────────────

impl Evaluator {
    fn build_dep_graph(&self) -> crate::depgraph::DepGraph {
        use crate::depgraph::DepGraph;
        let mut graph: DepGraph = HashMap::new();

        for item in &self.program.items {
            match item {
                TopLevelItem::Var(v) => {
                    let node = DeclId::Global(v.name.clone());
                    let mut deps = HashSet::new();
                    if let Some(expr) = &v.value {
                        self.collect_expr_deps(expr, &mut deps);
                    }
                    graph.insert(node, deps);
                }
                TopLevelItem::Section(s) => {
                    let top_name = s.path[0].clone();
                    let node = DeclId::Section(top_name);
                    let mut deps = HashSet::new();
                    for item in &s.items {
                        match item {
                            SectionItem::Field(f) => {
                                if let Some(FieldValue::Expr(e)) = &f.value {
                                    self.collect_expr_deps(e, &mut deps);
                                }
                            }
                            SectionItem::Spread(sp) => {
                                self.collect_expr_deps(&sp.expr, &mut deps);
                            }
                        }
                    }
                    graph.entry(node).or_default().extend(deps);
                }
                TopLevelItem::Dynamic(d) => {
                    let node = DeclId::Global(d.name.clone());
                    graph.insert(node, HashSet::new());
                }
                _ => {}
            }
        }
        graph
    }

    fn collect_expr_deps(&self, expr: &Expr, deps: &mut HashSet<DeclId>) {
        match expr {
            Expr::NamespaceRef(nr) => {
                if let Some(top) = nr.segments.first() {
                    if self.symbols.globals.contains_key(top.as_str()) {
                        deps.insert(DeclId::Global(top.clone()));
                    } else if self.symbols.sections.keys().any(|k| k.first() == Some(top)) {
                        deps.insert(DeclId::Section(top.clone()));
                    }
                }
            }
            Expr::Call { name, args, .. } => {
                for arg in args { self.collect_expr_deps(&arg.value, deps); }
                if let Some(fe) = self.symbols.functions.get(name) {
                    deps.extend(fe.closure_deps.clone());
                }
            }
            Expr::BinaryOp(b) => {
                self.collect_expr_deps(&b.lhs, deps);
                self.collect_expr_deps(&b.rhs, deps);
            }
            Expr::Unary { operand, .. } => self.collect_expr_deps(operand, deps),
            Expr::Comprehension { source, body, .. } => {
                self.collect_expr_deps(source, deps);
                self.collect_expr_deps(body, deps);
            }
            Expr::List(items, _) => {
                for item in items { self.collect_expr_deps(item, deps); }
            }
            Expr::Grouped(inner, _) => self.collect_expr_deps(inner, deps),
            Expr::FnCall(fc) => {
                for arg in &fc.args { self.collect_expr_deps(arg, deps); }
            }
            Expr::String(s) => {
                for part in &s.parts {
                    if let StringPart::Expr(e) = part { self.collect_expr_deps(e, deps); }
                }
            }
            Expr::Index { source, index, .. } => {
                self.collect_expr_deps(source, deps);
                self.collect_expr_deps(index, deps);
            }
            Expr::Literal(_) => {}
        }
    }
}

// ── Global evaluation ─────────────────────────────────────────────────────────

impl Evaluator {
    fn eval_global(&mut self, name: &str) -> Option<ConfigValue> {
        if let Some(cached) = self.global_cache.get(name) {
            return Some(cached.clone());
        }
        if self.evaluating.contains(name) {
            self.errors.push(SparError::EvalError {
                message: format!(
                    "cyclic reference: `{name}` depends on itself — \
                     check for circular var references"
                ),
                span: Span::dummy(),
            });
            return None;
        }

        let value_expr = self.program.items.iter().find_map(|item| {
            match item {
                TopLevelItem::Var(d) if d.name == name     => d.value.clone(),
                TopLevelItem::Dynamic(d) if d.name == name => d.value.clone(),
                _                                           => None,
            }
        });

        let expr = value_expr?;

        self.evaluating.insert(name.to_string());
        let result = self.eval_expr(&expr, &HashMap::new());
        self.evaluating.remove(name);

        match result {
            Ok(val) => {
                self.global_cache.insert(name.to_string(), val.clone());
                Some(val)
            }
            Err(e) => {
                self.push_eval_error(e);
                None
            }
        }
    }
}

// ── Section evaluation ────────────────────────────────────────────────────────

impl Evaluator {
    fn eval_section_by_top_name(&mut self, top_name: &str) {
        // Collect all section paths with this top name first (avoid borrow conflicts)
        let paths: Vec<Vec<String>> = self.program.items.iter()
            .filter_map(|item| {
                if let TopLevelItem::Section(s) = item {
                    if s.path.first().map(|s| s.as_str()) == Some(top_name) {
                        return Some(s.path.clone());
                    }
                }
                None
            })
            .collect();
        for path in paths {
            self.eval_section_by_path(&path);
        }
    }

    fn eval_section_by_path(&mut self, path: &[String]) -> Option<HashMap<String, ConfigValue>> {
        let path_vec = path.to_vec();
        if let Some(cached) = self.section_cache.get(&path_vec) {
            return Some(cached.clone());
        }
        if self.evaluating_sects.contains(&path_vec) {
            self.errors.push(SparError::EvalError {
                message: format!(
                    "cyclic section reference: `[{}]` spreads into itself",
                    path_vec.join(".")
                ),
                span: Span::dummy(),
            });
            return None;
        }

        let decl = self.program.items.iter().find_map(|item| {
            if let TopLevelItem::Section(d) = item {
                if d.path == path_vec { Some(d.clone()) } else { None }
            } else {
                None
            }
        });

        let decl = decl?;

        self.evaluating_sects.insert(path_vec.clone());
        let fields = self.eval_section_decl(&decl);
        self.evaluating_sects.remove(&path_vec);

        self.section_cache.insert(path_vec, fields.clone());
        Some(fields)
    }

    fn eval_section_decl(&mut self, decl: &SectionDecl) -> HashMap<String, ConfigValue> {
        let path = decl.path.clone();
        self.eval_section_fields(&decl.items.clone(), &path, &HashMap::new())
    }

    fn eval_section_fields(
        &mut self,
        items: &[SectionItem],
        parent_path: &[String],
        local_scope: &HashMap<String, ConfigValue>,
    ) -> HashMap<String, ConfigValue> {
        let mut result: HashMap<String, ConfigValue> = HashMap::new();

        for item in items {
            match item {
                SectionItem::Spread(spread) => {
                    let target = self.eval_spread(&spread.expr, local_scope);
                    if let Some(fields) = target {
                        for (k, v) in fields {
                            result.entry(k).or_insert(v);
                        }
                    }
                }

                SectionItem::Field(field) => {
                    match &field.value {
                        Some(FieldValue::Expr(val_expr)) => {
                            let val_expr = val_expr.clone();
                            match self.eval_expr(&val_expr, local_scope) {
                                Ok(ConfigValue::Section(map)) => {
                                    // Section-returning function call — register at nested path
                                    let nested_path = [parent_path, &[field.name.clone()]].concat();
                                    self.section_cache.insert(nested_path, map);
                                }
                                Ok(val) => { result.insert(field.name.clone(), val); }
                                Err(e)  => { self.push_eval_error(e); }
                            }
                        }
                        Some(FieldValue::Nested(sub_fields)) => {
                            let nested_path = [parent_path, &[field.name.clone()]].concat();
                            let nested_items: Vec<SectionItem> = sub_fields.iter()
                                .map(|f| SectionItem::Field(f.clone()))
                                .collect();
                            let nested_map = self.eval_section_fields(&nested_items, &nested_path, &HashMap::new());
                            self.section_cache.insert(nested_path, nested_map);
                            // Do NOT insert into result — nested sections aren't scalar values
                        }
                        None => {}
                    }
                }
            }
        }

        result
    }

    fn eval_spread(
        &mut self,
        expr: &Expr,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> Option<HashMap<String, ConfigValue>> {
        match expr {
            Expr::NamespaceRef(nr) => match nr.segments.as_slice() {
                [name] => self.eval_section_by_path(std::slice::from_ref(name)),
                [ns, name] if ns == "global" => {
                    self.eval_section_by_path(std::slice::from_ref(name))
                }
                [alias, name] => {
                    self.warnings.push(format!(
                        "spread `...{alias}::{name}` skipped — \
                         cross-file spreads are resolved by the CLI"
                    ));
                    None
                }
                segs => {
                    self.warnings.push(format!(
                        "spread `...{}` skipped — cross-file spreads are resolved by the CLI",
                        segs.join("::")
                    ));
                    None
                }
            },
            other => {
                let span = match other {
                    Expr::Call { span, .. } => span.clone(),
                    Expr::FnCall(fc) => fc.span.clone(),
                    _ => Span::dummy(),
                };
                match self.eval_expr(other, local_scope) {
                    Ok(ConfigValue::Section(map)) => Some(map),
                    Ok(v) => {
                        self.push_eval_error(EvalErr::TypeMismatch {
                            expected: "section",
                            got: v.type_name(),
                        });
                        None
                    }
                    Err(EvalErr::CyclicRef { name, .. }) => {
                        self.push_eval_error(EvalErr::CyclicRef { name, span });
                        None
                    }
                    Err(e) => {
                        self.push_eval_error(e);
                        None
                    }
                }
            }
        }
    }

    fn eval_index(
        &mut self,
        source: &Expr,
        index: &Expr,
        span: &Span,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> EvalResult_ {
        let source_val = self.eval_expr(source, local_scope)?;
        let index_val = self.eval_expr(index, local_scope)?;
        match (source_val, index_val) {
            (ConfigValue::List(items), ConfigValue::Int(i)) => {
                if i < 0 || i as usize >= items.len() {
                    Err(EvalErr::CyclicRef {
                        name: format!(
                            "index {} out of bounds for list of length {}",
                            i,
                            items.len()
                        ),
                        span: span.clone(),
                    })
                } else {
                    Ok(items[i as usize].clone())
                }
            }
            (_, iv) => Err(EvalErr::TypeMismatch {
                expected: "list[int]",
                got: iv.type_name(),
            }),
        }
    }
}

// ── Expression evaluation ─────────────────────────────────────────────────────

impl Evaluator {
    fn eval_expr(&mut self, expr: &Expr, local_scope: &HashMap<String, ConfigValue>) -> EvalResult_ {
        match expr {
            Expr::Literal(Literal::Int(n))   => Ok(ConfigValue::Int(*n)),
            Expr::Literal(Literal::Float(f)) => Ok(ConfigValue::Float(*f)),
            Expr::Literal(Literal::Bool(b))  => Ok(ConfigValue::Bool(*b)),
            Expr::String(s)                  => self.eval_interp_string(s, local_scope),
            Expr::List(items, _) => {
                let mut vals = Vec::with_capacity(items.len());
                for item in items {
                    vals.push(self.eval_expr(item, local_scope)?);
                }
                Ok(ConfigValue::List(vals))
            }
            Expr::Grouped(inner, _) => self.eval_expr(inner, local_scope),
            Expr::NamespaceRef(nr) => self.eval_namespace_ref(nr, local_scope),
            Expr::FnCall(fc)       => {
                let fc = fc.clone();
                self.eval_fn_call(&fc, local_scope)
            }
            Expr::BinaryOp(op) => {
                let op = op.clone();
                self.eval_binop(&op, local_scope)
            }
            Expr::Call { name, args, .. } => {
                let name = name.clone();
                let args = args.clone();
                self.eval_call(&name, &args, local_scope)
            }
            Expr::Unary { op, operand, .. } => {
                let operand = operand.clone();
                let op = op.clone();
                match (op, self.eval_expr(&operand, local_scope)?) {
                    (UnOp::Not, ConfigValue::Bool(b)) => Ok(ConfigValue::Bool(!b)),
                    (UnOp::Not, v) => Err(EvalErr::TypeMismatch { expected: "bool", got: v.type_name() }),
                    (UnOp::Neg, ConfigValue::Int(n)) => Ok(ConfigValue::Int(-n)),
                    (UnOp::Neg, ConfigValue::Float(f)) => Ok(ConfigValue::Float(-f)),
                    (UnOp::Neg, v) => Err(EvalErr::TypeMismatch { expected: "int or float", got: v.type_name() }),
                }
            }
            Expr::Index { source, index, span } => {
                let source = source.clone();
                let index = index.clone();
                let span = span.clone();
                self.eval_index(&source, &index, &span, local_scope)
            }
            Expr::Comprehension { var_name, source, body, .. } => {
                let var_name = var_name.clone();
                let source = source.clone();
                let body = body.clone();
                let source_val = self.eval_expr(&source, local_scope)?;
                match source_val {
                    ConfigValue::List(items) => {
                        let mut results = Vec::new();
                        for item in items {
                            let mut inner_scope = local_scope.clone();
                            inner_scope.insert(var_name.clone(), item);
                            results.push(self.eval_expr(&body, &inner_scope)?);
                        }
                        Ok(ConfigValue::List(results))
                    }
                    v => Err(EvalErr::TypeMismatch { expected: "list", got: v.type_name() }),
                }
            }
        }
    }

    fn eval_interp_string(&mut self, s: &InterpolString, local_scope: &HashMap<String, ConfigValue>) -> EvalResult_ {
        let mut result = String::new();
        for part in &s.parts {
            match part {
                StringPart::Literal(text) => result.push_str(text),
                StringPart::Expr(expr)    => {
                    let val = self.eval_expr(expr, local_scope)?;
                    result.push_str(&val.coerce_to_str());
                }
            }
        }
        Ok(ConfigValue::Str(result))
    }

    fn eval_section_field_direct(
        &mut self,
        section_path: &[String],
        field_name:   &str,
        span:         &Span,
    ) -> EvalResult_ {
        if let Some(cached) = self.section_cache.get(section_path) {
            if let Some(val) = cached.get(field_name) {
                return Ok(val.clone());
            }
        }

        let field_value = self.program.items.iter().find_map(|item| {
            if let TopLevelItem::Section(d) = item {
                if d.path == section_path {
                    return d.items.iter().find_map(|si| {
                        if let SectionItem::Field(f) = si {
                            if f.name == field_name { return f.value.clone(); }
                        }
                        None
                    });
                }
            }
            None
        });

        match field_value {
            None => Err(EvalErr::CyclicRef {
                name: format!("{}::{field_name}", section_path.join(".")),
                span: span.clone(),
            }),
            Some(FieldValue::Nested(_)) => {
                Err(EvalErr::NotScalar {
                    name: format!("{}::{field_name}", section_path.join(".")),
                    span: span.clone(),
                })
            }
            Some(FieldValue::Expr(e)) => {
                let val = self.eval_expr(&e, &HashMap::new())?;
                self.section_cache
                    .entry(section_path.to_vec())
                    .or_default()
                    .insert(field_name.to_string(), val.clone());
                Ok(val)
            }
        }
    }

    fn eval_namespace_ref(&mut self, nr: &NamespaceRef, local_scope: &HashMap<String, ConfigValue>) -> EvalResult_ {
        match nr.segments.as_slice() {
            [name] => {
                // Check local scope first
                if let Some(val) = local_scope.get(name.as_str()) {
                    return Ok(val.clone());
                }
                self.eval_global(name).ok_or_else(|| EvalErr::CyclicRef {
                    name: name.clone(),
                    span: nr.span.clone(),
                })
            }

            [ns, name] if ns == "global" => {
                self.eval_global(name).ok_or_else(|| EvalErr::CyclicRef {
                    name: name.clone(),
                    span: nr.span.clone(),
                })
            }

            [ns, field] => {
                if self.symbols.lookup_section(&[ns.to_string()]).is_some() {
                    self.eval_section_field_direct(&[ns.to_string()], field, &nr.span)
                } else if let Some(imp_prog) = self.imported_programs.get(ns.as_str()).cloned() {
                    let imp_sym = crate::resolver::Resolver::new()
                        .resolve(&imp_prog, &[])
                        .unwrap_or_else(|_| self.symbols.clone());
                    let mut sub = Evaluator::new(imp_sym, imp_prog);
                    sub.imported_programs = self.imported_programs.clone();
                    sub.eval_global(field).ok_or_else(|| EvalErr::ImportRef {
                        alias: ns.to_string(),
                        symbol: field.to_string(),
                    })
                } else {
                    Err(EvalErr::CyclicRef {
                        name: format!("{ns}::{field}"),
                        span: nr.span.clone(),
                    })
                }
            }

            [ns, section, field] if ns == "global" => {
                self.eval_section_field_direct(&[section.to_string()], field, &nr.span)
            }

            [ns, section, field] => {
                let path = vec![ns.to_string(), section.to_string()];
                if self.symbols.lookup_section(&path).is_some() {
                    self.eval_section_field_direct(&path, field, &nr.span)
                } else if let Some(imp_prog) = self.imported_programs.get(ns.as_str()).cloned() {
                    let imp_sym = crate::resolver::Resolver::new()
                        .resolve(&imp_prog, &[])
                        .unwrap_or_else(|_| self.symbols.clone());
                    let mut sub = Evaluator::new(imp_sym, imp_prog);
                    sub.imported_programs = self.imported_programs.clone();
                    sub.eval_section_field_direct(&[section.to_string()], field, &nr.span)
                        .map_err(|_| EvalErr::ImportRef {
                            alias: ns.to_string(),
                            symbol: format!("{section}::{field}"),
                        })
                } else {
                    Err(EvalErr::ImportRef {
                        alias: ns.to_string(),
                        symbol: format!("{section}::{field}"),
                    })
                }
            }

            segments if segments.len() >= 2 => {
                // 4+ segments: section_path = segments[..n-1], field = segments[n-1]
                let (field, section_path) = segments.split_last().unwrap();
                self.eval_section_field_direct(section_path, field.as_str(), &nr.span)
            }
            _ => Err(EvalErr::CyclicRef {
                name: nr.segments.join("::"),
                span: nr.span.clone(),
            }),
        }
    }

    fn eval_fn_call(&mut self, fc: &FnCall, local_scope: &HashMap<String, ConfigValue>) -> EvalResult_ {
        match fc.name.as_str() {
            "env" => {
                let key = match self.eval_expr(&fc.args[0], local_scope)? {
                    ConfigValue::Str(s) => s,
                    other => return Err(EvalErr::CyclicRef {
                        name: format!("env() arg must be str, got {}", other.type_name()),
                        span: Span::dummy(),
                    }),
                };
                std::env::var(&key)
                    .map(ConfigValue::Str)
                    .map_err(|_| EvalErr::EnvVarMissing(key))
            }
            "str" => {
                let val = self.eval_expr(&fc.args[0], local_scope)?;
                Ok(ConfigValue::Str(val.coerce_to_str()))
            }
            "int" => {
                let val = self.eval_expr(&fc.args[0], local_scope)?;
                match val {
                    ConfigValue::Int(i)   => Ok(ConfigValue::Int(i)),
                    ConfigValue::Float(f) => Ok(ConfigValue::Int(f as i64)),
                    ConfigValue::Str(s)   => s.trim().parse::<i64>().map(ConfigValue::Int).map_err(|_| {
                        EvalErr::CyclicRef {
                            name: format!("cannot convert {:?} to int", s),
                            span: fc.span.clone(),
                        }
                    }),
                    v => Err(EvalErr::TypeMismatch { expected: "int, float, or str", got: v.type_name() }),
                }
            }
            "float" => {
                let val = self.eval_expr(&fc.args[0], local_scope)?;
                match val {
                    ConfigValue::Int(i)   => Ok(ConfigValue::Float(i as f64)),
                    ConfigValue::Float(f) => Ok(ConfigValue::Float(f)),
                    ConfigValue::Str(s)   => s.trim().parse::<f64>().map(ConfigValue::Float).map_err(|_| {
                        EvalErr::CyclicRef {
                            name: format!("cannot convert {:?} to float", s),
                            span: fc.span.clone(),
                        }
                    }),
                    v => Err(EvalErr::TypeMismatch { expected: "int, float, or str", got: v.type_name() }),
                }
            }
            "bool" => {
                let val = self.eval_expr(&fc.args[0], local_scope)?;
                match val {
                    ConfigValue::Bool(b) => Ok(ConfigValue::Bool(b)),
                    ConfigValue::Str(s)  => match s.trim() {
                        "true"  => Ok(ConfigValue::Bool(true)),
                        "false" => Ok(ConfigValue::Bool(false)),
                        other   => Err(EvalErr::CyclicRef {
                            name: format!(
                                "cannot convert {:?} to bool (expected \"true\" or \"false\")",
                                other
                            ),
                            span: fc.span.clone(),
                        }),
                    },
                    v => Err(EvalErr::TypeMismatch { expected: "str or bool", got: v.type_name() }),
                }
            }
            other => Err(EvalErr::CyclicRef {
                name: format!("unknown built-in `{other}`"),
                span: Span::dummy(),
            }),
        }
    }

    fn eval_binop(&mut self, op: &BinaryOp, local_scope: &HashMap<String, ConfigValue>) -> EvalResult_ {
        if op.op == BinOp::Fallback {
            return match self.eval_expr(&op.lhs, local_scope) {
                Ok(val) => Ok(val),
                Err(EvalErr::EnvVarMissing(_)) | Err(EvalErr::ImportRef { .. }) => {
                    self.eval_expr(&op.rhs, local_scope)
                }
                Err(e) => Err(e),
            };
        }

        if matches!(op.op, BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq | BinOp::And | BinOp::Or) {
            return self.eval_comparison_or_logical(op, local_scope);
        }

        let lhs = self.eval_expr(&op.lhs, local_scope)?;
        let rhs = self.eval_expr(&op.rhs, local_scope)?;

        match (&op.op, &lhs, &rhs) {
            (BinOp::Add, ConfigValue::Int(a),   ConfigValue::Int(b))   => Ok(ConfigValue::Int(a + b)),
            (BinOp::Add, ConfigValue::Float(a), ConfigValue::Float(b)) => Ok(ConfigValue::Float(a + b)),
            (BinOp::Add, ConfigValue::Str(a),   ConfigValue::Str(b))   => {
                Ok(ConfigValue::Str(format!("{a}{b}")))
            }
            (BinOp::Sub, ConfigValue::Int(a),   ConfigValue::Int(b))   => Ok(ConfigValue::Int(a - b)),
            (BinOp::Sub, ConfigValue::Float(a), ConfigValue::Float(b)) => Ok(ConfigValue::Float(a - b)),
            (BinOp::Mul, ConfigValue::Int(a),   ConfigValue::Int(b))   => Ok(ConfigValue::Int(a * b)),
            (BinOp::Mul, ConfigValue::Float(a), ConfigValue::Float(b)) => Ok(ConfigValue::Float(a * b)),
            (BinOp::Div, ConfigValue::Int(_),   ConfigValue::Int(0))   => {
                Err(EvalErr::DivisionByZero(op.span.clone()))
            }
            (BinOp::Div, ConfigValue::Int(a),   ConfigValue::Int(b))   => Ok(ConfigValue::Int(a / b)),
            (BinOp::Div, ConfigValue::Float(a), ConfigValue::Float(b)) => {
                if *b == 0.0 { Err(EvalErr::DivisionByZero(op.span.clone())) }
                else { Ok(ConfigValue::Float(a / b)) }
            }
            (op_kind, l, r) => unreachable!(
                "evaluator reached invalid binop {:?} on {} and {} — \
                 type checker should have caught this",
                op_kind, l.type_name(), r.type_name()
            ),
        }
    }

    fn eval_comparison_or_logical(
        &mut self,
        b: &BinaryOp,
        local_scope: &HashMap<String, ConfigValue>,
    ) -> EvalResult_ {
        match b.op {
            BinOp::And => {
                let lhs = self.eval_expr(&b.lhs, local_scope)?;
                if let ConfigValue::Bool(false) = &lhs {
                    return Ok(ConfigValue::Bool(false));
                }
                let rhs = self.eval_expr(&b.rhs, local_scope)?;
                match (lhs, rhs) {
                    (ConfigValue::Bool(l), ConfigValue::Bool(r)) => Ok(ConfigValue::Bool(l && r)),
                    _ => unreachable!("typechecker ensures bool operands"),
                }
            }
            BinOp::Or => {
                let lhs = self.eval_expr(&b.lhs, local_scope)?;
                if let ConfigValue::Bool(true) = &lhs {
                    return Ok(ConfigValue::Bool(true));
                }
                let rhs = self.eval_expr(&b.rhs, local_scope)?;
                match (lhs, rhs) {
                    (ConfigValue::Bool(l), ConfigValue::Bool(r)) => Ok(ConfigValue::Bool(l || r)),
                    _ => unreachable!(),
                }
            }
            BinOp::Eq => {
                let lhs = self.eval_expr(&b.lhs, local_scope)?;
                let rhs = self.eval_expr(&b.rhs, local_scope)?;
                Ok(ConfigValue::Bool(lhs == rhs))
            }
            BinOp::NotEq => {
                let lhs = self.eval_expr(&b.lhs, local_scope)?;
                let rhs = self.eval_expr(&b.rhs, local_scope)?;
                Ok(ConfigValue::Bool(lhs != rhs))
            }
            BinOp::Lt   => self.eval_numeric_cmp(b, local_scope, |a, b| a < b, |a, b| a < b),
            BinOp::Gt   => self.eval_numeric_cmp(b, local_scope, |a, b| a > b, |a, b| a > b),
            BinOp::LtEq => self.eval_numeric_cmp(b, local_scope, |a, b| a <= b, |a, b| a <= b),
            BinOp::GtEq => self.eval_numeric_cmp(b, local_scope, |a, b| a >= b, |a, b| a >= b),
            _ => unreachable!(),
        }
    }

    fn eval_numeric_cmp(
        &mut self,
        b: &BinaryOp,
        local_scope: &HashMap<String, ConfigValue>,
        int_cmp:   impl Fn(i64, i64) -> bool,
        float_cmp: impl Fn(f64, f64) -> bool,
    ) -> EvalResult_ {
        let lhs = self.eval_expr(&b.lhs, local_scope)?;
        let rhs = self.eval_expr(&b.rhs, local_scope)?;
        match (lhs, rhs) {
            (ConfigValue::Int(l),   ConfigValue::Int(r))   => Ok(ConfigValue::Bool(int_cmp(l, r))),
            (ConfigValue::Float(l), ConfigValue::Float(r)) => Ok(ConfigValue::Bool(float_cmp(l, r))),
            _ => unreachable!("typechecker ensures numeric operands"),
        }
    }
}

// ── Function call evaluation ──────────────────────────────────────────────────

impl Evaluator {
    fn eval_call(
        &mut self,
        name: &str,
        args: &[CallArg],
        caller_scope: &HashMap<String, ConfigValue>,
    ) -> EvalResult_ {
        if self.call_depth >= MAX_CALL_DEPTH {
            return Err(EvalErr::MaxCallDepth { name: name.to_string() });
        }
        self.call_depth += 1;

        // Cross-file call: alias::fn(args)
        if let Some(sep) = name.find("::") {
            let alias = &name[..sep];
            let fn_name = &name[sep + 2..];
            if let Some(imp_prog) = self.imported_programs.get(alias).cloned() {
                let func_decl = imp_prog.items.iter().find_map(|item| {
                    if let TopLevelItem::Function(f) = item {
                        if f.name == fn_name && !f.is_private { return Some(f.clone()); }
                    }
                    None
                });
                if let Some(fd) = func_decl {
                    // Resolve + build minimal symbols for the imported program
                    let imp_sym = crate::resolver::Resolver::new()
                        .resolve(&imp_prog, &[])
                        .unwrap_or_else(|_| self.symbols.clone());
                    let mut local_scope: HashMap<String, ConfigValue> = HashMap::new();
                    for arg in args {
                        let val = self.eval_expr(&arg.value, caller_scope)?;
                        local_scope.insert(arg.param_name.clone(), val);
                    }
                    // Create sub-evaluator for imported program
                    let mut sub = Evaluator::new(imp_sym, imp_prog);
                    sub.call_depth = self.call_depth;
                    let result = sub.eval_func_stmts(&fd.body.stmts.clone(), &mut local_scope)?
                        .unwrap_or(ConfigValue::Int(0));
                    self.call_depth -= 1;
                    return Ok(result);
                }
            }
            self.call_depth -= 1;
            return Err(EvalErr::ImportRef { alias: alias.to_string(), symbol: fn_name.to_string() });
        }

        let func_decl = self.program.items.iter().find_map(|item| {
            if let TopLevelItem::Function(f) = item {
                if f.name == name { return Some(f.clone()); }
            }
            None
        }).unwrap(); // resolver ensures function exists

        // Evaluate args in caller scope
        let mut local_scope: HashMap<String, ConfigValue> = HashMap::new();
        for arg in args {
            let val = self.eval_expr(&arg.value, caller_scope)?;
            local_scope.insert(arg.param_name.clone(), val);
        }

        // Execute body statements; a Return statement propagates its value out
        let result = self.eval_func_stmts(&func_decl.body.stmts.clone(), &mut local_scope)?
            .unwrap(); // resolver ensures every path returns

        self.call_depth -= 1;
        Ok(result)
    }

    fn eval_func_stmts(
        &mut self,
        stmts: &[FuncStmt],
        local_scope: &mut HashMap<String, ConfigValue>,
    ) -> Result<Option<ConfigValue>, EvalErr> {
        for stmt in stmts {
            match stmt {
                FuncStmt::LocalVar(lv) => {
                    let val = self.eval_expr(&lv.value.clone(), local_scope)?;
                    local_scope.insert(lv.name.clone(), val);
                }
                FuncStmt::Return(ret_value, _) => {
                    let val = match ret_value {
                        ReturnValue::Expr(e) => self.eval_expr(&e.clone(), local_scope)?,
                        ReturnValue::SectionBlock(fields) => {
                            let fields = fields.clone();
                            let mut map = HashMap::new();
                            for rf in &fields {
                                let v = self.eval_expr(&rf.value, local_scope)?;
                                map.insert(rf.name.clone(), v);
                            }
                            ConfigValue::Section(map)
                        }
                    };
                    return Ok(Some(val));
                }
                FuncStmt::For { var_name, iterable, body, .. } => {
                    let items = match self.eval_expr(&iterable.clone(), local_scope)? {
                        ConfigValue::List(items) => items,
                        _ => unreachable!("typechecker ensures for-loop iterable is a list"),
                    };
                    for item in items {
                        let mut loop_scope = local_scope.clone();
                        loop_scope.insert(var_name.clone(), item);
                        let body = body.clone();
                        if let Some(v) = self.eval_func_stmts(&body, &mut loop_scope)? {
                            return Ok(Some(v));
                        }
                    }
                }
                FuncStmt::If(if_stmt) => {
                    let cond = self.eval_expr(&if_stmt.condition.clone(), local_scope)?;
                    let branch = match cond {
                        ConfigValue::Bool(true)  => if_stmt.then_stmts.clone(),
                        ConfigValue::Bool(false) => if_stmt.else_stmts.clone(),
                        _ => unreachable!("typechecker ensures bool condition"),
                    };
                    let mut branch_scope = local_scope.clone();
                    if let Some(v) = self.eval_func_stmts(&branch, &mut branch_scope)? {
                        return Ok(Some(v));
                    }
                    for (k, v) in branch_scope {
                        local_scope.insert(k, v);
                    }
                }
            }
        }
        Ok(None)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn eval_ok(src: &str) -> EvalResult {
        let tokens  = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        let table   = crate::resolver::Resolver::new().resolve(&program, &[]).expect("resolve");
        crate::typechecker::TypeChecker::check(&program, &table).expect("typecheck");
        Evaluator::evaluate(&program, &table).expect("eval failed")
    }

    fn eval_err(src: &str) -> Vec<String> {
        let tokens  = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        let table   = crate::resolver::Resolver::new().resolve(&program, &[]).expect("resolve");
        Evaluator::evaluate(&program, &table)
            .unwrap_err()
            .into_iter()
            .map(|e| e.to_string())
            .collect()
    }

    fn global(result: &EvalResult, name: &str) -> ConfigValue {
        result.globals[name].clone()
    }

    fn section_field(result: &EvalResult, path: &[&str], field: &str) -> ConfigValue {
        let key: Vec<String> = path.iter().map(|s| s.to_string()).collect();
        result.sections[&key][field].clone()
    }

    #[test]
    fn section_complete_when_var_references_its_field() {
        let src = r#"
[Man]{ aster: int = 6; };
[MetaData]{
    tool:    str = "stackforge";
    version: int = Man::aster;
    flag:    bool = false;
};
var x: str = MetaData::tool;
"#;
        let tokens  = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let symbols = crate::resolver::Resolver::new().resolve(&program, &[]).unwrap();
        let result  = Evaluator::evaluate(&program, &symbols).unwrap();

        let metadata = result.sections
            .get(&vec!["MetaData".to_string()])
            .expect("MetaData section must exist in EvalResult");

        assert!(metadata.contains_key("tool"),    "MetaData must contain 'tool'");
        assert!(metadata.contains_key("version"), "MetaData must contain 'version'");
        assert!(metadata.contains_key("flag"),    "MetaData must contain 'flag'");
    }

    #[test]
    fn variable_can_reference_section_field_correctly() {
        let src = r#"
[Config]{ host: str = "localhost"; };
var endpoint: str = Config::host;
"#;
        let tokens  = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let program = crate::parser::Parser::new(tokens).parse().unwrap();
        let symbols = crate::resolver::Resolver::new().resolve(&program, &[]).unwrap();
        let result  = Evaluator::evaluate(&program, &symbols).unwrap();

        assert_eq!(
            result.globals.get("endpoint"),
            Some(&ConfigValue::Str("localhost".to_string()))
        );
        assert!(result.sections.get(&vec!["Config".to_string()]).unwrap().contains_key("host"));
    }

    #[test]
    fn test_int_literal() {
        let r = eval_ok("var x: int = 42;");
        assert_eq!(global(&r, "x"), ConfigValue::Int(42));
    }

    #[test]
    fn test_float_literal() {
        let r = eval_ok("var x: float = 3.14;");
        assert_eq!(global(&r, "x"), ConfigValue::Float(3.14));
    }

    #[test]
    fn test_bool_true() {
        let r = eval_ok("var x: bool = true;");
        assert_eq!(global(&r, "x"), ConfigValue::Bool(true));
    }

    #[test]
    fn test_bool_false() {
        let r = eval_ok("var x: bool = false;");
        assert_eq!(global(&r, "x"), ConfigValue::Bool(false));
    }

    #[test]
    fn test_string_literal() {
        let r = eval_ok(r#"var x: str = "keel";"#);
        assert_eq!(global(&r, "x"), ConfigValue::Str("keel".into()));
    }

    #[test]
    fn test_int_addition() {
        let r = eval_ok("var x: int = 10 + 32;");
        assert_eq!(global(&r, "x"), ConfigValue::Int(42));
    }

    #[test]
    fn test_int_multiplication() {
        let r = eval_ok("var x: int = 6 * 7;");
        assert_eq!(global(&r, "x"), ConfigValue::Int(42));
    }

    #[test]
    fn test_int_subtraction() {
        let r = eval_ok("var x: int = 50 - 8;");
        assert_eq!(global(&r, "x"), ConfigValue::Int(42));
    }

    #[test]
    fn test_int_division() {
        let r = eval_ok("var x: int = 84 / 2;");
        assert_eq!(global(&r, "x"), ConfigValue::Int(42));
    }

    #[test]
    fn test_float_arithmetic() {
        let r = eval_ok("var x: float = 3.0 * 1.5;");
        assert_eq!(global(&r, "x"), ConfigValue::Float(4.5));
    }

    #[test]
    fn test_division_by_zero_error() {
        let errs = eval_err("var x: int = 10 / 0;");
        assert!(errs.iter().any(|e| e.contains("division by zero")), "got: {errs:?}");
    }

    #[test]
    fn test_string_concatenation() {
        let r = eval_ok(r#"var x: str = "hel" + "lo";"#);
        assert_eq!(global(&r, "x"), ConfigValue::Str("hello".into()));
    }

    #[test]
    fn test_string_interpolation_str_ref() {
        let r = eval_ok(r#"
            var name: str = "world";
            var msg: str = "hello ${global::name}";
        "#);
        assert_eq!(global(&r, "msg"), ConfigValue::Str("hello world".into()));
    }

    #[test]
    fn test_string_interpolation_int_coerced() {
        let r = eval_ok(r#"
            var port: int = 3000;
            var bind: str = "0.0.0.0:${global::port}";
        "#);
        assert_eq!(global(&r, "bind"), ConfigValue::Str("0.0.0.0:3000".into()));
    }

    #[test]
    fn test_str_coercion_of_int() {
        let r = eval_ok("var x: str = str(42);");
        assert_eq!(global(&r, "x"), ConfigValue::Str("42".into()));
    }

    #[test]
    fn test_str_coercion_of_bool() {
        let r = eval_ok("var x: str = str(true);");
        assert_eq!(global(&r, "x"), ConfigValue::Str("true".into()));
    }

    #[test]
    fn test_env_var_set() {
        std::env::set_var("SPAR_TEST_VAR", "hello");
        let r = eval_ok(r#"var x: str = env("SPAR_TEST_VAR");"#);
        std::env::remove_var("SPAR_TEST_VAR");
        assert_eq!(global(&r, "x"), ConfigValue::Str("hello".into()));
    }

    #[test]
    fn test_env_var_missing_error() {
        std::env::remove_var("SPAR_TEST_MISSING_XYZ");
        let errs = eval_err(r#"var x: str = env("SPAR_TEST_MISSING_XYZ");"#);
        assert!(
            errs.iter().any(|e| e.contains("not set") || e.contains("EnvVar")),
            "got: {errs:?}"
        );
    }

    #[test]
    fn test_env_fallback_var_set() {
        std::env::set_var("SPAR_TEST_VAR2", "set");
        let r = eval_ok(r#"var x: str = env("SPAR_TEST_VAR2") ?? "default";"#);
        std::env::remove_var("SPAR_TEST_VAR2");
        assert_eq!(global(&r, "x"), ConfigValue::Str("set".into()));
    }

    #[test]
    fn test_env_fallback_var_missing() {
        std::env::remove_var("SPAR_TEST_MISSING_XYZ");
        let r = eval_ok(r#"var x: str = env("SPAR_TEST_MISSING_XYZ") ?? "default";"#);
        assert_eq!(global(&r, "x"), ConfigValue::Str("default".into()));
    }

    #[test]
    fn test_namespace_ref_single_segment() {
        let r = eval_ok("var port: int = 3000; var copy: int = port;");
        assert_eq!(global(&r, "copy"), ConfigValue::Int(3000));
    }

    #[test]
    fn test_namespace_ref_global_prefix() {
        let r = eval_ok("var port: int = 3000; var copy: int = global::port;");
        assert_eq!(global(&r, "copy"), ConfigValue::Int(3000));
    }

    #[test]
    fn test_namespace_ref_section_field() {
        let r = eval_ok("[Db]{ pool: int = 5; }; var p: int = Db::pool;");
        assert_eq!(global(&r, "p"), ConfigValue::Int(5));
    }

    #[test]
    fn test_namespace_ref_nested_section_field_3seg() {
        let r = eval_ok(r#"
            [Server]{ rateLimit: section = { enabled: bool = true; }; };
            var isDone: bool = Server::rateLimit::enabled;
        "#);
        assert_eq!(global(&r, "isDone"), ConfigValue::Bool(true));
    }

    #[test]
    fn test_forward_reference() {
        let r = eval_ok(r#"
            var bind: str = "host:${global::port}";
            var port: int = 9000;
        "#);
        assert_eq!(global(&r, "bind"), ConfigValue::Str("host:9000".into()));
    }

    #[test]
    fn test_cycle_detection_error() {
        let errs = eval_err("var a: str = global::b; var b: str = global::a;");
        assert!(errs.iter().any(|e| e.contains("cyclic")), "got: {errs:?}");
    }

    #[test]
    fn test_simple_section_evaluation() {
        let r = eval_ok(r#"[Server]{ port: int = 8080; host: str = "localhost"; };"#);
        assert_eq!(section_field(&r, &["Server"], "port"), ConfigValue::Int(8080));
        assert_eq!(section_field(&r, &["Server"], "host"), ConfigValue::Str("localhost".into()));
    }

    #[test]
    fn test_section_field_references_global() {
        let r = eval_ok("var timeout: int = 30; [Db]{ timeout: int = global::timeout; };");
        assert_eq!(section_field(&r, &["Db"], "timeout"), ConfigValue::Int(30));
    }

    #[test]
    fn test_spread_merges_fields() {
        let r = eval_ok(r#"
            [Defaults]{ workers: int = 4; timeout: int = 30; };
            [Server]{ ...Defaults; port: int = 8080; };
        "#);
        assert_eq!(section_field(&r, &["Server"], "workers"), ConfigValue::Int(4));
        assert_eq!(section_field(&r, &["Server"], "port"),    ConfigValue::Int(8080));
    }

    #[test]
    fn test_spread_explicit_overrides_spread() {
        let r = eval_ok(r#"
            [Defaults]{ workers: int = 4; port: int = 3000; };
            [Server]{ ...Defaults; port: int = 8080; };
        "#);
        assert_eq!(section_field(&r, &["Server"], "port"),    ConfigValue::Int(8080));
        assert_eq!(section_field(&r, &["Server"], "workers"), ConfigValue::Int(4));
    }

    #[test]
    fn test_dynamic_mixed_list() {
        let r = eval_ok(r#"dynamic var tags = [2026, "prod", true];"#);
        match global(&r, "tags") {
            ConfigValue::List(items) => {
                assert_eq!(items[0], ConfigValue::Int(2026));
                assert_eq!(items[1], ConfigValue::Str("prod".into()));
                assert_eq!(items[2], ConfigValue::Bool(true));
            }
            _ => panic!("expected list"),
        }
    }

    #[test]
    fn nested_section_evaluated_in_sections_map() {
        let r = eval_ok(r#"[Outer]{ inner: section = { key: str = "v"; }; };"#);
        let nested_key = vec!["Outer".to_string(), "inner".to_string()];
        assert!(
            r.sections.contains_key(&nested_key),
            "nested section must appear in EvalResult.sections"
        );
        assert_eq!(
            r.sections[&nested_key]["key"],
            ConfigValue::Str("v".into())
        );
    }

    #[test]
    fn nested_section_twice_deep_evaluated() {
        let r = eval_ok("[A]{ b: section = { c: section = { val: int = 1; }; }; };");
        let inner_key = vec!["A".to_string(), "b".to_string(), "c".to_string()];
        assert!(
            r.sections.contains_key(&inner_key),
            "two-deep nested section must appear in EvalResult.sections"
        );
        assert_eq!(r.sections[&inner_key]["val"], ConfigValue::Int(1));
    }

    // ── Group 5: early-return evaluator ──────────────────────────────────────

    #[test]
    fn early_return_scalar_function() {
        let r = eval_ok(r#"
            function double(x: int) -> int {
                return x * 2;
            }
            var n: int = double(x: 5);
        "#);
        assert_eq!(global(&r, "n"), ConfigValue::Int(10));
    }

    #[test]
    fn early_return_from_if_branch() {
        let r = eval_ok(r#"
            function absVal(x: int) -> int {
                if x < 0 { return 0 - x; } else { return x; }
            }
            var a: int = absVal(x: 0 - 3);
            var b: int = absVal(x: 7);
        "#);
        assert_eq!(global(&r, "a"), ConfigValue::Int(3));
        assert_eq!(global(&r, "b"), ConfigValue::Int(7));
    }

    #[test]
    fn early_return_in_then_branch_else_falls_through() {
        let r = eval_ok(r#"
            function clamp(x: int) -> int {
                if x > 100 { return 100; }
                else { var y: int = x; }
                return y;
            }
            var a: int = clamp(x: 200);
            var b: int = clamp(x: 42);
        "#);
        assert_eq!(global(&r, "a"), ConfigValue::Int(100));
        assert_eq!(global(&r, "b"), ConfigValue::Int(42));
    }

    #[test]
    fn section_returning_function_result_in_section_cache() {
        let r = eval_ok(r#"
            function makeDb() -> section {
                return { host: str = "localhost"; port: int = 5432; };
            }
            [App]{ db: section = makeDb(); };
        "#);
        let db_path = vec!["App".to_string(), "db".to_string()];
        assert!(
            r.sections.contains_key(&db_path),
            "section fn result must appear in sections map"
        );
        assert_eq!(r.sections[&db_path]["host"], ConfigValue::Str("localhost".into()));
        assert_eq!(r.sections[&db_path]["port"], ConfigValue::Int(5432));
    }

    #[test]
    fn local_var_used_in_return_after_if() {
        let r = eval_ok(r#"
            function choose(flag: bool) -> int {
                if flag { return 1; }
                else { var result: int = 99; }
                return result;
            }
            var x: int = choose(flag: false);
        "#);
        assert_eq!(global(&r, "x"), ConfigValue::Int(99));
    }
}
