use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::error::{Span, SparError};
use crate::resolver::{FunctionEntry, GlobalEntry, SymbolTable};

pub fn display_type(ty: &SparType) -> String {
    match ty {
        SparType::Any => "Any".into(),
        SparType::Str => "str".into(),
        SparType::Int => "int".into(),
        SparType::Float => "float".into(),
        SparType::Bool => "bool".into(),
        SparType::InlineRecord => "Record".into(),
        SparType::Void => "void".into(),
        SparType::Shell => "ShellPlan".into(),
        SparType::Error => "error".into(),
        SparType::List(inner) => format!("List<{}>", display_type(inner)),
        SparType::Tuple(items) => format!(
            "({})",
            items
                .iter()
                .map(display_type)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        SparType::Named(name) => crate::naming::demangle(name),
        SparType::TypeParameter(name) => name.clone(),
        SparType::Applied { name, arguments } => format!(
            "{}<{}>",
            name,
            arguments
                .iter()
                .map(display_type)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        SparType::Function {
            params,
            return_type,
        } => format!(
            "fn({}) -> {}",
            params
                .iter()
                .map(|param| format!("{}: {}", param.name, display_type(&param.ty)))
                .collect::<Vec<_>>()
                .join(", "),
            display_type(return_type)
        ),
    }
}

fn bind_method_type_arguments(
    method: &str,
    entry: &FunctionEntry,
    arguments: &[SparType],
    substitution: &mut TypeSubstitution,
    span: &Span,
) -> Result<(), SparError> {
    if arguments.is_empty() {
        return Ok(());
    }
    if arguments.len() != entry.type_parameters.len() {
        return Err(SparError::TypeError {
            message: format!(
                "method '{method}' expects {} type arguments, found {}",
                entry.type_parameters.len(),
                arguments.len()
            ),
            hint: None,
            span: span.clone(),
        });
    }
    for (parameter, argument) in entry.type_parameters.iter().zip(arguments) {
        substitution.insert(parameter.name.clone(), argument.clone());
    }
    Ok(())
}

fn promise_type(inner: SparType) -> SparType {
    SparType::Applied {
        name: "Promise".to_string(),
        arguments: vec![inner],
    }
}

fn promise_inner(ty: &SparType) -> Option<&SparType> {
    match ty {
        SparType::Applied { name, arguments } if name == "Promise" && arguments.len() == 1 => {
            arguments.first()
        }
        _ => None,
    }
}

fn callable_return_type(entry: &FunctionEntry) -> SparType {
    if entry.is_async {
        promise_type(entry.ret.clone())
    } else {
        entry.ret.clone()
    }
}

fn await_hint(expected: &SparType, actual: &SparType) -> Option<String> {
    (promise_inner(actual) == Some(expected))
        .then(|| "use `await` to obtain the promise result".to_string())
}

fn is_legacy_callable_param(name: &str) -> bool {
    name.strip_prefix("arg")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()))
}

fn supports_value_equality(ty: &SparType) -> bool {
    match ty {
        SparType::Function { .. } | SparType::Shell | SparType::Void => false,
        SparType::List(inner) => supports_value_equality(inner),
        SparType::Tuple(items) => items.iter().all(supports_value_equality),
        SparType::Applied { name, arguments } => {
            name != "Promise" && arguments.iter().all(supports_value_equality)
        }
        _ => true,
    }
}

pub(crate) fn is_assignable(expected: &SparType, actual: &SparType) -> bool {
    if expected == &SparType::Any {
        return actual != &SparType::Void;
    }
    if expected == actual {
        return true;
    }
    if let (SparType::Applied { name, arguments }, SparType::Applied { name: actual_name, arguments: actual_arguments }) = (expected, actual) {
        if name == "ShellResult" && actual_name == "Result" && arguments == actual_arguments {
            return true;
        }
    }
    match (expected, actual) {
        // `Slice<T>` is the native-module spelling for "a list of T or a native Buffer": both
        // can be borrowed as contiguous memory (a list costs one copy, a Buffer none).
        (SparType::Applied { name, arguments }, actual)
            if name == "Slice" && arguments.len() == 1 =>
        {
            match actual {
                SparType::Named(buffer) => buffer == "Buffer",
                SparType::List(inner) => **inner == arguments[0],
                _ => false,
            }
        }
        (
            SparType::Function {
                params: expected_params,
                return_type: expected_return,
            },
            SparType::Function {
                params: actual_params,
                return_type: actual_return,
            },
        ) if expected_params.len() == actual_params.len() => {
            expected_params
                .iter()
                .zip(actual_params)
                .all(|(expected, actual)| {
                    (expected.name == actual.name || is_legacy_callable_param(&expected.name))
                        && expected.ty == actual.ty
                })
                && expected_return == actual_return
        }
        _ => false,
    }
}

pub(crate) type TypeSubstitution = HashMap<String, SparType>;

const SEQUENCE_SHAPE_KEY: &str = "__spar_sequence_shape";
const SEQUENCE_LIST_SHAPE: &str = "__spar_sequence_list";

/// A type that can be a `Table` row: a struct/record shape, not a scalar.
fn is_untyped_closure_expr(expression: &Expr) -> bool {
    matches!(expression, Expr::Closure { params, .. } if params.iter().any(|param| param.ty.is_none()))
}

fn is_row_type(ty: &SparType) -> bool {
    matches!(ty, SparType::Named(_) | SparType::Applied { .. })
}

/// A value fits a pipe parameter when the types are equal, or when a dynamic
/// `Record` is piped where a list of records is expected (see
/// `sequence_parts`); the runtime checks the value really is a list.
pub(crate) fn pipe_type_accepts(expected: &SparType, actual: &SparType) -> bool {
    if is_assignable(expected, actual)
        || unify_generic(
            expected,
            actual,
            &mut TypeSubstitution::new(),
            &Span::dummy(),
        )
        .is_ok()
    {
        return true;
    }
    let record = SparType::Named("Record".into());
    matches!(expected, SparType::List(inner) if inner.as_ref() == &record) && actual == &record
}

fn sequence_parts(actual: &SparType) -> Option<(SparType, SparType)> {
    match actual {
        SparType::List(inner) => Some((
            SparType::Named(SEQUENCE_LIST_SHAPE.into()),
            inner.as_ref().clone(),
        )),
        SparType::Applied { name, arguments }
            if matches!(name.as_str(), "Table" | "Stream") && arguments.len() == 1 =>
        {
            Some((SparType::Named(name.clone()), arguments[0].clone()))
        }
        // A dynamic `Record` (JSON field, `_` of unknown shape) may hold a list.
        // Accept it as a list of records; the runtime rejects it with a clear
        // error when the value turns out not to be a list.
        SparType::Named(name) if name == "Record" => Some((
            SparType::Named(SEQUENCE_LIST_SHAPE.into()),
            SparType::Named("Record".into()),
        )),
        _ => None,
    }
}

/// `Lookup<K, V>` is a stdlib-only pattern: any indexable container whose
/// key/value types match satisfies it once generics have been substituted.
fn lookup_accepts(expected: &SparType, actual: Option<&SparType>) -> bool {
    let (SparType::Applied { name, arguments }, Some(actual)) = (expected, actual) else {
        return false;
    };
    if name != "Lookup" || arguments.len() != 2 {
        return false;
    }
    lookup_parts(actual).is_some_and(|(key, value)| key == arguments[0] && value == arguments[1])
}

fn lookup_parts(actual: &SparType) -> Option<(SparType, SparType)> {
    match actual {
        SparType::List(inner) => Some((SparType::Int, inner.as_ref().clone())),
        SparType::Applied { name, arguments }
            if matches!(name.as_str(), "Table" | "Stream") && arguments.len() == 1 =>
        {
            Some((SparType::Int, arguments[0].clone()))
        }
        SparType::Applied { name, arguments } if name == "Map" && arguments.len() == 2 => {
            Some((arguments[0].clone(), arguments[1].clone()))
        }
        _ => None,
    }
}

pub(crate) fn substitute_type(ty: &SparType, substitution: &TypeSubstitution) -> SparType {
    match ty {
        SparType::TypeParameter(name) => substitution
            .get(name)
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        SparType::List(inner) => SparType::List(Box::new(substitute_type(inner, substitution))),
        SparType::Tuple(items) => SparType::Tuple(
            items
                .iter()
                .map(|item| substitute_type(item, substitution))
                .collect(),
        ),
        SparType::Applied { name, arguments } if name == "Sequence" && arguments.len() == 1 => {
            let inner = substitute_type(&arguments[0], substitution);
            match substitution.get(SEQUENCE_SHAPE_KEY) {
                Some(SparType::Named(shape)) if shape == SEQUENCE_LIST_SHAPE => {
                    SparType::List(Box::new(inner))
                }
                // Table rows are Records; mapping to scalars yields a plain list.
                Some(SparType::Named(shape)) if shape == "Table" && !is_row_type(&inner) => {
                    SparType::List(Box::new(inner))
                }
                Some(SparType::Named(shape)) if matches!(shape.as_str(), "Table" | "Stream") => {
                    SparType::Applied {
                        name: shape.clone(),
                        arguments: vec![inner],
                    }
                }
                _ => SparType::Applied {
                    name: name.clone(),
                    arguments: vec![inner],
                },
            }
        }
        SparType::Applied { name, arguments } => SparType::Applied {
            name: name.clone(),
            arguments: arguments
                .iter()
                .map(|argument| substitute_type(argument, substitution))
                .collect(),
        },
        SparType::Function {
            params,
            return_type,
        } => SparType::Function {
            params: params
                .iter()
                .map(|param| CallableParamType {
                    name: param.name.clone(),
                    ty: substitute_type(&param.ty, substitution),
                })
                .collect(),
            return_type: Box::new(substitute_type(return_type, substitution)),
        },
        _ => ty.clone(),
    }
}

pub(crate) fn mentions_type_parameter(ty: &SparType) -> bool {
    match ty {
        SparType::TypeParameter(_) => true,
        SparType::List(inner) => mentions_type_parameter(inner),
        SparType::Tuple(items) => items.iter().any(mentions_type_parameter),
        SparType::Applied { arguments, .. } => arguments.iter().any(mentions_type_parameter),
        SparType::Function {
            params,
            return_type,
        } => {
            params
                .iter()
                .any(|param| mentions_type_parameter(&param.ty))
                || mentions_type_parameter(return_type)
        }
        _ => false,
    }
}

fn collect_type_parameter_names(ty: &SparType, out: &mut Vec<String>) {
    match ty {
        SparType::TypeParameter(name) => out.push(name.clone()),
        SparType::List(inner) => collect_type_parameter_names(inner, out),
        SparType::Tuple(items) => {
            for item in items {
                collect_type_parameter_names(item, out);
            }
        }
        SparType::Applied { arguments, .. } => {
            for argument in arguments {
                collect_type_parameter_names(argument, out);
            }
        }
        SparType::Function {
            params,
            return_type,
        } => {
            for param in params {
                collect_type_parameter_names(&param.ty, out);
            }
            collect_type_parameter_names(return_type, out);
        }
        _ => {}
    }
}

fn external_type_parameter_names(params: &[(String, SparType)], ret: &SparType) -> Vec<String> {
    let mut names = Vec::new();
    for (_, ty) in params {
        collect_type_parameter_names(ty, &mut names);
    }
    collect_type_parameter_names(ret, &mut names);
    let mut seen = HashSet::new();
    names.retain(|name| seen.insert(name.clone()));
    names
}

pub(crate) fn unify_generic(
    pattern: &SparType,
    actual: &SparType,
    substitution: &mut TypeSubstitution,
    span: &Span,
) -> Result<(), SparError> {
    match pattern {
        SparType::Any if actual != &SparType::Void => Ok(()),
        SparType::TypeParameter(name) => match substitution.get(name) {
            Some(existing) if existing != actual => Err(SparError::TypeError {
                message: format!(
                    "conflicting inference for type parameter '{name}': {} and {}",
                    display_type(existing),
                    display_type(actual)
                ),
                hint: None,
                span: span.clone(),
            }),
            Some(_) => Ok(()),
            None => {
                substitution.insert(name.clone(), actual.clone());
                Ok(())
            }
        },
        SparType::List(pattern_inner) => match actual {
            SparType::List(actual_inner) => {
                unify_generic(pattern_inner, actual_inner, substitution, span)
            }
            _ => type_mismatch(pattern, actual, span),
        },
        SparType::Tuple(pattern_items) => match actual {
            SparType::Tuple(actual_items) if pattern_items.len() == actual_items.len() => {
                for (pattern, actual) in pattern_items.iter().zip(actual_items) {
                    unify_generic(pattern, actual, substitution, span)?;
                }
                Ok(())
            }
            _ => type_mismatch(pattern, actual, span),
        },
        SparType::Applied {
            name: pattern_name,
            arguments: pattern_arguments,
        } if pattern_name == "Sequence" && pattern_arguments.len() == 1 => {
            if let SparType::Applied { name, arguments } = actual {
                if name == "Sequence" && arguments.len() == 1 {
                    return unify_generic(&pattern_arguments[0], &arguments[0], substitution, span);
                }
            }
            let Some((shape, element)) = sequence_parts(actual) else {
                return type_mismatch(pattern, actual, span);
            };
            match substitution.get(SEQUENCE_SHAPE_KEY) {
                Some(existing) if existing != &shape => {
                    return Err(SparError::TypeError {
                        message: "structured sequence arguments must use the same container shape"
                            .into(),
                        hint: None,
                        span: span.clone(),
                    });
                }
                Some(_) => {}
                None => {
                    substitution.insert(SEQUENCE_SHAPE_KEY.into(), shape);
                }
            }
            unify_generic(&pattern_arguments[0], &element, substitution, span)
        }
        SparType::Applied {
            name: pattern_name,
            arguments: pattern_arguments,
        } if pattern_name == "Lookup" && pattern_arguments.len() == 2 => {
            if let SparType::Applied { name, arguments } = actual {
                if name == "Lookup" && arguments.len() == 2 {
                    unify_generic(&pattern_arguments[0], &arguments[0], substitution, span)?;
                    return unify_generic(&pattern_arguments[1], &arguments[1], substitution, span);
                }
            }
            let Some((key, value)) = lookup_parts(actual) else {
                return type_mismatch(pattern, actual, span);
            };
            unify_generic(&pattern_arguments[0], &key, substitution, span)?;
            unify_generic(&pattern_arguments[1], &value, substitution, span)
        }
        SparType::Applied {
            name: pattern_name,
            arguments: pattern_arguments,
        } => match actual {
            SparType::Applied {
                name: actual_name,
                arguments: actual_arguments,
            } if pattern_name == actual_name
                && pattern_arguments.len() == actual_arguments.len() =>
            {
                for (pattern, actual) in pattern_arguments.iter().zip(actual_arguments) {
                    unify_generic(pattern, actual, substitution, span)?;
                }
                Ok(())
            }
            _ => type_mismatch(pattern, actual, span),
        },
        SparType::Function {
            params: pattern_params,
            return_type: pattern_return,
        } => match actual {
            SparType::Function {
                params: actual_params,
                return_type: actual_return,
            } if pattern_params.len() == actual_params.len() => {
                for (pattern, actual) in pattern_params.iter().zip(actual_params) {
                    if pattern.name != actual.name && !is_legacy_callable_param(&pattern.name) {
                        return Err(SparError::TypeError {
                            message: format!(
                                "callable parameter name mismatch: expected '{}', found '{}'",
                                pattern.name, actual.name
                            ),
                            hint: None,
                            span: span.clone(),
                        });
                    }
                    unify_generic(&pattern.ty, &actual.ty, substitution, span)?;
                }
                unify_generic(pattern_return, actual_return, substitution, span)
            }
            _ => type_mismatch(pattern, actual, span),
        },
        _ if pattern == actual => Ok(()),
        _ => type_mismatch(pattern, actual, span),
    }
}

fn type_mismatch(pattern: &SparType, actual: &SparType, span: &Span) -> Result<(), SparError> {
    Err(SparError::TypeError {
        message: format!(
            "expected {}, found {}",
            display_type(pattern),
            display_type(actual)
        ),
        hint: None,
        span: span.clone(),
    })
}

fn substitute_field_shape(
    shape: &TypeFieldShape,
    substitution: &TypeSubstitution,
) -> TypeFieldShape {
    match shape {
        TypeFieldShape::Primitive(ty) => {
            TypeFieldShape::Primitive(substitute_type(ty, substitution))
        }
        TypeFieldShape::Named(name) => TypeFieldShape::Named(name.clone()),
        TypeFieldShape::TypeParameter(name) => {
            match substitute_type(&SparType::TypeParameter(name.clone()), substitution) {
                SparType::Named(name) => TypeFieldShape::Named(name),
                SparType::Applied { name, arguments } => {
                    TypeFieldShape::Applied { name, arguments }
                }
                ty => TypeFieldShape::Primitive(ty),
            }
        }
        TypeFieldShape::Applied { name, arguments } => TypeFieldShape::Applied {
            name: name.clone(),
            arguments: arguments
                .iter()
                .map(|argument| substitute_type(argument, substitution))
                .collect(),
        },
        TypeFieldShape::InlineRecord(fields) => TypeFieldShape::InlineRecord(
            fields
                .iter()
                .map(|field| substitute_type_field(field, substitution))
                .collect(),
        ),
    }
}

fn type_field_shape_is_option(shape: &TypeFieldShape) -> bool {
    matches!(
        shape,
        TypeFieldShape::Applied { name, arguments }
            if name == "Option" && arguments.len() == 1
    ) || matches!(
        shape,
        TypeFieldShape::Primitive(SparType::Applied { name, arguments })
            if name == "Option" && arguments.len() == 1
    )
}

fn type_field_is_omittable(field: &TypeField) -> bool {
    field.default.is_some() || type_field_shape_is_option(&field.shape)
}

pub(crate) fn substitute_type_field(
    field: &TypeField,
    substitution: &TypeSubstitution,
) -> TypeField {
    TypeField {
        name: field.name.clone(),
        shape: substitute_field_shape(&field.shape, substitution),
        default: field
            .default
            .as_ref()
            .map(|value| crate::loader::scope::substitute_default(value, substitution)),
        span: field.span.clone(),
    }
}

/// Instantiated parameter types of the function stage in `input |> stage`
/// (piped value first), for lowering closures whose parameters are inferred.
pub(crate) fn pipe_stage_parameters_with_locals(
    input: &Expr,
    stage: &Expr,
    symbols: &SymbolTable,
    locals: &HashMap<String, SparType>,
) -> Option<Vec<(String, SparType)>> {
    let current_impl = match locals.get("self") {
        Some(SparType::Named(owner)) => Some(owner.clone()),
        _ => None,
    };
    let span = input.span().cloned().unwrap_or_else(Span::dummy);
    TypeChecker {
        symbols,
        errors: Vec::new(),
        schema_bindings: HashMap::new(),
        current_struct: None,
        current_impl,
        mutable_bindings: HashSet::new(),
        current_method_receiver_mutable: false,
        expectations: Default::default(),
        in_shell_statement_scope: false,
        type_map: Default::default(),
        receiver_map: Default::default(),
        record_types: false,
    }
    .pipe_stage_parameter_types(input, stage, locals, &span)
    .ok()
}

pub(crate) fn infer_expression_with_locals(
    expr: &Expr,
    symbols: &SymbolTable,
    locals: &HashMap<String, SparType>,
) -> Option<SparType> {
    // Inside a method the receiver `self` is a local of the owning struct's
    // type, which is what grants access to that struct's private methods.
    let current_impl = match locals.get("self") {
        Some(SparType::Named(owner)) => Some(owner.clone()),
        _ => None,
    };
    TypeChecker {
        symbols,
        errors: Vec::new(),
        schema_bindings: HashMap::new(),
        current_struct: None,
        current_impl,
        mutable_bindings: HashSet::new(),
        current_method_receiver_mutable: false,
        expectations: Default::default(),
        in_shell_statement_scope: false,
        type_map: Default::default(),
        receiver_map: Default::default(),
        record_types: false,
    }
    .infer_type_with_locals(expr, locals)
}

/// A `TypeFieldShape` expanded one level — `Named(X)` resolved to `X`'s own
/// fields, so shape comparison only ever has to handle two cases.
enum ShapeKind {
    Primitive(SparType),
    InlineRecord(Vec<TypeField>),
}

/// `items` is exactly one `...Source;` spread and nothing else — the
/// spread-only body pattern that gets a structural shape check instead of
/// the "can't statically verify" skip a mixed spread+fields body gets.
fn spread_only_source(items: &[ObjectItem]) -> Option<&SpreadStmt> {
    match items {
        [ObjectItem::Spread(s)] => Some(s),
        _ => None,
    }
}

/// The spread's source struct name, if it's a same-file, single-segment
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
    /// Struct name → schema-derived field list, for structs validated
    /// against an `import schema "...";` but with no type binding of
    /// their own. Empty unless populated via `check_with_schema`.
    schema_bindings: HashMap<String, Vec<SchemaField>>,
    /// The struct currently being checked, for `self.field` type lookups.
    current_struct: Option<Vec<String>>,
    current_impl: Option<String>,
    mutable_bindings: HashSet<String>,
    current_method_receiver_mutable: bool,
    /// Declared types that a generic call's own type parameters may be inferred
    /// from (`return err(...)` in a `-> Result<int, str>` function), keyed by the
    /// call's name span.
    expectations: std::cell::RefCell<HashMap<(usize, usize), SparType>>,
    /// True while checking a `shell { ... }` block's own `.statements` (see
    /// `check_mixed_shell_with_locals`). A bare `return;` there returns from
    /// the *enclosing* function, whose real declared return type isn't
    /// threaded down to this call — `SparType::Any` stands in for it so a
    /// `return someValue;` can't false-positive, but `Any` alone still
    /// rejects a bare `return;` (`is_assignable(Any, actual)` requires
    /// `actual != Void`), which would false-positive on legitimate early
    /// returns. This flag tells `check_return_value` to skip that specific
    /// check instead, rather than threading a new parameter through every
    /// `check_func_stmts`/`check_if_stmt` call site.
    in_shell_statement_scope: bool,
    /// Inferred type of every expression the checker asked about, by byte span. Filled only when
    /// `record_types` is set (editor tooling); costs nothing otherwise.
    type_map: std::cell::RefCell<Vec<TypedSpan>>,
    receiver_map: std::cell::RefCell<Vec<TypedSpan>>,
    record_types: bool,
}

/// Everything the checker inferred, for editor tooling.
#[derive(Debug, Clone, Default)]
pub struct TypeMap {
    /// Inferred type per expression span.
    pub expressions: Vec<TypedSpan>,
    /// Receiver type of each `recv.member` / `recv.method()`, keyed by the dot's byte offset.
    pub receivers: Vec<TypedSpan>,
}

/// An expression's inferred type at a source range (byte offsets), for hover/completion.
#[derive(Debug, Clone)]
pub struct TypedSpan {
    pub start: usize,
    pub end: usize,
    pub ty: SparType,
}

impl<'a> TypeChecker<'a> {
    /// Return the compiler-resolved fields for a named/applied structured type.
    /// Tooling should use this instead of reimplementing generic substitution.
    pub fn fields_for_type(
        ty: &SparType,
        symbols: &SymbolTable,
    ) -> Option<(String, Vec<TypeField>)> {
        match ty {
            SparType::Named(name) => {
                if let Some(entry) = symbols.types.get(name) {
                    return Some((name.clone(), entry.fields.clone()));
                }
                let struct_entry = symbols.structs.get(&vec![name.clone()])?;
                let mut fields = struct_entry
                    .type_binding
                    .as_ref()
                    .and_then(|binding| Self::fields_for_type(binding, symbols))
                    .map(|(_, fields)| fields)
                    .unwrap_or_default();
                for (field_name, entry) in &struct_entry.fields {
                    if let Some(existing) =
                        fields.iter_mut().find(|field| field.name == *field_name)
                    {
                        if let Some(ty) = &entry.ty {
                            existing.shape = TypeFieldShape::Primitive(ty.clone());
                        }
                        existing.span = entry.span.clone();
                    } else {
                        fields.push(TypeField {
                            name: field_name.clone(),
                            shape: TypeFieldShape::Primitive(
                                entry.ty.clone().unwrap_or(SparType::Any),
                            ),
                            default: None,
                            span: entry.span.clone(),
                        });
                    }
                }
                fields.sort_by_key(|field| field.span.start);
                Some((name.clone(), fields))
            }
            SparType::Applied { name, arguments } if name == "MapEntry" && arguments.len() == 2 => {
                Some((
                    display_type(ty),
                    vec![
                        TypeField {
                            name: "key".into(),
                            shape: TypeFieldShape::Primitive(arguments[0].clone()),
                            default: None,
                            span: Span::dummy(),
                        },
                        TypeField {
                            name: "value".into(),
                            shape: TypeFieldShape::Primitive(arguments[1].clone()),
                            default: None,
                            span: Span::dummy(),
                        },
                    ],
                ))
            }
            SparType::Applied { name, arguments } => {
                let entry = symbols.types.get(name)?;
                if entry.type_parameters.len() != arguments.len() {
                    return None;
                }
                let substitution: TypeSubstitution = entry
                    .type_parameters
                    .iter()
                    .zip(arguments)
                    .map(|(parameter, argument)| (parameter.name.clone(), argument.clone()))
                    .collect();
                Some((
                    display_type(ty),
                    entry
                        .fields
                        .iter()
                        .map(|field| substitute_type_field(field, &substitution))
                        .collect(),
                ))
            }
            _ => None,
        }
    }

    /// Resolve one field using the same field/generic rules as the checker.
    pub fn field_type(ty: &SparType, field_name: &str, symbols: &SymbolTable) -> Option<SparType> {
        if matches!(ty, SparType::InlineRecord)
            || matches!(ty, SparType::Named(name) if name == "Record")
        {
            return Some(SparType::InlineRecord);
        }
        let (_, fields) = Self::fields_for_type(ty, symbols)?;
        let field = fields.iter().find(|field| field.name == field_name)?;
        Some(match &field.shape {
            TypeFieldShape::Primitive(ty) => ty.clone(),
            TypeFieldShape::Named(name) => SparType::Named(name.clone()),
            TypeFieldShape::TypeParameter(name) => SparType::TypeParameter(name.clone()),
            TypeFieldShape::Applied { name, arguments } => SparType::Applied {
                name: name.clone(),
                arguments: arguments.clone(),
            },
            TypeFieldShape::InlineRecord(_) => SparType::InlineRecord,
        })
    }

    fn type_fields_for(&self, ty: &SparType) -> Option<(String, Vec<TypeField>)> {
        Self::fields_for_type(ty, self.symbols)
    }

    /// Infer the fully resolved type of an expression using the same rules as
    /// normal type checking. Language tooling should use this instead of
    /// duplicating Spar's inference logic.
    pub fn infer_expression(expr: &Expr, symbols: &'a SymbolTable) -> Option<SparType> {
        TypeChecker {
            symbols,
            errors: Vec::new(),
            schema_bindings: HashMap::new(),
            current_struct: None,
            current_impl: None,
            mutable_bindings: HashSet::new(),
            current_method_receiver_mutable: false,
            expectations: Default::default(),
            in_shell_statement_scope: false,
            type_map: Default::default(),
            receiver_map: Default::default(),
            record_types: false,
        }
        .infer_type(expr)
    }

    pub fn check(program: &Program, symbols: &'a SymbolTable) -> Result<(), Vec<SparError>> {
        let mut tc = TypeChecker {
            symbols,
            errors: Vec::new(),
            schema_bindings: HashMap::new(),
            current_struct: None,
            current_impl: None,
            mutable_bindings: HashSet::new(),
            current_method_receiver_mutable: false,
            expectations: Default::default(),
            in_shell_statement_scope: false,
            type_map: Default::default(),
            receiver_map: Default::default(),
            record_types: false,
        };
        tc.check_program(program);
        if tc.errors.is_empty() {
            Ok(())
        } else {
            Err(tc.errors)
        }
    }

    /// Like `check`, but also returns the inferred type of every expression the checker visited.
    pub fn check_with_type_map(
        program: &Program,
        symbols: &'a SymbolTable,
    ) -> (Result<(), Vec<SparError>>, TypeMap) {
        let mut tc = TypeChecker {
            symbols,
            errors: Vec::new(),
            schema_bindings: HashMap::new(),
            current_struct: None,
            current_impl: None,
            mutable_bindings: HashSet::new(),
            current_method_receiver_mutable: false,
            expectations: Default::default(),
            in_shell_statement_scope: false,
            type_map: Default::default(),
            receiver_map: Default::default(),
            record_types: true,
        };
        tc.check_program(program);
        let map = TypeMap {
            expressions: tc.type_map.take(),
            receivers: tc.receiver_map.take(),
        };
        (
            if tc.errors.is_empty() {
                Ok(())
            } else {
                Err(tc.errors)
            },
            map,
        )
    }

    pub fn check_with_imports(
        program: &Program,
        symbols: &'a SymbolTable,
        _loaded: &std::collections::HashMap<String, crate::loader::LoadedImport>,
    ) -> Result<(), Vec<SparError>> {
        Self::check(program, symbols)
    }

    /// Like `check`, but also given every struct's schema-derived field
    /// list (from `loader::validate_schema_imports`'s `Ok` value) — a
    /// struct with no type binding but a name present in `bindings`
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
            current_struct: None,
            current_impl: None,
            mutable_bindings: HashSet::new(),
            current_method_receiver_mutable: false,
            expectations: Default::default(),
            in_shell_statement_scope: false,
            type_map: Default::default(),
            receiver_map: Default::default(),
            record_types: false,
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
        let mut module_locals = HashMap::new();
        for item in &program.items {
            match item {
                TopLevelItem::Import(_) => {}
                TopLevelItem::Var(decl) => self.check_var(decl),
                TopLevelItem::Dynamic(decl) => self.check_dynamic(decl),
                TopLevelItem::Struct(decl) => self.check_struct(decl),
                TopLevelItem::Impl(decl) => {
                    let owner = match &decl.target {
                        SparType::Named(name) => name.clone(),
                        SparType::Applied { name, .. } => name.clone(),
                        _ => continue,
                    };
                    let previous = self.current_impl.replace(owner);
                    for method in &decl.methods {
                        let previous_mut = self.current_method_receiver_mutable;
                        self.current_method_receiver_mutable = method
                            .receiver
                            .as_ref()
                            .is_some_and(|receiver| receiver.mutable);
                        self.check_function_decl(&method.function);
                        self.current_method_receiver_mutable = previous_mut;
                    }
                    self.current_impl = previous;
                }
                TopLevelItem::Function(f) => self.check_function_decl(f),
                TopLevelItem::Schema(_) => {}
                TopLevelItem::Type(decl) => self.check_type_decl(decl),
                TopLevelItem::Enum(_) => {} // nothing to typecheck — resolver already validated the declaration
                TopLevelItem::FunctionGroup(g) => {
                    for f in &g.functions {
                        self.check_function_decl(f);
                    }
                }
                TopLevelItem::SchemaFrom(_) => {} // never reaches the typechecker — schema files aren't typechecked (loader.rs handles them out-of-band)
                TopLevelItem::Task(decl) => self.check_task(decl),
                // Interactive input is checked as if inside an async function so
                // a final `await expr` line is allowed.
                TopLevelItem::Statement(statement) => self.check_func_stmts(
                    std::slice::from_ref(statement),
                    &SparType::Int,
                    &mut module_locals,
                    self.symbols.top_level_await,
                ),
            }
        }
    }

    fn check_var(&mut self, decl: &VarDecl) {
        // Anonymous inline-record shapes are internal-only; source code uses Record/Map or named structs.
        if decl.ty == SparType::InlineRecord {
            self.push_type_error(
                format!(
                    "anonymous inline record is not a valid declared type for variable '{}' — use `Record`, `Map<K, V>`, or a named struct instead",
                    decl.name
                ),
                None,
                decl.span.clone(),
            );
            return;
        }
        if decl.value.is_none() {
            self.push_type_error(
                format!(
                    "required variable `{}` has no value — add `= <value>` or initialize it with `none()` when its type is `Option<T>`",
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
        if decl.value.is_none() {
            self.push_type_error(
                format!(
                    "required dynamic variable `{}` has no value — add `= [...]` or initialize it explicitly",
                    decl.name
                ),
                None,
                decl.span.clone(),
            );
        }
    }

    fn check_type_decl(&mut self, decl: &TypeDecl) {
        for field in &decl.fields {
            let Some(default) = &field.default else {
                continue;
            };
            let expected = match &field.shape {
                TypeFieldShape::Primitive(ty) => Some(ty.clone()),
                TypeFieldShape::Named(name) => Some(SparType::Named(name.clone())),
                TypeFieldShape::TypeParameter(name) => Some(SparType::TypeParameter(name.clone())),
                TypeFieldShape::Applied { name, arguments } => Some(SparType::Applied {
                    name: name.clone(),
                    arguments: arguments.clone(),
                }),
                TypeFieldShape::InlineRecord(_) => None,
            };
            if let Some(expected) = expected {
                self.check_expr_type(default, &expected, &field.name, &field.span);
            }
        }
    }

    fn check_struct(&mut self, decl: &StructDecl) {
        for field in decl.type_decl().fields {
            if matches!(&field.default, Some(Expr::Call { name, args, .. }) if name == &decl.name && args.is_empty())
            {
                self.push_type_error("recursive struct default; use an explicit optional boundary such as Option<T> = none()", None, field.span);
            }
        }
        if decl.is_emit()
            && (!decl.type_parameters.is_empty()
                || decl
                    .type_decl()
                    .fields
                    .iter()
                    .any(|field| field.default.is_none()))
        {
            self.push_type_error(
                "#[emit] struct requires a concrete declaration with defaults for every field",
                None,
                decl.span.clone(),
            );
        }
        let struct_path = vec![decl.name.clone()];
        let prev_section = self.current_struct.replace(struct_path);
        let path_str = decl.name.clone();
        match &decl.type_binding {
            Some(binding) => self.check_type_binding(decl, binding, &path_str),
            None => {
                let fields: Vec<&FieldDecl> = decl
                    .items
                    .iter()
                    .filter_map(|i| {
                        if let ObjectItem::Field(f) = i {
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
                    None => self.check_untyped_struct_fields(&fields, &path_str),
                }
            }
        }
        self.current_struct = prev_section;
    }

    /// A struct with no type binding but a matching `import schema`
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
                    Some(FieldValue::Object(items)) => {
                        let subs: Vec<&FieldDecl> = items
                            .iter()
                            .filter_map(|i| {
                                if let ObjectItem::Field(f) = i {
                                    Some(f)
                                } else {
                                    None
                                }
                            })
                            .collect();
                        self.check_untyped_struct_fields(
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
        let SchemaFieldShape::Type(expected_ty) = &sf.shape;
        match &field.value {
            Some(FieldValue::Expr(value)) => {
                self.check_expr_type(value, expected_ty, &field.name, &field.span)
            }
            Some(FieldValue::Object(_)) => {
                self.push_type_error(
                    format!(
                        "field `{}` in struct `{path_str}` must be `{}` (required by the bound schema) but uses an anonymous object body `{{ ... }}`",
                        field.name, display_type(expected_ty)
                    ),
                    Some("construct a named struct value, or declare the schema field as `Record`/`Map` when dynamic data is intended".into()),
                    field.span.clone(),
                );
            }
            None => {
                self.push_type_error(
                    format!(
                        "required field `{}` in struct `{path_str}` has no value",
                        field.name
                    ),
                    None,
                    field.span.clone(),
                );
            }
        }
    }

    /// Every field in an unbound struct must have an
    /// explicit type — there is nothing to infer it from.
    fn check_untyped_struct_fields(&mut self, fields: &[&FieldDecl], path_str: &str) {
        for field in fields {
            match &field.ty {
                Some(ty) => self.check_field(field, ty, path_str),
                None => self.push_type_error(
                    format!(
                        "field `{}` in struct `{path_str}` has no type — unbound structs must declare each field's type explicitly",
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
            (SparType::InlineRecord, Some(FieldValue::Expr(e))) => {
                let actual = self.infer_type(e);
                if actual != Some(SparType::InlineRecord) {
                    self.push_type_error(
                        format!(
                            "field '{}' in struct `{path_str}` has an internal inline-record type but value is {}",
                            field.name,
                                actual.as_ref().map(display_type).unwrap_or_else(|| "unknown".into()),
                        ),
                        None,
                        field.span.clone(),
                    );
                }
                self.check_expr_internal(e);
                return;
            }
            (other_ty, Some(FieldValue::Object(_))) if *other_ty != SparType::InlineRecord => {
                self.push_type_error(
                    format!(
                        "field '{}' in struct `{path_str}` has type '{}' but uses an anonymous object body '{{ ... }}' — use a named struct constructor for typed structured values, or declare the field as `Record`/`Map`",
                        field.name,
                        display_type(other_ty)
                    ),
                    Some("use a named struct constructor for typed structured values, or `Record`/`Map` for dynamic data".into()),
                    field.span.clone(),
                );
                return;
            }
            (SparType::InlineRecord, Some(FieldValue::Object(sub_items))) => {
                // Recursively type-check the nested object shape. An unbound
                // struct has no declared shape to check a spread's contents
                // against — same "nothing to compare against" precedent
                // as an unbound top-level struct (check_struct, above).
                let nested_path = format!("{path_str}.{}", field.name);
                let subs: Vec<&FieldDecl> = sub_items
                    .iter()
                    .filter_map(|i| {
                        if let ObjectItem::Field(f) = i {
                            Some(f)
                        } else {
                            None
                        }
                    })
                    .collect();
                self.check_untyped_struct_fields(&subs, &nested_path);
                return;
            }
            (SparType::InlineRecord, None) => {
                self.push_type_error(
                    format!(
                        "required field '{}' in struct `{path_str}` has no value",
                        field.name
                    ),
                    None,
                    field.span.clone(),
                );
                return;
            }
            _ => {}
        }

        // Ordinary field validation. `Option<T>` is the sole absence model,
        // so omitting an Option field is equivalent to initializing it to `none()`.
        if field.value.is_none() {
            return; // Required constructor field, not an uninitialized instance.
        }
        if let Some(FieldValue::Expr(val)) = &field.value {
            self.check_expr_type(val, ty, &field.name, &field.span);
        }
    }

    fn check_type_binding(&mut self, decl: &StructDecl, binding: &TypeBinding, path_str: &str) {
        // If the type name itself doesn't exist, the resolver already
        // reported that — avoid a duplicate error here.
        let Some((type_name, fields)) = self.type_fields_for(&binding.ty) else {
            return;
        };

        // A struct that's ENTIRELY `...Source;` (no other fields) can be
        // checked structurally against the whole bound type — the spread
        // must supply exactly what the type requires. Reuses the same
        // "smart" shape comparison a spread-only nested field gets below.
        if let Some(spread) = spread_only_source(&decl.items) {
            self.check_spread_against_shape(spread, &fields, &type_name, path_str);
            return;
        }

        // A spread MIXED with other explicit fields — each resolvable
        // spread's contribution is merged with the explicit fields for
        // coverage/type checking; an unresolvable spread falls back to
        // skipping entirely (see check_mixed_spread_and_fields).
        let has_spreads = decl
            .items
            .iter()
            .any(|i| matches!(i, ObjectItem::Spread(_)));
        if has_spreads {
            self.check_mixed_spread_and_fields(&decl.items, &fields, &type_name, path_str);
            return;
        }

        let config_fields: Vec<&FieldDecl> = decl
            .items
            .iter()
            .filter_map(|i| {
                if let ObjectItem::Field(f) = i {
                    Some(f)
                } else {
                    None
                }
            })
            .collect();

        self.validate_type_fields(&fields, &config_fields, &type_name, path_str);
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
                None if !type_field_is_omittable(tf) => {
                    self.push_type_error(
                        format!(
                            "struct `{}` is missing required field `{}` (required by type `{}`)",
                            path_str, tf.name, type_name
                        ),
                        None,
                        tf.span.clone(),
                    );
                }
                None => {} // defaulted or Option<T>, fine to omit
                Some(cf) => match &tf.shape {
                    TypeFieldShape::Primitive(expected_ty) => match (&cf.ty, &cf.value) {
                        (Some(actual), _) if actual != expected_ty => {
                            self.push_type_error(
                                format!(
                                    "field `{}::{}` declared as `{}` but type `{}` expects `{}`",
                                    path_str,
                                    tf.name,
                                    display_type(actual),
                                    type_name,
                                    display_type(expected_ty),
                                ),
                                None,
                                cf.span.clone(),
                            );
                        }
                        (Some(_), _) => {}
                        (None, Some(FieldValue::Expr(expr))) => {
                            let scalar_mismatch = match self.infer_type(expr) {
                                Some(actual)
                                    if !matches!(expected_ty, SparType::List(_))
                                        && &actual != expected_ty =>
                                {
                                    Some(actual)
                                }
                                _ => None,
                            };
                            if let Some(actual) = scalar_mismatch {
                                self.push_type_error(
                                        format!(
                                            "field `{}::{}` declared as `{}` but type `{}` expects `{}`",
                                            path_str,
                                            tf.name,
                                            display_type(&actual),
                                            type_name,
                                            display_type(expected_ty),
                                        ),
                                        None,
                                        cf.span.clone(),
                                    );
                            } else {
                                let label = format!("{}::{}", path_str, tf.name);
                                self.check_expr_type(expr, expected_ty, &label, &cf.span);
                            }
                        }
                        (None, _) => {
                            self.push_type_error(
                                    format!(
                                        "field `{}::{}`'s value type could not be determined; type `{}` expects `{}`",
                                        path_str,
                                        tf.name,
                                        type_name,
                                        display_type(expected_ty),
                                    ),
                                    None,
                                    cf.span.clone(),
                                );
                        }
                    },
                    TypeFieldShape::InlineRecord(nested_type_fields) => {
                        self.validate_nested_type_field(
                            cf,
                            nested_type_fields,
                            type_name,
                            path_str,
                            &tf.name,
                        );
                    }
                    TypeFieldShape::Named(other_type_name) => {
                        let expected = SparType::Named(other_type_name.clone());
                        self.validate_field_value_against_type(cf, &expected, path_str, &tf.name);
                    }
                    TypeFieldShape::TypeParameter(expected) => {
                        let expected_ty = SparType::TypeParameter(expected.clone());
                        let actual_ty = cf.ty.clone().or_else(|| match &cf.value {
                            Some(FieldValue::Expr(expression)) => self.infer_type(expression),
                            _ => None,
                        });
                        if actual_ty.as_ref() != Some(&expected_ty) {
                            self.push_type_error(
                                format!(
                                    "field `{}::{}` expects `{}`",
                                    path_str,
                                    tf.name,
                                    display_type(&expected_ty)
                                ),
                                None,
                                cf.span.clone(),
                            );
                        }
                    }
                    TypeFieldShape::Applied { name, arguments } => {
                        let expected = SparType::Applied {
                            name: name.clone(),
                            arguments: arguments.clone(),
                        };
                        self.validate_field_value_against_type(cf, &expected, path_str, &tf.name);
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

    fn validate_field_value_against_type(
        &mut self,
        field: &FieldDecl,
        expected: &SparType,
        path_str: &str,
        field_name: &str,
    ) {
        let label = format!("{}::{}", path_str, field_name);
        if let Some(explicit) = &field.ty {
            if !is_assignable(expected, explicit) {
                self.push_type_error(
                    format!(
                        "field `{label}` declares `{}` but its bound type requires `{}`",
                        display_type(explicit),
                        display_type(expected),
                    ),
                    None,
                    field.span.clone(),
                );
                return;
            }
        }

        match &field.value {
            Some(FieldValue::Expr(expression)) => {
                self.check_expr_type(expression, expected, &label, &field.span);
            }
            Some(FieldValue::Object(items)) => {
                let expression = Expr::Object(items.clone(), field.span.clone());
                self.check_expr_type(&expression, expected, &label, &field.span);
            }
            None => self.push_type_error(
                format!("required field `{label}` has no value"),
                None,
                field.span.clone(),
            ),
        }
    }

    fn validate_field_value_against_type_with_locals(
        &mut self,
        field: &FieldDecl,
        expected: &SparType,
        path_str: &str,
        field_name: &str,
        locals: &HashMap<String, SparType>,
    ) {
        let label = format!("{}::{}", path_str, field_name);
        if let Some(explicit) = &field.ty {
            if !is_assignable(expected, explicit) {
                self.push_type_error(
                    format!(
                        "field `{label}` declares `{}` but its bound type requires `{}`",
                        display_type(explicit),
                        display_type(expected),
                    ),
                    None,
                    field.span.clone(),
                );
                return;
            }
        }

        let expression = match &field.value {
            Some(FieldValue::Expr(expression)) => expression.clone(),
            Some(FieldValue::Object(items)) => Expr::Object(items.clone(), field.span.clone()),
            None => {
                self.push_type_error(
                    format!("required field `{label}` has no value"),
                    None,
                    field.span.clone(),
                );
                return;
            }
        };

        self.expect_call_type(&expression, expected);
        if let Err(error) = self.check_expr_with_locals(&expression, locals) {
            self.errors.push(error);
        }
        match self.validate_literal_expected(
            &expression,
            expected,
            Some(locals),
            &format!("field `{label}`"),
            &field.span,
        ) {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                self.errors.push(error);
                return;
            }
        }
        let actual = self.infer_type_with_locals(&expression, locals);
        if !actual
            .as_ref()
            .is_some_and(|actual| is_assignable(expected, actual))
        {
            self.push_type_error(
                format!(
                    "field `{label}` expects `{}` but value has type `{}`",
                    display_type(expected),
                    actual
                        .as_ref()
                        .map(display_type)
                        .unwrap_or_else(|| "unknown".into()),
                ),
                None,
                field.span.clone(),
            );
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
        // A field carries an inline object payload if its value is FieldValue::Object,
        // regardless of whether its type is explicit or
        // inferred (None, under this binding).
        let explicit_non_section = matches!(&cf.ty, Some(ty) if *ty != SparType::InlineRecord);
        if explicit_non_section {
            self.push_type_error(
                format!(
                    "field `{}::{}` must use a named struct type (type `{}` requires structured data)",
                    path_str, field_name, type_name
                ),
                None,
                cf.span.clone(),
            );
            return;
        }
        let nested_items: &[ObjectItem] = match &cf.value {
            Some(FieldValue::Object(items)) => items,
            _ => {
                self.push_type_error(
                    format!(
                        "field `{}::{}` must have a compatible structured value",
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
        // structural shape check a spread-only bound struct gets above —
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
            .any(|i| matches!(i, ObjectItem::Spread(_)));
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
                if let ObjectItem::Field(f) = i {
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
                None if !type_field_is_omittable(tf) => {
                    self.push_type_error(
                        format!(
                            "struct `{}` is missing required field `{}` (required by type `{}`)",
                            path_str, tf.name, type_name
                        ),
                        None,
                        tf.span.clone(),
                    );
                }
                None => {} // defaulted or Option<T>, fine to omit
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
                    TypeFieldShape::InlineRecord(nested_type_fields) => {
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
                        let expected = SparType::Named(other_type_name.clone());
                        self.validate_field_value_against_type_with_locals(
                            cf, &expected, path_str, &tf.name, locals,
                        );
                    }
                    TypeFieldShape::TypeParameter(expected) => {
                        let expected_ty = SparType::TypeParameter(expected.clone());
                        let actual_ty = cf.ty.clone().or_else(|| match &cf.value {
                            Some(FieldValue::Expr(expression)) => {
                                self.infer_type_with_locals(expression, locals)
                            }
                            _ => None,
                        });
                        if actual_ty.as_ref() != Some(&expected_ty) {
                            self.push_type_error(
                                format!(
                                    "field `{}::{}` expects `{}`",
                                    path_str,
                                    tf.name,
                                    display_type(&expected_ty)
                                ),
                                None,
                                cf.span.clone(),
                            );
                        }
                    }
                    TypeFieldShape::Applied { name, arguments } => {
                        let expected = SparType::Applied {
                            name: name.clone(),
                            arguments: arguments.clone(),
                        };
                        self.validate_field_value_against_type_with_locals(
                            cf, &expected, path_str, &tf.name, locals,
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
        let nested_items: &[ObjectItem] = match &cf.value {
            Some(FieldValue::Object(items)) => items,
            _ => {
                self.push_type_error(
                    format!(
                        "field `{}::{}` must have a compatible structured value",
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
                if let ObjectItem::Field(f) = i {
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
        items: &[ObjectItem],
        expected: &[TypeField],
        expected_label: &str,
        path_str: &str,
    ) {
        let mut covered: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut resolved_spreads: Vec<(&str, Vec<TypeField>, &Span)> = Vec::new();

        for item in items {
            match item {
                ObjectItem::Field(f) => {
                    covered.insert(f.name.clone());
                }
                ObjectItem::Spread(sp) => {
                    let Some(name) = spread_source_name(sp) else {
                        return;
                    }; // unresolvable — skip the whole check
                    let Some(shape) = self.derive_struct_shape(name) else {
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
                if let ObjectItem::Field(f) = i {
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
                        (
                            ShapeKind::InlineRecord(actual_nested),
                            ShapeKind::InlineRecord(want_nested),
                        ) => {
                            self.check_spread_contribution(
                                &actual_nested,
                                &want_nested,
                                source_label,
                                expected_label,
                                span,
                            );
                        }
                        (ShapeKind::Primitive(_), ShapeKind::InlineRecord(_)) => {
                            self.push_type_error(
                                format!(
                                    "spread `...{}` field `{}` is a primitive value but `{}` expects a nested structured value",
                                    source_label, sf.name, expected_label
                                ),
                                None,
                                span.clone(),
                            );
                        }
                        (ShapeKind::InlineRecord(_), ShapeKind::Primitive(want)) => {
                            self.push_type_error(
                                format!(
                                    "spread `...{}` field `{}` is structured data but `{}` expects `{}`",
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

    /// Resolve `spread`'s source struct (same-file, single-segment
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
        let Some(source_shape) = self.derive_struct_shape(source_name) else {
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

    /// The structural shape of a top-level struct: if it's type-bound,
    /// that type's own fields ARE its shape (its instance already has to
    /// satisfy them exactly, via the normal check_type_binding path); if
    /// unbound, every field already has an explicit type (Phase 2's rule),
    /// so derive an equivalent ad-hoc shape straight from those.
    fn derive_struct_shape(&self, name: &str) -> Option<Vec<TypeField>> {
        let entry = self
            .symbols
            .structs
            .get(std::slice::from_ref(&name.to_string()))?;
        match &entry.type_binding {
            Some(ty) => self.type_fields_for(ty).map(|(_, fields)| fields),
            None => Some(self.derive_ad_hoc_shape(&[name.to_string()])),
        }
    }

    /// Recursively build a `Vec<TypeField>` shape from an unbound struct's
    /// own registered fields — nested record shapes are registered separately
    /// under their own path in the resolver,
    /// so a inline-record field recurses into `path + [field_name]`.
    fn derive_ad_hoc_shape(&self, path: &[String]) -> Vec<TypeField> {
        let Some(entry) = self.symbols.structs.get(path) else {
            return Vec::new();
        };
        entry
            .fields
            .iter()
            .map(|(name, fe)| {
                let shape = match &fe.ty {
                    Some(SparType::InlineRecord) => {
                        let nested_path: Vec<String> =
                            path.iter().cloned().chain([name.clone()]).collect();
                        TypeFieldShape::InlineRecord(self.derive_ad_hoc_shape(&nested_path))
                    }
                    Some(other) => TypeFieldShape::Primitive(other.clone()),
                    None => TypeFieldShape::Primitive(SparType::Str), // unreachable: unbound fields always have an explicit type
                };
                TypeField {
                    name: name.clone(),
                    shape,
                    default: None,
                    span: fe.span.clone(),
                }
            })
            .collect()
    }

    /// Structural exact-match comparison between two abstract shapes —
    /// used when spreading `...Source;` into a position with a known
    /// expected shape. Every expected field must be present in `source`
    /// with a matching type (recursively); an omittable/defaulted field may
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
                None if !type_field_is_omittable(ef) => {
                    self.push_type_error(
                        format!(
                            "spread `...{}` in `[{}]` is missing required field `{}` (required by `{}`)",
                            source_label, path_str, ef.name, expected_label
                        ),
                        None,
                        span.clone(),
                    );
                }
                None => {} // defaulted or Option<T>, fine to omit
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
                        (
                            ShapeKind::InlineRecord(actual_nested),
                            ShapeKind::InlineRecord(want_nested),
                        ) => {
                            self.check_shape_matches(
                                &actual_nested,
                                &want_nested,
                                source_label,
                                expected_label,
                                path_str,
                                span,
                            );
                        }
                        (ShapeKind::Primitive(_), ShapeKind::InlineRecord(_)) => {
                            self.push_type_error(
                                format!(
                                    "spread `...{}` field `{}` is a primitive value but `{}` expects a nested structured value",
                                    source_label, ef.name, expected_label
                                ),
                                None,
                                span.clone(),
                            );
                        }
                        (ShapeKind::InlineRecord(_), ShapeKind::Primitive(want)) => {
                            self.push_type_error(
                                format!(
                                    "spread `...{}` field `{}` is structured data but `{}` expects `{}`",
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
            TypeFieldShape::TypeParameter(name) => SparType::TypeParameter(name.clone()),
            TypeFieldShape::Applied { name, arguments } => SparType::Applied {
                name: name.clone(),
                arguments: arguments.clone(),
            },
            TypeFieldShape::InlineRecord(_) => SparType::InlineRecord,
        }
    }

    /// Expand a `TypeFieldShape` into its comparable kind — `Named(X)`
    /// expands to `X`'s own registered fields, same "resolve once, expand"
    /// semantics `SchemaFrom` already uses (loader.rs).
    fn expand_type_field_shape(&self, shape: &TypeFieldShape) -> ShapeKind {
        match shape {
            TypeFieldShape::Primitive(ty) => ShapeKind::Primitive(ty.clone()),
            TypeFieldShape::InlineRecord(fields) => ShapeKind::InlineRecord(fields.clone()),
            TypeFieldShape::Named(name) => match self.symbols.types.get(name) {
                Some(entry) => ShapeKind::InlineRecord(entry.fields.clone()),
                None => ShapeKind::InlineRecord(Vec::new()), // resolver already reported the undefined type
            },
            TypeFieldShape::TypeParameter(name) => {
                ShapeKind::Primitive(SparType::TypeParameter(name.clone()))
            }
            TypeFieldShape::Applied { name, arguments } => self
                .type_fields_for(&SparType::Applied {
                    name: name.clone(),
                    arguments: arguments.clone(),
                })
                .map(|(_, fields)| ShapeKind::InlineRecord(fields))
                .unwrap_or_else(|| ShapeKind::InlineRecord(Vec::new())),
        }
    }

    /// The instantiated parameter list of a function stage in `input |> stage`,
    /// with the piped value as the first parameter.
    fn pipe_stage_parameter_types(
        &self,
        input: &Expr,
        stage: &Expr,
        locals: &HashMap<String, SparType>,
        span: &Span,
    ) -> Result<Vec<(String, SparType)>, SparError> {
        let (name, name_span, mut arguments): (&String, &Span, Vec<CallArg>) = match stage {
            Expr::FnCall(call) => {
                let entry = self
                    .call_entry(&call.name)
                    .ok_or_else(|| SparError::TypeError {
                        message: format!(
                            "structured pipe cannot determine signature for '{}'",
                            call.name
                        ),
                        hint: missing_data_import_hint(&call.name),
                        span: call.span.clone(),
                    })?;
                let arguments = call.args.clone();
                (&call.name, &call.span, arguments)
            }
            Expr::Call {
                name,
                name_span,
                args,
                ..
            } => (name, name_span, args.clone()),
            _ => return Ok(Vec::new()),
        };
        let entry = self.call_entry(name).ok_or_else(|| SparError::TypeError {
            message: format!("structured pipe cannot determine signature for '{name}'"),
            hint: missing_data_import_hint(name),
            span: name_span.clone(),
        })?;
        let input_ty =
            self.infer_type_with_locals(input, locals)
                .ok_or_else(|| SparError::TypeError {
                    message: "cannot determine structured pipe input type".into(),
                    hint: None,
                    span: span.clone(),
                })?;
        let supplied: HashSet<&str> = arguments
            .iter()
            .map(|argument| argument.param_name.as_str())
            .collect();
        let implicit = entry
            .params
            .iter()
            .find(|(param_name, param_ty)| {
                !supplied.contains(param_name.as_str()) && pipe_type_accepts(param_ty, &input_ty)
            })
            .ok_or_else(|| SparError::TypeError {
                message: format!(
                    "structured pipe stage '{name}' has no unsupplied parameter compatible with {}",
                    display_type(&input_ty)
                ),
                hint: None,
                span: span.clone(),
            })?;
        arguments.push(CallArg {
            param_name: implicit.0.clone(),
            param_name_span: span.clone(),
            value: input.clone(),
            span: span.clone(),
        });
        let (_, parameters) =
            self.instantiate_call(name, &[], &arguments, Some(locals), name_span)?;
        Ok(parameters)
    }

    fn structured_pipe_type(
        &self,
        input: &Expr,
        stage: &Expr,
        locals: Option<&HashMap<String, SparType>>,
        span: &Span,
    ) -> Result<SparType, SparError> {
        let infer = |expr: &Expr| match locals {
            Some(locals) => self.infer_type_with_locals(expr, locals),
            None => self.infer_type(expr),
        };
        let input_ty = infer(input).ok_or_else(|| SparError::TypeError {
            message: "cannot determine structured pipe input type".into(),
            hint: None,
            span: span.clone(),
        })?;

        let check_callable = |callable: SparType,
                              extra_args: &[CallArg]|
         -> Result<SparType, SparError> {
            let SparType::Function {
                params,
                return_type,
            } = callable
            else {
                return Err(SparError::TypeError {
                    message: format!(
                        "structured pipe stage is not callable; got {}",
                        display_type(&callable)
                    ),
                    hint: Some(
                        "the right side of `|>` must be a function, closure, or function call"
                            .into(),
                    ),
                    span: span.clone(),
                });
            };

            let mut supplied = HashSet::new();
            for argument in extra_args {
                if !supplied.insert(argument.param_name.as_str()) {
                    return Err(SparError::TypeError {
                        message: format!("duplicate argument '{}'", argument.param_name),
                        hint: None,
                        span: argument.param_name_span.clone(),
                    });
                }
                if !params.iter().any(|param| param.name == argument.param_name) {
                    return Err(SparError::TypeError {
                        message: format!(
                            "structured pipe callable has no parameter named '{}'",
                            argument.param_name
                        ),
                        hint: None,
                        span: argument.param_name_span.clone(),
                    });
                }
            }

            let implicit = params
                .iter()
                .find(|param| {
                    !supplied.contains(param.name.as_str())
                        && pipe_type_accepts(&param.ty, &input_ty)
                })
                .ok_or_else(|| SparError::TypeError {
                    message: format!(
                        "structured pipe stage has no unsupplied parameter compatible with {}",
                        display_type(&input_ty)
                    ),
                    hint: None,
                    span: span.clone(),
                })?;

            for param in &params {
                if param.name == implicit.name {
                    continue;
                }
                if !supplied.contains(param.name.as_str()) {
                    return Err(SparError::TypeError {
                        message: format!(
                            "structured pipe callable is missing required argument '{}'",
                            param.name
                        ),
                        hint: Some(format!("add `{}: ...` to the pipe stage", param.name)),
                        span: span.clone(),
                    });
                }
            }

            for argument in extra_args {
                let expected = params
                    .iter()
                    .find(|param| param.name == argument.param_name)
                    .expect("validated named argument");
                if matches!(&argument.value, Expr::Closure { params, .. } if params.iter().any(|param| param.ty.is_none()))
                {
                    continue;
                }
                let actual = infer(&argument.value).ok_or_else(|| SparError::TypeError {
                    message: "cannot determine structured pipe argument type".into(),
                    hint: None,
                    span: argument.span.clone(),
                })?;
                if !pipe_type_accepts(&expected.ty, &actual) {
                    return Err(SparError::TypeError {
                        message: format!(
                            "structured pipe argument '{}' expects {} but got {}",
                            argument.param_name,
                            display_type(&expected.ty),
                            display_type(&actual)
                        ),
                        hint: None,
                        span: argument.span.clone(),
                    });
                }
            }
            Ok(*return_type)
        };

        match stage {
            Expr::FnCall(call) => {
                if let Some(entry) = self.call_entry(&call.name) {
                    let mut arguments = call.args.clone();
                    let supplied: HashSet<&str> = arguments
                        .iter()
                        .map(|argument| argument.param_name.as_str())
                        .collect();
                    let implicit = entry.params.iter().find(|(param_name, param_ty)| {
                        !supplied.contains(param_name.as_str()) && pipe_type_accepts(param_ty, &input_ty)
                    }).ok_or_else(|| SparError::TypeError {
                        message: format!(
                            "structured pipe stage '{}' has no unsupplied parameter compatible with {}",
                            call.name, display_type(&input_ty)
                        ),
                        hint: None,
                        span: span.clone(),
                    })?;
                    arguments.push(CallArg {
                        param_name: implicit.0.clone(),
                        param_name_span: span.clone(),
                        value: (*input).clone(),
                        span: span.clone(),
                    });
                    let (ret, parameters) = self
                        .instantiate_call(&call.name, &[], &arguments, locals, &call.span)
                        .map_err(|error| match error {
                            SparError::TypeError {
                                message,
                                hint,
                                span: error_span,
                            } => SparError::TypeError {
                                message: format!(
                                    "structured pipe stage '{}': {message}",
                                    call.name
                                ),
                                hint,
                                span: error_span,
                            },
                            other => other,
                        })?;
                    for argument in &arguments {
                        if matches!(&argument.value, Expr::Closure { params, .. } if params.iter().any(|param| param.ty.is_none()))
                        {
                            // Untyped closures are checked against the stage signature.
                            continue;
                        }
                        if let Some((_, expected)) = parameters
                            .iter()
                            .find(|(name, _)| name == &argument.param_name)
                        {
                            let actual =
                                infer(&argument.value).ok_or_else(|| SparError::TypeError {
                                    message: "cannot determine structured pipe argument type"
                                        .into(),
                                    hint: None,
                                    span: argument.span.clone(),
                                })?;
                            if !pipe_type_accepts(expected, &actual) {
                                return Err(SparError::TypeError {
                                    message: format!(
                                        "structured pipe cannot pass {} into parameter '{}' of type {}",
                                        display_type(&actual),
                                        argument.param_name,
                                        display_type(expected)
                                    ),
                                    hint: None,
                                    span: argument.span.clone(),
                                });
                            }
                        }
                    }
                    return Ok(ret);
                }
                let callable = locals
                    .and_then(|locals| locals.get(&call.name).cloned())
                    .or_else(|| {
                        self.infer_namespace_type(&NamespaceRef {
                            segments: vec![call.name.clone()],
                            span: call.span.clone(),
                        })
                    })
                    .ok_or_else(|| SparError::TypeError {
                        message: format!("structured pipe stage '{}' is not callable", call.name),
                        hint: None,
                        span: call.span.clone(),
                    })?;
                check_callable(callable, &call.args)
            }
            Expr::Call {
                name,
                name_span,
                type_arguments,
                args,
                ..
            } => {
                let Some(entry) = self.call_entry(name) else {
                    let callable = locals
                        .and_then(|locals| locals.get(name).cloned())
                        .or_else(|| {
                            self.infer_namespace_type(&NamespaceRef {
                                segments: vec![name.clone()],
                                span: name_span.clone(),
                            })
                        })
                        .ok_or_else(|| SparError::TypeError {
                            message: format!(
                                "structured pipe cannot determine signature for callable '{name}'"
                            ),
                            hint: None,
                            span: name_span.clone(),
                        })?;
                    if !type_arguments.is_empty() {
                        return Err(SparError::TypeError {
                            message: format!(
                                "function-valued callable '{name}' does not accept call-site type arguments"
                            ),
                            hint: None,
                            span: name_span.clone(),
                        });
                    }
                    return check_callable(callable, args);
                };
                let mut arguments = args.clone();
                let supplied: HashSet<&str> = arguments
                    .iter()
                    .map(|argument| argument.param_name.as_str())
                    .collect();
                let implicit = entry.params.iter().find(|(param_name, param_ty)| {
                    !supplied.contains(param_name.as_str()) && pipe_type_accepts(param_ty, &input_ty)
                }).ok_or_else(|| SparError::TypeError {
                    message: format!(
                        "structured pipe stage '{name}' has no unsupplied parameter compatible with {}",
                        display_type(&input_ty)
                    ),
                    hint: None,
                    span: name_span.clone(),
                })?;
                arguments.push(CallArg {
                    param_name: implicit.0.clone(),
                    param_name_span: span.clone(),
                    value: (*input).clone(),
                    span: span.clone(),
                });
                let (ret, parameters) = self
                    .instantiate_call(name, type_arguments, &arguments, locals, name_span)
                    .map_err(|error| match error {
                        SparError::TypeError {
                            message,
                            hint,
                            span: error_span,
                        } => SparError::TypeError {
                            message: format!("structured pipe stage '{name}': {message}"),
                            hint,
                            span: error_span,
                        },
                        other => other,
                    })?;
                for argument in &arguments {
                    if matches!(&argument.value, Expr::Closure { params, .. } if params.iter().any(|param| param.ty.is_none()))
                    {
                        // Untyped closures are checked against the stage signature.
                        continue;
                    }
                    if let Some((_, expected)) = parameters
                        .iter()
                        .find(|(param, _)| param == &argument.param_name)
                    {
                        let actual =
                            infer(&argument.value).ok_or_else(|| SparError::TypeError {
                                message: "cannot determine structured pipe argument type".into(),
                                hint: None,
                                span: argument.span.clone(),
                            })?;
                        if !pipe_type_accepts(expected, &actual) {
                            return Err(SparError::TypeError {
                                message: format!(
                                    "structured pipe cannot pass {} into parameter '{}' of type {}",
                                    display_type(&actual),
                                    argument.param_name,
                                    display_type(expected)
                                ),
                                hint: None,
                                span: argument.span.clone(),
                            });
                        }
                    }
                }
                Ok(ret)
            }
            Expr::NamespaceRef(reference) if reference.segments.len() == 1 => {
                let callable = locals
                    .and_then(|locals| locals.get(&reference.segments[0]).cloned())
                    .or_else(|| self.infer_namespace_type(reference))
                    .ok_or_else(|| SparError::TypeError {
                        message: format!(
                            "structured pipe stage '{}' is not callable",
                            reference.segments[0]
                        ),
                        hint: None,
                        span: reference.span.clone(),
                    })?;
                check_callable(callable, &[])
            }
            Expr::Closure { .. } => {
                let callable = infer(stage).ok_or_else(|| SparError::TypeError {
                    message: "structured pipe closure needs enough type information to determine its callable signature".into(),
                    hint: Some("annotate the closure parameter and return type".into()),
                    span: span.clone(),
                })?;
                check_callable(callable, &[])
            }
            other => {
                let callable = infer(other).ok_or_else(|| SparError::TypeError {
                    message: "structured pipe stage is not callable".into(),
                    hint: Some(
                        "the right side of `|>` must be a function, closure, or function call"
                            .into(),
                    ),
                    span: span.clone(),
                })?;
                check_callable(callable, &[])
            }
        }
    }

    fn instantiate_named_fn_call(
        &self,
        call: &FnCall,
        locals: Option<&HashMap<String, SparType>>,
    ) -> Result<Option<SparType>, SparError> {
        let builtin = match call.name.as_str() {
            "env" => Some(("name", SparType::Str, SparType::Str)),
            "str" => Some(("value", SparType::Any, SparType::Str)),
            "int" => Some(("value", SparType::Any, SparType::Int)),
            "float" => Some(("value", SparType::Any, SparType::Float)),
            "bool" => Some(("value", SparType::Any, SparType::Bool)),
            _ => None,
        };
        if let Some((parameter_name, parameter_type, return_type)) = builtin {
            let parameters = vec![(parameter_name.to_string(), parameter_type.clone())];
            self.validate_named_argument_shape(
                &format!("builtin function '{}'", call.name),
                &call.args,
                &parameters,
                |_| true,
                &call.span,
            )?;
            if parameter_type != SparType::Any {
                let argument = call
                    .args
                    .iter()
                    .find(|argument| argument.param_name == parameter_name)
                    .expect("named argument shape was validated");
                let actual = match locals {
                    Some(locals) => self.infer_type_with_locals(&argument.value, locals),
                    None => self.infer_type(&argument.value),
                };
                if let Some(actual) = actual {
                    if !is_assignable(&parameter_type, &actual) {
                        return Err(SparError::TypeError {
                            message: format!(
                                "argument '{}' to builtin '{}' expects {} but got {}",
                                parameter_name,
                                call.name,
                                display_type(&parameter_type),
                                display_type(&actual),
                            ),
                            hint: None,
                            span: argument.span.clone(),
                        });
                    }
                }
            }
            return Ok(Some(return_type));
        }
        if self.constructor_parameters(&call.name).is_some() {
            let (ret, _) =
                self.instantiate_call(&call.name, &[], &call.args, locals, &call.span)?;
            return Ok(Some(ret));
        }
        if self.call_entry(&call.name).is_none() {
            return Ok(None);
        }
        let (ret, _) = self.instantiate_call(&call.name, &[], &call.args, locals, &call.span)?;
        Ok(Some(ret))
    }

    fn infer_type(&self, expr: &Expr) -> Option<SparType> {
        let ty = self.infer_type_impl(expr);
        self.record_type(expr, &ty);
        self.record_receiver(expr, None);
        ty
    }

    /// For `recv.member` / `recv.method(..)` records the receiver's type at the `.` (the member
    /// expression's span starts there). Editor tooling looks members up by dot offset because the
    /// AST spans of these nodes do not cover the receiver. Fields of a dynamic `Record` are typed
    /// `Any` so `.asStr()` and friends resolve.
    fn record_receiver(&self, expr: &Expr, locals: Option<&HashMap<String, SparType>>) {
        if !self.record_types {
            return;
        }
        let (receiver, span) = match expr {
            Expr::FieldAccess { base, span, .. } => (base.as_ref(), span),
            Expr::MethodCall { receiver, span, .. } => (receiver.as_ref(), span),
            _ => return,
        };
        if let Some(ty) = self.tooling_receiver_type(receiver, locals, 0) {
            self.receiver_map.borrow_mut().push(TypedSpan {
                start: span.start,
                end: span.start + 1,
                ty,
            });
        }
    }

    fn tooling_receiver_type(
        &self,
        expr: &Expr,
        locals: Option<&HashMap<String, SparType>>,
        depth: usize,
    ) -> Option<SparType> {
        if depth > 12 {
            return None;
        }
        let direct = match locals {
            Some(locals) => self.infer_type_with_locals_impl(expr, locals),
            None => self.infer_type_impl(expr),
        };
        if direct.is_some() {
            return direct;
        }
        if let Expr::FieldAccess { base, .. } = expr {
            if matches!(
                self.tooling_receiver_type(base, locals, depth + 1),
                Some(SparType::InlineRecord | SparType::Any)
            ) {
                return Some(SparType::Any);
            }
        }
        None
    }

    fn record_type(&self, expr: &Expr, ty: &Option<SparType>) {
        if !self.record_types {
            return;
        }
        let (Some(ty), Some(span)) = (ty, expr_span_of(expr)) else {
            return;
        };
        if span.end > span.start {
            self.type_map.borrow_mut().push(TypedSpan {
                start: span.start,
                end: span.end,
                ty: ty.clone(),
            });
        }
    }

    fn infer_type_impl(&self, expr: &Expr) -> Option<SparType> {
        match expr {
            Expr::Object(_, _) => None, // shape only checkable against an expected type — see check_expr_type (Task 4)
            Expr::Literal(Literal::Int(_)) => Some(SparType::Int),
            Expr::Literal(Literal::Float(_)) => Some(SparType::Float),
            Expr::Literal(Literal::Bool(_)) => Some(SparType::Bool),
            Expr::String(_) => Some(SparType::Str),
            Expr::Tuple(items, _) => items
                .iter()
                .map(|item| self.infer_type(item))
                .collect::<Option<Vec<_>>>()
                .map(SparType::Tuple),
            Expr::TupleField { base, index, .. } => match self.infer_type(base)? {
                SparType::Tuple(items) => items.get(*index).cloned(),
                _ => None,
            },
            Expr::List(items, _) => items
                .first()
                .and_then(|e| self.infer_type(e))
                .map(|t| SparType::List(Box::new(t))),
            Expr::NamespaceRef(nr) => self.infer_namespace_type(nr),
            Expr::FieldAccess { base, field, .. } => self.infer_field_access(base, field),
            Expr::MethodCall {
                receiver,
                method,
                type_arguments,
                args,
                ..
            } => self.infer_method_call(receiver, method, type_arguments, args, None).ok(),
            Expr::StructuredPipe { input, stage, span } => {
                self.structured_pipe_type(input, stage, None, span).ok()
            }
            Expr::FnCall(fc) => match fc.name.as_str() {
                "env" | "str" => Some(SparType::Str),
                "int" => Some(SparType::Int),
                "float" => Some(SparType::Float),
                "bool" => Some(SparType::Bool),
                _ => {
                    if let Ok(Some(ret)) = self.instantiate_named_fn_call(fc, None) {
                        return Some(ret);
                    }
                    let reference = NamespaceRef {
                        segments: vec![fc.name.clone()],
                        span: fc.span.clone(),
                    };
                    match self.infer_namespace_type(&reference)? {
                        SparType::Function { return_type, .. } => Some(*return_type),
                        _ => None,
                    }
                }
            },
            Expr::BinaryOp(op) => {
                let lhs = self.infer_type(&op.lhs)?;
                let rhs = self.infer_type(&op.rhs)?;
                self.infer_binop_type(&op.op, &lhs, &rhs)
            }
            Expr::Grouped(inner, _) => self.infer_type(inner),
            Expr::Call {
                name,
                name_span,
                type_arguments,
                args,
                ..
            } => self
                .instantiate_call(name, type_arguments, args, None, name_span)
                .ok()
                .map(|(ret, _)| ret),
            Expr::Closure {
                params,
                return_type,
                body,
                ..
            } => {
                let mut closure_locals = HashMap::new();
                let params = params
                    .iter()
                    .map(|param| {
                        let ty = param.ty.clone()?;
                        closure_locals.insert(param.name.clone(), ty.clone());
                        Some(CallableParamType {
                            name: param.name.clone(),
                            ty,
                        })
                    })
                    .collect::<Option<Vec<_>>>()?;
                let result = return_type.clone().or_else(|| match body {
                    ClosureBody::Expr(value) => self.infer_type_with_locals(value, &closure_locals),
                    ClosureBody::Block(_) => None,
                })?;
                Some(SparType::Function {
                    params,
                    return_type: Box::new(result),
                })
            }
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
            Expr::Await { value, .. } => self
                .infer_type(value)
                .and_then(|ty| promise_inner(&ty).cloned()),
            Expr::Index { source, .. } => match self.infer_type(source)? {
                SparType::List(elem) => Some(*elem),
                SparType::Named(name) if name == "Bytes" => Some(SparType::Int),
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
            Expr::Shell(_) => Some(SparType::Shell),
            Expr::ExecShell(_) => Some(SparType::Named("ExecResult".to_string())),
            Expr::CommandSubstitution(_) => Some(SparType::Str),
        }
    }

    fn infer_namespace_type(&self, nr: &NamespaceRef) -> Option<SparType> {
        match nr.segments.as_slice() {
            [name] if name == "_" => self.lookup_global_type(name),
            [name] if name == "status" => Some(SparType::Named("ProcessStatus".into())),
            [name] if name == "lastJob" => Some(SparType::Named("Job".into())),
            [name] => self.lookup_global_type(name).or_else(|| {
                let entry = self
                    .symbols
                    .functions
                    .get(name)
                    .or_else(|| self.symbols.imported_functions.get(name))?;
                if !entry.type_parameters.is_empty() {
                    return None;
                }
                Some(SparType::Function {
                    params: entry
                        .params
                        .iter()
                        .map(|(name, ty)| CallableParamType {
                            name: name.clone(),
                            ty: ty.clone(),
                        })
                        .collect(),
                    return_type: Box::new(callable_return_type(entry)),
                })
            }),
            [ns, _name] if self.symbols.enums.contains_key(ns.as_str()) => {
                Some(SparType::Named(ns.clone()))
            }
            _ => None,
        }
    }

    fn infer_field_access(&self, base: &Expr, field: &str) -> Option<SparType> {
        if let Expr::NamespaceRef(nr) = base {
            if nr.segments == ["self"] {
                let section_path = self.current_struct.as_ref()?;
                return self
                    .symbols
                    .lookup_struct(section_path)
                    .and_then(|s| s.fields.get(field))
                    .and_then(|f| f.ty.clone());
            }
            if nr.segments == ["global"] {
                return self.lookup_global_type(field);
            }
            // base names a registered struct directly (e.g. an imported
            // `[Colors]{...}` spliced in as a local section) — resolve the
            // field straight off that struct rather than falling through
            // to `infer_type`, which only knows about `SparType::Named`
            // type instances, not struct namespaces.
            if let Some(section) = self.symbols.lookup_struct(&nr.segments) {
                if let Some(ty) = section.fields.get(field).and_then(|f| f.ty.clone()) {
                    return Some(ty);
                }
                if let Some(binding) = &section.type_binding {
                    return self
                        .type_fields_for(binding)
                        .and_then(|(_, fields)| {
                            fields.into_iter().find(|candidate| candidate.name == field)
                        })
                        .map(|field| self.field_shape_to_type(&field.shape));
                }
                return None;
            }
        }
        let base_ty = self.infer_type(base)?;
        self.infer_field_access_from_type(&base_ty, field)
    }

    fn method_owner_for_receiver(
        &self,
        receiver: &Expr,
        locals: Option<&HashMap<String, SparType>>,
    ) -> Option<String> {
        if let Expr::NamespaceRef(reference) = receiver {
            if reference.segments.len() == 1 {
                let name = &reference.segments[0];
                if locals.and_then(|locals| locals.get(name)).is_none()
                    && self.symbols.lookup_struct(&reference.segments).is_some()
                {
                    return Some(name.clone());
                }
            }
        }
        let ty = match locals {
            Some(locals) => self.infer_type_with_locals(receiver, locals),
            None => self.infer_type(receiver),
        }?;
        match ty {
            SparType::Named(name) => Some(name),
            SparType::Applied { name, .. } => Some(name),
            SparType::Str => Some("str".into()),
            SparType::Int => Some("int".into()),
            SparType::Float => Some("float".into()),
            SparType::Bool => Some("bool".into()),
            SparType::List(_) => Some("List".into()),
            SparType::InlineRecord => Some("Record".into()),
            SparType::Void => None,
            _ => Some("Any".into()),
        }
    }

    fn infer_method_call(
        &self,
        receiver: &Expr,
        method: &str,
        type_arguments: &[SparType],
        args: &[CallArg],
        locals: Option<&HashMap<String, SparType>>,
    ) -> Result<SparType, SparError> {
        let owner = self
            .method_owner_for_receiver(receiver, locals)
            .ok_or_else(|| SparError::TypeError {
                message: format!("cannot resolve method '{method}' for receiver"),
                hint: None,
                span: receiver.span().cloned().unwrap_or_else(Span::dummy),
            })?;
        let entry =
            self.symbols
                .lookup_method(&owner, method)
                .ok_or_else(|| SparError::TypeError {
                    message: format!("struct '{owner}' has no method '{method}'"),
                    hint: None,
                    span: receiver.span().cloned().unwrap_or_else(Span::dummy),
                })?;
        if entry.function.is_private && self.current_impl.as_deref() != Some(owner.as_str()) {
            return Err(SparError::TypeError {
                message: format!("method '{method}' is private to struct '{owner}'"),
                hint: None,
                span: receiver.span().cloned().unwrap_or_else(Span::dummy),
            });
        }
        let mut substitution = TypeSubstitution::new();
        bind_method_type_arguments(method, &entry.function, type_arguments, &mut substitution, receiver.span().unwrap_or(&entry.function.span))?;
        if entry.has_receiver {
            let actual_receiver = match locals {
                Some(locals) => self.infer_type_with_locals(receiver, locals),
                None => self.infer_type(receiver),
            };
            if let (Some(actual_receiver), Some((_, expected_receiver))) =
                (actual_receiver, entry.function.params.first())
            {
                let receiver_span = receiver.span().cloned().unwrap_or_else(Span::dummy);
                unify_generic(
                    expected_receiver,
                    &actual_receiver,
                    &mut substitution,
                    &receiver_span,
                )?;
            }
        }
        let params = if entry.has_receiver {
            &entry.function.params[1..]
        } else {
            &entry.function.params[..]
        };
        let mut seen = HashSet::new();
        for argument in args {
            if !seen.insert(argument.param_name.as_str()) {
                return Err(SparError::TypeError {
                    message: format!("duplicate argument '{}'", argument.param_name),
                    hint: None,
                    span: argument.param_name_span.clone(),
                });
            }
            let Some((_, pattern)) = params.iter().find(|(name, _)| name == &argument.param_name)
            else {
                return Err(SparError::TypeError {
                    message: format!(
                        "method '{method}' has no parameter named '{}'",
                        argument.param_name
                    ),
                    hint: None,
                    span: argument.param_name_span.clone(),
                });
            };
            let actual = match locals {
                Some(locals) => self.infer_type_with_locals(&argument.value, locals),
                None => self.infer_type(&argument.value),
            }
            .or_else(|| {
                self.infer_closure_signature_for_pattern(
                    &argument.value,
                    pattern,
                    &substitution,
                    locals,
                )
            });
            if let Some(actual) = actual {
                unify_generic(pattern, &actual, &mut substitution, &argument.span)?;
            }
        }
        let required = params
            .iter()
            .filter(|(name, _)| !entry.function.default_params.contains(name))
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        if let Some(missing) = required
            .into_iter()
            .find(|name| !seen.contains(name.as_str()))
        {
            return Err(SparError::TypeError {
                message: format!("method '{method}' is missing required argument '{missing}'"),
                hint: None,
                span: receiver.span().cloned().unwrap_or_else(Span::dummy),
            });
        }
        let eventual_return = substitute_type(&entry.function.ret, &substitution);
        // Mirror instantiate_named_fn_call's handling of a plain async
        // function call: an async method's declared return type is its
        // *resolved* type, not the Promise<T> a caller actually gets back —
        // `await`ing it needs that wrapped here, the same as any other
        // async call. Without this, `await receiver.asyncMethod()` failed
        // to typecheck at all ("cannot await `T`; expected `Promise<T>`").
        Ok(if entry.function.is_async {
            promise_type(eventual_return)
        } else {
            eventual_return
        })
    }

    fn infer_field_access_from_type(&self, base_ty: &SparType, field: &str) -> Option<SparType> {
        match base_ty {
            SparType::Error if matches!(field, "message" | "kind") => Some(SparType::Str),
            // A field of a dynamic Record is itself dynamic; `asStr()`,
            // `asInt()`, ... bridge it to a static type.
            SparType::Named(type_name) if type_name == "Record" => Some(base_ty.clone()),
            SparType::Named(type_name) => {
                let path = vec![type_name.clone()];
                if let Some(section) = self.symbols.lookup_struct(&path) {
                    if let Some(entry) = section.fields.get(field) {
                        if let Some(ty) = &entry.ty {
                            return Some(ty.clone());
                        }
                    }
                    if let Some(binding) = &section.type_binding {
                        return self
                            .type_fields_for(binding)
                            .and_then(|(_, fields)| {
                                fields.into_iter().find(|candidate| candidate.name == field)
                            })
                            .map(|field| self.field_shape_to_type(&field.shape));
                    }
                }
                self.symbols
                    .types
                    .get(type_name)
                    .and_then(|te| te.fields.iter().find(|f| f.name == field))
                    .map(|f| self.field_shape_to_type(&f.shape))
            }
            applied @ SparType::Applied { .. } => self
                .type_fields_for(applied)
                .and_then(|(_, fields)| {
                    fields.into_iter().find(|candidate| candidate.name == field)
                })
                .map(|field| self.field_shape_to_type(&field.shape)),
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
                .map(callable_return_type)
                .or_else(|| {
                    self.symbols
                        .hosts
                        .get(&(segments[0].to_string(), segments[1].to_string()))
                        .map(|host_fn| host_fn.ret.clone())
                })
                .or_else(|| {
                    self.symbols
                        .natives
                        .get(&(segments[0].to_string(), segments[1].to_string()))
                        .map(|native_fn| native_fn.ret.clone())
                }),
            1 => self.symbols.functions.get(name).map(callable_return_type),
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
                (SparType::Shell, SparType::Shell) => Some(SparType::Shell),
                _ => None,
            },
            BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => match (lhs, rhs) {
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

    fn validate_object_literal_expected(
        &self,
        expr: &Expr,
        expected: &SparType,
        locals: Option<&HashMap<String, SparType>>,
        label: &str,
        span: &Span,
    ) -> Result<(), SparError> {
        let Expr::Object(items, _) = expr else {
            return Ok(());
        };
        match expected {
            SparType::Named(name) if name == "Record" => Ok(()),
            // `Any` accepts everything by definition, including a dynamic
            // object literal — this was falling through to the generic
            // "object literals are only valid for Record or Map" rejection
            // below instead.
            SparType::Any => Ok(()),
            SparType::Applied { name, arguments } if name == "Map" && arguments.len() == 2 => {
                let key_type = &arguments[0];
                let value_type = &arguments[1];
                // An empty `{}` has no actual keys to be the wrong type —
                // don't reject `Map<int, V> = {};` just because object
                // literals are otherwise str-keyed sugar.
                if !items.is_empty() && !is_assignable(key_type, &SparType::Str) {
                    return Err(SparError::TypeError {
                        message: format!(
                            "`{label}` uses `{{ ... }}` Map syntax, whose keys are `str`, but the declared key type is `{}`",
                            display_type(key_type),
                        ),
                        hint: Some("use `Map<str, V>` for object-literal Map syntax".into()),
                        span: span.clone(),
                    });
                }
                let map_type = SparType::Applied {
                    name: "Map".into(),
                    arguments: arguments.clone(),
                };
                for item in items {
                    match item {
                        ObjectItem::Spread(spread) => {
                            let actual = match locals {
                                Some(locals) => self.infer_type_with_locals(&spread.expr, locals),
                                None => self.infer_type(&spread.expr),
                            };
                            if !actual
                                .as_ref()
                                .is_some_and(|actual| is_assignable(&map_type, actual))
                            {
                                return Err(SparError::TypeError {
                                    message: format!(
                                        "Map spread in `{label}` expects `{}` but found `{}`",
                                        display_type(&map_type),
                                        actual
                                            .as_ref()
                                            .map(display_type)
                                            .unwrap_or_else(|| "unknown".into()),
                                    ),
                                    hint: Some("spread another Map with the same key/value types".into()),
                                    span: spread.span.clone(),
                                });
                            }
                        }
                        ObjectItem::Field(field) => {
                            let Some(field_value) = &field.value else {
                                return Err(SparError::TypeError {
                                    message: format!("Map entry '{}' has no value", field.name),
                                    hint: None,
                                    span: field.span.clone(),
                                });
                            };
                            let owned_expr;
                            let value_expr = match field_value {
                                FieldValue::Expr(expr) => expr,
                                FieldValue::Object(nested) => {
                                    owned_expr = Expr::Object(nested.clone(), field.span.clone());
                                    &owned_expr
                                }
                            };
                            if matches!(value_expr, Expr::Object(_, _)) {
                                self.validate_object_literal_expected(
                                    value_expr,
                                    value_type,
                                    locals,
                                    &format!("{label}.{}", field.name),
                                    &field.span,
                                )?;
                                continue;
                            }
                            let actual = match locals {
                                Some(locals) => self.infer_type_with_locals(value_expr, locals),
                                None => self.infer_type(value_expr),
                            };
                            if !actual
                                .as_ref()
                                .is_some_and(|actual| is_assignable(value_type, actual) || lookup_accepts(value_type, Some(actual)))
                            {
                                return Err(SparError::TypeError {
                                    message: format!(
                                        "Map entry '{}.{}' expects `{}` but found `{}`",
                                        label,
                                        field.name,
                                        display_type(value_type),
                                        actual
                                            .as_ref()
                                            .map(display_type)
                                            .unwrap_or_else(|| "unknown".into()),
                                    ),
                                    hint: None,
                                    span: field.span.clone(),
                                });
                            }
                        }
                    }
                }
                Ok(())
            }
            SparType::Named(name) => Err(SparError::TypeError {
                message: format!(
                    "`{label}` expects `{name}`, but `{{ ... }}` is a dynamic object literal — use the named constructor `{name}(...)`"
                ),
                hint: Some(format!(
                    "construct `{name}` explicitly with `{name}(field: value, ...)`"
                )),
                span: span.clone(),
            }),
            SparType::Applied { .. } => {
                let expected_name = display_type(expected);
                Err(SparError::TypeError {
                    message: format!(
                        "`{label}` expects `{expected_name}`, but `{{ ... }}` is a dynamic object literal — typed structured values require a named constructor"
                    ),
                    hint: Some(format!(
                        "construct `{expected_name}` explicitly instead of using `{{ ... }}`"
                    )),
                    span: span.clone(),
                })
            }
            _ => Err(SparError::TypeError {
                message: format!(
                    "`{label}` has type `{}` but value is a dynamic object literal `{{ ... }}` — object literals are only valid for `Record` or `Map` values",
                    display_type(expected)
                ),
                hint: None,
                span: span.clone(),
            }),
        }
    }

    fn validate_literal_expected(
        &self,
        expr: &Expr,
        expected: &SparType,
        locals: Option<&HashMap<String, SparType>>,
        label: &str,
        span: &Span,
    ) -> Result<bool, SparError> {
        if matches!(expr, Expr::Object(_, _)) {
            self.validate_object_literal_expected(expr, expected, locals, label, span)?;
            return Ok(true);
        }
        if let (Expr::Tuple(items, _), SparType::Tuple(element_types)) = (expr, expected) {
            if items.len() != element_types.len() {
                return Err(SparError::TypeError {
                    message: format!(
                        "`{label}` expects {} tuple elements but found {}",
                        element_types.len(),
                        items.len()
                    ),
                    hint: None,
                    span: span.clone(),
                });
            }
            for (index, (item, element_type)) in items.iter().zip(element_types).enumerate() {
                let item_label = format!("{label}.{index}");
                if self.validate_literal_expected(item, element_type, locals, &item_label, span)? {
                    continue;
                }
                let actual = match locals {
                    Some(locals) => self.infer_type_with_locals(item, locals),
                    None => self.infer_type(item),
                };
                if !actual
                    .as_ref()
                    .is_some_and(|actual| is_assignable(element_type, actual))
                {
                    return Err(SparError::TypeError {
                        message: format!(
                            "`{item_label}` expects `{}` but found `{}`",
                            display_type(element_type),
                            actual
                                .as_ref()
                                .map(display_type)
                                .unwrap_or_else(|| "unknown".into())
                        ),
                        hint: None,
                        span: span.clone(),
                    });
                }
            }
            return Ok(true);
        }
        if let (Expr::List(items, _), SparType::List(element_type)) = (expr, expected) {
            for (index, item) in items.iter().enumerate() {
                let item_label = format!("{label}[{index}]");
                if self.validate_literal_expected(item, element_type, locals, &item_label, span)? {
                    continue;
                }
                let actual = match locals {
                    Some(locals) => self.infer_type_with_locals(item, locals),
                    None => self.infer_type(item),
                };
                if !actual.as_ref().is_some_and(|actual| {
                    is_assignable(element_type, actual)
                        || lookup_accepts(element_type, Some(actual))
                }) {
                    return Err(SparError::TypeError {
                        message: format!(
                            "`{item_label}` expects `{}` but found `{}`",
                            display_type(element_type),
                            actual
                                .as_ref()
                                .map(display_type)
                                .unwrap_or_else(|| "unknown".into()),
                        ),
                        hint: None,
                        span: span.clone(),
                    });
                }
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// Validates object/list literals against their expected dynamic data type.
    /// Named structured values are never implicitly constructed from `{ ... }`.
    fn check_expr_type(&mut self, expr: &Expr, declared_ty: &SparType, label: &str, span: &Span) {
        self.expect_call_type(expr, declared_ty);
        if matches!(declared_ty, SparType::Function { .. }) && matches!(expr, Expr::Closure { .. })
        {
            if let Err(error) =
                self.check_closure_against_expected(expr, declared_ty, &HashMap::new(), false)
            {
                self.errors.push(error);
            }
            return;
        }
        self.check_expr_internal(expr);

        match self.validate_literal_expected(expr, declared_ty, None, label, span) {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                self.errors.push(error);
                return;
            }
        }

        let inferred = match self.infer_type(expr) {
            Some(t) => t,
            None => return,
        };

        if !is_assignable(declared_ty, &inferred) {
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
        if self.record_types {
            let _ = self.infer_type(expr);
        }
        match expr {
            Expr::Object(items, _) => {
                for item in items {
                    match item {
                        ObjectItem::Field(f) => {
                            if let Some(FieldValue::Expr(e)) = &f.value {
                                self.check_expr_internal(e);
                            }
                        }
                        ObjectItem::Spread(sp) => self.check_expr_internal(&sp.expr),
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
                                | (SparType::Shell, SparType::Shell)
                        ),
                        BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => matches!(
                            (l, r),
                            (SparType::Int, SparType::Int) | (SparType::Float, SparType::Float)
                        ),
                        BinOp::Fallback => l == r,
                        BinOp::Eq | BinOp::NotEq => {
                            (l == r && supports_value_equality(l))
                                || (matches!(l, SparType::Named(name) if name == "Record")
                                    && matches!(r, SparType::Int | SparType::Float | SparType::Str | SparType::Bool))
                                || (matches!(r, SparType::Named(name) if name == "Record")
                                    && matches!(l, SparType::Int | SparType::Float | SparType::Str | SparType::Bool))
                        }
                        BinOp::Lt
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
                            BinOp::Rem => "%",
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
                    self.check_expr_internal(&arg.value);
                }
                if let Err(error) = self.instantiate_named_fn_call(fc, None) {
                    self.errors.push(error);
                }
            }
            Expr::String(s) => {
                for part in &s.parts {
                    if let StringPart::Expr(e) = part {
                        self.check_expr_internal(e);
                    }
                }
            }
            Expr::List(items, _) | Expr::Tuple(items, _) => {
                for item in items {
                    self.check_expr_internal(item);
                }
            }
            Expr::Grouped(inner, _) => self.check_expr_internal(inner),
            Expr::TupleField {
                base,
                index,
                index_span,
                ..
            } => {
                self.check_expr_internal(base);
                match self.infer_type(base) {
                    Some(SparType::Tuple(items)) if *index < items.len() => {}
                    Some(SparType::Tuple(items)) => self.push_type_error(
                        format!(
                            "tuple index {index} out of bounds for {} elements",
                            items.len()
                        ),
                        None,
                        index_span.clone(),
                    ),
                    Some(other) => self.push_type_error(
                        format!("cannot use tuple access on `{}`", display_type(&other)),
                        None,
                        index_span.clone(),
                    ),
                    None => {}
                }
            }
            Expr::Call { args, .. } => {
                for arg in args {
                    self.check_expr_internal(&arg.value);
                }
                let result = self.check_call(expr);
                if let Err(e) = result {
                    self.errors.push(e);
                }
            }
            Expr::Closure { body, .. } => {
                if let ClosureBody::Expr(value) = body {
                    self.check_expr_internal(value);
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
                                        .map(display_type)
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
                                        .map(display_type)
                                        .unwrap_or_else(|| "unknown".into()),
                                ),
                                None,
                                span.clone(),
                            );
                        }
                    }
                }
            }
            Expr::Await { value, span } => {
                self.check_expr_internal(value);
                if self.symbols.top_level_await {
                    // Declarations are re-evaluated whenever the session
                    // replays, so an awaited value must not be stored in one.
                    self.push_type_error(
                        "`await` in a declaration is not supported at the prompt",
                        Some(
                            "await the value as its own line (`await get(url: ...)`), then use `_`"
                                .into(),
                        ),
                        span.clone(),
                    );
                } else {
                    self.push_type_error(
                        "`await` is only valid inside an async function",
                        Some("move `await` into an `async function`".into()),
                        span.clone(),
                    );
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
                                .map(display_type)
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
                                .map(display_type)
                                .unwrap_or_else(|| "unknown".into()),
                        ),
                        None,
                        span.clone(),
                    );
                }
            }
            Expr::Literal(_) => {}
            Expr::NamespaceRef(_) => {}
            Expr::MethodCall { receiver, args, .. } => {
                self.check_expr_internal(receiver);
                for argument in args {
                    self.check_expr_internal(&argument.value);
                }
            }
            Expr::StructuredPipe { input, stage, span } => {
                self.check_expr_internal(input);
                match stage.as_ref() {
                    Expr::FnCall(call) => {
                        for argument in &call.args {
                            self.check_expr_internal(&argument.value);
                        }
                    }
                    Expr::Call { args, .. } => {
                        for argument in args {
                            self.check_expr_internal(&argument.value);
                        }
                    }
                    other => self.check_expr_internal(other),
                }
                if let Err(error) = self.structured_pipe_type(input, stage, None, span) {
                    self.errors.push(error);
                }
            }
            Expr::FieldAccess { base, .. } => self.check_expr_internal(base),
            Expr::Shell(shell) | Expr::CommandSubstitution(shell) => {
                if let Err(error) =
                    self.check_mixed_shell_with_locals(shell, &HashMap::new(), false)
                {
                    self.errors.push(error);
                }
            }
            Expr::ExecShell(shell) => {
                if shell_contains_mixed_pipeline(shell) {
                    self.push_type_error(
                        "`exec` does not support structured mixed pipelines; run the pipeline as a `~` statement inside a function returning `ShellResult<T, E>` instead",
                        None,
                        shell.span.clone(),
                    );
                }
            }
        }
    }

    // ── Call argument type checking ───────────────────────────────────────────

    fn constructor_parameters(&self, name: &str) -> Option<Vec<(String, SparType)>> {
        let path = vec![name.to_string()];
        let section = self.symbols.lookup_struct(&path)?;
        let mut parameters = Vec::new();
        for (field_name, field) in &section.fields {
            let ty = field.ty.clone().or_else(|| {
                section.type_binding.as_ref().and_then(|binding| {
                    self.type_fields_for(binding).and_then(|(_, fields)| {
                        fields
                            .into_iter()
                            .find(|candidate| candidate.name == *field_name)
                            .map(|candidate| self.field_shape_to_type(&candidate.shape))
                    })
                })
            })?;
            parameters.push((field_name.clone(), ty));
        }
        Some(parameters)
    }

    fn call_entry(&self, name: &str) -> Option<&FunctionEntry> {
        let segments: Vec<&str> = name.split("::").collect();
        match segments.as_slice() {
            [function] => self.symbols.functions.get(*function),
            [group, function] => self
                .symbols
                .function_groups
                .get(*group)
                .and_then(|entry| entry.functions.get(*function))
                .or_else(|| self.symbols.imported_functions.get(name)),
            _ => None,
        }
    }

    fn infer_closure_signature_for_pattern(
        &self,
        expression: &Expr,
        pattern: &SparType,
        substitution: &TypeSubstitution,
        outer_locals: Option<&HashMap<String, SparType>>,
    ) -> Option<SparType> {
        let Expr::Closure {
            params,
            return_type,
            body,
            ..
        } = expression
        else {
            return None;
        };
        let substituted = substitute_type(pattern, substitution);
        let SparType::Function {
            params: expected_params,
            ..
        } = substituted
        else {
            return None;
        };
        if params.len() != expected_params.len() {
            return None;
        }

        let mut locals = outer_locals.cloned().unwrap_or_default();
        let mut actual_params = Vec::with_capacity(params.len());
        for (param, expected) in params.iter().zip(expected_params.iter()) {
            let actual = param.ty.clone().or_else(|| {
                (!mentions_type_parameter(&expected.ty)).then(|| expected.ty.clone())
            })?;
            locals.insert(param.name.clone(), actual.clone());
            actual_params.push(CallableParamType {
                name: param.name.clone(),
                ty: actual,
            });
        }

        let actual_return = match return_type {
            Some(explicit) => explicit.clone(),
            None => match body {
                ClosureBody::Expr(value) => self.infer_type_with_locals(value, &locals)?,
                ClosureBody::Block(_) => return None,
            },
        };
        Some(SparType::Function {
            params: actual_params,
            return_type: Box::new(actual_return),
        })
    }

    /// Records that `expr`, if it is a call, is expected to produce `expected`.
    fn expect_call_type(&self, expr: &Expr, expected: &SparType) {
        let key = match expr {
            Expr::Call { name_span, .. } => (name_span.start, name_span.end),
            Expr::FnCall(call) => (call.span.start, call.span.end),
            Expr::Grouped(inner, _) => return self.expect_call_type(inner, expected),
            _ => return,
        };
        self.expectations.borrow_mut().insert(key, expected.clone());
    }

    fn validate_named_argument_shape<F>(
        &self,
        callable: &str,
        arguments: &[CallArg],
        parameters: &[(String, SparType)],
        is_required: F,
        span: &Span,
    ) -> Result<(), SparError>
    where
        F: Fn(&str) -> bool,
    {
        let mut seen = HashSet::new();
        for argument in arguments {
            if !seen.insert(argument.param_name.as_str()) {
                return Err(SparError::TypeError {
                    message: format!("duplicate argument '{}'", argument.param_name),
                    hint: None,
                    span: argument.param_name_span.clone(),
                });
            }
            if !parameters
                .iter()
                .any(|(parameter_name, _)| parameter_name == &argument.param_name)
            {
                return Err(SparError::TypeError {
                    message: format!(
                        "{callable} has no parameter named '{}'",
                        argument.param_name
                    ),
                    hint: parameters.first().map(|_| {
                        let names = parameters
                            .iter()
                            .map(|(name, _)| format!("`{name}: ...`"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("available named arguments: {names}")
                    }),
                    span: argument.param_name_span.clone(),
                });
            }
        }
        if let Some((missing, _)) = parameters.iter().find(|(parameter_name, _)| {
            is_required(parameter_name) && !seen.contains(parameter_name.as_str())
        }) {
            return Err(SparError::TypeError {
                message: format!("{callable} is missing required argument '{missing}'"),
                hint: Some(format!("add `{missing}: ...` to the call")),
                span: span.clone(),
            });
        }
        Ok(())
    }

    fn instantiate_call(
        &self,
        name: &str,
        type_arguments: &[SparType],
        arguments: &[CallArg],
        locals: Option<&HashMap<String, SparType>>,
        span: &Span,
    ) -> Result<(SparType, Vec<(String, SparType)>), SparError> {
        if name == "panic" {
            let parameters = vec![("message".to_string(), SparType::Str)];
            self.validate_named_argument_shape(
                "function 'panic'",
                arguments,
                &parameters,
                |_| true,
                span,
            )?;
            return Ok((SparType::Void, parameters));
        }
        if !name.contains("::") {
            if self.constructor_parameters(name).is_some() {
                let entry = self
                    .symbols
                    .types
                    .get(name)
                    .expect("struct metadata registered");
                if type_arguments.len() != entry.type_parameters.len() {
                    return Err(SparError::TypeError {
                        message: format!(
                            "struct '{name}' expects {} type arguments, found {}",
                            entry.type_parameters.len(),
                            type_arguments.len()
                        ),
                        hint: None,
                        span: span.clone(),
                    });
                }
                let owner = if type_arguments.is_empty() {
                    SparType::Named(name.to_string())
                } else {
                    SparType::Applied {
                        name: name.to_string(),
                        arguments: type_arguments.to_vec(),
                    }
                };
                let (_, fields) =
                    Self::fields_for_type(&owner, self.symbols).expect("checked struct arity");
                let parameters = fields
                    .iter()
                    .map(|field| (field.name.clone(), self.field_shape_to_type(&field.shape)))
                    .collect::<Vec<_>>();
                self.validate_named_argument_shape(
                    &format!("struct constructor '{name}'"),
                    arguments,
                    &parameters,
                    |field| {
                        fields
                            .iter()
                            .any(|candidate| candidate.name == field && candidate.default.is_none())
                    },
                    span,
                )?;
                return Ok((owner, parameters));
            }
        }
        let Some(entry) = self.call_entry(name) else {
            if !name.contains("::") {
                let callable = locals
                    .and_then(|locals| locals.get(name).cloned())
                    .or_else(|| {
                        let reference = NamespaceRef {
                            segments: vec![name.to_string()],
                            span: span.clone(),
                        };
                        self.infer_namespace_type(&reference)
                    });
                if let Some(SparType::Function {
                    params,
                    return_type,
                }) = callable
                {
                    let parameters = params
                        .into_iter()
                        .map(|parameter| (parameter.name, parameter.ty))
                        .collect::<Vec<_>>();
                    self.validate_named_argument_shape(
                        &format!("callable '{name}'"),
                        arguments,
                        &parameters,
                        |_| true,
                        span,
                    )?;
                    return Ok((*return_type, parameters));
                }
            }
            let segments: Vec<&str> = name.split("::").collect();
            if let [namespace, function] = segments.as_slice() {
                if let Some(host) = self
                    .symbols
                    .hosts
                    .get(&(namespace.to_string(), function.to_string()))
                {
                    return self.instantiate_external_signature(
                        name,
                        &host.ret,
                        &host.params,
                        type_arguments,
                        arguments,
                        locals,
                        span,
                    );
                }
                if let Some(native) = self
                    .symbols
                    .natives
                    .get(&(namespace.to_string(), function.to_string()))
                {
                    return self.instantiate_external_signature(
                        name,
                        &native.ret,
                        &native.params,
                        type_arguments,
                        arguments,
                        locals,
                        span,
                    );
                }
            }
            return self
                .call_return_type(name)
                .map(|ret| (ret, Vec::new()))
                .ok_or_else(|| SparError::TypeError {
                    message: format!("cannot determine signature for function '{name}'"),
                    hint: None,
                    span: span.clone(),
                });
        };

        self.validate_named_argument_shape(
            &format!("function '{name}'"),
            arguments,
            &entry.params,
            |parameter_name| !entry.default_params.contains(parameter_name),
            span,
        )?;

        let mut substitution = TypeSubstitution::new();
        for (parameter, argument) in entry.type_parameters.iter().zip(type_arguments) {
            substitution.insert(parameter.name.clone(), argument.clone());
        }

        // Closures with untyped parameters are inferred last, once the other
        // arguments have bound the type parameters they depend on.
        for closures_pass in [false, true] {
            for argument in arguments {
                if is_untyped_closure_expr(&argument.value) != closures_pass {
                    continue;
                }
                let Some((_, pattern)) = entry
                    .params
                    .iter()
                    .find(|(parameter_name, _)| parameter_name == &argument.param_name)
                else {
                    continue;
                };
                let actual = match locals {
                    Some(locals) => self.infer_type_with_locals(&argument.value, locals),
                    None => self.infer_type(&argument.value),
                }
                .or_else(|| {
                    self.infer_closure_signature_for_pattern(
                        &argument.value,
                        pattern,
                        &substitution,
                        locals,
                    )
                });
                if let Some(actual) = actual {
                    unify_generic(pattern, &actual, &mut substitution, &argument.span)?;
                }
            }
        }

        // Type parameters the arguments could not pin down may still follow from
        // the type this call is expected to produce.
        if entry
            .type_parameters
            .iter()
            .any(|parameter| !substitution.contains_key(&parameter.name))
        {
            let expected = self
                .expectations
                .borrow()
                .get(&(span.start, span.end))
                .cloned();
            if let Some(expected) = expected {
                let mut trial = substitution.clone();
                if unify_generic(&entry.ret, &expected, &mut trial, span).is_ok() {
                    substitution = trial;
                }
            }
        }

        if let Some(parameter) = entry
            .type_parameters
            .iter()
            .find(|parameter| !substitution.contains_key(&parameter.name))
        {
            return Err(SparError::TypeError {
                message: format!(
                    "cannot infer type parameter '{}' for function '{}'; add an explicit type argument such as {}<{}>(...)",
                    parameter.name, name, name, parameter.name
                ),
                hint: Some("supply explicit leading type arguments".into()),
                span: span.clone(),
            });
        }

        let eventual_return = substitute_type(&entry.ret, &substitution);
        let call_return = if entry.is_async {
            promise_type(eventual_return)
        } else {
            eventual_return
        };
        Ok((
            call_return,
            entry
                .params
                .iter()
                .map(|(parameter_name, ty)| {
                    (parameter_name.clone(), substitute_type(ty, &substitution))
                })
                .collect(),
        ))
    }

    /// Instantiate type variables appearing in host/native signatures from
    /// the call-site argument types. Native signatures intentionally do not
    /// carry a separate generic-parameter declaration; any `TypeParameter`
    /// in the registered signature is universally quantified for that call.
    fn instantiate_external_signature(
        &self,
        name: &str,
        ret: &SparType,
        params: &[(String, SparType)],
        type_arguments: &[SparType],
        arguments: &[CallArg],
        locals: Option<&HashMap<String, SparType>>,
        span: &Span,
    ) -> Result<(SparType, Vec<(String, SparType)>), SparError> {
        self.validate_named_argument_shape(
            "native/host function",
            arguments,
            params,
            |_| true,
            span,
        )?;
        let type_parameters = external_type_parameter_names(params, ret);
        if type_parameters.is_empty() && !type_arguments.is_empty() {
            return Err(SparError::TypeError {
                message: format!("native/host function '{name}' does not accept type arguments"),
                hint: None,
                span: span.clone(),
            });
        }
        if type_arguments.len() > type_parameters.len() {
            return Err(SparError::TypeError {
                message: format!(
                    "native/host function '{name}' accepts at most {} type argument{}, found {}",
                    type_parameters.len(),
                    if type_parameters.len() == 1 { "" } else { "s" },
                    type_arguments.len()
                ),
                hint: None,
                span: span.clone(),
            });
        }
        let mut substitution = TypeSubstitution::new();
        for (parameter, argument) in type_parameters.iter().zip(type_arguments) {
            substitution.insert(parameter.clone(), argument.clone());
        }
        for closures_pass in [false, true] {
            for argument in arguments {
                if is_untyped_closure_expr(&argument.value) != closures_pass {
                    continue;
                }
                let Some((_, pattern)) = params
                    .iter()
                    .find(|(parameter_name, _)| parameter_name == &argument.param_name)
                else {
                    continue;
                };
                let actual = match locals {
                    Some(locals) => self.infer_type_with_locals(&argument.value, locals),
                    None => self.infer_type(&argument.value),
                }
                .or_else(|| {
                    self.infer_closure_signature_for_pattern(
                        &argument.value,
                        pattern,
                        &substitution,
                        locals,
                    )
                });
                // A fully concrete parameter has nothing to infer; leave any
                // mismatch to the per-argument check, which words it as
                // "argument 'x' expects T but got U".
                if !mentions_type_parameter(pattern) {
                    continue;
                }
                if let Some(actual) = actual {
                    unify_generic(pattern, &actual, &mut substitution, &argument.span)?;
                }
            }
        }

        if type_parameters
            .iter()
            .any(|parameter| !substitution.contains_key(parameter))
        {
            let expected = self
                .expectations
                .borrow()
                .get(&(span.start, span.end))
                .cloned();
            if let Some(expected) = expected {
                let mut trial = substitution.clone();
                if unify_generic(ret, &expected, &mut trial, span).is_ok() {
                    substitution = trial;
                }
            }
        }

        if let Some(parameter) = type_parameters
            .iter()
            .find(|parameter| !substitution.contains_key(*parameter))
        {
            return Err(SparError::TypeError {
                message: format!(
                    "cannot infer type parameter '{parameter}' for native/host function '{name}'; add an explicit type argument such as {name}<{parameter}>(...)"
                ),
                hint: Some("supply an explicit leading type argument".into()),
                span: span.clone(),
            });
        }

        Ok((
            substitute_type(ret, &substitution),
            params
                .iter()
                .map(|(name, ty)| (name.clone(), substitute_type(ty, &substitution)))
                .collect(),
        ))
    }

    fn check_call(&self, call: &Expr) -> Result<(), SparError> {
        self.check_instantiated_call(call, None)
    }

    fn check_call_with_locals(
        &self,
        call: &Expr,
        locals: &HashMap<String, SparType>,
    ) -> Result<(), SparError> {
        self.check_instantiated_call(call, Some(locals))
    }

    fn check_instantiated_call(
        &self,
        call: &Expr,
        locals: Option<&HashMap<String, SparType>>,
    ) -> Result<(), SparError> {
        let Expr::Call {
            name,
            name_span,
            type_arguments,
            args,
            ..
        } = call
        else {
            return Ok(());
        };
        let (_, parameters) =
            self.instantiate_call(name, type_arguments, args, locals, name_span)?;
        for argument in args {
            let Some((_, expected)) = parameters
                .iter()
                .find(|(parameter_name, _)| parameter_name == &argument.param_name)
            else {
                continue;
            };
            if self.validate_literal_expected(
                &argument.value,
                expected,
                locals,
                &format!("argument '{}'", argument.param_name),
                &argument.span,
            )? {
                continue;
            }
            let actual = match locals {
                Some(locals) => self.infer_type_with_locals(&argument.value, locals),
                None => self.infer_type(&argument.value),
            }
            .or_else(|| {
                self.infer_closure_signature_for_pattern(
                    &argument.value,
                    expected,
                    &TypeSubstitution::new(),
                    locals,
                )
            });
            if !actual
                .as_ref()
                .is_some_and(|actual| is_assignable(expected, actual))
                && !lookup_accepts(expected, actual.as_ref())
            {
                return Err(SparError::TypeError {
                    message: format!(
                        "argument '{}' expects {} but got {}",
                        argument.param_name,
                        display_type(expected),
                        actual
                            .as_ref()
                            .map(display_type)
                            .unwrap_or_else(|| "unknown".into()),
                    ),
                    hint: None,
                    span: argument.span.clone(),
                });
            }
        }
        Ok(())
    }

    fn check_expr_with_locals(
        &mut self,
        expr: &Expr,
        locals: &HashMap<String, SparType>,
    ) -> Result<(), SparError> {
        self.check_expr_with_locals_in_context(expr, locals, false)
    }

    fn check_expr_with_locals_in_context(
        &mut self,
        expr: &Expr,
        locals: &HashMap<String, SparType>,
        is_async: bool,
    ) -> Result<(), SparError> {
        if self.record_types {
            let _ = self.infer_type_with_locals(expr, locals);
        }
        match expr {
            Expr::Object(items, _) => {
                for item in items {
                    match item {
                        ObjectItem::Field(f) => {
                            if let Some(FieldValue::Expr(e)) = &f.value {
                                self.check_expr_with_locals_in_context(e, locals, is_async)?;
                            }
                        }
                        ObjectItem::Spread(sp) => {
                            self.check_expr_with_locals_in_context(&sp.expr, locals, is_async)?
                        }
                    }
                }
                Ok(())
            }
            Expr::BinaryOp(op) => {
                self.check_expr_with_locals_in_context(&op.lhs, locals, is_async)?;
                self.check_expr_with_locals_in_context(&op.rhs, locals, is_async)?;
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
            Expr::Closure { params, body, .. } => {
                if let Some(param) = params.iter().find(|param| param.ty.is_none()) {
                    return Err(SparError::TypeError {
                        message: format!("cannot infer closure parameter '{}'", param.name),
                        hint: Some(format!(
                            "add a type annotation: `fn({}: Type) => ...` or provide an expected `fn(...) -> ...` type",
                            param.name
                        )),
                        span: param.span.clone(),
                    });
                }
                let mut closure_locals = locals.clone();
                for param in params {
                    if let Some(ty) = &param.ty {
                        closure_locals.insert(param.name.clone(), ty.clone());
                    }
                }
                if let ClosureBody::Expr(value) = body {
                    self.check_expr_with_locals_in_context(value, &closure_locals, is_async)?;
                }
                Ok(())
            }
            Expr::FnCall(fc) => {
                for arg in &fc.args {
                    self.check_expr_with_locals_in_context(arg, locals, is_async)?;
                }
                if self.call_entry(&fc.name).is_some()
                    || self.constructor_parameters(&fc.name).is_some()
                {
                    self.instantiate_named_fn_call(fc, Some(locals))?;
                }
                Ok(())
            }
            Expr::String(s) => {
                for part in &s.parts {
                    if let StringPart::Expr(e) = part {
                        self.check_expr_with_locals_in_context(e, locals, is_async)?;
                    }
                }
                Ok(())
            }
            Expr::List(items, _) | Expr::Tuple(items, _) => {
                for item in items {
                    self.check_expr_with_locals_in_context(item, locals, is_async)?;
                }
                Ok(())
            }
            Expr::Grouped(inner, _) => {
                self.check_expr_with_locals_in_context(inner, locals, is_async)
            }
            Expr::TupleField {
                base,
                index,
                index_span,
                ..
            } => {
                self.check_expr_with_locals_in_context(base, locals, is_async)?;
                match self.infer_type_with_locals(base, locals) {
                    Some(SparType::Tuple(items)) if *index < items.len() => Ok(()),
                    Some(SparType::Tuple(items)) => Err(SparError::TypeError {
                        message: format!(
                            "tuple index {index} out of bounds for {} elements",
                            items.len()
                        ),
                        hint: None,
                        span: index_span.clone(),
                    }),
                    Some(other) => Err(SparError::TypeError {
                        message: format!("cannot use tuple access on `{}`", display_type(&other)),
                        hint: None,
                        span: index_span.clone(),
                    }),
                    None => Ok(()),
                }
            }
            Expr::Call { args, .. } => {
                for arg in args {
                    self.check_expr_with_locals_in_context(&arg.value, locals, is_async)?;
                }
                self.check_call_with_locals(expr, locals)?;
                Ok(())
            }
            Expr::Unary { operand, .. } => {
                self.check_expr_with_locals_in_context(operand, locals, is_async)
            }
            Expr::Await { value, span } => {
                if !is_async {
                    return Err(SparError::TypeError {
                        message: "`await` is only valid inside an async function".into(),
                        hint: Some("mark the containing function `async` or remove `await`".into()),
                        span: span.clone(),
                    });
                }
                self.check_expr_with_locals_in_context(value, locals, is_async)?;
                let operand_type = self.infer_type_with_locals(value, locals);
                if operand_type.as_ref().and_then(promise_inner).is_none() {
                    return Err(SparError::TypeError {
                        message: format!(
                            "cannot await `{}`; expected `Promise<T>`",
                            operand_type
                                .as_ref()
                                .map(display_type)
                                .unwrap_or_else(|| "unknown".into())
                        ),
                        hint: None,
                        span: span.clone(),
                    });
                }
                Ok(())
            }
            Expr::Index { source, index, .. } => {
                self.check_expr_with_locals_in_context(source, locals, is_async)?;
                self.check_expr_with_locals_in_context(index, locals, is_async)
            }
            Expr::Comprehension {
                source,
                body,
                var_name,
                ..
            } => {
                self.check_expr_with_locals_in_context(source, locals, is_async)?;
                // The loop variable is in scope for the body; give it the
                // source's element type so e.g. `await item` type-checks.
                let mut body_locals = locals.clone();
                if let Some(SparType::List(elem_ty)) = self.infer_type_with_locals(source, locals) {
                    body_locals.insert(var_name.clone(), *elem_ty);
                }
                self.check_expr_with_locals_in_context(body, &body_locals, is_async)?;
                Ok(())
            }
            Expr::Literal(_) => Ok(()),
            Expr::NamespaceRef(_) => Ok(()),
            Expr::MethodCall {
                receiver,
                method,
                type_arguments,
                args,
                span,
                ..
            } => {
                self.check_expr_with_locals_in_context(receiver, locals, is_async)?;
                let owner = self
                    .method_owner_for_receiver(receiver, Some(locals))
                    .ok_or_else(|| SparError::TypeError {
                        message: format!("cannot resolve method '{method}' for receiver"),
                        hint: None,
                        span: span.clone(),
                    })?;
                let entry = self.symbols.lookup_method(&owner, method).ok_or_else(|| {
                    SparError::TypeError {
                        message: format!("struct '{owner}' has no method '{method}'"),
                        hint: None,
                        span: span.clone(),
                    }
                })?;
                let receiver_is_static = matches!(receiver.as_ref(), Expr::NamespaceRef(reference)
                    if reference.segments.len() == 1
                    && !locals.contains_key(&reference.segments[0])
                    && self.symbols.lookup_struct(&reference.segments).is_some());
                if receiver_is_static == entry.has_receiver {
                    return Err(SparError::TypeError {
                        message: if receiver_is_static {
                            format!("instance method '{method}' requires a struct value receiver")
                        } else {
                            format!("static method '{method}' must be called on struct '{owner}'")
                        },
                        hint: None,
                        span: span.clone(),
                    });
                }
                if entry.function.is_private && self.current_impl.as_deref() != Some(owner.as_str())
                {
                    return Err(SparError::TypeError {
                        message: format!("method '{method}' is private to struct '{owner}'"),
                        hint: None,
                        span: span.clone(),
                    });
                }
                if entry.receiver_mutable {
                    fn receiver_root(expr: &Expr) -> Option<&str> {
                        match expr {
                            Expr::NamespaceRef(reference) if reference.segments.len() == 1 => {
                                Some(reference.segments[0].as_str())
                            }
                            Expr::FieldAccess { base, .. } => receiver_root(base),
                            _ => None,
                        }
                    }
                    let Some(binding) = receiver_root(receiver) else {
                        return Err(SparError::TypeError {
                            message: format!(
                                "mutable method '{method}' requires a mutable lvalue receiver"
                            ),
                            hint: Some(
                                "store the value in a `var mut` binding before calling the method"
                                    .into(),
                            ),
                            span: span.clone(),
                        });
                    };
                    let receiver_is_mutable = if locals.contains_key(binding) {
                        self.mutable_bindings.contains(binding)
                    } else {
                        matches!(
                            self.symbols.lookup_global(binding),
                            Some(GlobalEntry::Var { mutable: true, .. })
                        )
                    };
                    if !receiver_is_mutable {
                        return Err(SparError::TypeError {
                            message: format!(
                                "mutable method '{method}' requires mutable binding '{binding}'"
                            ),
                            hint: Some(format!("declare it as `var mut {binding} = ...`")),
                            span: span.clone(),
                        });
                    }
                }
                let params = if entry.has_receiver {
                    &entry.function.params[1..]
                } else {
                    &entry.function.params[..]
                };
                let mut seen = HashSet::new();
                for argument in args {
                    if !seen.insert(argument.param_name.as_str()) {
                        return Err(SparError::TypeError {
                            message: format!("duplicate argument '{}'", argument.param_name),
                            hint: None,
                            span: argument.param_name_span.clone(),
                        });
                    }
                    if !params.iter().any(|(name, _)| name == &argument.param_name) {
                        return Err(SparError::TypeError {
                            message: format!(
                                "method '{method}' has no parameter named '{}'",
                                argument.param_name
                            ),
                            hint: None,
                            span: argument.param_name_span.clone(),
                        });
                    }
                }
                if let Some((missing, _)) = params.iter().find(|(name, _)| {
                    !entry.function.default_params.contains(name) && !seen.contains(name.as_str())
                }) {
                    return Err(SparError::TypeError {
                        message: format!(
                            "method '{method}' is missing required argument '{missing}'"
                        ),
                        hint: None,
                        span: span.clone(),
                    });
                }

                let mut substitution = TypeSubstitution::new();
                bind_method_type_arguments(method, &entry.function, type_arguments, &mut substitution, span)?;
                if entry.has_receiver {
                    if let (Some(actual_receiver), Some((_, expected_receiver))) = (
                        self.infer_type_with_locals(receiver, locals),
                        entry.function.params.first(),
                    ) {
                        unify_generic(
                            expected_receiver,
                            &actual_receiver,
                            &mut substitution,
                            span,
                        )?;
                    }
                }
                for argument in args {
                    let (_, pattern) = params
                        .iter()
                        .find(|(name, _)| name == &argument.param_name)
                        .expect("named method argument was validated above");
                    let actual = self
                        .infer_type_with_locals(&argument.value, locals)
                        .or_else(|| {
                            self.infer_closure_signature_for_pattern(
                                &argument.value,
                                pattern,
                                &substitution,
                                Some(locals),
                            )
                        });
                    if let Some(actual) = actual {
                        unify_generic(pattern, &actual, &mut substitution, &argument.span)?;
                    }
                }

                for argument in args {
                    let (_, pattern) = params
                        .iter()
                        .find(|(name, _)| name == &argument.param_name)
                        .expect("named method argument was validated above");
                    let expected = substitute_type(pattern, &substitution);
                    if matches!(&argument.value, Expr::Closure { .. })
                        && matches!(expected, SparType::Function { .. })
                    {
                        self.check_closure_against_expected(
                            &argument.value,
                            &expected,
                            locals,
                            is_async,
                        )?;
                    } else {
                        self.check_expr_with_locals_in_context(&argument.value, locals, is_async)?;
                        if let Some(actual) = self.infer_type_with_locals(&argument.value, locals) {
                            if !is_assignable(&expected, &actual) {
                                return Err(SparError::TypeError {
                                    message: format!(
                                        "method '{method}' argument '{}' expects '{}' but received '{}'",
                                        argument.param_name,
                                        display_type(&expected),
                                        display_type(&actual)
                                    ),
                                    hint: await_hint(&expected, &actual),
                                    span: argument.span.clone(),
                                });
                            }
                        }
                    }
                }
                Ok(())
            }
            Expr::StructuredPipe { input, stage, span } => {
                self.check_expr_with_locals_in_context(input, locals, is_async)?;
                // A closure with untyped parameters takes them from the piped
                // input (`rows |> where(fn(r) => r.ok)`), so it cannot be
                // checked on its own; it is checked below against the
                // parameter type the stage signature gives it.
                let is_deferred = |expr: &Expr| {
                    matches!(expr, Expr::Closure { params, .. }
                        if params.iter().any(|param| param.ty.is_none()))
                };
                let mut deferred: Vec<(&Expr, Option<usize>, Option<&str>)> = Vec::new();
                match stage.as_ref() {
                    Expr::FnCall(call) => {
                        for (index, argument) in call.args.iter().enumerate() {
                            if is_deferred(&argument.value) {
                                deferred.push((
                                    &argument.value,
                                    Some(index),
                                    Some(&argument.param_name),
                                ));
                            } else {
                                self.check_expr_with_locals_in_context(
                                    &argument.value,
                                    locals,
                                    is_async,
                                )?;
                            }
                        }
                    }
                    Expr::Call { args, .. } => {
                        for argument in args {
                            if is_deferred(&argument.value) {
                                deferred.push((&argument.value, None, Some(&argument.param_name)));
                            } else {
                                self.check_expr_with_locals_in_context(
                                    &argument.value,
                                    locals,
                                    is_async,
                                )?;
                            }
                        }
                    }
                    other => self.check_expr_with_locals_in_context(other, locals, is_async)?,
                }
                if !deferred.is_empty() {
                    let parameters = self.pipe_stage_parameter_types(input, stage, locals, span)?;
                    for (closure, position, name) in deferred {
                        let expected = match (position, name) {
                            // The piped value is the first parameter.
                            (Some(index), _) => parameters.get(index + 1).map(|(_, ty)| ty),
                            (None, Some(name)) => parameters
                                .iter()
                                .find(|(parameter, _)| parameter == name)
                                .map(|(_, ty)| ty),
                            _ => None,
                        };
                        match expected {
                            Some(expected @ SparType::Function { .. }) => {
                                let expected = expected.clone();
                                self.check_closure_against_expected(
                                    closure, &expected, locals, is_async,
                                )?;
                            }
                            _ => {
                                self.check_expr_with_locals_in_context(closure, locals, is_async)?
                            }
                        }
                    }
                }
                self.structured_pipe_type(input, stage, Some(locals), span)
                    .map(|_| ())
            }
            Expr::FieldAccess { base, .. } => {
                self.check_expr_with_locals_in_context(base, locals, is_async)
            }
            Expr::Shell(shell) => self.check_mixed_shell_with_locals(shell, locals, is_async),
            Expr::CommandSubstitution(shell) => {
                if shell_contains_mixed_pipeline(shell) {
                    Err(SparError::TypeError {
                        message: "command substitution does not support structured mixed pipelines in v1; run the pipeline as a `~` statement inside a function returning `ShellResult<T, E>` instead".into(),
                        hint: None,
                        span: shell.span.clone(),
                    })
                } else {
                    Ok(())
                }
            }
            Expr::ExecShell(shell) => {
                if shell_contains_mixed_pipeline(shell) {
                    Err(SparError::TypeError {
                        message: "`exec` does not support structured mixed pipelines; run the pipeline as a `~` statement inside a function returning `ShellResult<T, E>` instead".into(),
                        hint: None,
                        span: shell.span.clone(),
                    })
                } else {
                    Ok(())
                }
            }
        }
    }

    fn check_mixed_shell_with_locals(
        &mut self,
        shell: &ShellExpr,
        locals: &HashMap<String, SparType>,
        is_async: bool,
    ) -> Result<(), SparError> {
        for (_, step) in &shell.steps {
            let ShellStep::MixedPipeline(pipeline) = step else {
                continue;
            };
            let registry = crate::structured_input::StructuredInputRegistry::builtin();
            let resolved = registry.resolve(
                pipeline.decoder.decoder.namespace,
                &pipeline.decoder.decoder.name,
                &pipeline.decoder.decoder.span,
            )?;
            for arg in &pipeline.decoder.args {
                self.check_expr_with_locals_in_context(&arg.value, locals, is_async)?;
                let expected = if arg.name == "streaming" {
                    if resolved.descriptor.kind != crate::structured_input::DecoderKind::Scoc {
                        return Err(SparError::TypeError {
                            message: "`streaming` is a SCOC decoder control and is not valid for native codecs".into(),
                            hint: None,
                            span: arg.span.clone(),
                        });
                    }
                    SparType::Bool
                } else {
                    let spec = resolved
                        .descriptor
                        .options
                        .iter()
                        .find(|spec| spec.name == arg.name)
                        .ok_or_else(|| SparError::TypeError {
                            message: format!(
                                "unknown option `{}` for decoder `{}`",
                                arg.name, pipeline.decoder.decoder.name
                            ),
                            hint: None,
                            span: arg.span.clone(),
                        })?;
                    match spec.kind {
                        crate::structured_input::DecoderOptionKind::Bool => SparType::Bool,
                        crate::structured_input::DecoderOptionKind::Integer => SparType::Int,
                        crate::structured_input::DecoderOptionKind::Float => SparType::Float,
                        crate::structured_input::DecoderOptionKind::String
                        | crate::structured_input::DecoderOptionKind::Enum => SparType::Str,
                    }
                };
                if let Some(actual) = self.infer_type_with_locals(&arg.value, locals) {
                    let compatible = actual == expected
                        || (expected == SparType::Float && actual == SparType::Int);
                    if !compatible {
                        return Err(SparError::TypeError {
                            message: format!(
                                "decoder option `{}` expects {}, got {}",
                                arg.name,
                                display_type(&expected),
                                display_type(&actual)
                            ),
                            hint: None,
                            span: arg.span.clone(),
                        });
                    }
                }
                if arg.name == "streaming"
                    && matches!(&arg.value, Expr::Literal(Literal::Bool(true)))
                    && !resolved.descriptor.capabilities.streaming
                {
                    return Err(SparError::TypeError {
                        message: format!(
                            "decoder `{}` does not support streaming",
                            pipeline.decoder.decoder.name
                        ),
                        hint: Some(
                            "remove `streaming: true` or choose a streaming-capable SCOC parser"
                                .into(),
                        ),
                        span: arg.span.clone(),
                    });
                }
                if arg.name == "streaming"
                    && resolved.forced_streaming
                        == Some(crate::structured_input::StreamingMode::Enabled)
                    && matches!(&arg.value, Expr::Literal(Literal::Bool(false)))
                {
                    return Err(SparError::TypeError {
                        message: format!(
                            "decoder `{}` is a streaming compatibility alias and cannot set `streaming: false`",
                            pipeline.decoder.decoder.name
                        ),
                        hint: Some(format!("use `from {}` for automatic/buffered selection", resolved.canonical_name)),
                        span: arg.span.clone(),
                    });
                }
            }
            let mut current = mixed_decoder_stream_type(&pipeline.decoder);
            for (index, stage) in pipeline.stages.iter().enumerate() {
                let temp_name = format!("__sparMixedInput{index}");
                let mut stage_locals = locals.clone();
                stage_locals.insert(temp_name.clone(), current.clone());
                let input = Expr::NamespaceRef(NamespaceRef {
                    segments: vec![temp_name],
                    span: pipeline.decoder.span.clone(),
                });
                let pipe = Expr::StructuredPipe {
                    input: Box::new(input),
                    stage: Box::new(stage.clone()),
                    span: stage
                        .span()
                        .cloned()
                        .unwrap_or_else(|| pipeline.span.clone()),
                };
                self.check_expr_with_locals_in_context(&pipe, &stage_locals, is_async)?;
                current = self
                    .infer_type_with_locals(&pipe, &stage_locals)
                    .ok_or_else(|| SparError::TypeError {
                        message: "cannot determine mixed structured pipeline stage type".into(),
                        hint: None,
                        span: stage
                            .span()
                            .cloned()
                            .unwrap_or_else(|| pipeline.span.clone()),
                    })?;
            }
        }

        // `shell.statements` (ordinary `var`/`if`/`for`/... statements inside
        // the `shell { ... }` literal, as opposed to `.steps`, its pipeline
        // commands) were never visited by any typechecking pass — only the
        // resolver's `resolve_shell_statements` walked them, and it only
        // checks scoping (undefined variables, illegal captured-binding
        // mutation), not types. A call to a method/field that doesn't exist
        // inside one of these statements passed `spar check` silently and
        // only surfaced at evaluation time as an opaque "internal lowering
        // error", instead of the normal, actionable type error the identical
        // code gets outside a shell block. `FuncStmt` is `Statement` (see its
        // type alias) so the ordinary function-body statement checker applies
        // directly; `SparType::Any` stands in for the enclosing function's
        // real return type since a `return` here returns from that function,
        // not from this shell value, and threading the real one down here
        // would need a broader signature change than this fix calls for.
        // `in_shell_statement_scope` additionally tells `check_return_value`
        // to accept a bare `return;` here regardless of `ret_ty` — `Any`
        // alone still rejects `ReturnValue::Void` (`is_assignable` requires
        // non-void), which would false-positive on a real, legitimate early
        // `return;` inside a shell block (confirmed live: "function declares
        // return type 'Any' but this 'return;' provides no value").
        let mut shell_locals = locals.clone();
        let was_in_shell_statement_scope = self.in_shell_statement_scope;
        self.in_shell_statement_scope = true;
        self.check_func_stmts(
            &shell.statements,
            &SparType::Any,
            &mut shell_locals,
            is_async,
        );
        self.in_shell_statement_scope = was_in_shell_statement_scope;
        Ok(())
    }

    // ── Function declaration type checking ────────────────────────────────────

    /// Task parameters must be scalar (no `list`/inline record — a shell
    /// argument is always a single string on the command line), metadata
    /// fields must type as their expected scalar (`description`/`cwd`/
    /// env values as `str`, `default`/`quiet` as `bool`), and every `run`
    /// block interpolation must type as *some* scalar — `task_lowering`
    /// (Task 6's later half) is what rejects a scalar expression that
    /// illegally mixes a task parameter with other values, since that's a
    /// lowering-representability concern, not a type concern.
    fn check_task(&mut self, decl: &TaskDecl) {
        for param in &decl.params {
            if matches!(
                param.ty,
                SparType::InlineRecord | SparType::List(_) | SparType::Shell
            ) {
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
        for (key, value) in &decl.env {
            self.check_task_scalar_field(value, &format!("env.{key}"), &SparType::Str, &decl.span);
        }

        let local_types: HashMap<String, SparType> = decl
            .params
            .iter()
            .map(|p| (p.name.clone(), p.ty.clone()))
            .collect();
        for block in &decl.run_blocks {
            match &block.body {
                RunBody::Bash(commands) => {
                    for command in commands {
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
                RunBody::Native(shell) => {
                    if let Err(e) =
                        self.check_expr_with_locals(&Expr::Shell(shell.clone()), &local_types)
                    {
                        self.errors.push(e);
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

    /// A `shell { ... }` or `command ...` used as a statement builds a plan and
    /// throws it away, so nothing runs. Reject it instead of failing silently.
    fn reject_discarded_shell_plans(&mut self, statements: &[FuncStmt]) {
        for statement in statements {
            match statement {
                FuncStmt::Expression(Expr::Shell(shell), span) => {
                    if shell.run_now {
                        continue;
                    }
                    self.push_type_error(
                        "this shell value is created but never run, so it does nothing",
                        Some(
                            "use `~ cmd` to run a command as a statement, `$(cmd)` to run it and \
                             capture its output, or write the command inside a function that \
                             returns `ShellResult<T, E>`"
                                .into(),
                        ),
                        if shell.span.start == 0 && shell.span.end == 0 {
                            span.clone()
                        } else {
                            shell.span.clone()
                        },
                    );
                }
                FuncStmt::If(if_stmt) => {
                    self.reject_discarded_shell_plans(&if_stmt.then_stmts);
                    self.reject_discarded_shell_plans(&if_stmt.else_stmts);
                }
                FuncStmt::For(for_stmt) => self.reject_discarded_shell_plans(&for_stmt.body),
                FuncStmt::While(while_stmt) => self.reject_discarded_shell_plans(&while_stmt.body),
                FuncStmt::Try(try_stmt) => {
                    self.reject_discarded_shell_plans(&try_stmt.body);
                    self.reject_discarded_shell_plans(&try_stmt.handler);
                }
                _ => {}
            }
        }
    }

    fn check_function_decl(&mut self, f: &FunctionDecl) {
        if !matches!(&f.ret, SparType::Applied { name, arguments } if name == "ShellResult" && arguments.len() == 2) {
            self.reject_discarded_shell_plans(&f.body.stmts);
        }
        let mut default_locals = HashMap::new();
        for param in &f.params {
            let Some(default) = &param.default else {
                default_locals.insert(param.name.clone(), param.ty.clone());
                continue;
            };
            self.expect_call_type(default, &param.ty);
            if let Err(error) = self.check_expr_with_locals(default, &default_locals) {
                self.errors.push(error);
                continue;
            }
            match self.validate_literal_expected(
                default,
                &param.ty,
                Some(&default_locals),
                &param.name,
                &param.span,
            ) {
                Ok(true) => {
                    default_locals.insert(param.name.clone(), param.ty.clone());
                    continue;
                }
                Err(error) => {
                    self.errors.push(error);
                    continue;
                }
                Ok(false) => {}
            }
            let actual = self.infer_type_with_locals(default, &default_locals);
            if !actual
                .as_ref()
                .is_some_and(|actual| is_assignable(&param.ty, actual))
            {
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
            default_locals.insert(param.name.clone(), param.ty.clone());
        }

        let mut local_types: HashMap<String, SparType> = f
            .params
            .iter()
            .map(|p| (p.name.clone(), p.ty.clone()))
            .collect();
        self.mutable_bindings.clear();
        if self.current_method_receiver_mutable {
            self.mutable_bindings.insert("self".to_string());
        }
        self.check_func_stmts(&f.body.stmts, &f.ret, &mut local_types, f.is_async);
    }

    fn check_closure_against_expected(
        &mut self,
        closure: &Expr,
        expected: &SparType,
        outer_locals: &HashMap<String, SparType>,
        is_async: bool,
    ) -> Result<SparType, SparError> {
        let Expr::Closure {
            params,
            return_type,
            body,
            span,
        } = closure
        else {
            return Err(SparError::TypeError {
                message: "internal type error: expected closure expression".into(),
                hint: None,
                span: Span::dummy(),
            });
        };
        let SparType::Function {
            params: expected_params,
            return_type: expected_return,
        } = expected
        else {
            return Err(SparError::TypeError {
                message: format!(
                    "closure requires a callable expected type, found '{}'",
                    display_type(expected)
                ),
                hint: None,
                span: span.clone(),
            });
        };
        if params.len() != expected_params.len() {
            return Err(SparError::TypeError {
                message: format!(
                    "closure expects {} parameter{}, but target callable requires {}",
                    params.len(),
                    if params.len() == 1 { "" } else { "s" },
                    expected_params.len()
                ),
                hint: None,
                span: span.clone(),
            });
        }

        let mut locals = outer_locals.clone();
        for (param, expected_param) in params.iter().zip(expected_params) {
            if !is_legacy_callable_param(&expected_param.name) && param.name != expected_param.name
            {
                return Err(SparError::TypeError {
                    message: format!(
                        "closure parameter is named '{}' but target callable expects '{}'",
                        param.name, expected_param.name
                    ),
                    hint: Some("callable parameter names are part of the function type".into()),
                    span: param.span.clone(),
                });
            }
            if let Some(explicit) = &param.ty {
                if explicit != &expected_param.ty {
                    return Err(SparError::TypeError {
                        message: format!(
                            "closure parameter '{}' has type '{}' but expected '{}'",
                            param.name,
                            display_type(explicit),
                            display_type(&expected_param.ty)
                        ),
                        hint: None,
                        span: param.span.clone(),
                    });
                }
            }
            locals.insert(param.name.clone(), expected_param.ty.clone());
        }

        if let Some(explicit_return) = return_type {
            if explicit_return != expected_return.as_ref() {
                return Err(SparError::TypeError {
                    message: format!(
                        "closure return type '{}' does not match expected '{}'",
                        display_type(explicit_return),
                        display_type(expected_return)
                    ),
                    hint: None,
                    span: span.clone(),
                });
            }
        }

        match body {
            ClosureBody::Expr(value) => {
                self.check_expr_with_locals_in_context(value, &locals, is_async)?;
                let actual = self.infer_type_with_locals(value, &locals).ok_or_else(|| {
                    SparError::TypeError {
                        message: "cannot infer closure return type".into(),
                        hint: Some(format!(
                            "annotate the closure return type as `-> {}`",
                            display_type(expected_return)
                        )),
                        span: span.clone(),
                    }
                })?;
                if !is_assignable(expected_return.as_ref(), &actual) {
                    return Err(SparError::TypeError {
                        message: format!(
                            "closure returns '{}' but expected '{}'",
                            display_type(&actual),
                            display_type(expected_return)
                        ),
                        hint: await_hint(expected_return, &actual),
                        span: span.clone(),
                    });
                }
            }
            ClosureBody::Block(body) => {
                let mut block_locals = locals;
                self.check_func_stmts(
                    body.stmts.as_slice(),
                    expected_return,
                    &mut block_locals,
                    is_async,
                );
            }
        }

        Ok(expected.clone())
    }

    fn check_func_stmts(
        &mut self,
        stmts: &[FuncStmt],
        ret_ty: &SparType,
        local_types: &mut HashMap<String, SparType>,
        is_async: bool,
    ) {
        for stmt in stmts {
            match stmt {
                FuncStmt::TupleBinding {
                    names,
                    ty,
                    value,
                    span,
                } => {
                    if let Err(error) =
                        self.check_expr_with_locals_in_context(value, local_types, is_async)
                    {
                        self.errors.push(error);
                    }
                    let actual = self.infer_type_with_locals(value, local_types);
                    let tuple_ty = ty.as_ref().or(actual.as_ref());
                    match tuple_ty {
                        Some(SparType::Tuple(items)) if items.len() == names.len() => {
                            if let Some(declared) = ty {
                                if let Err(error) = self.validate_literal_expected(
                                    value,
                                    declared,
                                    Some(local_types),
                                    "tuple binding",
                                    span,
                                ) {
                                    self.errors.push(error);
                                }
                                if let Some(actual) = &actual {
                                    if !is_assignable(declared, actual) {
                                        self.push_type_error(
                                            format!(
                                                "tuple binding expects `{}` but found `{}`",
                                                display_type(declared),
                                                display_type(actual)
                                            ),
                                            None,
                                            span.clone(),
                                        );
                                    }
                                }
                            }
                            for ((name, _), item_ty) in names.iter().zip(items) {
                                local_types.insert(name.clone(), item_ty.clone());
                                self.mutable_bindings.remove(name);
                            }
                        }
                        Some(SparType::Tuple(items)) => self.push_type_error(
                            format!(
                                "tuple binding has {} names but value has {} elements",
                                names.len(),
                                items.len()
                            ),
                            None,
                            span.clone(),
                        ),
                        Some(other) => self.push_type_error(
                            format!(
                                "tuple binding requires a tuple, found `{}`",
                                display_type(other)
                            ),
                            None,
                            span.clone(),
                        ),
                        None => self.push_type_error(
                            "cannot infer tuple binding type",
                            None,
                            span.clone(),
                        ),
                    }
                }
                FuncStmt::LocalVar(lv) => {
                    if lv.mutable {
                        self.mutable_bindings.insert(lv.name.clone());
                    } else {
                        self.mutable_bindings.remove(&lv.name);
                    }
                    if let (Some(expected @ SparType::Function { .. }), Expr::Closure { .. }) =
                        (lv.ty.as_ref(), &lv.value)
                    {
                        match self.check_closure_against_expected(
                            &lv.value,
                            expected,
                            local_types,
                            is_async,
                        ) {
                            Ok(ty) => {
                                local_types.insert(lv.name.clone(), ty);
                            }
                            Err(error) => self.errors.push(error),
                        }
                        continue;
                    }

                    if let Some(declared) = lv.ty.as_ref() {
                        self.expect_call_type(&lv.value, declared);
                    }
                    if let Err(e) =
                        self.check_expr_with_locals_in_context(&lv.value, local_types, is_async)
                    {
                        self.errors.push(e);
                    }

                    if let Some(declared) = lv.ty.as_ref() {
                        match self.validate_literal_expected(
                            &lv.value,
                            declared,
                            Some(local_types),
                            &format!("local variable '{}'", lv.name),
                            &lv.span,
                        ) {
                            Ok(true) => {
                                local_types.insert(lv.name.clone(), declared.clone());
                                continue;
                            }
                            Ok(false) => {}
                            Err(error) => {
                                self.errors.push(error);
                                local_types.insert(lv.name.clone(), declared.clone());
                                continue;
                            }
                        }
                    }

                    let actual = self.infer_type_with_locals(&lv.value, local_types);
                    match (lv.ty.as_ref(), actual) {
                            (Some(declared), Some(ref actual))
                                if is_assignable(declared, actual)
                                    || matches!((declared, actual),
                                        (SparType::Named(left), SparType::Named(right))
                                            if (left == "ExecResult" && right == "ProcessResult")
                                                || (left == "ProcessResult" && right == "ExecResult")) =>
                            {
                                local_types.insert(lv.name.clone(), declared.clone());
                            }
                            (Some(declared), Some(actual)) => {
                                self.errors.push(SparError::TypeError {
                                message: format!(
                                    "local variable '{}' declared as '{}' but assigned a value of type '{}'",
                                    lv.name, display_type(declared), display_type(&actual)
                                ),
                                hint: await_hint(declared, &actual),
                                span: lv.span.clone(),
                            })
                            }
                            (None, Some(actual)) => {
                                local_types.insert(lv.name.clone(), actual);
                            }
                            // `[]` carries no element type of its own; the
                            // declared list type supplies it.
                            (Some(declared @ SparType::List(_)), None)
                                if matches!(&lv.value, Expr::List(items, _) if items.is_empty()) =>
                            {
                                local_types.insert(lv.name.clone(), declared.clone());
                            }
                            (_, None) => {
                                if let Expr::Closure { params, .. } = &lv.value {
                                    if let Some(param) = params.iter().find(|param| param.ty.is_none()) {
                                        self.errors.push(SparError::TypeError {
                                            message: format!("cannot infer closure parameter '{}'", param.name),
                                            hint: Some(format!("add a type annotation: `fn({}: Type) => ...` or declare the variable as `fn(...) -> ...`", param.name)),
                                            span: param.span.clone(),
                                        });
                                        continue;
                                    }
                                }
                                self.errors.push(SparError::TypeError {
                                    message: format!("cannot infer type of var '{}'", lv.name),
                                    hint: None,
                                    span: lv.span.clone(),
                                });
                            }
                        }
                }
                FuncStmt::Expression(expr, _) => {
                    if let Err(error) =
                        self.check_expr_with_locals_in_context(expr, local_types, is_async)
                    {
                        self.errors.push(error);
                    }
                }
                FuncStmt::Assignment { name, value, span } => {
                    if let Some(target) = local_types
                        .get(name)
                        .cloned()
                        .or_else(|| self.lookup_global_type(name))
                    {
                        self.expect_call_type(value, &target);
                    }
                    if let Err(error) =
                        self.check_expr_with_locals_in_context(value, local_types, is_async)
                    {
                        self.errors.push(error);
                    }
                    let expected = local_types
                        .get(name)
                        .cloned()
                        .or_else(|| self.lookup_global_type(name));
                    if let Some(expected) = expected.as_ref() {
                        match self.validate_literal_expected(
                            value,
                            expected,
                            Some(local_types),
                            &format!("binding '{name}'"),
                            span,
                        ) {
                            Ok(true) => continue,
                            Ok(false) => {}
                            Err(error) => {
                                self.errors.push(error);
                                continue;
                            }
                        }
                    }
                    let actual = self.infer_type_with_locals(value, local_types);
                    if let (Some(expected), Some(actual)) = (expected, actual) {
                        if !is_assignable(&expected, &actual) {
                            self.errors.push(SparError::TypeError {
                                message: format!(
                                    "binding '{name}' has type '{}' but is assigned a value of type '{}'",
                                    display_type(&expected),
                                    display_type(&actual)
                                ),
                                hint: await_hint(&expected, &actual),
                                span: span.clone(),
                            });
                        }
                    }
                }
                FuncStmt::FieldAssignment {
                    base,
                    fields,
                    value,
                    span,
                } => {
                    if let Err(error) =
                        self.check_expr_with_locals_in_context(value, local_types, is_async)
                    {
                        self.errors.push(error);
                    }
                    let mut expected = local_types
                        .get(base)
                        .cloned()
                        .or_else(|| self.lookup_global_type(base));
                    for field in fields {
                        expected =
                            expected.and_then(|ty| self.infer_field_access_from_type(&ty, field));
                    }
                    if let Some(expected) = expected.as_ref() {
                        match self.validate_literal_expected(
                            value,
                            expected,
                            Some(local_types),
                            "field assignment",
                            span,
                        ) {
                            Ok(true) => continue,
                            Ok(false) => {}
                            Err(error) => {
                                self.errors.push(error);
                                continue;
                            }
                        }
                    }
                    let actual = self.infer_type_with_locals(value, local_types);
                    if let (Some(expected), Some(actual)) = (expected, actual) {
                        if !is_assignable(&expected, &actual) {
                            self.errors.push(SparError::TypeError {
                                message: format!(
                                    "field assignment has type '{}' but field expects '{}'",
                                    display_type(&actual),
                                    display_type(&expected)
                                ),
                                hint: await_hint(&expected, &actual),
                                span: span.clone(),
                            });
                        }
                    }
                }
                FuncStmt::Break(_) | FuncStmt::Continue(_) => {}
                FuncStmt::Return(ret_value, span) => {
                    self.check_return_value(ret_value, ret_ty, local_types, span, is_async);
                }
                FuncStmt::For(statement) => {
                    let iterable_ty = self.infer_type_with_locals(&statement.iterable, local_types);
                    let elem_ty = match iterable_ty {
                        Some(SparType::List(elem)) => Some(*elem),
                        Some(other) => {
                            self.errors.push(SparError::TypeError {
                                message: format!(
                                    "`for ... in` requires a list, found '{}'",
                                    display_type(&other)
                                ),
                                hint: None,
                                span: statement.span.clone(),
                            });
                            None
                        }
                        None => None,
                    };
                    if let Err(e) = self.check_expr_with_locals_in_context(
                        &statement.iterable,
                        local_types,
                        is_async,
                    ) {
                        self.errors.push(e);
                    }
                    let mut loop_types = local_types.clone();
                    if let Some(elem) = elem_ty {
                        match &statement.binding {
                            ForBinding::Value { name, .. } => {
                                loop_types.insert(name.clone(), elem);
                            }
                            ForBinding::Indexed {
                                index_name,
                                value_name,
                                ..
                            } => {
                                loop_types.insert(index_name.clone(), SparType::Int);
                                loop_types.insert(value_name.clone(), elem);
                            }
                        }
                    }
                    let body = statement.body.clone();
                    self.check_func_stmts(&body, ret_ty, &mut loop_types, is_async);
                }
                FuncStmt::While(statement) => {
                    if let Some(condition) = &statement.condition {
                        if let Err(e) =
                            self.check_expr_with_locals_in_context(condition, local_types, is_async)
                        {
                            self.errors.push(e);
                        }
                        let cond_ty = self.infer_type_with_locals(condition, local_types);
                        if cond_ty != Some(SparType::Bool) {
                            self.errors.push(SparError::TypeError {
                                message: format!(
                                    "while condition must be 'bool', found '{}'",
                                    cond_ty
                                        .as_ref()
                                        .map(display_type)
                                        .unwrap_or_else(|| "unknown".into()),
                                ),
                                hint: None,
                                span: statement.span.clone(),
                            });
                        }
                    }
                    let mut loop_types = local_types.clone();
                    let body = statement.body.clone();
                    self.check_func_stmts(&body, ret_ty, &mut loop_types, is_async);
                }
                FuncStmt::If(if_stmt) => {
                    let if_stmt = if_stmt.clone();
                    self.check_if_stmt(&if_stmt, ret_ty, local_types, is_async);
                }
                FuncStmt::Try(statement) => {
                    let mut body_types = local_types.clone();
                    self.check_func_stmts(&statement.body, ret_ty, &mut body_types, is_async);
                    let mut catch_types = local_types.clone();
                    if let Some(name) = &statement.catch_name {
                        catch_types.insert(name.clone(), SparType::Error);
                    }
                    self.check_func_stmts(&statement.handler, ret_ty, &mut catch_types, is_async);
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
        is_async: bool,
    ) {
        if matches!(ret_ty, SparType::Function { .. }) {
            if let ReturnValue::Expr(expr @ Expr::Closure { .. }) = ret_value {
                if let Err(error) =
                    self.check_closure_against_expected(expr, ret_ty, local_types, is_async)
                {
                    self.errors.push(error);
                }
                return;
            }
        }

        let constructor_type = match ret_ty {
            SparType::Applied { name, arguments } if name == "ShellResult" && arguments.len() == 2 => {
                SparType::Applied { name: "Result".into(), arguments: arguments.clone() }
            }
            other => other.clone(),
        };
        if let ReturnValue::Expr(expr) = ret_value {
            self.expect_call_type(expr, &constructor_type);
            if !matches!(ret_ty, SparType::Void) {
                match self.validate_literal_expected(
                    expr,
                    ret_ty,
                    Some(local_types),
                    "return value",
                    span,
                ) {
                    Ok(true) => {
                        if let Err(error) =
                            self.check_expr_with_locals_in_context(expr, local_types, is_async)
                        {
                            self.errors.push(error);
                        }
                        return;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        self.errors.push(error);
                        return;
                    }
                }
            }
        }

        match (ret_ty, ret_value) {
            (SparType::Void, ReturnValue::Void) => {}
            (_, ReturnValue::Void) if self.in_shell_statement_scope => {}
            (SparType::Void, ReturnValue::Expr(_)) => {
                self.errors.push(SparError::TypeError {
                    message: "function declares return type 'void' but this 'return' provides a value — use bare 'return;'"
                        .to_string(),
                    hint: None,
                    span: span.clone(),
                });
            }
            (ty, ReturnValue::Void) => {
                self.errors.push(SparError::TypeError {
                    message: format!(
                        "function declares return type '{}' but this 'return;' provides no value",
                        display_type(ty)
                    ),
                    hint: None,
                    span: span.clone(),
                });
            }

            // Dynamic object literals are deliberately limited to dynamic data.
            // They never act as an implicit constructor for a named structured type.
            (SparType::Named(name), ReturnValue::Expr(Expr::Object(items, _)))
                if name == "Record" =>
            {
                self.check_return_object_items(items, local_types, is_async);
            }
            (SparType::Applied { name, .. }, ReturnValue::Expr(Expr::Object(items, _)))
                if name == "Map" =>
            {
                self.check_return_object_items(items, local_types, is_async);
            }
            (
                ty @ (SparType::Named(_) | SparType::Applied { .. }),
                ReturnValue::Expr(Expr::Object(_, _)),
            ) => {
                let expected = display_type(ty);
                self.errors.push(SparError::TypeError {
                    message: format!(
                        "function declares return type '{expected}', but `{{ ... }}` is a dynamic object literal — return a named value instead"
                    ),
                    hint: Some(format!(
                        "construct the return value explicitly with `{expected}(field: value, ...)`"
                    )),
                    span: span.clone(),
                });
            }

            // A list of dynamic records/maps may contain object literals; a list of
            // named structured values must contain explicit constructors.
            (SparType::List(elem_ty), ReturnValue::Expr(Expr::List(items, _)))
                if matches!(elem_ty.as_ref(), SparType::Named(name) if name == "Record")
                    || matches!(elem_ty.as_ref(), SparType::Applied { name, .. } if name == "Map") =>
            {
                for item in items {
                    if let Err(error) =
                        self.check_expr_with_locals_in_context(item, local_types, is_async)
                    {
                        self.errors.push(error);
                    }
                }
            }
            (SparType::List(elem_ty), ReturnValue::Expr(Expr::List(items, _)))
                if matches!(
                    elem_ty.as_ref(),
                    SparType::Named(_) | SparType::Applied { .. }
                ) && items.iter().any(|item| matches!(item, Expr::Object(_, _))) =>
            {
                let expected = display_type(elem_ty.as_ref());
                self.errors.push(SparError::TypeError {
                    message: format!(
                        "function returns a list of `{expected}`, but one or more elements use `{{ ... }}` — typed list elements require named constructors"
                    ),
                    hint: Some(format!(
                        "construct each element explicitly with `{expected}(...)`"
                    )),
                    span: span.clone(),
                });
            }

            // InlineRecord is an internal migration marker only. Source code can
            // no longer declare an inline-record return type.
            (SparType::InlineRecord, ReturnValue::Expr(_)) => {
                self.errors.push(SparError::TypeError {
                    message: "anonymous typed object returns are no longer supported — return a named struct value, or use `Record`/`Map` for dynamic data"
                        .to_string(),
                    hint: None,
                    span: span.clone(),
                });
            }

            (ty, ReturnValue::Expr(expr)) => {
                if let Err(error) =
                    self.check_expr_with_locals_in_context(expr, local_types, is_async)
                {
                    self.errors.push(error);
                }

                if matches!(ty, SparType::TypeParameter(_)) {
                    return;
                }

                let actual = self.infer_type_with_locals(expr, local_types);
                if !actual
                    .as_ref()
                    .is_some_and(|actual| is_assignable(ty, actual))
                {
                    self.errors.push(SparError::TypeError {
                        message: format!(
                            "function declares return type '{}' but this 'return' provides '{}'",
                            display_type(ty),
                            actual
                                .as_ref()
                                .map(display_type)
                                .unwrap_or_else(|| "unknown".into()),
                        ),
                        hint: actual.as_ref().and_then(|actual| await_hint(ty, actual)),
                        span: span.clone(),
                    });
                }
            }
        }
    }

    fn check_return_object_items(
        &mut self,
        items: &[ObjectItem],
        local_types: &HashMap<String, SparType>,
        is_async: bool,
    ) {
        for item in items {
            match item {
                ObjectItem::Field(field) => {
                    if let Some(FieldValue::Expr(value)) = &field.value {
                        if let Err(error) =
                            self.check_expr_with_locals_in_context(value, local_types, is_async)
                        {
                            self.errors.push(error);
                        }
                    }
                }
                ObjectItem::Spread(spread) => {
                    if let Err(error) =
                        self.check_expr_with_locals_in_context(&spread.expr, local_types, is_async)
                    {
                        self.errors.push(error);
                    }
                }
            }
        }
    }

    fn check_if_stmt(
        &mut self,
        if_stmt: &IfStmt,
        ret_ty: &SparType,
        local_types: &mut HashMap<String, SparType>,
        is_async: bool,
    ) {
        if let Err(e) =
            self.check_expr_with_locals_in_context(&if_stmt.condition, local_types, is_async)
        {
            self.errors.push(e);
        }
        let cond_ty = self.infer_type_with_locals(&if_stmt.condition, local_types);
        if cond_ty != Some(SparType::Bool) {
            self.errors.push(SparError::TypeError {
                message: format!(
                    "if condition must be 'bool', found '{}'",
                    cond_ty
                        .as_ref()
                        .map(display_type)
                        .unwrap_or_else(|| "unknown".into()),
                ),
                hint: None,
                span: if_stmt.span.clone(),
            });
        }

        let mut then_types = local_types.clone();
        self.check_func_stmts(&if_stmt.then_stmts, ret_ty, &mut then_types, is_async);
        let mut else_types = local_types.clone();
        self.check_func_stmts(&if_stmt.else_stmts, ret_ty, &mut else_types, is_async);
    }

    // ── Type inference with local variable scope ──────────────────────────────

    fn infer_type_with_locals(
        &self,
        expr: &Expr,
        locals: &HashMap<String, SparType>,
    ) -> Option<SparType> {
        let ty = self.infer_type_with_locals_impl(expr, locals);
        self.record_type(expr, &ty);
        self.record_receiver(expr, Some(locals));
        ty
    }

    fn infer_type_with_locals_impl(
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
            Expr::FnCall(fc) => {
                match fc.name.as_str() {
                    "env" | "str" => return Some(SparType::Str),
                    "int" => return Some(SparType::Int),
                    "float" => return Some(SparType::Float),
                    "bool" => return Some(SparType::Bool),
                    _ => {}
                }
                if let Ok(Some(ret)) = self.instantiate_named_fn_call(fc, Some(locals)) {
                    return Some(ret);
                }
                let callable = locals.get(&fc.name).cloned().or_else(|| {
                    let reference = NamespaceRef {
                        segments: vec![fc.name.clone()],
                        span: fc.span.clone(),
                    };
                    self.infer_namespace_type(&reference)
                })?;
                match callable {
                    SparType::Function {
                        params,
                        return_type,
                    } if params.len() == fc.args.len() => {
                        if fc.args.iter().all(|arg| {
                            let Some(expected) = params
                                .iter()
                                .find(|parameter| parameter.name == arg.param_name)
                            else {
                                return false;
                            };
                            self.infer_type_with_locals(&arg.value, locals)
                                .as_ref()
                                .is_some_and(|actual| is_assignable(&expected.ty, actual))
                        }) {
                            Some(*return_type)
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            }
            Expr::FieldAccess { base, field, .. } => {
                if let Expr::NamespaceRef(nr) = base.as_ref() {
                    if nr.segments.len() == 1 {
                        if let Some(ty) = locals.get(&nr.segments[0]) {
                            return match ty {
                                SparType::Error => Some(SparType::Str),
                                named @ SparType::Named(_) => {
                                    self.infer_field_access_from_type(named, field)
                                }
                                applied @ SparType::Applied { .. } => self
                                    .type_fields_for(applied)
                                    .and_then(|(_, fields)| {
                                        fields
                                            .into_iter()
                                            .find(|candidate| &candidate.name == field)
                                    })
                                    .map(|field| self.field_shape_to_type(&field.shape)),
                                _ => None,
                            };
                        }
                        if self.symbols.lookup_struct(&nr.segments).is_some() {
                            return self.infer_field_access(base, field);
                        }
                    }
                }
                let base_ty = self.infer_type_with_locals(base, locals)?;
                self.infer_field_access_from_type(&base_ty, field)
            }
            Expr::MethodCall {
                receiver,
                method,
                type_arguments,
                args,
                ..
            } => self
                .infer_method_call(receiver, method, type_arguments, args, Some(locals))
                .ok(),
            Expr::StructuredPipe { input, stage, span } => self
                .structured_pipe_type(input, stage, Some(locals), span)
                .ok(),
            Expr::Closure {
                params,
                return_type,
                body,
                ..
            } => {
                let mut closure_locals = locals.clone();
                let params = params
                    .iter()
                    .map(|param| {
                        let ty = param.ty.clone()?;
                        closure_locals.insert(param.name.clone(), ty.clone());
                        Some(CallableParamType {
                            name: param.name.clone(),
                            ty,
                        })
                    })
                    .collect::<Option<Vec<_>>>()?;
                let result = return_type.clone().or_else(|| match body {
                    ClosureBody::Expr(value) => self.infer_type_with_locals(value, &closure_locals),
                    ClosureBody::Block(_) => None,
                })?;
                Some(SparType::Function {
                    params,
                    return_type: Box::new(result),
                })
            }
            Expr::Call {
                name,
                name_span,
                type_arguments,
                args,
                ..
            } => self
                .instantiate_call(name, type_arguments, args, Some(locals), name_span)
                .ok()
                .map(|(ret, _)| ret),
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
            Expr::Await { value, .. } => self
                .infer_type_with_locals(value, locals)
                .and_then(|ty| promise_inner(&ty).cloned()),
            Expr::Index { source, index, .. } => {
                let idx_ty = self.infer_type_with_locals(index, locals)?;
                if idx_ty != SparType::Int {
                    return None;
                }
                match self.infer_type_with_locals(source, locals)? {
                    SparType::List(elem) => Some(*elem),
                    SparType::Named(name) if name == "Bytes" => Some(SparType::Int),
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
            Expr::Tuple(items, _) => items
                .iter()
                .map(|item| self.infer_type_with_locals(item, locals))
                .collect::<Option<Vec<_>>>()
                .map(SparType::Tuple),
            Expr::TupleField { base, index, .. } => {
                match self.infer_type_with_locals(base, locals)? {
                    SparType::Tuple(items) => items.get(*index).cloned(),
                    _ => None,
                }
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
                if lty == rty
                    && matches!(
                        lty,
                        SparType::Int | SparType::Float | SparType::Str | SparType::Shell
                    )
                {
                    Some(lty)
                } else {
                    None
                }
            }
            BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                if lty == rty && matches!(lty, SparType::Int | SparType::Float) {
                    Some(lty)
                } else {
                    None
                }
            }
            BinOp::Eq | BinOp::NotEq => {
                // A dynamic (Record) value may be compared for equality with a
                // primitive; the runtime compares the underlying values.
                let dynamic_pair = |dynamic: &SparType, other: &SparType| {
                    matches!(dynamic, SparType::Named(name) if name == "Record")
                        && matches!(
                            other,
                            SparType::Int | SparType::Float | SparType::Str | SparType::Bool
                        )
                };
                if (lty == rty && supports_value_equality(&lty))
                    || dynamic_pair(&lty, &rty)
                    || dynamic_pair(&rty, &lty)
                {
                    Some(SparType::Bool)
                } else {
                    None
                }
            }
            BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq => {
                // A dynamic (Record) value orders against a number or string;
                // the runtime compares the underlying values.
                let dynamic_order = |dynamic: &SparType, other: &SparType| {
                    matches!(dynamic, SparType::Named(name) if name == "Record")
                        && matches!(other, SparType::Int | SparType::Float | SparType::Str)
                };
                if dynamic_order(&lty, &rty)
                    || dynamic_order(&rty, &lty)
                    || (matches!(lty, SparType::Int | SparType::Float) && lty == rty)
                {
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

/// `where`, `take`, `map` ... live in `std/data` and must be imported; say so
/// instead of leaving the bare "cannot determine signature" message.
fn missing_data_import_hint(name: &str) -> Option<String> {
    crate::stdlib::DATA_FUNCTIONS
        .contains(&name)
        .then(|| format!("import it first: import pkg {{ {name} }} from \"std/data\";"))
}

fn shell_contains_mixed_pipeline(shell: &ShellExpr) -> bool {
    shell
        .steps
        .iter()
        .any(|(_, step)| matches!(step, ShellStep::MixedPipeline(_)))
}

fn mixed_decoder_stream_type(decoder: &ShellDecodeStage) -> SparType {
    let registry = crate::structured_input::StructuredInputRegistry::builtin();
    let element = registry
        .resolve(
            decoder.decoder.namespace,
            &decoder.decoder.name,
            &decoder.decoder.span,
        )
        .ok()
        .map(|resolved| {
            use crate::structured_input::DecoderOutputShape;
            match resolved
                .descriptor
                .stream_item
                .unwrap_or(resolved.descriptor.normalized_output)
            {
                DecoderOutputShape::Scalar => SparType::Str,
                DecoderOutputShape::List => {
                    SparType::List(Box::new(SparType::Named("Record".into())))
                }
                DecoderOutputShape::Record | DecoderOutputShape::Table => {
                    SparType::Named("Record".into())
                }
            }
        })
        .unwrap_or_else(|| SparType::Named("Record".into()));
    SparType::Applied {
        name: "Stream".into(),
        arguments: vec![element],
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
    fn test_struct_field_as_call_argument_resolves_type() {
        // Regression: static field access on a concrete struct must preserve
        // the declared field type when used as a named call argument.
        check_ok(
            r##"
            function greet(name: str) -> str {
                return name;
            };
            struct Colors { red: str = "#ff0000"; };
            var msg: str = greet(name: Colors().red);
        "##,
        );
    }

    #[test]
    fn test_clean_program() {
        check_ok(
            r#"
            var port: int = 3000;
            var name: str = "keel";
            struct Server { bind: str = "0.0.0.0"; };
        "#,
        );
    }

    #[test]
    fn test_required_var_no_value() {
        assert!(has_type_error("var port: int;", "required variable"));
        assert!(has_type_error("var port: int;", "port"));
    }

    #[test]
    fn test_option_var_can_explicitly_start_none() {
        // `none()` is a prelude function spliced in by `inject_prelude`
        // (see stdlib/mod.rs), not visible to the bare `Resolver::new()`
        // path `check_ok` uses — go through `Engine`, which runs the full
        // pipeline including prelude injection, instead.
        crate::Engine::default()
            .check_source("var port: Option<int> = none();")
            .expect("type check failed unexpectedly");
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
                r#"task Deploy { private: "yes"; run { echo deploy; }; };"#,
                "private",
            ),
            ("task Deploy { group: 1; run { echo deploy; }; };", "group"),
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
    fn test_struct_field_ref_type() {
        let src = r#"
            struct Db { pool: int = 5; };
            var p: int = Db().pool;
        "#;
        check_ok(src);
    }

    #[test]
    fn test_env_in_str_field() {
        check_ok(r#"var mode: str = env(name: "APP_MODE");"#);
    }

    #[test]
    fn test_env_in_int_field() {
        assert!(has_type_error(
            r#"var port: int = env(name: "PORT");"#,
            "type mismatch"
        ));
    }

    #[test]
    fn test_env_fallback_str() {
        check_ok(r#"var mode: str = env(name: "MODE") ?? "dev";"#);
    }

    #[test]
    fn test_env_fallback_int_mismatch() {
        let src = r#"var port: int = env(name: "PORT") ?? 3000;"#;
        let errs = check_err(src);
        assert!(
            errs.iter()
                .any(|e| e.contains("??") || e.contains("Fallback")),
            "got: {errs:?}"
        );
    }

    #[test]
    fn test_required_field_in_struct() {
        check_ok("struct Server { port: int; };");
        assert!(has_type_error(
            "struct Server { port: int; }; var server: Server = Server();",
            "missing required argument"
        ));
    }

    #[test]
    fn test_option_field_in_struct_can_default_to_none() {
        // See test_option_var_can_explicitly_start_none: `none()` needs
        // prelude injection, which the bare `check_ok` path skips.
        crate::Engine::default()
            .check_source("struct Server { port: Option<int> = none(); };")
            .expect("type check failed unexpectedly");
    }

    #[test]
    fn test_valid_typed_list() {
        check_ok("var ports: [int] = [3000, 8080, 9090];");
    }

    #[test]
    fn test_invalid_typed_list_element() {
        assert!(has_type_error(
            r#"var ports: [int] = [3000, "bad", 9090];"#,
            "expects `int` but found `str`"
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
    fn named_struct_is_required_for_typed_nesting() {
        check_ok(
            r#"
            struct Inner { key: str = ""; };
            struct Outer { inner: Inner = Inner(key: "v"); };
        "#,
        );
    }

    #[test]
    fn removed_section_type_is_rejected_by_parser() {
        let src = "var x: section = {};";
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let err = crate::parser::Parser::new(tokens).parse().unwrap_err();
        assert!(err.to_string().contains("section"));
        assert!(err.to_string().contains("Record") || err.to_string().contains("named"));
    }

    #[test]
    fn removed_legacy_section_declaration_is_rejected_by_parser() {
        let src = "[A]{ value: int = 1; };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        let err = crate::parser::Parser::new(tokens).parse().unwrap_err();
        assert!(err.to_string().contains("legacy section"));
        assert!(err.to_string().contains("struct"));
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
    fn record_function_can_return_dynamic_object_literal() {
        check_ok(
            r#"
            function make() -> Record {
                return { port: 8080; };
            };
        "#,
        );
    }

    #[test]
    fn struct_function_returns_named_constructor() {
        check_ok(
            r#"
            struct Server { port: int = 0; };
            function make() -> Server {
                return Server(port: 8080);
            };
        "#,
        );
    }

    #[test]
    fn struct_function_rejects_anonymous_object_return() {
        assert!(has_type_error(
            r#"
            struct Server { port: int = 0; };
            function make() -> Server { return { port: 8080; }; };
        "#,
            "constructor",
        ));
    }

    #[test]
    fn terminal_branch_local_is_not_available_after_if() {
        let src = r#"
            function f(b: bool) -> int {
                if b { return 0; } else { var y: int = 1; }
                return y;
            };
        "#;
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        assert!(crate::resolver::Resolver::new()
            .resolve(&program, &[])
            .is_err());
    }

    #[test]
    fn local_var_type_mismatch_in_function_is_error() {
        assert!(has_type_error(
            r#"function f() -> int { var x: int = "hello"; return x; };"#,
            "declared as",
        ));
    }

    #[test]
    fn mixed_decoder_accepts_typed_scoc_options() {
        check_ok(
            r#"
            function main() -> __shell {
                var useRaw: bool = true;
                return __shell { printf x | from df(raw: useRaw, streaming: false); };
            };
            "#,
        );
    }

    #[test]
    fn mixed_decoder_rejects_unknown_scoc_option() {
        let errors = check_err(
            r#"
            function main() -> __shell {
                return __shell { printf x | from df(doesNotExist: true); };
            };
            "#,
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("unknown option `doesNotExist`")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn mixed_decoder_rejects_wrong_option_type() {
        let errors = check_err(
            r#"
            function main() -> __shell {
                return __shell { printf x | from df(raw: "yes"); };
            };
            "#,
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("decoder option `raw` expects bool")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn mixed_decoder_rejects_forced_streaming_for_batch_only_parser() {
        let errors = check_err(
            r#"
            function main() -> __shell {
                return __shell { printf x | from df(streaming: true); };
            };
            "#,
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("does not support streaming")),
            "got: {errors:?}"
        );
    }
}

fn expr_span_of(expr: &Expr) -> Option<Span> {
    Some(match expr {
        Expr::Literal(_) => return None,
        Expr::String(value) => value.span.clone(),
        Expr::NamespaceRef(value) => value.span.clone(),
        Expr::FnCall(value) => value.span.clone(),
        Expr::BinaryOp(value) => value.span.clone(),
        Expr::List(_, span)
        | Expr::Tuple(_, span)
        | Expr::TupleField { span, .. }
        | Expr::Grouped(_, span)
        | Expr::Call { span, .. }
        | Expr::Closure { span, .. }
        | Expr::Unary { span, .. }
        | Expr::Await { span, .. }
        | Expr::Comprehension { span, .. }
        | Expr::Index { span, .. }
        | Expr::FieldAccess { span, .. }
        | Expr::MethodCall { span, .. }
        | Expr::StructuredPipe { span, .. }
        | Expr::Object(_, span) => span.clone(),
        Expr::Shell(value) | Expr::ExecShell(value) | Expr::CommandSubstitution(value) => {
            value.span.clone()
        }
    })
}
