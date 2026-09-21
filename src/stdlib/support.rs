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
        fields
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect::<indexmap::IndexMap<_, _>>(),
    )
}

pub(crate) fn serde_to_value(value: serde_json::Value) -> Result<Value, SparError> {
    crate::structured_codec::json_value_to_runtime(value)
}

pub(crate) fn value_to_serde(value: &Value) -> Result<serde_json::Value, SparError> {
    crate::structured_codec::runtime_value_to_json(value)
}
