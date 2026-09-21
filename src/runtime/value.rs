use indexmap::IndexMap;

use crate::error::{Span, SparError};
use crate::evaluator::{ConfigValue, PromiseHandle};

use super::resource::ResourceId;
use super::{ClosureValue, MixedShellValue, Schema, ShellProgramValue, TableValue};

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Void,
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Object(IndexMap<String, Value>),
    Map(Vec<(Value, Value)>),
    Option(std::option::Option<Box<Value>>),
    Result(std::result::Result<Box<Value>, Box<Value>>),
    Table(TableValue),
    Schema(Schema),
    Error {
        message: String,
        kind: String,
        code: i64,
        cause: Option<Box<Value>>,
    },
    Shell(spar_command::ShellPlan),
    MixedShell(MixedShellValue),
    ShellProgram(ShellProgramValue),
    Promise(PromiseHandle),
    Resource(ResourceId),
    Closure(ClosureValue),
    Function(crate::compiled::FunctionId),
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
            Value::Map(_) => "Map",
            Value::Option(_) => "Option",
            Value::Result(_) => "Result",
            Value::Table(_) => "Table",
            Value::Schema(_) => "Schema",
            Value::Error { .. } => "error",
            Value::Shell(_) | Value::MixedShell(_) | Value::ShellProgram(_) => "shell",
            Value::Promise(_) => "Promise",
            Value::Resource(_) => "resource",
            Value::Closure(_) | Value::Function(_) => "fn",
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

    pub(crate) fn is_data_comparable(&self) -> bool {
        match self {
            Value::Void
            | Value::Int(_)
            | Value::Float(_)
            | Value::Bool(_)
            | Value::String(_)
            | Value::Bytes(_) => true,
            Value::List(values) => values.iter().all(Value::is_data_comparable),
            Value::Object(values) => values.values().all(Value::is_data_comparable),
            Value::Map(entries) => entries
                .iter()
                .all(|(key, value)| key.is_data_comparable() && value.is_data_comparable()),
            Value::Option(value) => match value.as_deref() {
                Some(value) => value.is_data_comparable(),
                None => true,
            },
            Value::Result(value) => match value {
                Ok(value) | Err(value) => value.is_data_comparable(),
            },
            Value::Table(table) => table.rows().iter().all(Value::is_data_comparable),
            Value::Schema(_) | Value::Error { .. } => true,
            Value::Shell(_)
            | Value::MixedShell(_)
            | Value::ShellProgram(_)
            | Value::Promise(_)
            | Value::Resource(_)
            | Value::Closure(_)
            | Value::Function(_) => false,
        }
    }

    pub(crate) fn data_ordering(&self, other: &Value) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Value::Int(left), Value::Int(right)) => Some(left.cmp(right)),
            (Value::Float(left), Value::Float(right)) => left.partial_cmp(right),
            (Value::String(left), Value::String(right)) => Some(left.cmp(right)),
            (Value::Bool(left), Value::Bool(right)) => Some(left.cmp(right)),
            _ => None,
        }
    }

    pub fn try_into_config(self, span: &Span) -> Result<ConfigValue, SparError> {
        match self {
            Value::Void => Ok(ConfigValue::Int(0)),
            Value::Int(value) => Ok(ConfigValue::Int(value)),
            Value::Float(value) => Ok(ConfigValue::Float(value)),
            Value::Bool(value) => Ok(ConfigValue::Bool(value)),
            Value::String(value) => Ok(ConfigValue::Str(value)),
            Value::Bytes(values) => Ok(ConfigValue::Section(indexmap::IndexMap::from([(
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
                    .collect::<Result<indexmap::IndexMap<_, _>, _>>()?,
            )),
            Value::Map(_) => Err(SparError::EvalError {
                message: "map values cannot be converted to configuration values directly".into(),
                span: span.clone(),
            }),
            Value::Option(_) => Err(SparError::EvalError {
                message: "Option values cannot be converted to configuration values directly".into(),
                span: span.clone(),
            }),
            Value::Result(_) => Err(SparError::EvalError {
                message: "Result values cannot be converted to configuration values directly".into(),
                span: span.clone(),
            }),
            Value::Table(_) => Err(SparError::EvalError {
                message: "table values cannot be converted to configuration values; serialize them explicitly".into(),
                span: span.clone(),
            }),
            Value::Schema(_) => Err(SparError::EvalError {
                message: "schema values cannot be converted to configuration values directly".into(),
                span: span.clone(),
            }),
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
            Value::MixedShell(_) => Err(SparError::EvalError {
                message: "mixed shell values are runtime-only and cannot be converted to configuration values".into(),
                span: span.clone(),
            }),
            Value::ShellProgram(program) => Ok(ConfigValue::ShellProgram(program)),
            Value::Promise(handle) => Ok(ConfigValue::Promise(handle)),
            Value::Resource(_) => Err(SparError::EvalError {
                message: "runtime resource values cannot be converted to configuration values"
                    .into(),
                span: span.clone(),
            }),
            Value::Closure(_) | Value::Function(_) => Err(SparError::EvalError {
                message: "function values cannot be converted to configuration values".into(),
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
        let source = ConfigValue::Section(indexmap::IndexMap::from([
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

    #[test]
    fn closure_is_not_config_serializable() {
        let closure = ClosureValue {
            captured: super::super::Frame::new(0),
            parameter_slots: Vec::new(),
            body: Vec::new(),
            module: crate::compiled::ModuleId(0),
            span: Span::dummy(),
        };
        let error = Value::Closure(closure)
            .try_into_config(&Span::dummy())
            .unwrap_err();
        assert!(error.to_string().contains("function values"));
    }
}
