use std::collections::HashMap;

use crate::error::{Span, SparError};
use crate::runtime::Value;

pub(crate) fn error(message: impl Into<String>) -> SparError {
    SparError::EvalError {
        message: message.into(),
        span: Span::dummy(),
    }
}

pub(crate) fn string_arg<'a>(args: &'a [Value], index: usize, name: &str) -> Result<&'a str, SparError> {
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

pub(crate) fn owned_bytes_arg(args: &[Value], index: usize, name: &str) -> Result<Vec<u8>, SparError> {
    match args.get(index) {
        Some(Value::Bytes(value)) => Ok(value.clone()),
        Some(Value::Object(fields)) => {
            let Some(Value::List(values)) = fields.get("values") else {
                return Err(error(format!("native argument '{name}' is not a Bytes value")));
            };
            values
                .iter()
                .map(|value| match value {
                    Value::Int(value) if (0..=255).contains(value) => Ok(*value as u8),
                    _ => Err(error(format!("native argument '{name}' contains a byte outside 0..255"))),
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

pub(crate) fn string_list_arg(args: &[Value], index: usize, name: &str) -> Result<Vec<String>, SparError> {
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
        fields
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect::<HashMap<_, _>>(),
    )
}

pub(crate) fn serde_to_value(value: serde_json::Value) -> Result<Value, SparError> {
    match value {
        serde_json::Value::Null => Ok(Value::Void),
        serde_json::Value::Bool(value) => Ok(Value::Bool(value)),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(Value::Int(value))
            } else if let Some(value) = value.as_f64() {
                Ok(Value::Float(value))
            } else {
                Err(error("JSON number is outside Spar's numeric range"))
            }
        }
        serde_json::Value::String(value) => Ok(Value::String(value)),
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(serde_to_value)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::List),
        serde_json::Value::Object(values) => values
            .into_iter()
            .map(|(key, value)| serde_to_value(value).map(|value| (key, value)))
            .collect::<Result<HashMap<_, _>, _>>()
            .map(Value::Object),
    }
}

pub(crate) fn value_to_serde(value: &Value) -> Result<serde_json::Value, SparError> {
    match value {
        Value::Void => Ok(serde_json::Value::Null),
        Value::Int(value) => Ok(serde_json::json!(value)),
        Value::Float(value) => Ok(serde_json::json!(value)),
        Value::Bool(value) => Ok(serde_json::json!(value)),
        Value::String(value) => Ok(serde_json::json!(value)),
        Value::Bytes(value) => Ok(serde_json::Value::Array(
            value.iter().map(|value| serde_json::json!(value)).collect(),
        )),
        Value::List(values) => values
            .iter()
            .map(value_to_serde)
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| value_to_serde(value).map(|value| (key.clone(), value)))
            .collect::<Result<serde_json::Map<_, _>, _>>()
            .map(serde_json::Value::Object),
        Value::Error {
            message,
            kind,
            code,
            cause,
        } => {
            let mut object = serde_json::Map::new();
            object.insert("message".into(), serde_json::Value::String(message.clone()));
            object.insert("kind".into(), serde_json::Value::String(kind.clone()));
            object.insert("code".into(), serde_json::json!(code));
            if let Some(cause) = cause {
                object.insert("cause".into(), value_to_serde(cause)?);
            }
            Ok(serde_json::Value::Object(object))
        }
        Value::Shell(_) | Value::ShellProgram(_) | Value::Promise(_) | Value::Resource(_) => {
            Err(error(format!("{} cannot be encoded as JSON", value.type_name())))
        }
    }
}
