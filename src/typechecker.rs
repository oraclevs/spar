use std::collections::HashMap;

use crate::ast::*;
use crate::error::{SparError, Span};
use crate::resolver::{stmts_always_return, GlobalEntry, SymbolTable};

pub fn display_type(ty: &SparType) -> String {
    match ty {
        SparType::Str          => "str".into(),
        SparType::Int          => "int".into(),
        SparType::Float        => "float".into(),
        SparType::Bool         => "bool".into(),
        SparType::Section      => "section".into(),
        SparType::List(inner)  => format!("[{}]", display_type(inner)),
    }
}

pub struct TypeChecker<'a> {
    symbols: &'a SymbolTable,
    errors:  Vec<SparError>,
}

impl<'a> TypeChecker<'a> {
    pub fn check(program: &Program, symbols: &'a SymbolTable) -> Result<(), Vec<SparError>> {
        let mut tc = TypeChecker { symbols, errors: Vec::new() };
        tc.check_program(program);
        if tc.errors.is_empty() { Ok(()) } else { Err(tc.errors) }
    }

    pub fn check_with_imports(
        program: &Program,
        symbols: &'a SymbolTable,
        _loaded: &std::collections::HashMap<String, crate::loader::LoadedImport>,
    ) -> Result<(), Vec<SparError>> {
        Self::check(program, symbols)
    }

    fn push_type_error(&mut self, message: impl Into<String>, hint: Option<String>, span: Span) {
        self.errors.push(SparError::TypeError {
            message: message.into(),
            hint,
            span,
        });
    }

    fn check_program(&mut self, program: &Program) {
        for item in &program.items {
            match item {
                TopLevelItem::Import(_)     => {}
                TopLevelItem::Var(decl)     => self.check_var(decl),
                TopLevelItem::Dynamic(decl) => self.check_dynamic(decl),
                TopLevelItem::Section(decl) => self.check_section(decl),
                TopLevelItem::Function(f)   => self.check_function_decl(f),
                TopLevelItem::SchemaSection(_) => {}
            }
        }
    }

    fn check_var(&mut self, decl: &VarDecl) {
        // Rule A: 'section' is not a valid type for global variables
        if decl.ty == SparType::Section {
            self.push_type_error(
                format!(
                    "'section' is not a valid type for variable '{}' — \
                     declare a named section with '[SectionName]{{ ... }};' instead",
                    decl.name
                ),
                None,
                decl.span.clone(),
            );
            return;
        }
        if !decl.optional && decl.value.is_none() {
            self.push_type_error(
                format!(
                    "required variable `{}` has no value — add `= <value>` or mark optional with `?`",
                    decl.name
                ),
                None,
                decl.span.clone(),
            );
            return;
        }
        if let Some(val) = &decl.value {
            self.check_expr_type(val, &decl.ty, &decl.name, &decl.span);
        }
    }

    fn check_dynamic(&mut self, decl: &DynamicDecl) {
        if !decl.optional && decl.value.is_none() {
            self.push_type_error(
                format!(
                    "required dynamic variable `{}` has no value — add `= [...]` or mark optional with `?`",
                    decl.name
                ),
                None,
                decl.span.clone(),
            );
        }
    }

    fn check_section(&mut self, decl: &SectionDecl) {
        let path_str = decl.path.join(".");
        for item in &decl.items {
            if let SectionItem::Field(field) = item {
                self.check_field(field, &path_str);
            }
        }
    }

    fn check_field(&mut self, field: &FieldDecl, path_str: &str) {
        // Rule B: validate body vs type compatibility
        match (&field.ty, &field.value) {
            (SparType::Section, Some(FieldValue::Expr(e))) => {
                let actual = self.infer_type(e);
                if actual != Some(SparType::Section) {
                    self.push_type_error(
                        format!(
                            "field '{}' in '[{path_str}]' has type 'section' but value is {} \
                             — use '= {{ ... }}' for a nested section body or a function returning 'section'",
                            field.name,
                            actual.as_ref().map(|t| display_type(t)).unwrap_or_else(|| "unknown".into()),
                        ),
                        None,
                        field.span.clone(),
                    );
                }
                self.check_expr_internal(e);
                return;
            }
            (ty, Some(FieldValue::Nested(_))) if *ty != SparType::Section => {
                self.push_type_error(
                    format!(
                        "field '{}' in '[{path_str}]' has type '{}' but uses a section \
                         body '{{ ... }}' — only 'section'-typed fields can have a nested body",
                        field.name, display_type(ty)
                    ),
                    Some("change the field type to 'section' or use an expression value".into()),
                    field.span.clone(),
                );
                return;
            }
            (SparType::Section, Some(FieldValue::Nested(sub_fields))) => {
                // Recursively type-check the nested section
                let nested_path = format!("{path_str}.{}", field.name);
                for sub in sub_fields {
                    self.check_field(sub, &nested_path);
                }
                return;
            }
            (SparType::Section, None) => {
                if !field.optional {
                    self.push_type_error(
                        format!(
                            "required field '{}' in section '[{path_str}]' has no value",
                            field.name
                        ),
                        None,
                        field.span.clone(),
                    );
                }
                return;
            }
            _ => {}
        }

        // Original logic for non-section fields:
        if !field.optional && field.value.is_none() {
            self.push_type_error(
                format!(
                    "required field `{}` in section `[{path_str}]` has no value",
                    field.name
                ),
                None,
                field.span.clone(),
            );
            return;
        }
        if let Some(FieldValue::Expr(val)) = &field.value {
            self.check_expr_type(val, &field.ty, &field.name, &field.span);
        }
    }

    fn infer_type(&self, expr: &Expr) -> Option<SparType> {
        match expr {
            Expr::Literal(Literal::Int(_))   => Some(SparType::Int),
            Expr::Literal(Literal::Float(_)) => Some(SparType::Float),
            Expr::Literal(Literal::Bool(_))  => Some(SparType::Bool),
            Expr::String(_)                  => Some(SparType::Str),
            Expr::List(items, _) => {
                items.first()
                    .and_then(|e| self.infer_type(e))
                    .map(|t| SparType::List(Box::new(t)))
            }
            Expr::NamespaceRef(nr) => self.infer_namespace_type(nr),
            Expr::FnCall(fc) => match fc.name.as_str() {
                "env" | "str" => Some(SparType::Str),
                "int"         => Some(SparType::Int),
                "float"       => Some(SparType::Float),
                "bool"        => Some(SparType::Bool),
                _             => None,
            },
            Expr::BinaryOp(op) => {
                let lhs = self.infer_type(&op.lhs)?;
                let rhs = self.infer_type(&op.rhs)?;
                self.infer_binop_type(&op.op, &lhs, &rhs)
            }
            Expr::Grouped(inner, _) => self.infer_type(inner),
            Expr::Call { name, .. } => {
                if name.contains("::") { return None; } // cross-file call — type unknown here
                self.symbols.functions.get(name).map(|fe| fe.ret.clone())
            }
            Expr::Unary { op, operand, .. } => match op {
                UnOp::Not => {
                    let t = self.infer_type(operand)?;
                    if t == SparType::Bool { Some(SparType::Bool) } else { None }
                }
                UnOp::Neg => {
                    let t = self.infer_type(operand)?;
                    if matches!(t, SparType::Int | SparType::Float) { Some(t) } else { None }
                }
            },
            Expr::Index { source, .. } => match self.infer_type(source)? {
                SparType::List(elem) => Some(*elem),
                _ => None,
            },
            Expr::Comprehension { source, .. } => {
                // At global scope we can't infer the body type without loop-variable locals;
                // source-not-a-list errors are reported by check_expr_internal.
                let source_ty = self.infer_type(source)?;
                match source_ty {
                    SparType::List(_) => None, // body type unknown without locals
                    _ => None,               // source is not a list; error reported elsewhere
                }
            }
        }
    }

    fn infer_namespace_type(&self, nr: &NamespaceRef) -> Option<SparType> {
        match nr.segments.as_slice() {
            [name] => self.lookup_global_type(name),
            [ns, name] if ns == "global" => self.lookup_global_type(name),
            [ns, name] => {
                let key = vec![ns.clone()];
                self.symbols.lookup_section(&key)
                    .and_then(|s| s.fields.get(name.as_str()))
                    .map(|f| f.ty.clone())
            }
            [_, section, field] => {
                let key = vec![section.clone()];
                self.symbols.lookup_section(&key)
                    .and_then(|s| s.fields.get(field.as_str()))
                    .map(|f| f.ty.clone())
            }
            _ => None,
        }
    }

    fn lookup_global_type(&self, name: &str) -> Option<SparType> {
        match self.symbols.lookup_global(name)? {
            GlobalEntry::Var { ty, .. } => Some(ty.clone()),
            GlobalEntry::Dynamic { .. } => None,
        }
    }

    fn infer_binop_type(&self, op: &BinOp, lhs: &SparType, rhs: &SparType) -> Option<SparType> {
        match op {
            BinOp::Add => match (lhs, rhs) {
                (SparType::Str,   SparType::Str)   => Some(SparType::Str),
                (SparType::Int,   SparType::Int)   => Some(SparType::Int),
                (SparType::Float, SparType::Float) => Some(SparType::Float),
                _                              => None,
            },
            BinOp::Sub | BinOp::Mul | BinOp::Div => match (lhs, rhs) {
                (SparType::Int,   SparType::Int)   => Some(SparType::Int),
                (SparType::Float, SparType::Float) => Some(SparType::Float),
                _                              => None,
            },
            BinOp::Fallback => {
                if lhs == rhs { Some(lhs.clone()) } else { None }
            }
            BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq => {
                // Comparison operators return bool
                Some(SparType::Bool)
            }
            BinOp::And | BinOp::Or => {
                // Logical operators require bool operands and return bool
                if *lhs == SparType::Bool && *rhs == SparType::Bool {
                    Some(SparType::Bool)
                } else {
                    None
                }
            }
        }
    }

    fn check_expr_type(&mut self, expr: &Expr, declared_ty: &SparType, label: &str, span: &Span) {
        self.check_expr_internal(expr);

        let inferred = match self.infer_type(expr) {
            Some(t) => t,
            None    => return,
        };

        if let Expr::List(items, _) = expr {
            if let SparType::List(elem_ty) = declared_ty {
                for item in items {
                    if let Some(item_ty) = self.infer_type(item) {
                        if &item_ty != elem_ty.as_ref() {
                            self.push_type_error(
                                format!(
                                    "list element type mismatch in `{label}`: \
                                     expected `{}`, found `{}`",
                                    display_type(elem_ty),
                                    display_type(&item_ty),
                                ),
                                Some(format!(
                                    "all elements in `[{}]` must be `{}`",
                                    display_type(elem_ty),
                                    display_type(elem_ty),
                                )),
                                span.clone(),
                            );
                        }
                    }
                }
                return;
            }
        }

        if &inferred != declared_ty {
            self.push_type_error(
                format!(
                    "type mismatch for `{label}`: declared as `{}` but value is `{}`",
                    display_type(declared_ty),
                    display_type(&inferred),
                ),
                Some(format!(
                    "expected `{}`, found `{}`",
                    display_type(declared_ty),
                    display_type(&inferred),
                )),
                span.clone(),
            );
        }
    }

    fn check_expr_internal(&mut self, expr: &Expr) {
        match expr {
            Expr::BinaryOp(op) => {
                self.check_expr_internal(&op.lhs);
                self.check_expr_internal(&op.rhs);

                let lhs_ty = self.infer_type(&op.lhs);
                let rhs_ty = self.infer_type(&op.rhs);

                if let (Some(l), Some(r)) = (&lhs_ty, &rhs_ty) {
                    let valid = match op.op {
                        BinOp::Add => matches!((l, r),
                            (SparType::Str,   SparType::Str)   |
                            (SparType::Int,   SparType::Int)   |
                            (SparType::Float, SparType::Float)),
                        BinOp::Sub | BinOp::Mul | BinOp::Div => matches!((l, r),
                            (SparType::Int,   SparType::Int) |
                            (SparType::Float, SparType::Float)),
                        BinOp::Fallback => l == r,
                        BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq => {
                            // Comparison operators work on comparable types
                            matches!((l, r),
                                (SparType::Int, SparType::Int) |
                                (SparType::Float, SparType::Float) |
                                (SparType::Str, SparType::Str) |
                                (SparType::Bool, SparType::Bool))
                        }
                        BinOp::And | BinOp::Or => {
                            l == &SparType::Bool && r == &SparType::Bool
                        }
                    };
                    if !valid {
                        let op_sym = match op.op {
                            BinOp::Add      => "+",
                            BinOp::Sub      => "-",
                            BinOp::Mul      => "*",
                            BinOp::Div      => "/",
                            BinOp::Fallback => "??",
                            BinOp::Eq       => "==",
                            BinOp::NotEq    => "!=",
                            BinOp::Lt       => "<",
                            BinOp::Gt       => ">",
                            BinOp::LtEq     => "<=",
                            BinOp::GtEq     => ">=",
                            BinOp::And      => "&&",
                            BinOp::Or       => "||",
                        };
                        let msg = match op.op {
                            BinOp::And | BinOp::Or => format!(
                                "operator `{op_sym}` requires bool operands but got `{}` and `{}`",
                                display_type(l),
                                display_type(r),
                            ),
                            _ => format!(
                                "operator `{op_sym}` cannot be applied to `{}` and `{}`",
                                display_type(l),
                                display_type(r),
                            ),
                        };
                        self.push_type_error(msg, None, op.span.clone());
                    }
                }
            }
            Expr::FnCall(fc) => {
                for arg in &fc.args { self.check_expr_internal(arg); }
            }
            Expr::String(s) => {
                for part in &s.parts {
                    if let StringPart::Expr(e) = part { self.check_expr_internal(e); }
                }
            }
            Expr::List(items, _) => {
                for item in items { self.check_expr_internal(item); }
            }
            Expr::Grouped(inner, _) => self.check_expr_internal(inner),
            Expr::Call { args, .. } => {
                for arg in args { self.check_expr_internal(&arg.value); }
                let result = self.check_call(expr);
                if let Err(e) = result {
                    self.errors.push(e);
                }
            }
            Expr::Unary { op, operand, span, .. } => {
                self.check_expr_internal(operand);
                let operand_ty = self.infer_type(operand);
                match op {
                    UnOp::Not => {
                        if operand_ty != Some(SparType::Bool) {
                            self.push_type_error(
                                format!(
                                    "operator `!` requires a bool operand, got {}",
                                    operand_ty.as_ref().map(|t| display_type(t)).unwrap_or_else(|| "unknown".into()),
                                ),
                                None,
                                span.clone(),
                            );
                        }
                    }
                    UnOp::Neg => {
                        if !matches!(&operand_ty, Some(SparType::Int) | Some(SparType::Float)) {
                            self.push_type_error(
                                format!(
                                    "unary `-` requires int or float, got {}",
                                    operand_ty.as_ref().map(|t| display_type(t)).unwrap_or_else(|| "unknown".into()),
                                ),
                                None,
                                span.clone(),
                            );
                        }
                    }
                }
            }
            Expr::Index { source, index, span } => {
                self.check_expr_internal(source);
                self.check_expr_internal(index);
                let index_ty = self.infer_type(index);
                if index_ty != Some(SparType::Int) {
                    self.push_type_error(
                        format!(
                            "list index must be int, got {}",
                            index_ty.as_ref().map(|t| display_type(t)).unwrap_or_else(|| "unknown".into()),
                        ),
                        None,
                        span.clone(),
                    );
                }
                let source_ty = self.infer_type(source);
                if let Some(ty) = &source_ty {
                    if !matches!(ty, SparType::List(_)) {
                        self.push_type_error(
                            format!("cannot index into `{}`", display_type(ty)),
                            None,
                            span.clone(),
                        );
                    }
                }
            }
            Expr::Comprehension { source, body, span, .. } => {
                self.check_expr_internal(source);
                self.check_expr_internal(body);
                let source_ty = self.infer_type(source);
                if !matches!(&source_ty, Some(SparType::List(_))) {
                    self.push_type_error(
                        format!(
                            "for-comprehension source must be a list, got {}",
                            source_ty.as_ref().map(|t| display_type(t)).unwrap_or_else(|| "unknown".into()),
                        ),
                        None,
                        span.clone(),
                    );
                }
            }
            Expr::Literal(_) => {}
            Expr::NamespaceRef(_) => {}
        }
    }

    // ── Call argument type checking ───────────────────────────────────────────

    fn check_call(&self, call: &Expr) -> Result<(), SparError> {
        if let Expr::Call { name, args, .. } = call {
            if let Some(entry) = self.symbols.functions.get(name) {
                for arg in args {
                    let param_ty = entry.params.iter()
                        .find(|(n, _)| n == &arg.param_name)
                        .map(|(_, t)| t.clone());
                    if let Some(param_ty) = param_ty {
                        let actual = self.infer_type(&arg.value);
                        if actual.as_ref() != Some(&param_ty) {
                            return Err(SparError::TypeError {
                                message: format!(
                                    "argument '{}' expects {} but got {}",
                                    arg.param_name,
                                    display_type(&param_ty),
                                    actual.as_ref().map(|t| display_type(t)).unwrap_or_else(|| "unknown".into()),
                                ),
                                hint: None,
                                span: arg.span.clone(),
                            });
                        }
                    }
                    // bad param name: already caught by resolver
                }
            }
            // undefined function: already caught by resolver
        }
        Ok(())
    }

    fn check_call_with_locals(
        &self,
        call: &Expr,
        locals: &HashMap<String, SparType>,
    ) -> Result<(), SparError> {
        if let Expr::Call { name, args, .. } = call {
            if let Some(entry) = self.symbols.functions.get(name) {
                for arg in args {
                    let param_ty = entry.params.iter()
                        .find(|(n, _)| n == &arg.param_name)
                        .map(|(_, t)| t.clone());
                    if let Some(param_ty) = param_ty {
                        let actual = self.infer_type_with_locals(&arg.value, locals);
                        if actual.as_ref() != Some(&param_ty) {
                            return Err(SparError::TypeError {
                                message: format!(
                                    "argument '{}' expects {} but got {}",
                                    arg.param_name,
                                    display_type(&param_ty),
                                    actual.as_ref().map(|t| display_type(t)).unwrap_or_else(|| "unknown".into()),
                                ),
                                hint: None,
                                span: arg.span.clone(),
                            });
                        }
                    }
                    // bad param name: already caught by resolver
                }
            }
            // undefined function: already caught by resolver
        }
        Ok(())
    }

    fn check_expr_with_locals(
        &self,
        expr: &Expr,
        locals: &HashMap<String, SparType>,
    ) -> Result<(), SparError> {
        match expr {
            Expr::BinaryOp(op) => {
                self.check_expr_with_locals(&op.lhs, locals)?;
                self.check_expr_with_locals(&op.rhs, locals)?;
                // Validate operator type constraints
                if self.infer_binary_type_with_locals(op, locals).is_none() {
                    return Err(SparError::TypeError {
                        message: format!(
                            "type error in binary expression: incompatible operand types for {:?}",
                            op.op
                        ),
                        hint: None,
                        span: op.span.clone(),
                    });
                }
                Ok(())
            }
            Expr::FnCall(fc) => {
                for arg in &fc.args {
                    self.check_expr_with_locals(arg, locals)?;
                }
                Ok(())
            }
            Expr::String(s) => {
                for part in &s.parts {
                    if let StringPart::Expr(e) = part {
                        self.check_expr_with_locals(e, locals)?;
                    }
                }
                Ok(())
            }
            Expr::List(items, _) => {
                for item in items {
                    self.check_expr_with_locals(item, locals)?;
                }
                Ok(())
            }
            Expr::Grouped(inner, _) => self.check_expr_with_locals(inner, locals),
            Expr::Call { args, .. } => {
                for arg in args {
                    self.check_expr_with_locals(&arg.value, locals)?;
                }
                self.check_call_with_locals(expr, locals)?;
                Ok(())
            }
            Expr::Unary { operand, .. } => {
                self.check_expr_with_locals(operand, locals)
            }
            Expr::Index { source, index, .. } => {
                self.check_expr_with_locals(source, locals)?;
                self.check_expr_with_locals(index, locals)
            }
            Expr::Comprehension { source, body, .. } => {
                self.check_expr_with_locals(source, locals)?;
                self.check_expr_with_locals(body, locals)?;
                Ok(())
            }
            Expr::Literal(_) => Ok(()),
            Expr::NamespaceRef(_) => Ok(()),
        }
    }

    // ── Function declaration type checking ────────────────────────────────────

    fn check_function_decl(&mut self, f: &FunctionDecl) {
        let mut local_types: HashMap<String, SparType> = f.params.iter()
            .map(|p| (p.name.clone(), p.ty.clone()))
            .collect();
        self.check_func_stmts(&f.body.stmts, &f.ret, &mut local_types);
    }

    fn check_func_stmts(
        &mut self,
        stmts: &[FuncStmt],
        ret_ty: &SparType,
        local_types: &mut HashMap<String, SparType>,
    ) {
        for stmt in stmts {
            match stmt {
                FuncStmt::LocalVar(lv) => {
                    if let Err(e) = self.check_expr_with_locals(&lv.value, local_types) {
                        self.errors.push(e);
                    }
                    let actual = self.infer_type_with_locals(&lv.value, local_types);
                    match actual {
                        Some(ref t) if t == &lv.ty => {
                            local_types.insert(lv.name.clone(), lv.ty.clone());
                        }
                        Some(t) => self.errors.push(SparError::TypeError {
                            message: format!(
                                "local variable '{}' declared as '{}' but assigned a value of type '{}'",
                                lv.name, display_type(&lv.ty), display_type(&t)
                            ),
                            hint: None,
                            span: lv.span.clone(),
                        }),
                        None => self.errors.push(SparError::TypeError {
                            message: format!("cannot infer type of var '{}'", lv.name),
                            hint: None,
                            span: lv.span.clone(),
                        }),
                    }
                }
                FuncStmt::Return(ret_value, span) => {
                    self.check_return_value(ret_value, ret_ty, local_types, span);
                }
                FuncStmt::For { var_name, iterable, body, span } => {
                    let iterable_ty = self.infer_type_with_locals(iterable, local_types);
                    let elem_ty = match iterable_ty {
                        Some(SparType::List(elem)) => Some(*elem),
                        Some(other) => {
                            self.errors.push(SparError::TypeError {
                                message: format!("`for ... in` requires a list, found '{}'", display_type(&other)),
                                hint: None,
                                span: span.clone(),
                            });
                            None
                        }
                        None => None,
                    };
                    if let Err(e) = self.check_expr_with_locals(iterable, local_types) {
                        self.errors.push(e);
                    }
                    let mut loop_types = local_types.clone();
                    if let Some(elem) = elem_ty {
                        loop_types.insert(var_name.clone(), elem);
                    }
                    let body = body.clone();
                    self.check_func_stmts(&body, ret_ty, &mut loop_types);
                }
                FuncStmt::If(if_stmt) => {
                    let if_stmt = if_stmt.clone();
                    self.check_if_stmt(&if_stmt, ret_ty, local_types);
                }
            }
        }
    }

    fn check_return_value(
        &mut self,
        ret_value: &ReturnValue,
        ret_ty: &SparType,
        local_types: &HashMap<String, SparType>,
        span: &Span,
    ) {
        match (ret_ty, ret_value) {
            (SparType::Section, ReturnValue::SectionBlock(fields)) => {
                for field in fields {
                    if let Err(e) = self.check_expr_with_locals(&field.value, local_types) {
                        self.errors.push(e);
                    }
                    let actual = self.infer_type_with_locals(&field.value, local_types);
                    if actual.as_ref() != Some(&field.ty) {
                        if let Some(actual_ty) = actual {
                            self.errors.push(SparError::TypeError {
                                message: format!(
                                    "return field '{}' declared as '{}' but value has type '{}'",
                                    field.name, display_type(&field.ty), display_type(&actual_ty)
                                ),
                                hint: None,
                                span: field.span.clone(),
                            });
                        }
                    }
                }
            }
            (SparType::Section, ReturnValue::Expr(_)) => {
                self.errors.push(SparError::TypeError {
                    message: "function declares return type 'section' but this 'return' \
                               provides an expression — use 'return { field: type = expr; ... };'"
                        .to_string(),
                    hint: None,
                    span: span.clone(),
                });
            }
            (ty, ReturnValue::Expr(e)) => {
                if let Err(err) = self.check_expr_with_locals(e, local_types) {
                    self.errors.push(err);
                }
                let actual = self.infer_type_with_locals(e, local_types);
                if actual.as_ref() != Some(ty) {
                    self.errors.push(SparError::TypeError {
                        message: format!(
                            "function declares return type '{}' but this 'return' provides '{}'",
                            display_type(ty),
                            actual.as_ref().map(|t| display_type(t)).unwrap_or_else(|| "unknown".into()),
                        ),
                        hint: None,
                        span: span.clone(),
                    });
                }
            }
            (ty, ReturnValue::SectionBlock(_)) => {
                self.errors.push(SparError::TypeError {
                    message: format!(
                        "function declares return type '{}' but this 'return' provides a \
                         section block — only functions returning 'section' can use '{{ ... }}'",
                        display_type(ty)
                    ),
                    hint: None,
                    span: span.clone(),
                });
            }
        }
    }

    fn check_if_stmt(
        &mut self,
        if_stmt: &IfStmt,
        ret_ty: &SparType,
        local_types: &mut HashMap<String, SparType>,
    ) {
        if let Err(e) = self.check_expr_with_locals(&if_stmt.condition, local_types) {
            self.errors.push(e);
        }
        let cond_ty = self.infer_type_with_locals(&if_stmt.condition, local_types);
        if cond_ty != Some(SparType::Bool) {
            self.errors.push(SparError::TypeError {
                message: format!(
                    "if condition must be 'bool', found '{}'",
                    cond_ty.as_ref().map(|t| display_type(t)).unwrap_or_else(|| "unknown".into()),
                ),
                hint: None,
                span: if_stmt.span.clone(),
            });
        }

        let mut then_types = local_types.clone();
        self.check_func_stmts(&if_stmt.then_stmts, ret_ty, &mut then_types);
        let mut else_types = local_types.clone();
        self.check_func_stmts(&if_stmt.else_stmts, ret_ty, &mut else_types);

        let then_terminal = stmts_always_return(&if_stmt.then_stmts);
        let else_terminal = stmts_always_return(&if_stmt.else_stmts);

        // Only check type agreement when NEITHER branch is terminal
        if !then_terminal && !else_terminal {
            for (name, then_ty) in &then_types {
                if local_types.contains_key(name) { continue; }
                if let Some(else_ty) = else_types.get(name) {
                    if then_ty != else_ty {
                        self.errors.push(SparError::TypeError {
                            message: format!(
                                "'{}' has type '{}' in the if-branch but '{}' in the else-branch \
                                 — both branches must declare it with the same type",
                                name, display_type(then_ty), display_type(else_ty)
                            ),
                            hint: None,
                            span: if_stmt.span.clone(),
                        });
                    }
                }
            }
        }

        // Merge non-terminal branch(es) into outer scope
        match (then_terminal, else_terminal) {
            (true, true)  => {}
            (false, true) => { local_types.extend(then_types); }
            (true, false) => { local_types.extend(else_types); }
            (false, false) => {
                for (name, ty) in &then_types {
                    if else_types.get(name) == Some(ty) {
                        local_types.insert(name.clone(), ty.clone());
                    }
                }
            }
        }
    }

    // ── Type inference with local variable scope ──────────────────────────────

    fn infer_type_with_locals(
        &self,
        expr: &Expr,
        locals: &HashMap<String, SparType>,
    ) -> Option<SparType> {
        match expr {
            Expr::Literal(lit) => match lit {
                Literal::Int(_)   => Some(SparType::Int),
                Literal::Float(_) => Some(SparType::Float),
                Literal::Bool(_)  => Some(SparType::Bool),
            },
            Expr::String(_) => Some(SparType::Str),
            Expr::NamespaceRef(nr) if nr.segments.len() == 1 => {
                let name = &nr.segments[0];
                if let Some(ty) = locals.get(name) {
                    return Some(ty.clone());
                }
                self.infer_type(expr)
            }
            Expr::Call { name, .. } => {
                if name.contains("::") { return None; } // cross-file call — type unknown here
                self.symbols.functions.get(name).map(|fe| fe.ret.clone())
            }
            Expr::Unary { op, operand, .. } => match op {
                UnOp::Not => {
                    let t = self.infer_type_with_locals(operand, locals)?;
                    if t != SparType::Bool { return None; }
                    Some(SparType::Bool)
                }
                UnOp::Neg => {
                    let t = self.infer_type_with_locals(operand, locals)?;
                    if matches!(t, SparType::Int | SparType::Float) { Some(t) } else { None }
                }
            },
            Expr::Index { source, index, .. } => {
                let idx_ty = self.infer_type_with_locals(index, locals)?;
                if idx_ty != SparType::Int { return None; }
                match self.infer_type_with_locals(source, locals)? {
                    SparType::List(elem) => Some(*elem),
                    _ => None,
                }
            }
            Expr::BinaryOp(b) => self.infer_binary_type_with_locals(b, locals),
            Expr::Comprehension { source, body, var_name, .. } => {
                let source_ty = self.infer_type_with_locals(source, locals)?;
                let elem_ty = match source_ty {
                    SparType::List(inner) => *inner,
                    _ => return None, // source not a list
                };
                let mut inner_locals = locals.clone();
                inner_locals.insert(var_name.clone(), elem_ty);
                let body_ty = self.infer_type_with_locals(body, &inner_locals)?;
                Some(SparType::List(Box::new(body_ty)))
            }
            Expr::List(items, _) => {
                let first = items.first().and_then(|e| self.infer_type_with_locals(e, locals))?;
                Some(SparType::List(Box::new(first)))
            }
            Expr::Grouped(inner, _) => self.infer_type_with_locals(inner, locals),
            _ => self.infer_type(expr),
        }
    }

    fn infer_binary_type_with_locals(
        &self,
        b: &BinaryOp,
        locals: &HashMap<String, SparType>,
    ) -> Option<SparType> {
        let lty = self.infer_type_with_locals(&b.lhs, locals)?;
        let rty = self.infer_type_with_locals(&b.rhs, locals)?;
        match b.op {
            BinOp::Add => {
                if lty == rty && matches!(lty, SparType::Int | SparType::Float | SparType::Str) {
                    Some(lty)
                } else {
                    None
                }
            }
            BinOp::Sub | BinOp::Mul | BinOp::Div => {
                if lty == rty && matches!(lty, SparType::Int | SparType::Float) {
                    Some(lty)
                } else {
                    None
                }
            }
            BinOp::Eq | BinOp::NotEq => {
                if lty == rty { Some(SparType::Bool) } else { None }
            }
            BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq => {
                if matches!(lty, SparType::Int | SparType::Float) && lty == rty {
                    Some(SparType::Bool)
                } else {
                    None
                }
            }
            BinOp::And | BinOp::Or => {
                if lty == SparType::Bool && rty == SparType::Bool {
                    Some(SparType::Bool)
                } else {
                    None
                }
            }
            BinOp::Fallback => {
                if lty == rty { Some(lty) } else { None }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_ok(src: &str) {
        let tokens  = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        let table   = crate::resolver::Resolver::new().resolve(&program, &[]).expect("resolve");
        TypeChecker::check(&program, &table).expect("type check failed unexpectedly");
    }

    fn check_err(src: &str) -> Vec<String> {
        let tokens  = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        let table   = crate::resolver::Resolver::new().resolve(&program, &[]).expect("resolve");
        TypeChecker::check(&program, &table)
            .unwrap_err()
            .into_iter()
            .map(|e| e.to_string())
            .collect()
    }

    fn has_type_error(src: &str, fragment: &str) -> bool {
        check_err(src).iter().any(|e| e.contains(fragment))
    }

    #[test]
    fn test_clean_program() {
        check_ok(r#"
            var port: int = 3000;
            var name: str = "keel";
            [Server]{ bind: str = "0.0.0.0"; };
        "#);
    }

    #[test]
    fn test_required_var_no_value() {
        assert!(has_type_error("var port: int;", "required variable"));
        assert!(has_type_error("var port: int;", "port"));
    }

    #[test]
    fn test_optional_var_no_value() {
        check_ok("var port?: int;");
    }

    #[test]
    fn test_int_var_int_value() {
        check_ok("var port: int = 3000;");
    }

    #[test]
    fn test_str_var_int_mismatch() {
        assert!(has_type_error("var name: str = 3000;", "type mismatch"));
        assert!(has_type_error("var name: str = 3000;", "name"));
    }

    #[test]
    fn test_int_var_str_mismatch() {
        assert!(has_type_error(r#"var port: int = "3000";"#, "type mismatch"));
    }

    #[test]
    fn test_bool_var_int_mismatch() {
        assert!(has_type_error("var flag: bool = 1;", "type mismatch"));
    }

    #[test]
    fn test_valid_arithmetic() {
        check_ok("var total: int = 30 * 3;");
    }

    #[test]
    fn test_invalid_str_plus_int() {
        let src = r#"
            var s: str = "hello";
            var n: int = 5;
            var bad: str = s + n;
        "#;
        assert!(has_type_error(src, "operator `+`"));
    }

    #[test]
    fn test_valid_str_concat() {
        let src = r#"
            var a: str = "hello";
            var b: str = " world";
            var c: str = a + b;
        "#;
        check_ok(src);
    }

    #[test]
    fn test_namespace_ref_int_to_int() {
        let src = r#"
            var port: int = 3000;
            var p2: int = global::port;
        "#;
        check_ok(src);
    }

    #[test]
    fn test_namespace_ref_str_to_int_mismatch() {
        let src = r#"
            var name: str = "keel";
            var bad: int = global::name;
        "#;
        assert!(has_type_error(src, "type mismatch"));
    }

    #[test]
    fn test_section_field_ref_type() {
        let src = r#"
            [Db]{ pool: int = 5; };
            var p: int = Db::pool;
        "#;
        check_ok(src);
    }

    #[test]
    fn test_env_in_str_field() {
        check_ok(r#"var mode: str = env("APP_MODE");"#);
    }

    #[test]
    fn test_env_in_int_field() {
        assert!(has_type_error(r#"var port: int = env("PORT");"#, "type mismatch"));
    }

    #[test]
    fn test_env_fallback_str() {
        check_ok(r#"var mode: str = env("MODE") ?? "dev";"#);
    }

    #[test]
    fn test_env_fallback_int_mismatch() {
        let src = r#"var port: int = env("PORT") ?? 3000;"#;
        let errs = check_err(src);
        assert!(
            errs.iter().any(|e| e.contains("??") || e.contains("Fallback")),
            "got: {errs:?}"
        );
    }

    #[test]
    fn test_required_field_in_section() {
        assert!(has_type_error("[Server]{ port: int; };", "required field"));
        assert!(has_type_error("[Server]{ port: int; };", "port"));
    }

    #[test]
    fn test_optional_field_in_section() {
        check_ok("[Server]{ port?: int; };");
    }

    #[test]
    fn test_valid_typed_list() {
        check_ok("var ports: [int] = [3000, 8080, 9090];");
    }

    #[test]
    fn test_invalid_typed_list_element() {
        assert!(has_type_error(r#"var ports: [int] = [3000, "bad", 9090];"#, "list element type mismatch"));
    }

    #[test]
    fn test_dynamic_mixed_list_no_error() {
        check_ok(r#"dynamic var tags = [2026, "prod", true];"#);
    }

    #[test]
    fn test_grouped_expr_type() {
        check_ok("var x: int = (3 + 5);");
    }

    #[test]
    fn test_fallback_matching_types() {
        check_ok("var x: int = 3 ?? 5;");
    }

    #[test]
    fn test_multiple_type_errors_collected() {
        let src = r#"
            var a: int;
            var b: str;
        "#;
        assert_eq!(check_err(src).len(), 2);
    }

    #[test]
    fn section_type_nested_body_valid() {
        check_ok(r#"[Outer]{ inner: section = { key: str = "v"; }; };"#);
    }

    #[test]
    fn section_type_rejected_for_global_var() {
        // May be rejected at parse or type-check; either is acceptable
        let src = "var x: section;";
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        match crate::parser::Parser::new(tokens).parse() {
            Err(_) => return, // parser rejection is fine
            Ok(program) => {
                let symbols = crate::resolver::Resolver::new().resolve(&program, &[])
                    .unwrap_or_else(|_| crate::resolver::SymbolTable {
                        globals: Default::default(),
                        sections: Default::default(),
                        imports: Default::default(),
                        functions: Default::default(),
                    });
                let result = TypeChecker::check(&program, &symbols);
                assert!(result.is_err(), "var of type 'section' must be rejected");
            }
        }
    }

    #[test]
    fn section_field_with_expr_value_rejected() {
        let src = "[A]{ inner: section = 42; };";
        // This may be caught at parse time (section type should get nested body, not expr)
        // But if parse succeeds, type checker must catch it
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        match crate::parser::Parser::new(tokens).parse() {
            Err(_) => return, // parse rejection is fine
            Ok(program) => {
                let symbols = crate::resolver::Resolver::new().resolve(&program, &[])
                    .unwrap_or_else(|_| crate::resolver::SymbolTable {
                        globals: Default::default(),
                        sections: Default::default(),
                        imports: Default::default(),
                        functions: Default::default(),
                    });
                let result = TypeChecker::check(&program, &symbols);
                assert!(result.is_err(), "section field with expr value must be rejected");
            }
        }
    }

    #[test]
    fn typecheck_function_body_call_arg_type_mismatch() {
        // Wrong arg type inside a function body must be caught
        let src = r#"
            function double(x: int) -> int { return x; }
            function caller(s: str) -> int {
                var result: int = double(x: s);
                return result;
            }
        "#;
        let errs = check_err(src);
        let errs_str = format!("{:?}", errs);
        assert!(errs_str.contains("int") || errs_str.contains("str") || errs_str.contains("type") || errs_str.contains("arg"),
            "expected type mismatch error, got: {errs:?}");
    }

    // ── Group 4: early-return type checking ───────────────────────────────────

    #[test]
    fn return_with_correct_type_is_ok() {
        check_ok(r#"
            function f(x: int) -> int {
                return x;
            }
        "#);
    }

    #[test]
    fn return_with_wrong_type_is_error() {
        assert!(has_type_error(
            r#"function f(x: str) -> int { return x; }"#,
            "return",
        ));
    }

    #[test]
    fn return_in_both_branches_is_ok() {
        check_ok(r#"
            function pick(b: bool) -> int {
                if b { return 1; } else { return 2; }
            }
        "#);
    }

    #[test]
    fn return_wrong_type_in_if_branch_is_error() {
        assert!(has_type_error(
            r#"function f(b: bool, x: str) -> int {
                if b { return x; } else { return 0; }
            }"#,
            "return",
        ));
    }

    #[test]
    fn section_fn_with_section_block_return_is_ok() {
        check_ok(r#"
            function make() -> section {
                return { port: int = 8080; };
            }
        "#);
    }

    #[test]
    fn section_fn_expr_return_is_error() {
        assert!(has_type_error(
            r#"function make() -> section { return 42; }"#,
            "section",
        ));
    }

    #[test]
    fn non_section_fn_section_block_return_is_error() {
        assert!(has_type_error(
            r#"function make() -> int { return { port: int = 8080; }; }"#,
            "section",
        ));
    }

    #[test]
    fn terminal_branch_exemption_local_available_after_if() {
        // then-branch always returns; else-branch declares `y` — `y` must be available after
        check_ok(r#"
            function f(b: bool) -> int {
                if b { return 0; } else { var y: int = 1; }
                return y;
            }
        "#);
    }

    #[test]
    fn local_var_type_mismatch_in_function_is_error() {
        assert!(has_type_error(
            r#"function f() -> int { var x: int = "hello"; return x; }"#,
            "declared as",
        ));
    }
}
