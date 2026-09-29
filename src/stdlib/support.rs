use crate::runtime::value::Shared;
use crate::error::{Span, SparError};
use crate::runtime::Value;

pub(crate) fn error(message: impl Into<String>) -> SparError {
    SparError::EvalError {
        message: message.into(),
        span: Span::dummy(),
    }
}

pub(crate) fn string_arg<'a>(
    args: &'a [Value],
    index: usize,
    name: &str,
) -> Result<&'a str, SparError> {
    match args.get(index) {
        Some(Value::String(value)) => Ok(value),
        Some(value) => Err(error(format!(
            "native argument '{name}' expected str, received {}",
            value.type_name()
        ))),
        None => Err(error(format!("missing native argument '{name}'"))),
    }
}

pub(crate) fn int_arg(args: &[Value], index: usize, name: &str) -> Result<i64, SparError> {
    match args.get(index) {
        Some(Value::Int(value)) => Ok(*value),
        Some(value) => Err(error(format!(
            "native argument '{name}' expected int, received {}",
            value.type_name()
        ))),
        None => Err(error(format!("missing native argument '{name}'"))),
    }
}

pub(crate) fn float_arg(args: &[Value], index: usize, name: &str) -> Result<f64, SparError> {
    match args.get(index) {
        Some(Value::Float(value)) => Ok(*value),
        Some(Value::Int(value)) => Ok(*value as f64),
        Some(value) => Err(error(format!(
            "native argument '{name}' expected float, received {}",
            value.type_name()
        ))),
        None => Err(error(format!("missing native argument '{name}'"))),
    }
}

pub(crate) fn bool_arg(args: &[Value], index: usize, name: &str) -> Result<bool, SparError> {
    match args.get(index) {
        Some(Value::Bool(value)) => Ok(*value),
        Some(value) => Err(error(format!(
            "native argument '{name}' expected bool, received {}",
            value.type_name()
        ))),
        None => Err(error(format!("missing native argument '{name}'"))),
    }
}

pub(crate) fn owned_bytes_arg(
    args: &[Value],
    index: usize,
    name: &str,
) -> Result<Vec<u8>, SparError> {
    match args.get(index) {
        Some(Value::Bytes(value)) => Ok(value.clone()),
        Some(Value::Object(fields)) => {
            let Some(Value::List(values)) = fields.get("values") else {
                return Err(error(format!(
                    "native argument '{name}' is not a Bytes value"
                )));
            };
            values
                .iter()
                .map(|value| match value {
                    Value::Int(value) if (0..=255).contains(value) => Ok(*value as u8),
                    _ => Err(error(format!(
                        "native argument '{name}' contains a byte outside 0..255"
                    ))),
                })
                .collect()
        }
        Some(value) => Err(error(format!(
            "native argument '{name}' expected Bytes, received {}",
            value.type_name()
        ))),
        None => Err(error(format!("missing native argument '{name}'"))),
    }
}

pub(crate) fn string_list_arg(
    args: &[Value],
    index: usize,
    name: &str,
) -> Result<Vec<String>, SparError> {
    let value = args
        .get(index)
        .ok_or_else(|| error(format!("missing native argument '{name}'")))?;
    let Value::List(values) = value else {
        return Err(error(format!(
            "native argument '{name}' expected List<str>, received {}",
            value.type_name()
        )));
    };
    values
        .iter()
        .map(|value| match value {
            Value::String(value) => Ok(value.clone()),
            other => Err(error(format!(
                "native argument '{name}' expected List<str>, received element {}",
                other.type_name()
            ))),
        })
        .collect()
}

pub(crate) fn object<K, I>(fields: I) -> Value
where
    K: Into<String>,
    I: IntoIterator<Item = (K, Value)>,
{
    Value::Object(
        Shared::from(fields
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect::<indexmap::IndexMap<_, _>>()),
    )
}

pub(crate) fn serde_to_value(value: serde_json::Value) -> Result<Value, SparError> {
    crate::structured_codec::json_value_to_runtime(value)
}

pub(crate) fn value_to_serde(value: &Value) -> Result<serde_json::Value, SparError> {
    crate::structured_codec::runtime_value_to_json(value)
}

/// Declared-type context used by typed JSON decoding. The decoder is shared
/// by `std/json` and HTTP; it never guesses a struct shape from the payload.
pub(crate) struct JsonDecodeEnvironment<'a> {
    pub(crate) symbols: &'a crate::resolver::SymbolTable,
    pub(crate) evaluate_default: &'a mut dyn FnMut(&crate::ast::Expr, &crate::ast::SparType) -> Result<Value, SparError>,
}

pub(crate) fn decode_json_typed(
    value: serde_json::Value,
    expected: &crate::ast::SparType,
    environment: &mut JsonDecodeEnvironment<'_>,
) -> Result<Value, SparError> {
    decode_json_at(value, expected, environment, "$")
}

fn decode_json_at(
    value: serde_json::Value,
    expected: &crate::ast::SparType,
    environment: &mut JsonDecodeEnvironment<'_>,
    path: &str,
) -> Result<Value, SparError> {
    use crate::ast::SparType;

    if matches!(value, serde_json::Value::Null) {
        return match expected {
            SparType::Applied { name, arguments } if name == "Option" && arguments.len() == 1 => {
                Ok(Value::Option(None))
            }
            _ => Err(json_decode_error(
                path,
                expected,
                "null",
                "null is only valid for Option<T>",
            )),
        };
    }

    if let SparType::Applied { name, arguments } = expected {
        if name == "Option" && arguments.len() == 1 {
            return decode_json_at(value, &arguments[0], environment, path)
                .map(|value| Value::Option(Some(Box::new(value))));
        }
    }

    match expected {
        SparType::Any => serde_to_value(value),
        SparType::Str => match value {
            serde_json::Value::String(value) => Ok(Value::String(value)),
            other => Err(json_type_mismatch(path, expected, &other)),
        },
        SparType::Bool => match value {
            serde_json::Value::Bool(value) => Ok(Value::Bool(value)),
            other => Err(json_type_mismatch(path, expected, &other)),
        },
        SparType::Int => match value {
            serde_json::Value::Number(value) => value
                .as_i64()
                .map(Value::Int)
                .ok_or_else(|| json_decode_error(path, expected, "number", "expected an integer in Spar int range")),
            other => Err(json_type_mismatch(path, expected, &other)),
        },
        SparType::Float => match value {
            serde_json::Value::Number(value) => value
                .as_f64()
                .map(Value::Float)
                .ok_or_else(|| json_decode_error(path, expected, "number", "number is outside Spar float range")),
            other => Err(json_type_mismatch(path, expected, &other)),
        },
        SparType::List(element) => decode_json_list(value, element, environment, path),
        SparType::Applied { name, arguments } if name == "List" && arguments.len() == 1 => {
            decode_json_list(value, &arguments[0], environment, path)
        }
        SparType::Applied { name, arguments } if name == "Map" && arguments.len() == 2 => {
            if arguments[0] != SparType::Str {
                return Err(error(format!(
                    "{path}: JSON object decoding requires Map<str, V>, found {}",
                    crate::typechecker::display_type(expected)
                )));
            }
            decode_json_map(value, &arguments[1], environment, path)
        }
        SparType::Named(name) if name == "Record" => match value {
            serde_json::Value::Object(_) => serde_to_value(value),
            other => Err(json_type_mismatch(path, expected, &other)),
        },
        SparType::InlineRecord => match value {
            serde_json::Value::Object(_) => serde_to_value(value),
            other => Err(json_type_mismatch(path, expected, &other)),
        },
        SparType::Named(name) => decode_named_json(value, name, &[], environment, path),
        SparType::Applied { name, arguments }
            if !matches!(name.as_str(), "Option" | "Map" | "List") =>
        {
            decode_named_json(value, name, arguments, environment, path)
        }
        SparType::TypeParameter(name) => Err(error(format!(
            "{path}: typed JSON target '{name}' was not reified at runtime"
        ))),
        other => Err(error(format!(
            "{path}: JSON cannot be decoded as {}",
            crate::typechecker::display_type(other)
        ))),
    }
}

fn decode_json_list(
    value: serde_json::Value,
    element: &crate::ast::SparType,
    environment: &mut JsonDecodeEnvironment<'_>,
    path: &str,
) -> Result<Value, SparError> {
    let values = match value {
        serde_json::Value::Array(values) => values,
        other => {
            return Err(json_type_mismatch(
                path,
                &crate::ast::SparType::List(Box::new(element.clone())),
                &other,
            ));
        }
    };
    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            decode_json_at(value, element, environment, &format!("{path}[{index}]"))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|items| Value::List(Shared::from(items)))
}

fn decode_json_map(
    value: serde_json::Value,
    value_type: &crate::ast::SparType,
    environment: &mut JsonDecodeEnvironment<'_>,
    path: &str,
) -> Result<Value, SparError> {
    let values = match value {
        serde_json::Value::Object(values) => values,
        other => {
            return Err(json_decode_error(
                path,
                &crate::ast::SparType::Applied {
                    name: "Map".into(),
                    arguments: vec![crate::ast::SparType::Str, value_type.clone()],
                },
                json_value_kind(&other),
                "expected a JSON object",
            ));
        }
    };
    values
        .into_iter()
        .map(|(key, value)| {
            let child = json_field_path(path, &key);
            decode_json_at(value, value_type, environment, &child)
                .map(|value| (Value::String(key), value))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|pairs| Value::Map(pairs.into()))
}

#[derive(Clone)]
struct JsonFieldSpec {
    name: String,
    ty: crate::ast::SparType,
    default: Option<crate::ast::Expr>,
}

fn decode_named_json(
    value: serde_json::Value,
    name: &str,
    type_arguments: &[crate::ast::SparType],
    environment: &mut JsonDecodeEnvironment<'_>,
    path: &str,
) -> Result<Value, SparError> {
    let mut input = match value {
        serde_json::Value::Object(values) => values,
        other => {
            return Err(json_decode_error(
                path,
                &named_type(name, type_arguments),
                json_value_kind(&other),
                "expected a JSON object",
            ));
        }
    };

    let fields = named_json_fields(name, type_arguments, environment).ok_or_else(|| {
        error(format!(
            "{path}: cannot decode JSON into unknown declared type '{}'",
            crate::typechecker::display_type(&named_type(name, type_arguments))
        ))
    })?;
    let mut output = indexmap::IndexMap::new();
    for field in fields {
        let field_path = json_field_path(path, &field.name);
        if let Some(value) = input.remove(&field.name) {
            output.insert(
                field.name,
                decode_json_at(value, &field.ty, environment, &field_path)?,
            );
            continue;
        }
        if let Some(default) = field.default {
            output.insert(field.name, (environment.evaluate_default)(&default, &field.ty)?);
            continue;
        }
        return Err(error(format!("{field_path}: missing required field")));
    }
    // Unknown fields are intentionally ignored. Remote APIs can add fields
    // without breaking a typed consumer that only declares what it uses.
    Ok(Value::Object(Shared::from(output)))
}

fn named_json_fields(
    name: &str,
    type_arguments: &[crate::ast::SparType],
    environment: &mut JsonDecodeEnvironment<'_>,
) -> Option<Vec<JsonFieldSpec>> {
    use crate::ast::SparType;

    let entry = environment.symbols.types.get(name)?;
    if entry.type_parameters.len() != type_arguments.len() {
        return None;
    }
    let substitution = entry
        .type_parameters
        .iter()
        .zip(type_arguments)
        .map(|(parameter, argument)| (parameter.name.clone(), argument.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    Some(
        entry
            .fields
            .iter()
            .map(|field| JsonFieldSpec {
                name: field.name.clone(),
                ty: substitute_json_type(&field_shape_type(&field.shape), &substitution),
                default: field.default.as_ref().map(|value| crate::loader::scope::substitute_default(value, &substitution)),
            })
            .collect(),
    )
}

fn fields_for_declared_type(
    ty: &crate::ast::SparType,
    environment: &mut JsonDecodeEnvironment<'_>,
) -> Option<Vec<JsonFieldSpec>> {
    match ty {
        crate::ast::SparType::Named(name) => named_json_fields(name, &[], environment),
        crate::ast::SparType::Applied { name, arguments } => {
            named_json_fields(name, arguments, environment)
        }
        _ => None,
    }
}

fn field_shape_type(shape: &crate::ast::TypeFieldShape) -> crate::ast::SparType {
    use crate::ast::{SparType, TypeFieldShape};
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

fn substitute_json_type(
    ty: &crate::ast::SparType,
    substitution: &std::collections::HashMap<String, crate::ast::SparType>,
) -> crate::ast::SparType {
    use crate::ast::{CallableParamType, SparType};
    match ty {
        SparType::TypeParameter(name) => substitution.get(name).cloned().unwrap_or_else(|| ty.clone()),
        SparType::List(inner) => SparType::List(Box::new(substitute_json_type(inner, substitution))),
        SparType::Applied { name, arguments } => SparType::Applied {
            name: name.clone(),
            arguments: arguments
                .iter()
                .map(|argument| substitute_json_type(argument, substitution))
                .collect(),
        },
        SparType::Function { params, return_type } => SparType::Function {
            params: params
                .iter()
                .map(|parameter| CallableParamType {
                    name: parameter.name.clone(),
                    ty: substitute_json_type(&parameter.ty, substitution),
                })
                .collect(),
            return_type: Box::new(substitute_json_type(return_type, substitution)),
        },
        other => other.clone(),
    }
}

fn named_type(name: &str, arguments: &[crate::ast::SparType]) -> crate::ast::SparType {
    if arguments.is_empty() {
        crate::ast::SparType::Named(name.to_string())
    } else {
        crate::ast::SparType::Applied {
            name: name.to_string(),
            arguments: arguments.to_vec(),
        }
    }
}

fn json_field_path(path: &str, field: &str) -> String {
    if field.chars().next().is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && field.chars().all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        format!("{path}.{field}")
    } else {
        format!("{path}[{field:?}]")
    }
}

fn json_type_mismatch(
    path: &str,
    expected: &crate::ast::SparType,
    value: &serde_json::Value,
) -> SparError {
    json_decode_error(path, expected, json_value_kind(value), "type mismatch")
}

fn json_decode_error(
    path: &str,
    expected: &crate::ast::SparType,
    found: &str,
    detail: &str,
) -> SparError {
    error(format!(
        "{path}: expected {}, found {found} ({detail})",
        crate::typechecker::display_type(expected)
    ))
}

fn json_value_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(number) if number.is_i64() || number.is_u64() => "int",
        serde_json::Value::Number(_) => "float",
        serde_json::Value::String(_) => "str",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}
