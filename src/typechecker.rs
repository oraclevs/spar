use std::collections::HashMap;

use crate::ast::*;
use crate::error::{Span, SparError};
use crate::resolver::{stmts_always_return, GlobalEntry, SymbolTable};

pub fn display_type(ty: &SparType) -> String {
    match ty {
        SparType::Str => "str".into(),
        SparType::Int => "int".into(),
        SparType::Float => "float".into(),
        SparType::Bool => "bool".into(),
        SparType::Section => "section".into(),
        SparType::List(inner) => format!("[{}]", display_type(inner)),
        SparType::Named(name) => name.clone(),
    }
}

/// A `TypeFieldShape` expanded one level — `Named(X)` resolved to `X`'s own
/// fields, so shape comparison only ever has to handle two cases.
enum ShapeKind {
    Primitive(SparType),
    Section(Vec<TypeField>),
}

/// `items` is exactly one `...Source;` spread and nothing else — the
/// spread-only body pattern that gets a structural shape check instead of
/// the "can't statically verify" skip a mixed spread+fields body gets.
fn spread_only_source(items: &[SectionItem]) -> Option<&SpreadStmt> {
    match items {
        [SectionItem::Spread(s)] => Some(s),
        _ => None,
    }
}

/// The spread's source section name, if it's a same-file, single-segment
/// reference (`...Name;`) — the only shape a shape can be statically
/// resolved for. Anything else (a function call, a multi-segment/
/// cross-file reference) has no statically-known shape to check.
fn spread_source_name(spread: &SpreadStmt) -> Option<&str> {
    match &spread.expr {
        Expr::NamespaceRef(nr) if nr.segments.len() == 1 => Some(nr.segments[0].as_str()),
        _ => None,
    }
}

pub struct TypeChecker<'a> {
    symbols: &'a SymbolTable,
    errors: Vec<SparError>,
    /// Section name → schema-derived field list, for sections validated
    /// against an `import schema "...";` but with no `-> Type` binding of
    /// their own. Empty unless populated via `check_with_schema`.
    schema_bindings: HashMap<String, Vec<SchemaField>>,
    /// The section currently being checked, for `self.field` type lookups.
    current_section: Option<Vec<String>>,
}

impl<'a> TypeChecker<'a> {
    /// Infer the fully resolved type of an expression using the same rules as
    /// normal type checking. Language tooling should use this instead of
    /// duplicating Spar's inference logic.
    pub fn infer_expression(expr: &Expr, symbols: &'a SymbolTable) -> Option<SparType> {
        TypeChecker {
            symbols,
            errors: Vec::new(),
            schema_bindings: HashMap::new(),
            current_section: None,
        }
        .infer_type(expr)
    }

    pub fn check(program: &Program, symbols: &'a SymbolTable) -> Result<(), Vec<SparError>> {
        let mut tc = TypeChecker {
            symbols,
            errors: Vec::new(),
            schema_bindings: HashMap::new(),
            current_section: None,
        };
        tc.check_program(program);
        if tc.errors.is_empty() {
            Ok(())
        } else {
            Err(tc.errors)
        }
    }

    pub fn check_with_imports(
        program: &Program,
        symbols: &'a SymbolTable,
        _loaded: &std::collections::HashMap<String, crate::loader::LoadedImport>,
    ) -> Result<(), Vec<SparError>> {
        Self::check(program, symbols)
    }

    /// Like `check`, but also given every section's schema-derived field
    /// list (from `loader::validate_schema_imports`'s `Ok` value) — a
    /// section with no `-> Type` binding but a name present in `bindings`
    /// is checked against its schema shape instead of requiring every
    /// field to declare its own type explicitly.
    pub fn check_with_schema(
        program: &Program,
        symbols: &'a SymbolTable,
        schema_bindings: HashMap<String, Vec<SchemaField>>,
    ) -> Result<(), Vec<SparError>> {
        let mut tc = TypeChecker {
            symbols,
            errors: Vec::new(),
            schema_bindings,
            current_section: None,
        };
        tc.check_program(program);
        if tc.errors.is_empty() {
            Ok(())
        } else {
            Err(tc.errors)
        }
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
                TopLevelItem::Import(_) => {}
                TopLevelItem::Var(decl) => self.check_var(decl),
                TopLevelItem::Dynamic(decl) => self.check_dynamic(decl),
                TopLevelItem::Section(decl) => self.check_section(decl),
                TopLevelItem::Function(f) => self.check_function_decl(f),
                TopLevelItem::SchemaSection(_) => {}
                TopLevelItem::Type(_) => {} // Task 4 replaces this with real validation
                TopLevelItem::Enum(_) => {} // nothing to typecheck — resolver already validated the declaration
                TopLevelItem::FunctionGroup(g) => {
                    for f in &g.functions {
                        self.check_function_decl(f);
                    }
                }
                TopLevelItem::SchemaFrom(_) => {} // never reaches the typechecker — schema files aren't typechecked (loader.rs handles them out-of-band)
                TopLevelItem::Task(decl) => self.check_task(decl),
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
        let prev_section = self.current_section.replace(decl.path.clone());
        let path_str = decl.path.join(".");
        match &decl.type_binding {
            Some(binding) => self.check_type_binding(decl, binding, &path_str),
            None => {
                let fields: Vec<&FieldDecl> = decl
                    .items
                    .iter()
                    .filter_map(|i| {
                        if let SectionItem::Field(f) = i {
                            Some(f)
                        } else {
                            None
                        }
                    })
                    .collect();
                match self.schema_bindings.get(&path_str).cloned() {
                    Some(schema_fields) => {
                        self.check_schema_bound_fields(&fields, &schema_fields, &path_str)
                    }
                    None => self.check_untyped_section_fields(&fields, &path_str),
                }
            }
        }
        self.current_section = prev_section;
    }

    /// A section with no `-> Type` binding but a matching `import schema`
    /// entry — each field's expected shape comes from the schema instead of
    /// requiring `field: Type = value;` to spell the type out again. An
    /// explicit local type, if present, still wins (existing behavior,
    /// unchanged). A field the schema doesn't declare is left for
    /// `loader::validate_schema_imports`'s own "not declared in the schema"
    /// error — this only still walks its expression so calls/refs inside it
    /// get resolved and checked.
    fn check_schema_bound_fields(
        &mut self,
        fields: &[&FieldDecl],
        schema_fields: &[SchemaField],
        path_str: &str,
    ) {
        for field in fields {
            if let Some(ty) = &field.ty {
                self.check_field(field, ty, path_str);
                continue;
            }
            match schema_fields.iter().find(|sf| sf.name == field.name) {
                Some(sf) => self.check_field_against_schema(field, sf, path_str),
                None => match &field.value {
                    Some(FieldValue::Expr(e)) => self.check_expr_internal(e),
                    Some(FieldValue::Nested(items)) => {
                        let subs: Vec<&FieldDecl> = items
                            .iter()
                            .filter_map(|i| {
                                if let SectionItem::Field(f) = i {
                                    Some(f)
                                } else {
                                    None
                                }
                            })
                            .collect();
                        self.check_untyped_section_fields(
                            &subs,
                            &format!("{path_str}.{}", field.name),
                        );
                    }
                    None => {}
                },
            }
        }
    }

    fn check_field_against_schema(&mut self, field: &FieldDecl, sf: &SchemaField, path_str: &str) {
        match &sf.shape {
            SchemaFieldShape::Primitive(expected_ty) => match &field.value {
                Some(FieldValue::Expr(val)) => {
                    self.check_expr_type(val, expected_ty, &field.name, &field.span)
                }
                Some(FieldValue::Nested(_)) => {
                    self.push_type_error(
                        format!(
                            "field `{}` in section `[{path_str}]` must be `{}` (required by the bound schema) \
                             but uses a section body `{{ ... }}`",
                            field.name, display_type(expected_ty)
                        ),
                        None,
                        field.span.clone(),
                    );
                }
                None => {
                    if !field.optional {
                        self.push_type_error(
                            format!(
                                "required field `{}` in section `[{path_str}]` has no value",
                                field.name
                            ),
                            None,
                            field.span.clone(),
                        );
                    }
                }
            },
            SchemaFieldShape::Section(nested_schema_fields) => match &field.value {
                Some(FieldValue::Nested(items)) => {
                    let subs: Vec<&FieldDecl> = items
                        .iter()
                        .filter_map(|i| {
                            if let SectionItem::Field(f) = i {
                                Some(f)
                            } else {
                                None
                            }
                        })
                        .collect();
                    let nested_path = format!("{path_str}.{}", field.name);
                    self.check_schema_bound_fields(&subs, nested_schema_fields, &nested_path);
                }
                Some(FieldValue::Expr(e)) => {
                    let actual = self.infer_type(e);
                    if actual != Some(SparType::Section) {
                        self.push_type_error(
                            format!(
                                "field '{}' in '[{path_str}]' must be a nested section (required by the bound \
                                 schema) but value is {}",
                                field.name,
                                actual.as_ref().map(|t| display_type(t)).unwrap_or_else(|| "unknown".into()),
                            ),
                            None,
                            field.span.clone(),
                        );
                    }
                    self.check_expr_internal(e);
                }
                None => {
                    if !field.optional {
                        self.push_type_error(
                            format!(
                                "required field `{}` in section `[{path_str}]` has no value",
                                field.name
                            ),
                            None,
                            field.span.clone(),
                        );
                    }
                }
            },
        }
    }

    /// Every field in a section with no `-> TypeName` binding must have an
    /// explicit type — there is nothing to infer it from.
    fn check_untyped_section_fields(&mut self, fields: &[&FieldDecl], path_str: &str) {
        for field in fields {
            match &field.ty {
                Some(ty) => self.check_field(field, ty, path_str),
                None => self.push_type_error(
                    format!(
                        "field `{}` in section `[{path_str}]` has no type — sections without \
                         a `-> Type` binding must declare each field's type explicitly",
                        field.name
                    ),
                    None,
                    field.span.clone(),
                ),
            }
        }
    }

    fn check_field(&mut self, field: &FieldDecl, ty: &SparType, path_str: &str) {
        // Rule B: validate body vs type compatibility
        match (ty, &field.value) {
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
            (other_ty, Some(FieldValue::Nested(_))) if *other_ty != SparType::Section => {
                self.push_type_error(
                    format!(
                        "field '{}' in '[{path_str}]' has type '{}' but uses a section \
                         body '{{ ... }}' — only 'section'-typed fields can have a nested body",
                        field.name,
                        display_type(other_ty)
                    ),
                    Some("change the field type to 'section' or use an expression value".into()),
                    field.span.clone(),
                );
                return;
            }
            (SparType::Section, Some(FieldValue::Nested(sub_items))) => {
                // Recursively type-check the nested section. An unbound
                // section has no `-> Type` to check a spread's contents
                // against — same "nothing to compare against" precedent
                // as an unbound top-level section (check_section, above).
                let nested_path = format!("{path_str}.{}", field.name);
                let subs: Vec<&FieldDecl> = sub_items
                    .iter()
                    .filter_map(|i| {
                        if let SectionItem::Field(f) = i {
                            Some(f)
                        } else {
                            None
                        }
                    })
                    .collect();
                self.check_untyped_section_fields(&subs, &nested_path);
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
            self.check_expr_type(val, ty, &field.name, &field.span);
        }
    }

    fn check_type_binding(&mut self, decl: &SectionDecl, binding: &TypeBinding, path_str: &str) {
        // If the type name itself doesn't exist, the resolver already
        // reported that — avoid a duplicate error here.
        let Some(entry) = self.symbols.types.get(&binding.name).cloned() else {
            return;
        };

        // A section that's ENTIRELY `...Source;` (no other fields) can be
        // checked structurally against the whole bound type — the spread
        // must supply exactly what the type requires. Reuses the same
        // "smart" shape comparison a spread-only nested field gets below.
        if let Some(spread) = spread_only_source(&decl.items) {
            self.check_spread_against_shape(spread, &entry.fields, &binding.name, path_str);
            return;
        }

        // A spread MIXED with other explicit fields — each resolvable
        // spread's contribution is merged with the explicit fields for
        // coverage/type checking; an unresolvable spread falls back to
        // skipping entirely (see check_mixed_spread_and_fields).
        let has_spreads = decl
            .items
            .iter()
            .any(|i| matches!(i, SectionItem::Spread(_)));
        if has_spreads {
            self.check_mixed_spread_and_fields(&decl.items, &entry.fields, &binding.name, path_str);
            return;
        }

        let config_fields: Vec<&FieldDecl> = decl
            .items
            .iter()
            .filter_map(|i| {
                if let SectionItem::Field(f) = i {
                    Some(f)
                } else {
                    None
                }
            })
            .collect();

        self.validate_type_fields(&entry.fields, &config_fields, &binding.name, path_str);
    }

    fn validate_type_fields(
        &mut self,
        type_fields: &[TypeField],
        config_fields: &[&FieldDecl],
        type_name: &str,
        path_str: &str,
    ) {
        self.validate_type_fields_with_coverage(
            type_fields,
            config_fields,
            type_name,
            path_str,
            None,
        );
    }

    /// `covered_by_spread`, when given, names fields a resolvable spread
    /// mixed in alongside `config_fields` already supplies — those don't
    /// need to appear in `config_fields` itself to satisfy a required
    /// field. `None` (the common case, no spread involved) behaves exactly
    /// as before.
    fn validate_type_fields_with_coverage(
        &mut self,
        type_fields: &[TypeField],
        config_fields: &[&FieldDecl],
        type_name: &str,
        path_str: &str,
        covered_by_spread: Option<&std::collections::HashSet<String>>,
    ) {
        for tf in type_fields {
            let cf = config_fields.iter().find(|f| f.name == tf.name);
            let spread_covers = covered_by_spread.is_some_and(|names| names.contains(&tf.name));
            match cf {
                None if spread_covers => {} // a mixed-in spread supplies this field
                None if !tf.optional => {
                    self.push_type_error(
                        format!(
                            "section `[{}]` is missing required field `{}` (required by type `{}`)",
                            path_str, tf.name, type_name
                        ),
                        None,
                        tf.span.clone(),
                    );
                }
                None => {} // optional, fine to omit
                Some(cf) => match &tf.shape {
                    TypeFieldShape::Primitive(expected_ty) => {
                        // The field's type is either explicit (Some) or
                        // inferred from its value (None, under this
                        // binding) — either way, compare the effective
                        // type against what the bound type declares.
                        let actual_ty = match &cf.ty {
                            Some(t) => Some(t.clone()),
                            None => match &cf.value {
                                Some(FieldValue::Expr(e)) => self.infer_type(e),
                                _ => None,
                            },
                        };
                        match actual_ty {
                            Some(actual) if &actual != expected_ty => {
                                self.push_type_error(
                                    format!(
                                        "field `{}::{}` declared as `{}` but type `{}` expects `{}`",
                                        path_str, tf.name, display_type(&actual), type_name, display_type(expected_ty),
                                    ),
                                    None,
                                    cf.span.clone(),
                                );
                            }
                            Some(_) => {} // matches
                            None => {
                                self.push_type_error(
                                    format!(
                                        "field `{}::{}`'s value type could not be determined; type `{}` expects `{}`",
                                        path_str, tf.name, type_name, display_type(expected_ty),
                                    ),
                                    None,
                                    cf.span.clone(),
                                );
                            }
                        }
                    }
                    TypeFieldShape::Section(nested_type_fields) => {
                        self.validate_nested_type_field(
                            cf,
                            nested_type_fields,
                            type_name,
                            path_str,
                            &tf.name,
                        );
                    }
                    TypeFieldShape::Named(other_type_name) => {
                        let Some(other_entry) = self.symbols.types.get(other_type_name).cloned()
                        else {
                            continue; // resolver already reported the undefined type
                        };
                        self.validate_nested_type_field(
                            cf,
                            &other_entry.fields,
                            other_type_name,
                            path_str,
                            &tf.name,
                        );
                    }
                },
            }
        }

        for cf in config_fields {
            if !type_fields.iter().any(|tf| tf.name == cf.name) {
                self.push_type_error(
                    format!(
                        "field `{}::{}` is not declared in type `{}`",
                        path_str, cf.name, type_name
                    ),
                    None,
                    cf.span.clone(),
                );
            }
        }
    }

    fn validate_nested_type_field(
        &mut self,
        cf: &FieldDecl,
        nested_type_fields: &[TypeField],
        type_name: &str,
        path_str: &str,
        field_name: &str,
    ) {
        // A field is a nested section if its value is FieldValue::Nested,
        // regardless of whether its type is explicit (Some(Section)) or
        // inferred (None, under this binding).
        let explicit_non_section = matches!(&cf.ty, Some(ty) if *ty != SparType::Section);
        if explicit_non_section {
            self.push_type_error(
                format!(
                    "field `{}::{}` must be type `section` (type `{}` requires a nested section)",
                    path_str, field_name, type_name
                ),
                None,
                cf.span.clone(),
            );
            return;
        }
        let nested_items: &[SectionItem] = match &cf.value {
            Some(FieldValue::Nested(items)) => items,
            _ => {
                self.push_type_error(
                    format!(
                        "field `{}::{}` must have an inline section value (`{{ ... }}`)",
                        path_str, field_name
                    ),
                    None,
                    cf.span.clone(),
                );
                return;
            }
        };
        let nested_path = format!("{}::{}", path_str, field_name);

        // A nested field body that's ENTIRELY `...Source;` gets the same
        // structural shape check a spread-only bound section gets above —
        // the spread must supply exactly what this field's expected shape
        // requires.
        if let Some(spread) = spread_only_source(nested_items) {
            self.check_spread_against_shape(spread, nested_type_fields, type_name, &nested_path);
            return;
        }

        // A spread mixed with explicit fields — same "can't statically
        // attribute coverage" precedent as the top-level case.
        let has_spreads = nested_items
            .iter()
            .any(|i| matches!(i, SectionItem::Spread(_)));
        if has_spreads {
            self.check_mixed_spread_and_fields(
                nested_items,
                nested_type_fields,
                type_name,
                &nested_path,
            );
            return;
        }

        let nested_config: Vec<&FieldDecl> = nested_items
            .iter()
            .filter_map(|i| {
                if let SectionItem::Field(f) = i {
                    Some(f)
                } else {
                    None
                }
            })
            .collect();
        self.validate_type_fields(nested_type_fields, &nested_config, type_name, &nested_path);
    }

    /// Locals-aware twin of `validate_type_fields_with_coverage`, for an
    /// object literal inside a function body (a local var's value, a
    /// return value) whose field expressions may reference params/locals
    /// — invisible to the global-scope `self.infer_type`. Does not support
    /// mixed spread+field coverage (out of scope for this pass); a spread
    /// present among `config_fields`' source items is simply invisible to
    /// the required/extra-field checks below (same "can't statically know"
    /// precedent used elsewhere for spreads, just without the coverage
    /// tracking `validate_type_fields_with_coverage` layers on top).
    fn validate_type_fields_with_locals(
        &mut self,
        type_fields: &[TypeField],
        config_fields: &[&FieldDecl],
        type_name: &str,
        path_str: &str,
        locals: &HashMap<String, SparType>,
    ) {
        for tf in type_fields {
            let cf = config_fields.iter().find(|f| f.name == tf.name);
            match cf {
                None if !tf.optional => {
                    self.push_type_error(
                        format!(
                            "section `[{}]` is missing required field `{}` (required by type `{}`)",
                            path_str, tf.name, type_name
                        ),
                        None,
                        tf.span.clone(),
                    );
                }
                None => {} // optional, fine to omit
                Some(cf) => match &tf.shape {
                    TypeFieldShape::Primitive(expected_ty) => {
                        let actual_ty = match &cf.ty {
                            Some(t) => Some(t.clone()),
                            None => match &cf.value {
                                Some(FieldValue::Expr(e)) => self.infer_type_with_locals(e, locals),
                                _ => None,
                            },
                        };
                        match actual_ty {
                            Some(actual) if &actual != expected_ty => {
                                self.push_type_error(
                                    format!(
                                        "field `{}::{}` declared as `{}` but type `{}` expects `{}`",
                                        path_str, tf.name, display_type(&actual), type_name, display_type(expected_ty),
                                    ),
                                    None,
                                    cf.span.clone(),
                                );
                            }
                            Some(_) => {} // matches
                            None => {
                                self.push_type_error(
                                    format!(
                                        "field `{}::{}`'s value type could not be determined; type `{}` expects `{}`",
                                        path_str, tf.name, type_name, display_type(expected_ty),
                                    ),
                                    None,
                                    cf.span.clone(),
                                );
                            }
                        }
                    }
                    TypeFieldShape::Section(nested_type_fields) => {
                        self.validate_nested_type_field_with_locals(
                            cf,
                            nested_type_fields,
                            type_name,
                            path_str,
                            &tf.name,
                            locals,
                        );
                    }
                    TypeFieldShape::Named(other_type_name) => {
                        let Some(other_entry) = self.symbols.types.get(other_type_name).cloned()
                        else {
                            continue; // resolver already reported the undefined type
                        };
                        self.validate_nested_type_field_with_locals(
                            cf,
                            &other_entry.fields,
                            other_type_name,
                            path_str,
                            &tf.name,
                            locals,
                        );
                    }
                },
            }
        }

        for cf in config_fields {
            if !type_fields.iter().any(|tf| tf.name == cf.name) {
                self.push_type_error(
                    format!(
                        "field `{}::{}` is not declared in type `{}`",
                        path_str, cf.name, type_name
                    ),
                    None,
                    cf.span.clone(),
                );
            }
        }
    }

    fn validate_nested_type_field_with_locals(
        &mut self,
        cf: &FieldDecl,
        nested_type_fields: &[TypeField],
        type_name: &str,
        path_str: &str,
        field_name: &str,
        locals: &HashMap<String, SparType>,
    ) {
        let nested_items: &[SectionItem] = match &cf.value {
            Some(FieldValue::Nested(items)) => items,
            _ => {
                self.push_type_error(
                    format!(
                        "field `{}::{}` must have an inline section value (`{{ ... }}`)",
                        path_str, field_name
                    ),
                    None,
                    cf.span.clone(),
                );
                return;
            }
        };
        let nested_config: Vec<&FieldDecl> = nested_items
            .iter()
            .filter_map(|i| {
                if let SectionItem::Field(f) = i {
                    Some(f)
                } else {
                    None
                }
            })
            .collect();
        let nested_path = format!("{}::{}", path_str, field_name);
        self.validate_type_fields_with_locals(
            nested_type_fields,
            &nested_config,
            type_name,
            &nested_path,
            locals,
        );
    }

    /// A spread MIXED with explicit fields (not spread-only, which gets
    /// `check_spread_against_shape` above) — each *resolvable* spread's
    /// contributed field names count toward satisfying required fields,
    /// and are checked against `expected` for type-correctness and
    /// undeclared/extra fields, same as an explicit field would be. If
    /// ANY spread present can't be resolved (a function call, a
    /// multi-segment/cross-file reference), its contribution is
    /// genuinely unknowable — falls back to skipping the whole check,
    /// same conservative precedent used everywhere else a spread's
    /// contents can't be statically determined.
    fn check_mixed_spread_and_fields(
        &mut self,
        items: &[SectionItem],
        expected: &[TypeField],
        expected_label: &str,
        path_str: &str,
    ) {
        let mut covered: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut resolved_spreads: Vec<(&str, Vec<TypeField>, &Span)> = Vec::new();

        for item in items {
            match item {
                SectionItem::Field(f) => {
                    covered.insert(f.name.clone());
                }
                SectionItem::Spread(sp) => {
                    let Some(name) = spread_source_name(sp) else {
                        return;
                    }; // unresolvable — skip the whole check
                    let Some(shape) = self.derive_section_shape(name) else {
                        return;
                    };
                    for tf in &shape {
                        covered.insert(tf.name.clone());
                    }
                    resolved_spreads.push((name, shape, &sp.span));
                }
            }
        }

        for (source_label, shape, span) in resolved_spreads {
            self.check_spread_contribution(&shape, expected, source_label, expected_label, span);
        }

        let config_fields: Vec<&FieldDecl> = items
            .iter()
            .filter_map(|i| {
                if let SectionItem::Field(f) = i {
                    Some(f)
                } else {
                    None
                }
            })
            .collect();
        self.validate_type_fields_with_coverage(
            expected,
            &config_fields,
            expected_label,
            path_str,
            Some(&covered),
        );
    }

    /// Check one resolvable spread's OWN contributed fields against
    /// `expected` — type-correctness and "not declared" only, no
    /// missing-required check (a mixed spread only needs to supply PART
    /// of `expected`; `check_mixed_spread_and_fields` checks the union
    /// for required-field coverage separately).
    fn check_spread_contribution(
        &mut self,
        source: &[TypeField],
        expected: &[TypeField],
        source_label: &str,
        expected_label: &str,
        span: &Span,
    ) {
        for sf in source {
            match expected.iter().find(|ef| ef.name == sf.name) {
                None => {
                    self.push_type_error(
                        format!(
                            "spread `...{}` field `{}` is not declared in `{}`",
                            source_label, sf.name, expected_label
                        ),
                        None,
                        span.clone(),
                    );
                }
                Some(ef) => {
                    let source_kind = self.expand_type_field_shape(&sf.shape);
                    let expected_kind = self.expand_type_field_shape(&ef.shape);
                    match (source_kind, expected_kind) {
                        (ShapeKind::Primitive(actual), ShapeKind::Primitive(want)) => {
                            if actual != want {
                                self.push_type_error(
                                    format!(
                                        "spread `...{}` field `{}` is `{}` but `{}` expects `{}`",
                                        source_label,
                                        sf.name,
                                        display_type(&actual),
                                        expected_label,
                                        display_type(&want)
                                    ),
                                    None,
                                    span.clone(),
                                );
                            }
                        }
                        (ShapeKind::Section(actual_nested), ShapeKind::Section(want_nested)) => {
                            self.check_spread_contribution(
                                &actual_nested,
                                &want_nested,
                                source_label,
                                expected_label,
                                span,
                            );
                        }
                        (ShapeKind::Primitive(_), ShapeKind::Section(_)) => {
                            self.push_type_error(
                                format!(
                                    "spread `...{}` field `{}` is a primitive value but `{}` expects a nested section",
                                    source_label, sf.name, expected_label
                                ),
                                None,
                                span.clone(),
                            );
                        }
                        (ShapeKind::Section(_), ShapeKind::Primitive(want)) => {
                            self.push_type_error(
                                format!(
                                    "spread `...{}` field `{}` is a nested section but `{}` expects `{}`",
                                    source_label, sf.name, expected_label, display_type(&want)
                                ),
                                None,
                                span.clone(),
                            );
                        }
                    }
                }
            }
        }
    }

    /// Resolve `spread`'s source section (same-file, single-segment
    /// `...Name;` only — matches the scope `eval_spread`/`resolve_spread`
    /// already support) and structurally compare its shape against
    /// `expected` — exact match, recursively, same rule the rest of the
    /// type system uses everywhere else. A source that can't be resolved
    /// this way (a function call, a multi-segment/cross-file reference)
    /// has no statically-known shape — nothing to check, not an error.
    fn check_spread_against_shape(
        &mut self,
        spread: &SpreadStmt,
        expected: &[TypeField],
        expected_label: &str,
        path_str: &str,
    ) {
        let Some(source_name) = spread_source_name(spread) else {
            return;
        };
        let Some(source_shape) = self.derive_section_shape(source_name) else {
            return;
        };
        self.check_shape_matches(
            &source_shape,
            expected,
            source_name,
            expected_label,
            path_str,
            &spread.span,
        );
    }

    /// The structural shape of a top-level section: if it's type-bound,
    /// that type's own fields ARE its shape (its instance already has to
    /// satisfy them exactly, via the normal check_type_binding path); if
    /// unbound, every field already has an explicit type (Phase 2's rule),
    /// so derive an equivalent ad-hoc shape straight from those.
    fn derive_section_shape(&self, name: &str) -> Option<Vec<TypeField>> {
        let entry = self
            .symbols
            .sections
            .get(std::slice::from_ref(&name.to_string()))?;
        match &entry.type_binding {
            Some(type_name) => self.symbols.types.get(type_name).map(|t| t.fields.clone()),
            None => Some(self.derive_ad_hoc_shape(&[name.to_string()])),
        }
    }

    /// Recursively build a `Vec<TypeField>` shape from an UNBOUND section's
    /// own registered fields — nested sections are registered separately
    /// under their own path (see resolver.rs's `register_nested_section`),
    /// so a `section`-typed field recurses into `path + [field_name]`.
    fn derive_ad_hoc_shape(&self, path: &[String]) -> Vec<TypeField> {
        let Some(entry) = self.symbols.sections.get(path) else {
            return Vec::new();
        };
        entry
            .fields
            .iter()
            .map(|(name, fe)| {
                let shape = match &fe.ty {
                    Some(SparType::Section) => {
                        let nested_path: Vec<String> =
                            path.iter().cloned().chain([name.clone()]).collect();
                        TypeFieldShape::Section(self.derive_ad_hoc_shape(&nested_path))
                    }
                    Some(other) => TypeFieldShape::Primitive(other.clone()),
                    None => TypeFieldShape::Primitive(SparType::Str), // unreachable: unbound fields always have an explicit type
                };
                TypeField {
                    name: name.clone(),
                    optional: fe.optional,
                    shape,
                    span: fe.span.clone(),
                }
            })
            .collect()
    }

    /// Structural exact-match comparison between two abstract shapes —
    /// used when spreading `...Source;` into a position with a known
    /// expected shape. Every expected field must be present in `source`
    /// with a matching type (recursively); an expected-optional field may
    /// be absent; any field `source` has that `expected` doesn't declare
    /// is an error — the same exact-match rule the type system already
    /// applies everywhere else (Phase 2's strictness rule).
    fn check_shape_matches(
        &mut self,
        source: &[TypeField],
        expected: &[TypeField],
        source_label: &str,
        expected_label: &str,
        path_str: &str,
        span: &Span,
    ) {
        for ef in expected {
            match source.iter().find(|f| f.name == ef.name) {
                None if !ef.optional => {
                    self.push_type_error(
                        format!(
                            "spread `...{}` in `[{}]` is missing required field `{}` (required by `{}`)",
                            source_label, path_str, ef.name, expected_label
                        ),
                        None,
                        span.clone(),
                    );
                }
                None => {} // optional, fine to omit
                Some(sf) => {
                    let source_kind = self.expand_type_field_shape(&sf.shape);
                    let expected_kind = self.expand_type_field_shape(&ef.shape);
                    match (source_kind, expected_kind) {
                        (ShapeKind::Primitive(actual), ShapeKind::Primitive(want)) => {
                            if actual != want {
                                self.push_type_error(
                                    format!(
                                        "spread `...{}` field `{}` is `{}` but `{}` expects `{}`",
                                        source_label,
                                        ef.name,
                                        display_type(&actual),
                                        expected_label,
                                        display_type(&want)
                                    ),
                                    None,
                                    span.clone(),
                                );
                            }
                        }
                        (ShapeKind::Section(actual_nested), ShapeKind::Section(want_nested)) => {
                            self.check_shape_matches(
                                &actual_nested,
                                &want_nested,
                                source_label,
                                expected_label,
                                path_str,
                                span,
                            );
                        }
                        (ShapeKind::Primitive(_), ShapeKind::Section(_)) => {
                            self.push_type_error(
                                format!(
                                    "spread `...{}` field `{}` is a primitive value but `{}` expects a nested section",
                                    source_label, ef.name, expected_label
                                ),
                                None,
                                span.clone(),
                            );
                        }
                        (ShapeKind::Section(_), ShapeKind::Primitive(want)) => {
                            self.push_type_error(
                                format!(
                                    "spread `...{}` field `{}` is a nested section but `{}` expects `{}`",
                                    source_label, ef.name, expected_label, display_type(&want)
                                ),
                                None,
                                span.clone(),
                            );
                        }
                    }
                }
            }
        }

        for sf in source {
            if !expected.iter().any(|ef| ef.name == sf.name) {
                self.push_type_error(
                    format!(
                        "spread `...{}` field `{}` is not declared in `{}`",
                        source_label, sf.name, expected_label
                    ),
                    None,
                    span.clone(),
                );
            }
        }
    }

    /// A `TypeField`'s own declared shape, expressed as the `SparType` it
    /// evaluates to when read through `Named::field` — not to be confused
    /// with `expand_type_field_shape` below, which expands `Named` shapes
    /// into their nested fields for structural comparison instead.
    fn field_shape_to_type(&self, shape: &TypeFieldShape) -> SparType {
        match shape {
            TypeFieldShape::Primitive(ty) => ty.clone(),
            TypeFieldShape::Named(name) => SparType::Named(name.clone()),
            TypeFieldShape::Section(_) => SparType::Section,
        }
    }

    /// Expand a `TypeFieldShape` into its comparable kind — `Named(X)`
    /// expands to `X`'s own registered fields, same "resolve once, expand"
    /// semantics `SchemaFrom` already uses (loader.rs).
    fn expand_type_field_shape(&self, shape: &TypeFieldShape) -> ShapeKind {
        match shape {
            TypeFieldShape::Primitive(ty) => ShapeKind::Primitive(ty.clone()),
            TypeFieldShape::Section(fields) => ShapeKind::Section(fields.clone()),
            TypeFieldShape::Named(name) => match self.symbols.types.get(name) {
                Some(entry) => ShapeKind::Section(entry.fields.clone()),
                None => ShapeKind::Section(Vec::new()), // resolver already reported the undefined type
            },
        }
    }

    fn infer_type(&self, expr: &Expr) -> Option<SparType> {
        match expr {
            Expr::Object(_, _) => None, // shape only checkable against an expected type — see check_expr_type (Task 4)
            Expr::Literal(Literal::Int(_)) => Some(SparType::Int),
            Expr::Literal(Literal::Float(_)) => Some(SparType::Float),
            Expr::Literal(Literal::Bool(_)) => Some(SparType::Bool),
            Expr::String(_) => Some(SparType::Str),
            Expr::List(items, _) => items
                .first()
                .and_then(|e| self.infer_type(e))
                .map(|t| SparType::List(Box::new(t))),
            Expr::NamespaceRef(nr) => self.infer_namespace_type(nr),
            Expr::FieldAccess { base, field, .. } => self.infer_field_access(base, field),
            Expr::FnCall(fc) => match fc.name.as_str() {
                "env" | "str" => Some(SparType::Str),
                "int" => Some(SparType::Int),
                "float" => Some(SparType::Float),
                "bool" => Some(SparType::Bool),
                _ => None,
            },
            Expr::BinaryOp(op) => {
                let lhs = self.infer_type(&op.lhs)?;
                let rhs = self.infer_type(&op.rhs)?;
                self.infer_binop_type(&op.op, &lhs, &rhs)
            }
            Expr::Grouped(inner, _) => self.infer_type(inner),
            Expr::Call { name, .. } => self.call_return_type(name),
            Expr::Unary { op, operand, .. } => match op {
                UnOp::Not => {
                    let t = self.infer_type(operand)?;
                    if t == SparType::Bool {
                        Some(SparType::Bool)
                    } else {
                        None
                    }
                }
                UnOp::Neg => {
                    let t = self.infer_type(operand)?;
                    if matches!(t, SparType::Int | SparType::Float) {
                        Some(t)
                    } else {
                        None
                    }
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
                    _ => None,                 // source is not a list; error reported elsewhere
                }
            }
        }
    }

    fn infer_namespace_type(&self, nr: &NamespaceRef) -> Option<SparType> {
        match nr.segments.as_slice() {
            [name] => self.lookup_global_type(name),
            [ns, _name] if self.symbols.enums.contains_key(ns.as_str()) => {
                Some(SparType::Named(ns.clone()))
            }
            _ => None,
        }
    }

    fn infer_field_access(&self, base: &Expr, field: &str) -> Option<SparType> {
        if let Expr::NamespaceRef(nr) = base {
            if nr.segments == ["self"] {
                let section_path = self.current_section.as_ref()?;
                return self
                    .symbols
                    .lookup_section(section_path)
                    .and_then(|s| s.fields.get(field))
                    .and_then(|f| f.ty.clone());
            }
            if nr.segments == ["global"] {
                return self.lookup_global_type(field);
            }
            // base names a registered section directly (e.g. an imported
            // `[Colors]{...}` spliced in as a local section) — resolve the
            // field straight off that section rather than falling through
            // to `infer_type`, which only knows about `SparType::Named`
            // type instances, not sections.
            if let Some(section) = self.symbols.lookup_section(&nr.segments) {
                return section.fields.get(field).and_then(|f| f.ty.clone());
            }
        }
        let base_ty = self.infer_type(base)?;
        match base_ty {
            SparType::Named(type_name) => self
                .symbols
                .types
                .get(&type_name)
                .and_then(|te| te.fields.iter().find(|f| &f.name == field))
                .map(|f| self.field_shape_to_type(&f.shape)),
            _ => None,
        }
    }

    /// Return type of a `Expr::Call.name` string, for calls that are
    /// statically resolvable here: local plain functions (1 segment) and
    /// local functionGroup calls (2 segments, `Group::fn`) — both have a
    /// `FunctionEntry` already in `self.symbols`. Cross-file calls (2
    /// segments where the first isn't a local functionGroup, or 3 segments)
    /// stay opaque — this typechecker doesn't load imported files' symbols.
    fn call_return_type(&self, name: &str) -> Option<SparType> {
        let segments: Vec<&str> = name.split("::").collect();
        match segments.len() {
            2 => self
                .symbols
                .function_groups
                .get(segments[0])
                .and_then(|g| g.functions.get(segments[1]))
                .map(|fe| fe.ret.clone()),
            1 => self.symbols.functions.get(name).map(|fe| fe.ret.clone()),
            _ => None,
        }
    }

    /// If `name` is a global var whose declared type is `SparType::Named(X)`,
    /// returns `X`. `None` for any other global (or a non-Named type).
    fn lookup_global_type(&self, name: &str) -> Option<SparType> {
        match self.symbols.lookup_global(name)? {
            GlobalEntry::Var { ty, .. } => Some(ty.clone()),
            GlobalEntry::Dynamic { .. } => None,
        }
    }

    fn infer_binop_type(&self, op: &BinOp, lhs: &SparType, rhs: &SparType) -> Option<SparType> {
        match op {
            BinOp::Add => match (lhs, rhs) {
                (SparType::Str, SparType::Str) => Some(SparType::Str),
                (SparType::Int, SparType::Int) => Some(SparType::Int),
                (SparType::Float, SparType::Float) => Some(SparType::Float),
                _ => None,
            },
            BinOp::Sub | BinOp::Mul | BinOp::Div => match (lhs, rhs) {
                (SparType::Int, SparType::Int) => Some(SparType::Int),
                (SparType::Float, SparType::Float) => Some(SparType::Float),
                _ => None,
            },
            BinOp::Fallback => {
                if lhs == rhs {
                    Some(lhs.clone())
                } else {
                    None
                }
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

    /// Validates an object literal's items against a declared type's own
    /// fields — reuses `validate_type_fields`, the exact function `->
    /// TypeName` section bindings already use for structural validation.
    fn check_object_against_named(&mut self, items: &[SectionItem], name: &str, label: &str) {
        let Some(entry) = self.symbols.types.get(name).cloned() else {
            return; // resolver already reported the undefined type
        };
        let config_fields: Vec<&FieldDecl> = items
            .iter()
            .filter_map(|i| {
                if let SectionItem::Field(f) = i {
                    Some(f)
                } else {
                    None
                }
            })
            .collect();
        self.validate_type_fields(&entry.fields, &config_fields, name, label);
    }

    fn check_expr_type(&mut self, expr: &Expr, declared_ty: &SparType, label: &str, span: &Span) {
        self.check_expr_internal(expr);

        if let Expr::Object(items, _) = expr {
            match declared_ty {
                SparType::Named(name) => {
                    self.check_object_against_named(items, name, label);
                }
                _ => {
                    self.push_type_error(
                        format!(
                            "`{label}` has type `{}` but value is an object literal `{{ ... }}` — \
                             object literals can only be used for a declared `type [X]{{...}}`",
                            display_type(declared_ty)
                        ),
                        None,
                        span.clone(),
                    );
                }
            }
            return;
        }

        // Checked BEFORE the `infer_type` early-return below: when a list's
        // first element is an object literal, `infer_type(Expr::List)`
        // itself returns `None` (it infers from the first element, and
        // `infer_type(Expr::Object)` is always `None`) — so this block
        // would never be reached if placed after that early return.
        if let Expr::List(items, _) = expr {
            if let SparType::List(elem_ty) = declared_ty {
                for item in items {
                    if let Expr::Object(obj_items, _) = item {
                        match elem_ty.as_ref() {
                            SparType::Named(name) => {
                                self.check_object_against_named(obj_items, name, label);
                            }
                            other => {
                                self.push_type_error(
                                    format!(
                                        "list element in `{label}` has type `{}` but value is an \
                                         object literal `{{ ... }}` — object literals can only be \
                                         used for a declared `type [X]{{...}}`",
                                        display_type(other)
                                    ),
                                    None,
                                    span.clone(),
                                );
                            }
                        }
                        continue;
                    }
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

        let inferred = match self.infer_type(expr) {
            Some(t) => t,
            None => return,
        };

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
            Expr::Object(items, _) => {
                for item in items {
                    match item {
                        SectionItem::Field(f) => {
                            if let Some(FieldValue::Expr(e)) = &f.value {
                                self.check_expr_internal(e);
                            }
                        }
                        SectionItem::Spread(sp) => self.check_expr_internal(&sp.expr),
                    }
                }
            }
            Expr::BinaryOp(op) => {
                self.check_expr_internal(&op.lhs);
                self.check_expr_internal(&op.rhs);

                let lhs_ty = self.infer_type(&op.lhs);
                let rhs_ty = self.infer_type(&op.rhs);

                if let (Some(l), Some(r)) = (&lhs_ty, &rhs_ty) {
                    let valid = match op.op {
                        BinOp::Add => matches!(
                            (l, r),
                            (SparType::Str, SparType::Str)
                                | (SparType::Int, SparType::Int)
                                | (SparType::Float, SparType::Float)
                        ),
                        BinOp::Sub | BinOp::Mul | BinOp::Div => matches!(
                            (l, r),
                            (SparType::Int, SparType::Int) | (SparType::Float, SparType::Float)
                        ),
                        BinOp::Fallback => l == r,
                        BinOp::Eq
                        | BinOp::NotEq
                        | BinOp::Lt
                        | BinOp::Gt
                        | BinOp::LtEq
                        | BinOp::GtEq => {
                            // Comparison operators work on comparable types
                            matches!(
                                (l, r),
                                (SparType::Int, SparType::Int)
                                    | (SparType::Float, SparType::Float)
                                    | (SparType::Str, SparType::Str)
                                    | (SparType::Bool, SparType::Bool)
                            )
                        }
                        BinOp::And | BinOp::Or => l == &SparType::Bool && r == &SparType::Bool,
                    };
                    if !valid {
                        let op_sym = match op.op {
                            BinOp::Add => "+",
                            BinOp::Sub => "-",
                            BinOp::Mul => "*",
                            BinOp::Div => "/",
                            BinOp::Fallback => "??",
                            BinOp::Eq => "==",
                            BinOp::NotEq => "!=",
                            BinOp::Lt => "<",
                            BinOp::Gt => ">",
                            BinOp::LtEq => "<=",
                            BinOp::GtEq => ">=",
                            BinOp::And => "&&",
                            BinOp::Or => "||",
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
                for arg in &fc.args {
                    self.check_expr_internal(arg);
                }
            }
            Expr::String(s) => {
                for part in &s.parts {
                    if let StringPart::Expr(e) = part {
                        self.check_expr_internal(e);
                    }
                }
            }
            Expr::List(items, _) => {
                for item in items {
                    self.check_expr_internal(item);
                }
            }
            Expr::Grouped(inner, _) => self.check_expr_internal(inner),
            Expr::Call { args, .. } => {
                for arg in args {
                    self.check_expr_internal(&arg.value);
                }
                let result = self.check_call(expr);
                if let Err(e) = result {
                    self.errors.push(e);
                }
            }
            Expr::Unary {
                op, operand, span, ..
            } => {
                self.check_expr_internal(operand);
                let operand_ty = self.infer_type(operand);
                match op {
                    UnOp::Not => {
                        if operand_ty != Some(SparType::Bool) {
                            self.push_type_error(
                                format!(
                                    "operator `!` requires a bool operand, got {}",
                                    operand_ty
                                        .as_ref()
                                        .map(|t| display_type(t))
                                        .unwrap_or_else(|| "unknown".into()),
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
                                    operand_ty
                                        .as_ref()
                                        .map(|t| display_type(t))
                                        .unwrap_or_else(|| "unknown".into()),
                                ),
                                None,
                                span.clone(),
                            );
                        }
                    }
                }
            }
            Expr::Index {
                source,
                index,
                span,
            } => {
                self.check_expr_internal(source);
                self.check_expr_internal(index);
                let index_ty = self.infer_type(index);
                if index_ty != Some(SparType::Int) {
                    self.push_type_error(
                        format!(
                            "list index must be int, got {}",
                            index_ty
                                .as_ref()
                                .map(|t| display_type(t))
                                .unwrap_or_else(|| "unknown".into()),
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
            Expr::Comprehension {
                source, body, span, ..
            } => {
                self.check_expr_internal(source);
                self.check_expr_internal(body);
                let source_ty = self.infer_type(source);
                if !matches!(&source_ty, Some(SparType::List(_))) {
                    self.push_type_error(
                        format!(
                            "for-comprehension source must be a list, got {}",
                            source_ty
                                .as_ref()
                                .map(|t| display_type(t))
                                .unwrap_or_else(|| "unknown".into()),
                        ),
                        None,
                        span.clone(),
                    );
                }
            }
            Expr::Literal(_) => {}
            Expr::NamespaceRef(_) => {}
            Expr::FieldAccess { base, .. } => self.check_expr_internal(base),
        }
    }

    // ── Call argument type checking ───────────────────────────────────────────

    /// Coarse structural shape check: does this object literal have every
    /// required field of `name`'s declared type, correctly typed (for
    /// primitive fields — nested/Named fields recurse), with no extra
    /// fields? No per-field error detail (unlike `validate_type_fields`) —
    /// intentionally matches `check_call`'s existing whole-argument error
    /// granularity, not a shortcut.
    fn object_matches_named_type(&self, items: &[SectionItem], name: &str) -> bool {
        let Some(entry) = self.symbols.types.get(name) else {
            return false;
        };
        let config_fields: Vec<&FieldDecl> = items
            .iter()
            .filter_map(|i| {
                if let SectionItem::Field(f) = i {
                    Some(f)
                } else {
                    None
                }
            })
            .collect();
        for tf in &entry.fields {
            let cf = config_fields.iter().find(|f| f.name == tf.name);
            match cf {
                None if !tf.optional => return false,
                None => continue,
                Some(cf) => match &tf.shape {
                    TypeFieldShape::Primitive(expected_ty) => {
                        let actual = match &cf.ty {
                            Some(t) => Some(t.clone()),
                            None => match &cf.value {
                                Some(FieldValue::Expr(e)) => self.infer_type(e),
                                _ => None,
                            },
                        };
                        if actual.as_ref() != Some(expected_ty) {
                            return false;
                        }
                    }
                    TypeFieldShape::Section(nested) => {
                        let Some(FieldValue::Nested(nested_items)) = &cf.value else {
                            return false;
                        };
                        if !self.nested_items_match_type_fields(nested_items, nested) {
                            return false;
                        }
                    }
                    TypeFieldShape::Named(other_name) => {
                        let Some(FieldValue::Nested(nested_items)) = &cf.value else {
                            return false;
                        };
                        if !self.object_matches_named_type(nested_items, other_name) {
                            return false;
                        }
                    }
                },
            }
        }
        for cf in &config_fields {
            if !entry.fields.iter().any(|tf| tf.name == cf.name) {
                return false;
            }
        }
        true
    }

    /// Locals-aware twin of `object_matches_named_type`, for a call
    /// argument built inside a function body.
    fn object_matches_named_type_with_locals(
        &self,
        items: &[SectionItem],
        name: &str,
        locals: &HashMap<String, SparType>,
    ) -> bool {
        let Some(entry) = self.symbols.types.get(name) else {
            return false;
        };
        let config_fields: Vec<&FieldDecl> = items
            .iter()
            .filter_map(|i| {
                if let SectionItem::Field(f) = i {
                    Some(f)
                } else {
                    None
                }
            })
            .collect();
        for tf in &entry.fields {
            let cf = config_fields.iter().find(|f| f.name == tf.name);
            match cf {
                None if !tf.optional => return false,
                None => continue,
                Some(cf) => match &tf.shape {
                    TypeFieldShape::Primitive(expected_ty) => {
                        let actual = match &cf.ty {
                            Some(t) => Some(t.clone()),
                            None => match &cf.value {
                                Some(FieldValue::Expr(e)) => self.infer_type_with_locals(e, locals),
                                _ => None,
                            },
                        };
                        if actual.as_ref() != Some(expected_ty) {
                            return false;
                        }
                    }
                    TypeFieldShape::Section(nested) => {
                        let Some(FieldValue::Nested(nested_items)) = &cf.value else {
                            return false;
                        };
                        if !self.nested_items_match_type_fields(nested_items, nested) {
                            return false;
                        }
                    }
                    TypeFieldShape::Named(other_name) => {
                        let Some(FieldValue::Nested(nested_items)) = &cf.value else {
                            return false;
                        };
                        if !self.object_matches_named_type_with_locals(
                            nested_items,
                            other_name,
                            locals,
                        ) {
                            return false;
                        }
                    }
                },
            }
        }
        for cf in &config_fields {
            if !entry.fields.iter().any(|tf| tf.name == cf.name) {
                return false;
            }
        }
        true
    }

    /// Structural check for a nested `section`-shaped field (not a `Named`
    /// type — an inline `TypeFieldShape::Section(...)`).
    fn nested_items_match_type_fields(
        &self,
        items: &[SectionItem],
        type_fields: &[TypeField],
    ) -> bool {
        let config_fields: Vec<&FieldDecl> = items
            .iter()
            .filter_map(|i| {
                if let SectionItem::Field(f) = i {
                    Some(f)
                } else {
                    None
                }
            })
            .collect();
        for tf in type_fields {
            let cf = config_fields.iter().find(|f| f.name == tf.name);
            match cf {
                None if !tf.optional => return false,
                None => continue,
                Some(cf) => match &tf.shape {
                    TypeFieldShape::Primitive(expected_ty) => {
                        let actual = match &cf.ty {
                            Some(t) => Some(t.clone()),
                            None => match &cf.value {
                                Some(FieldValue::Expr(e)) => self.infer_type(e),
                                _ => None,
                            },
                        };
                        if actual.as_ref() != Some(expected_ty) {
                            return false;
                        }
                    }
                    TypeFieldShape::Section(nested) => {
                        let Some(FieldValue::Nested(nested_items)) = &cf.value else {
                            return false;
                        };
                        if !self.nested_items_match_type_fields(nested_items, nested) {
                            return false;
                        }
                    }
                    TypeFieldShape::Named(other_name) => {
                        let Some(FieldValue::Nested(nested_items)) = &cf.value else {
                            return false;
                        };
                        if !self.object_matches_named_type(nested_items, other_name) {
                            return false;
                        }
                    }
                },
            }
        }
        for cf in &config_fields {
            if !type_fields.iter().any(|tf| tf.name == cf.name) {
                return false;
            }
        }
        true
    }

    fn check_call(&self, call: &Expr) -> Result<(), SparError> {
        if let Expr::Call { name, args, .. } = call {
            if let Some(entry) = self.symbols.functions.get(name) {
                for arg in args {
                    let param_ty = entry
                        .params
                        .iter()
                        .find(|(n, _)| n == &arg.param_name)
                        .map(|(_, t)| t.clone());
                    if let Some(param_ty) = param_ty {
                        if let Expr::Object(items, _) = &arg.value {
                            let ok = match &param_ty {
                                SparType::Named(name) => {
                                    self.object_matches_named_type(items, name)
                                }
                                _ => false,
                            };
                            if !ok {
                                return Err(SparError::TypeError {
                                    message: format!(
                                        "argument '{}' expects {} but this object literal doesn't match its shape",
                                        arg.param_name,
                                        display_type(&param_ty),
                                    ),
                                    hint: None,
                                    span: arg.span.clone(),
                                });
                            }
                            continue;
                        }
                        let actual = self.infer_type(&arg.value);
                        if actual.as_ref() != Some(&param_ty) {
                            return Err(SparError::TypeError {
                                message: format!(
                                    "argument '{}' expects {} but got {}",
                                    arg.param_name,
                                    display_type(&param_ty),
                                    actual
                                        .as_ref()
                                        .map(|t| display_type(t))
                                        .unwrap_or_else(|| "unknown".into()),
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
                    let param_ty = entry
                        .params
                        .iter()
                        .find(|(n, _)| n == &arg.param_name)
                        .map(|(_, t)| t.clone());
                    if let Some(param_ty) = param_ty {
                        if let Expr::Object(items, _) = &arg.value {
                            let ok = match &param_ty {
                                SparType::Named(name) => {
                                    self.object_matches_named_type_with_locals(items, name, locals)
                                }
                                _ => false,
                            };
                            if !ok {
                                return Err(SparError::TypeError {
                                    message: format!(
                                        "argument '{}' expects {} but this object literal doesn't match its shape",
                                        arg.param_name,
                                        display_type(&param_ty),
                                    ),
                                    hint: None,
                                    span: arg.span.clone(),
                                });
                            }
                            continue;
                        }
                        let actual = self.infer_type_with_locals(&arg.value, locals);
                        if actual.as_ref() != Some(&param_ty) {
                            return Err(SparError::TypeError {
                                message: format!(
                                    "argument '{}' expects {} but got {}",
                                    arg.param_name,
                                    display_type(&param_ty),
                                    actual
                                        .as_ref()
                                        .map(|t| display_type(t))
                                        .unwrap_or_else(|| "unknown".into()),
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
            Expr::Object(items, _) => {
                for item in items {
                    match item {
                        SectionItem::Field(f) => {
                            if let Some(FieldValue::Expr(e)) = &f.value {
                                self.check_expr_with_locals(e, locals)?;
                            }
                        }
                        SectionItem::Spread(sp) => self.check_expr_with_locals(&sp.expr, locals)?,
                    }
                }
                Ok(())
            }
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
            Expr::Unary { operand, .. } => self.check_expr_with_locals(operand, locals),
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
            Expr::FieldAccess { base, .. } => self.check_expr_with_locals(base, locals),
        }
    }

    // ── Function declaration type checking ────────────────────────────────────

    /// Task parameters must be scalar (no `list`/`section` — a shell
    /// argument is always a single string on the command line), metadata
    /// fields must type as their expected scalar (`description`/`cwd`/
    /// env values as `str`, `default`/`quiet` as `bool`), and every `run`
    /// block interpolation must type as *some* scalar — `task_lowering`
    /// (Task 6's later half) is what rejects a scalar expression that
    /// illegally mixes a task parameter with other values, since that's a
    /// lowering-representability concern, not a type concern.
    fn check_task(&mut self, decl: &TaskDecl) {
        for param in &decl.params {
            if matches!(param.ty, SparType::Section | SparType::List(_)) {
                self.push_type_error(
                    format!(
                        "task parameter '{}': type must be 'str', 'int', 'float', or 'bool' — \
                         task arguments come from the command line as single scalar values",
                        param.name
                    ),
                    None,
                    param.span.clone(),
                );
            }
            if let Some(default) = &param.default {
                self.check_task_scalar_field(
                    default,
                    &format!("parameter '{}' default", param.name),
                    &param.ty,
                    &param.span,
                );
            }
        }

        if let Some(expr) = &decl.description {
            self.check_task_scalar_field(expr, "description", &SparType::Str, &decl.span);
        }
        if let Some(expr) = &decl.default {
            self.check_task_scalar_field(expr, "default", &SparType::Bool, &decl.span);
        }
        if let Some(expr) = &decl.quiet {
            self.check_task_scalar_field(expr, "quiet", &SparType::Bool, &decl.span);
        }
        if let Some(expr) = &decl.private {
            self.check_task_scalar_field(expr, "private", &SparType::Bool, &decl.span);
        }
        if let Some(expr) = &decl.group {
            self.check_task_scalar_field(expr, "group", &SparType::Str, &decl.span);
        }
        if let Some(expr) = &decl.confirm {
            self.check_task_scalar_field(expr, "confirm", &SparType::Str, &decl.span);
        }
        if let Some(expr) = &decl.cwd {
            self.check_task_scalar_field(expr, "cwd", &SparType::Str, &decl.span);
        }
        if let Some(expr) = &decl.shell {
            self.check_task_string_list_field(expr, "shell", &decl.span);
        }
        for (key, value) in &decl.env {
            self.check_task_scalar_field(value, &format!("env.{key}"), &SparType::Str, &decl.span);
        }

        let local_types: HashMap<String, SparType> = decl
            .params
            .iter()
            .map(|p| (p.name.clone(), p.ty.clone()))
            .collect();
        for block in &decl.run_blocks {
            for command in &block.commands {
                for part in &command.parts {
                    if let ShellTemplatePart::Expr(expr) = part {
                        if let Err(e) = self.check_expr_with_locals(expr, &local_types) {
                            self.errors.push(e);
                            continue;
                        }
                        match self.infer_type_with_locals(expr, &local_types) {
                            Some(SparType::Str | SparType::Int | SparType::Float | SparType::Bool) => {}
                            Some(other) => self.push_type_error(
                                format!(
                                    "task 'run' interpolation must be a scalar value (str, int, float, or bool), \
                                     found {}",
                                    display_type(&other)
                                ),
                                None,
                                command.span.clone(),
                            ),
                            None => {} // unresolvable type — a more specific error was already reported
                        }
                    }
                }
            }
        }
    }

    fn check_task_scalar_field(
        &mut self,
        expr: &Expr,
        label: &str,
        expected: &SparType,
        span: &Span,
    ) {
        if let Err(e) = self.check_expr_with_locals(expr, &HashMap::new()) {
            self.errors.push(e);
            return;
        }
        match self.infer_type(expr) {
            Some(actual) if &actual == expected => {}
            Some(actual) => self.push_type_error(
                format!(
                    "task '{}' must be a {}, found {}",
                    label,
                    display_type(expected),
                    display_type(&actual)
                ),
                None,
                span.clone(),
            ),
            None => {} // unresolvable — a more specific error was already reported
        }
    }

    fn check_task_string_list_field(&mut self, expr: &Expr, label: &str, span: &Span) {
        if let Err(e) = self.check_expr_with_locals(expr, &HashMap::new()) {
            self.errors.push(e);
            return;
        }
        if let Expr::List(items, _) = expr {
            if items.is_empty() {
                self.push_type_error(
                    format!("task '{label}' must be a non-empty [str]"),
                    None,
                    span.clone(),
                );
                return;
            }
            if items
                .iter()
                .all(|item| self.infer_type(item) == Some(SparType::Str))
            {
                return;
            }
            self.push_type_error(
                format!("task '{label}' must be a non-empty [str]"),
                None,
                span.clone(),
            );
            return;
        }
        match self.infer_type(expr) {
            Some(SparType::List(inner)) if *inner == SparType::Str => {}
            Some(actual) => self.push_type_error(
                format!(
                    "task '{label}' must be a non-empty [str], found {}",
                    display_type(&actual)
                ),
                None,
                span.clone(),
            ),
            None => self.push_type_error(
                format!("task '{label}' must be a non-empty [str]"),
                None,
                span.clone(),
            ),
        }
    }

    fn check_function_decl(&mut self, f: &FunctionDecl) {
        for param in &f.params {
            let Some(default) = &param.default else {
                continue;
            };
            if let Err(error) = self.check_expr_with_locals(default, &HashMap::new()) {
                self.errors.push(error);
                continue;
            }
            let actual = self.infer_type(default);
            if actual.as_ref() != Some(&param.ty) {
                self.push_type_error(
                    format!(
                        "parameter '{}' default must be a {}, found {}",
                        param.name,
                        display_type(&param.ty),
                        actual
                            .as_ref()
                            .map(display_type)
                            .unwrap_or_else(|| "unknown".to_string())
                    ),
                    None,
                    param.span.clone(),
                );
            }
        }

        let mut local_types: HashMap<String, SparType> = f
            .params
            .iter()
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

                    let handled_as_named_object = match (&lv.ty, &lv.value) {
                        (SparType::Named(name), Expr::Object(items, _)) => {
                            let config_fields: Vec<&FieldDecl> = items
                                .iter()
                                .filter_map(|i| {
                                    if let SectionItem::Field(f) = i {
                                        Some(f)
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            if let Some(entry) = self.symbols.types.get(name).cloned() {
                                self.validate_type_fields_with_locals(
                                    &entry.fields,
                                    &config_fields,
                                    name,
                                    &lv.name,
                                    local_types,
                                );
                            }
                            true
                        }
                        (SparType::List(elem_ty), Expr::List(elems, _))
                            if matches!(elem_ty.as_ref(), SparType::Named(_)) =>
                        {
                            let SparType::Named(name) = elem_ty.as_ref() else {
                                unreachable!()
                            };
                            for elem in elems {
                                if let Expr::Object(items, _) = elem {
                                    let config_fields: Vec<&FieldDecl> = items
                                        .iter()
                                        .filter_map(|i| {
                                            if let SectionItem::Field(f) = i {
                                                Some(f)
                                            } else {
                                                None
                                            }
                                        })
                                        .collect();
                                    if let Some(entry) = self.symbols.types.get(name).cloned() {
                                        self.validate_type_fields_with_locals(
                                            &entry.fields,
                                            &config_fields,
                                            name,
                                            &lv.name,
                                            local_types,
                                        );
                                    }
                                }
                            }
                            true
                        }
                        _ => false,
                    };

                    if handled_as_named_object {
                        local_types.insert(lv.name.clone(), lv.ty.clone());
                    } else {
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
                }
                FuncStmt::Return(ret_value, span) => {
                    self.check_return_value(ret_value, ret_ty, local_types, span);
                }
                FuncStmt::For {
                    var_name,
                    iterable,
                    body,
                    span,
                } => {
                    let iterable_ty = self.infer_type_with_locals(iterable, local_types);
                    let elem_ty = match iterable_ty {
                        Some(SparType::List(elem)) => Some(*elem),
                        Some(other) => {
                            self.errors.push(SparError::TypeError {
                                message: format!(
                                    "`for ... in` requires a list, found '{}'",
                                    display_type(&other)
                                ),
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
                    let Some(field_ty) = &field.ty else {
                        // A `section` return has no declared type to infer
                        // this field's type from — same "must be explicit"
                        // precedent as an untyped top-level section.
                        self.errors.push(SparError::TypeError {
                            message: format!(
                                "return field '{}' has no type — a function returning 'section' \
                                 has nothing to infer field types from; declare each field's type explicitly",
                                field.name
                            ),
                            hint: None,
                            span: field.span.clone(),
                        });
                        continue;
                    };
                    let actual = self.infer_type_with_locals(&field.value, local_types);
                    if actual.as_ref() != Some(field_ty) {
                        if let Some(actual_ty) = actual {
                            self.errors.push(SparError::TypeError {
                                message: format!(
                                    "return field '{}' declared as '{}' but value has type '{}'",
                                    field.name,
                                    display_type(field_ty),
                                    display_type(&actual_ty)
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
            (SparType::Named(name), ReturnValue::SectionBlock(fields)) => {
                let Some(entry) = self.symbols.types.get(name).cloned() else {
                    return;
                }; // resolver already reported it
                for rf in fields {
                    if let Err(e) = self.check_expr_with_locals(&rf.value, local_types) {
                        self.errors.push(e);
                    }
                }
                let config_fields: Vec<FieldDecl> = fields
                    .iter()
                    .map(|rf| FieldDecl {
                        name: rf.name.clone(),
                        optional: false,
                        ty: rf.ty.clone(),
                        value: Some(FieldValue::Expr(rf.value.clone())),
                        span: rf.span.clone(),
                    })
                    .collect();
                let config_field_refs: Vec<&FieldDecl> = config_fields.iter().collect();
                self.validate_type_fields_with_locals(
                    &entry.fields,
                    &config_field_refs,
                    name,
                    "return",
                    local_types,
                );
            }
            (SparType::List(elem_ty), ReturnValue::Expr(Expr::List(items, _)))
                if matches!(elem_ty.as_ref(), SparType::Named(_)) =>
            {
                let SparType::Named(name) = elem_ty.as_ref() else {
                    unreachable!()
                };
                let Some(entry) = self.symbols.types.get(name).cloned() else {
                    return;
                };
                for item in items {
                    if let Expr::Object(obj_items, _) = item {
                        if let Err(e) = self.check_expr_with_locals(item, local_types) {
                            self.errors.push(e);
                        }
                        let config_fields: Vec<&FieldDecl> = obj_items
                            .iter()
                            .filter_map(|i| {
                                if let SectionItem::Field(f) = i {
                                    Some(f)
                                } else {
                                    None
                                }
                            })
                            .collect();
                        self.validate_type_fields_with_locals(
                            &entry.fields,
                            &config_fields,
                            name,
                            "return",
                            local_types,
                        );
                    } else {
                        self.errors.push(SparError::TypeError {
                            message: format!(
                                "function declares return type '[{}]' but this list element is not an object literal",
                                name
                            ),
                            hint: None,
                            span: span.clone(),
                        });
                    }
                }
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
                            actual
                                .as_ref()
                                .map(|t| display_type(t))
                                .unwrap_or_else(|| "unknown".into()),
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
                    cond_ty
                        .as_ref()
                        .map(|t| display_type(t))
                        .unwrap_or_else(|| "unknown".into()),
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
                if local_types.contains_key(name) {
                    continue;
                }
                if let Some(else_ty) = else_types.get(name) {
                    if then_ty != else_ty {
                        self.errors.push(SparError::TypeError {
                            message: format!(
                                "'{}' has type '{}' in the if-branch but '{}' in the else-branch \
                                 — both branches must declare it with the same type",
                                name,
                                display_type(then_ty),
                                display_type(else_ty)
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
            (true, true) => {}
            (false, true) => {
                local_types.extend(then_types);
            }
            (true, false) => {
                local_types.extend(else_types);
            }
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
                Literal::Int(_) => Some(SparType::Int),
                Literal::Float(_) => Some(SparType::Float),
                Literal::Bool(_) => Some(SparType::Bool),
            },
            Expr::String(_) => Some(SparType::Str),
            Expr::NamespaceRef(nr) if nr.segments.len() == 1 => {
                let name = &nr.segments[0];
                if let Some(ty) = locals.get(name) {
                    return Some(ty.clone());
                }
                self.infer_type(expr)
            }
            Expr::FieldAccess { base, field, .. } => {
                if let Expr::NamespaceRef(nr) = base.as_ref() {
                    if nr.segments.len() == 1 {
                        if let Some(ty) = locals.get(&nr.segments[0]) {
                            return match ty {
                                SparType::Named(type_name) => self
                                    .symbols
                                    .types
                                    .get(type_name)
                                    .and_then(|te| te.fields.iter().find(|f| &f.name == field))
                                    .map(|f| self.field_shape_to_type(&f.shape)),
                                _ => None,
                            };
                        }
                    }
                }
                self.infer_field_access(base, field)
            }
            Expr::Call { name, .. } => self.call_return_type(name),
            Expr::Unary { op, operand, .. } => match op {
                UnOp::Not => {
                    let t = self.infer_type_with_locals(operand, locals)?;
                    if t != SparType::Bool {
                        return None;
                    }
                    Some(SparType::Bool)
                }
                UnOp::Neg => {
                    let t = self.infer_type_with_locals(operand, locals)?;
                    if matches!(t, SparType::Int | SparType::Float) {
                        Some(t)
                    } else {
                        None
                    }
                }
            },
            Expr::Index { source, index, .. } => {
                let idx_ty = self.infer_type_with_locals(index, locals)?;
                if idx_ty != SparType::Int {
                    return None;
                }
                match self.infer_type_with_locals(source, locals)? {
                    SparType::List(elem) => Some(*elem),
                    _ => None,
                }
            }
            Expr::BinaryOp(b) => self.infer_binary_type_with_locals(b, locals),
            Expr::Comprehension {
                source,
                body,
                var_name,
                ..
            } => {
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
                let first = items
                    .first()
                    .and_then(|e| self.infer_type_with_locals(e, locals))?;
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
                if lty == rty {
                    Some(SparType::Bool)
                } else {
                    None
                }
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
                if lty == rty {
                    Some(lty)
                } else {
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_ok(src: &str) {
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        let table = crate::resolver::Resolver::new()
            .resolve(&program, &[])
            .expect("resolve");
        TypeChecker::check(&program, &table).expect("type check failed unexpectedly");
    }

    fn check_err(src: &str) -> Vec<String> {
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        let table = crate::resolver::Resolver::new()
            .resolve(&program, &[])
            .expect("resolve");
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
    fn test_section_field_as_call_argument_resolves_type() {
        // Regression: `infer_field_access` used to only resolve a field's
        // type off `self`, `global`, or a `SparType::Named` instance —
        // a plain top-level section (e.g. one spliced in via a selective
        // import) fell through to `None`, which `check_call`'s stricter
        // "actual != expected" comparison then reported as `unknown`.
        check_ok(
            r##"
            function greet(name: str) -> str {
                return name;
            };
            [Colors]{ red: str = "#ff0000"; };
            var msg: str = greet(name: Colors.red);
        "##,
        );
    }

    #[test]
    fn test_clean_program() {
        check_ok(
            r#"
            var port: int = 3000;
            var name: str = "keel";
            [Server]{ bind: str = "0.0.0.0"; };
        "#,
        );
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
        assert!(has_type_error(
            r#"var port: int = "3000";"#,
            "type mismatch"
        ));
    }

    #[test]
    fn test_bool_var_int_mismatch() {
        assert!(has_type_error("var flag: bool = 1;", "type mismatch"));
    }

    #[test]
    fn task_v2_metadata_types_are_checked() {
        for (src, field) in [
            (
                r#"task [Deploy] { private: "yes"; run { echo deploy; }; };"#,
                "private",
            ),
            (
                "task [Deploy] { group: 1; run { echo deploy; }; };",
                "group",
            ),
            (
                "task [Deploy] { shell: []; run { echo deploy; }; };",
                "shell",
            ),
            (
                "task [Deploy] { shell: [1]; run { echo deploy; }; };",
                "shell",
            ),
        ] {
            assert!(
                has_type_error(src, field),
                "expected type error for {field}: {src}"
            );
        }
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
            var p2: int = global.port;
        "#;
        check_ok(src);
    }

    #[test]
    fn test_namespace_ref_str_to_int_mismatch() {
        let src = r#"
            var name: str = "keel";
            var bad: int = global.name;
        "#;
        assert!(has_type_error(src, "type mismatch"));
    }

    #[test]
    fn test_section_field_ref_type() {
        let src = r#"
            [Db]{ pool: int = 5; };
            var p: int = Db.pool;
        "#;
        check_ok(src);
    }

    #[test]
    fn test_env_in_str_field() {
        check_ok(r#"var mode: str = env("APP_MODE");"#);
    }

    #[test]
    fn test_env_in_int_field() {
        assert!(has_type_error(
            r#"var port: int = env("PORT");"#,
            "type mismatch"
        ));
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
            errs.iter()
                .any(|e| e.contains("??") || e.contains("Fallback")),
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
        assert!(has_type_error(
            r#"var ports: [int] = [3000, "bad", 9090];"#,
            "list element type mismatch"
        ));
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
                let symbols = crate::resolver::Resolver::new()
                    .resolve(&program, &[])
                    .unwrap_or_else(|_| crate::resolver::SymbolTable {
                        globals: Default::default(),
                        sections: Default::default(),
                        imports: Default::default(),
                        functions: Default::default(),
                        types: Default::default(),
                        enums: Default::default(),
                        function_groups: Default::default(),
                        tasks: Default::default(),
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
                let symbols = crate::resolver::Resolver::new()
                    .resolve(&program, &[])
                    .unwrap_or_else(|_| crate::resolver::SymbolTable {
                        globals: Default::default(),
                        sections: Default::default(),
                        imports: Default::default(),
                        functions: Default::default(),
                        types: Default::default(),
                        enums: Default::default(),
                        function_groups: Default::default(),
                        tasks: Default::default(),
                    });
                let result = TypeChecker::check(&program, &symbols);
                assert!(
                    result.is_err(),
                    "section field with expr value must be rejected"
                );
            }
        }
    }

    #[test]
    fn typecheck_function_body_call_arg_type_mismatch() {
        // Wrong arg type inside a function body must be caught
        let src = r#"
            function double(x: int) -> int { return x; };
            function caller(s: str) -> int {
                var result: int = double(x: s);
                return result;
            };
        "#;
        let errs = check_err(src);
        let errs_str = format!("{:?}", errs);
        assert!(
            errs_str.contains("int")
                || errs_str.contains("str")
                || errs_str.contains("type")
                || errs_str.contains("arg"),
            "expected type mismatch error, got: {errs:?}"
        );
    }

    // ── Group 4: early-return type checking ───────────────────────────────────

    #[test]
    fn return_with_correct_type_is_ok() {
        check_ok(
            r#"
            function f(x: int) -> int {
                return x;
            };
        "#,
        );
    }

    #[test]
    fn return_with_wrong_type_is_error() {
        assert!(has_type_error(
            r#"function f(x: str) -> int { return x; };"#,
            "return",
        ));
    }

    #[test]
    fn return_in_both_branches_is_ok() {
        check_ok(
            r#"
            function pick(b: bool) -> int {
                if b { return 1; } else { return 2; }
            };
        "#,
        );
    }

    #[test]
    fn return_wrong_type_in_if_branch_is_error() {
        assert!(has_type_error(
            r#"function f(b: bool, x: str) -> int {
                if b { return x; } else { return 0; }
            };"#,
            "return",
        ));
    }

    #[test]
    fn section_fn_with_section_block_return_is_ok() {
        check_ok(
            r#"
            function make() -> section {
                return { port: int = 8080; };
            };
        "#,
        );
    }

    #[test]
    fn section_fn_expr_return_is_error() {
        assert!(has_type_error(
            r#"function make() -> section { return 42; };"#,
            "section",
        ));
    }

    #[test]
    fn non_section_fn_section_block_return_is_error() {
        assert!(has_type_error(
            r#"function make() -> int { return { port: int = 8080; }; };"#,
            "section",
        ));
    }

    #[test]
    fn terminal_branch_exemption_local_available_after_if() {
        // then-branch always returns; else-branch declares `y` — `y` must be available after
        check_ok(
            r#"
            function f(b: bool) -> int {
                if b { return 0; } else { var y: int = 1; }
                return y;
            };
        "#,
        );
    }

    #[test]
    fn local_var_type_mismatch_in_function_is_error() {
        assert!(has_type_error(
            r#"function f() -> int { var x: int = "hello"; return x; };"#,
            "declared as",
        ));
    }
}
