use std::collections::HashMap;

use crate::evaluator::{ConfigValue, PromiseHandle};
use crate::error::{Span, SparError};

use super::resource::ResourceId;
use super::ShellProgramValue;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Void,
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Object(HashMap<String, Value>),
    Error {
        message: String,
        kind: String,
        code: i64,
        cause: Option<Box<Value>>,
    },
    Shell(spar_command::ShellPlan),
    ShellProgram(ShellProgramValue),
    Promise(PromiseHandle),
    Resource(ResourceId),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Void => "void",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Bool(_) => "bool",
            Value::String(_) => "str",
            Value::Bytes(_) => "Bytes",
            Value::List(_) => "list",
            Value::Object(_) => "section",
            Value::Error { .. } => "error",
            Value::Shell(_) | Value::ShellProgram(_) => "shell",
            Value::Promise(_) => "Promise",
            Value::Resource(_) => "resource",
        }
    }

    pub fn from_config(value: ConfigValue) -> Self {
        match value {
            ConfigValue::Str(value) => Value::String(value),
            ConfigValue::Int(value) => Value::Int(value),
            ConfigValue::Float(value) => Value::Float(value),
            ConfigValue::Bool(value) => Value::Bool(value),
            ConfigValue::List(values) => {
                Value::List(values.into_iter().map(Value::from_config).collect())
            }
            ConfigValue::Section(values) => Value::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, Value::from_config(value)))
                    .collect(),
            ),
            ConfigValue::Shell(plan) => Value::Shell(plan),
            ConfigValue::ShellProgram(program) => Value::ShellProgram(program),
            ConfigValue::Promise(handle) => Value::Promise(handle),
            ConfigValue::Error {
                message,
                kind,
                code,
                cause,
            } => Value::Error {
                message,
                kind,
                code,
                cause: cause.map(|cause| Box::new(Value::from_config(*cause))),
            },
        }
    }

    pub fn try_into_config(self, span: &Span) -> Result<ConfigValue, SparError> {
        match self {
            Value::Void => Ok(ConfigValue::Int(0)),
            Value::Int(value) => Ok(ConfigValue::Int(value)),
            Value::Float(value) => Ok(ConfigValue::Float(value)),
            Value::Bool(value) => Ok(ConfigValue::Bool(value)),
            Value::String(value) => Ok(ConfigValue::Str(value)),
            Value::Bytes(values) => Ok(ConfigValue::Section(HashMap::from([(
                "values".into(),
                ConfigValue::List(
                    values
                        .into_iter()
                        .map(|value| ConfigValue::Int(i64::from(value)))
                        .collect(),
                ),
            )]))),
            Value::List(values) => Ok(ConfigValue::List(
                values
                    .into_iter()
                    .map(|value| value.try_into_config(span))
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            Value::Object(values) => Ok(ConfigValue::Section(
                values
                    .into_iter()
                    .map(|(key, value)| value.try_into_config(span).map(|value| (key, value)))
                    .collect::<Result<HashMap<_, _>, _>>()?,
            )),
            Value::Error {
                message,
                kind,
                code,
                cause,
            } => Ok(ConfigValue::Error {
                message,
                kind,
                code,
                cause: match cause {
                    Some(cause) => Some(Box::new(cause.try_into_config(span)?)),
                    None => None,
                },
            }),
            Value::Shell(plan) => Ok(ConfigValue::Shell(plan)),
            Value::ShellProgram(program) => Ok(ConfigValue::ShellProgram(program)),
            Value::Promise(handle) => Ok(ConfigValue::Promise(handle)),
            Value::Resource(_) => Err(SparError::EvalError {
                message: "runtime resource values cannot be converted to configuration values"
                    .into(),
                span: span.clone(),
            }),
        }
    }
}

impl From<ConfigValue> for Value {
    fn from(value: ConfigValue) -> Self {
        Value::from_config(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trip_preserves_data_values() {
        let source = ConfigValue::Section(HashMap::from([
            ("name".into(), ConfigValue::Str("spar".into())),
            ("count".into(), ConfigValue::Int(3)),
        ]));
        let runtime = Value::from_config(source.clone());
        let round_trip = runtime.try_into_config(&Span::dummy()).unwrap();
        assert_eq!(round_trip, source);
    }

    #[test]
    fn resource_is_not_config_serializable() {
        let error = Value::Resource(ResourceId(7))
            .try_into_config(&Span::dummy())
            .unwrap_err();
        assert!(error.to_string().contains("resource"));
    }
}
