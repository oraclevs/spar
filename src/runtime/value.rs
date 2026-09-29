use indexmap::IndexMap;
use std::sync::Arc;

use crate::error::{Span, SparError};
use crate::evaluator::{ConfigValue, PromiseHandle};

use super::resource::ResourceId;
use super::{ClosureValue, MixedShellValue, Schema, ShellProgramValue, TableValue};

/// Payload of `Value::Error`, boxed to keep `Value` small.
#[derive(Clone, Debug, PartialEq)]
pub struct ErrorValue {
    pub message: String,
    pub kind: String,
    pub code: i64,
    pub cause: Option<Box<Value>>,
}

/// Copy-on-write shared payload for the large `Value` variants.
///
/// Cloning is a reference-count bump; any mutation goes through `DerefMut`,
/// which clones the payload first if it is shared (`Arc::make_mut`). This
/// preserves Spar's value semantics while making reads of records and lists
/// O(1) instead of a deep copy, and keeps `Value` small.
pub struct Shared<T>(Arc<T>);

impl<T> Clone for Shared<T> {
    #[inline]
    fn clone(&self) -> Self {
        Shared(Arc::clone(&self.0))
    }
}

impl<T> std::ops::Deref for Shared<T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: Clone> std::ops::DerefMut for Shared<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        Arc::make_mut(&mut self.0)
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Shared<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<T: PartialEq> PartialEq for Shared<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
    }
}

impl<T> From<T> for Shared<T> {
    #[inline]
    fn from(value: T) -> Self {
        Shared(Arc::new(value))
    }
}

impl<T: Default> Default for Shared<T> {
    fn default() -> Self {
        Shared(Arc::new(T::default()))
    }
}

impl<T: Clone> Shared<T> {
    /// Takes the payload out, cloning only if it is still shared.
    #[inline]
    pub fn into_inner(self) -> T {
        Arc::try_unwrap(self.0).unwrap_or_else(|shared| (*shared).clone())
    }
}

impl IntoIterator for Shared<Vec<Value>> {
    type Item = Value;
    type IntoIter = std::vec::IntoIter<Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.into_inner().into_iter()
    }
}

impl<'a> IntoIterator for &'a Shared<Vec<Value>> {
    type Item = &'a Value;
    type IntoIter = std::slice::Iter<'a, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl FromIterator<Value> for Shared<Vec<Value>> {
    fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Self {
        Shared::from(iter.into_iter().collect::<Vec<_>>())
    }
}

impl IntoIterator for Shared<IndexMap<String, Value>> {
    type Item = (String, Value);
    type IntoIter = indexmap::map::IntoIter<String, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.into_inner().into_iter()
    }
}

impl<'a> IntoIterator for &'a Shared<IndexMap<String, Value>> {
    type Item = (&'a String, &'a Value);
    type IntoIter = indexmap::map::Iter<'a, String, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl FromIterator<(String, Value)> for Shared<IndexMap<String, Value>> {
    fn from_iter<I: IntoIterator<Item = (String, Value)>>(iter: I) -> Self {
        Shared::from(iter.into_iter().collect::<IndexMap<_, _>>())
    }
}

pub type ObjectMap = Shared<IndexMap<String, Value>>;
pub type ListVec = Shared<Vec<Value>>;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Void,
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    Bytes(Vec<u8>),
    /// Opaque shell argv expansion produced by `List<str>.asArgs()`.
    /// Each element is already one argv entry; the shell runtime must never
    /// join or re-tokenize these strings.
    Args(Vec<String>),
    List(ListVec),
    Object(ObjectMap),
    Map(MapValue),
    Option(std::option::Option<Box<Value>>),
    Result(std::result::Result<Box<Value>, Box<Value>>),
    Table(Shared<TableValue>),
    Schema(Shared<Schema>),
    Error(Box<ErrorValue>),
    Shell(Shared<spar_command::ShellPlan>),
    MixedShell(Shared<MixedShellValue>),
    ShellProgram(Shared<ShellProgramValue>),
    Promise(PromiseHandle),
    Resource(ResourceId),
    Closure(Shared<ClosureValue>),
    Function(crate::compiled::FunctionId),
}

/// Backing store for `Map<K,V>`. Preserves insertion order (like
/// `Value::Object`'s `IndexMap`) while giving `get`/`insert`/`remove`/
/// `containsKey` O(1) average cost instead of the O(n) linear scan a bare
/// `Vec<(Value, Value)>` forced on every one of those calls.
///
/// Measured impact of the old representation: building an author index over
/// a real 574-work / 1,370-author dataset (`researchgraph`, a Spar consumer
/// project) — an ordinary "look up or insert into a growing map" loop — cost
/// ~48s, versus <1s for every other pipeline stage (JSON load, graph build,
/// BFS, stats, CSV/JSON export) combined. See `SPAR_RUNTIME_FINDINGS.md`
/// Finding 17 in that project.
#[derive(Clone, Debug, Default)]
pub struct MapValue {
    entries: Shared<IndexMap<MapKey, Value>>,
}

/// A hashable, `Eq` projection of a map key. Spar's `Map<K,V>` keys are
/// almost always `str`/`int`/`bool`/`float` in practice; any other value
/// type still works correctly as a key (`Other`), just without the O(1)
/// fast path — every `Other` key shares one hash bucket and is then
/// compared with real `Value` equality within it, which stays correct, just
/// not fast for that rare case.
#[derive(Clone, Debug)]
enum MapKey {
    Str(String),
    Int(i64),
    Bool(bool),
    FloatBits(u64),
    Other(Box<Value>),
}

impl MapKey {
    fn from_value(value: &Value) -> MapKey {
        match value {
            Value::String(value) => MapKey::Str(value.clone()),
            Value::Int(value) => MapKey::Int(*value),
            Value::Bool(value) => MapKey::Bool(*value),
            Value::Float(value) => MapKey::FloatBits(value.to_bits()),
            other => MapKey::Other(Box::new(other.clone())),
        }
    }

    fn to_value(&self) -> Value {
        match self {
            MapKey::Str(value) => Value::String(value.clone()),
            MapKey::Int(value) => Value::Int(*value),
            MapKey::Bool(value) => Value::Bool(*value),
            MapKey::FloatBits(bits) => Value::Float(f64::from_bits(*bits)),
            MapKey::Other(value) => (**value).clone(),
        }
    }
}

impl PartialEq for MapKey {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (MapKey::Str(a), MapKey::Str(b)) => a == b,
            (MapKey::Int(a), MapKey::Int(b)) => a == b,
            (MapKey::Bool(a), MapKey::Bool(b)) => a == b,
            (MapKey::FloatBits(a), MapKey::FloatBits(b)) => a == b,
            (MapKey::Other(a), MapKey::Other(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for MapKey {}

impl std::hash::Hash for MapKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            MapKey::Str(value) => {
                0u8.hash(state);
                value.hash(state);
            }
            MapKey::Int(value) => {
                1u8.hash(state);
                value.hash(state);
            }
            MapKey::Bool(value) => {
                2u8.hash(state);
                value.hash(state);
            }
            MapKey::FloatBits(value) => {
                3u8.hash(state);
                value.hash(state);
            }
            // Every non-primitive key collides into this one bucket. Still
            // correct — `PartialEq` is still checked for every candidate in
            // the bucket — just O(n) among non-primitive keys specifically,
            // which real programs essentially never key a map by.
            MapKey::Other(_) => 4u8.hash(state),
        }
    }
}

impl MapValue {
    pub fn new() -> Self {
        Self {
            entries: IndexMap::new().into(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// O(1) average.
    pub fn get(&self, key: &Value) -> Option<&Value> {
        self.entries.get(&MapKey::from_value(key))
    }

    /// O(1) average.
    pub fn contains_key(&self, key: &Value) -> bool {
        self.entries.contains_key(&MapKey::from_value(key))
    }

    /// O(1) average. Matches the old `Vec`-based behavior: an existing key's
    /// value is replaced in place (keeping its original position), a new key
    /// is appended.
    pub fn insert(&mut self, key: Value, value: Value) -> Option<Value> {
        self.entries.insert(MapKey::from_value(&key), value)
    }

    /// O(1) average lookup + O(n) shift, matching the old `Vec::remove`'s
    /// order-preserving cost — removal was never the bottleneck; lookup was.
    pub fn remove(&mut self, key: &Value) -> Option<Value> {
        self.entries.shift_remove(&MapKey::from_value(key))
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn contains_value(&self, value: &Value) -> bool {
        self.entries.values().any(|existing| existing == value)
    }

    /// Reconstructs each key value; O(n), same as any other "list every
    /// entry" operation.
    pub fn iter(&self) -> impl Iterator<Item = (Value, &Value)> + '_ {
        self.entries.iter().map(|(key, value)| (key.to_value(), value))
    }

    pub fn keys(&self) -> impl Iterator<Item = Value> + '_ {
        self.entries.keys().map(MapKey::to_value)
    }

    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.entries.values()
    }
}

impl From<Vec<(Value, Value)>> for MapValue {
    /// Later duplicate keys win (a real map's semantics), matching what
    /// `.insert()` would produce if called once per pair in order.
    fn from(pairs: Vec<(Value, Value)>) -> Self {
        let mut map = MapValue::new();
        for (key, value) in pairs {
            map.insert(key, value);
        }
        map
    }
}

impl FromIterator<(Value, Value)> for MapValue {
    fn from_iter<I: IntoIterator<Item = (Value, Value)>>(iter: I) -> Self {
        let mut map = MapValue::new();
        for (key, value) in iter {
            map.insert(key, value);
        }
        map
    }
}

impl IntoIterator for MapValue {
    type Item = (Value, Value);
    type IntoIter = std::vec::IntoIter<(Value, Value)>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries
            .into_inner()
            .into_iter()
            .map(|(key, value)| (key.to_value(), value))
            .collect::<Vec<_>>()
            .into_iter()
    }
}

impl PartialEq for MapValue {
    fn eq(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len()
            && self.entries.iter().eq(other.entries.iter())
    }
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
            Value::Args(_) => "Args",
            Value::List(_) => "list",
            Value::Object(_) => "Record",
            Value::Map(_) => "Map",
            Value::Option(_) => "Option",
            Value::Result(_) => "Result",
            Value::Table(_) => "Table",
            Value::Schema(_) => "Schema",
            Value::Error(_) => "error",
            Value::Shell(_) | Value::MixedShell(_) | Value::ShellProgram(_) => "shell",
            Value::Promise(_) => "Promise",
            Value::Resource(_) => "resource",
            Value::Closure(_) | Value::Function(_) => "fn",
        }
    }

    pub fn render_display(&self) -> String {
        self.render_with_context(false)
    }

    fn render_with_context(&self, structural: bool) -> String {
        match self {
            Value::Void => "void".into(),
            Value::Int(value) => value.to_string(),
            Value::Float(value) => value.to_string(),
            Value::Bool(value) => value.to_string(),
            Value::String(value) if structural => format!("{value:?}"),
            Value::String(value) => value.clone(),
            Value::Bytes(values) => format!("Bytes({})", values.len()),
            Value::Args(values) => format!("<args count={}>", values.len()),
            Value::List(values) => format!(
                "[{}]",
                values
                    .iter()
                    .map(|value| value.render_with_context(true))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Value::Object(values) => format!(
                "{{{}}}",
                values
                    .iter()
                    .map(|(key, value)| format!("{}: {}", render_object_key(key), value.render_with_context(true)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Value::Map(entries) => format!(
                "{{{}}}",
                entries
                    .iter()
                    .map(|(key, value)| format!(
                        "{}: {}",
                        key.render_with_context(true),
                        value.render_with_context(true)
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Value::Option(Some(value)) => {
                format!("Some({})", value.render_with_context(true))
            }
            Value::Option(None) => "None".into(),
            Value::Result(Ok(value)) => format!("Ok({})", value.render_with_context(true)),
            Value::Result(Err(value)) => format!("Err({})", value.render_with_context(true)),
            Value::Table(table) => format!("<table rows={}>", table.len()),
            Value::Schema(schema) => format!("<schema fields={}>", schema.fields.len()),
            Value::Error(error) => {
                let ErrorValue { message, kind, code, cause } = &**error;
                let mut rendered = format!(
                    "error(kind: {kind:?}, message: {message:?}, code: {code}"
                );
                if let Some(cause) = cause {
                    rendered.push_str(&format!(", cause: {}", cause.render_with_context(true)));
                }
                rendered.push(')');
                rendered
            }
            Value::Shell(_) | Value::MixedShell(_) | Value::ShellProgram(_) => "<shell>".into(),
            Value::Promise(_) => "<promise>".into(),
            Value::Resource(_) => "<resource>".into(),
            Value::Closure(_) | Value::Function(_) => "<fn>".into(),
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
            ConfigValue::Object(values) => Value::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, Value::from_config(value)))
                    .collect(),
            ),
            ConfigValue::Map(values) => Value::Map(
                values
                    .into_iter()
                    .map(|(key, value)| {
                        (Value::from_config(key), Value::from_config(value))
                    })
                    .collect(),
            ),
            ConfigValue::Option(value) => {
                Value::Option(value.map(|value| Box::new(Value::from_config(*value))))
            }
            ConfigValue::Result(value) => Value::Result(match value {
                Ok(value) => Ok(Box::new(Value::from_config(*value))),
                Err(value) => Err(Box::new(Value::from_config(*value))),
            }),
            ConfigValue::Shell(plan) => Value::Shell(Shared::from(plan)),
            ConfigValue::ShellProgram(program) => Value::ShellProgram(Shared::from(program)),
            ConfigValue::Promise(handle) => Value::Promise(handle),
            ConfigValue::Error {
                message,
                kind,
                code,
                cause,
            } => Value::Error(Box::new(ErrorValue {
                message,
                kind,
                code,
                cause: cause.map(|cause| Box::new(Value::from_config(*cause))),
            })),
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
            Value::Args(_) => false,
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
            Value::Schema(_) | Value::Error(_) => true,
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
            Value::Bytes(values) => Ok(ConfigValue::Object(indexmap::IndexMap::from([(
                "values".into(),
                ConfigValue::List(
                    values
                        .into_iter()
                        .map(|value| ConfigValue::Int(i64::from(value)))
                        .collect(),
                ),
            )]))),
            Value::Args(_) => Err(SparError::EvalError {
                message: "Args values are shell-only and cannot be converted to configuration values".into(),
                span: span.clone(),
            }),
            Value::List(values) => Ok(ConfigValue::List(
                values
                    .into_iter()
                    .map(|value| value.try_into_config(span))
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            Value::Object(values) => Ok(ConfigValue::Object(
                values
                    .into_iter()
                    .map(|(key, value)| value.try_into_config(span).map(|value| (key, value)))
                    .collect::<Result<indexmap::IndexMap<_, _>, _>>()?,
            )),
            Value::Map(values) => Ok(ConfigValue::Map(
                values
                    .into_iter()
                    .map(|(key, value)| {
                        Ok((key.try_into_config(span)?, value.try_into_config(span)?))
                    })
                    .collect::<Result<Vec<_>, SparError>>()?,
            )),
            Value::Option(value) => Ok(ConfigValue::Option(match value {
                Some(value) => Some(Box::new(value.try_into_config(span)?)),
                None => None,
            })),
            Value::Result(value) => Ok(ConfigValue::Result(match value {
                Ok(value) => Ok(Box::new(value.try_into_config(span)?)),
                Err(value) => Err(Box::new(value.try_into_config(span)?)),
            })),
            Value::Table(_) => Err(SparError::EvalError {
                message: "table values cannot be converted to configuration values; serialize them explicitly".into(),
                span: span.clone(),
            }),
            Value::Schema(_) => Err(SparError::EvalError {
                message: "schema values cannot be converted to configuration values directly".into(),
                span: span.clone(),
            }),
            Value::Error(error) => {
                let ErrorValue { message, kind, code, cause } = *error;
                Ok(ConfigValue::Error {
                message,
                kind,
                code,
                cause: match cause {
                    Some(cause) => Some(Box::new(cause.try_into_config(span)?)),
                    None => None,
                },
            })
            }
            Value::Shell(plan) => Ok(ConfigValue::Shell((plan).into_inner())),
            Value::MixedShell(_) => Err(SparError::EvalError {
                message: "mixed shell values are runtime-only and cannot be converted to configuration values".into(),
                span: span.clone(),
            }),
            Value::ShellProgram(program) => Ok(ConfigValue::ShellProgram((program).into_inner())),
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

fn render_object_key(key: &str) -> String {
    let mut chars = key.chars();
    let is_identifier = chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric());
    if is_identifier {
        key.to_string()
    } else {
        format!("{key:?}")
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
    fn display_renderer_handles_sum_and_error_values_without_panicking() {
        let samples = vec![
            Value::Void,
            Value::Int(1),
            Value::Float(1.5),
            Value::Bool(true),
            Value::String("x".into()),
            Value::Bytes(vec![1, 2]),
            Value::List(Shared::from(vec![Value::String("x".into())])),
            Value::Map(vec![(Value::String("k".into()), Value::Int(1))].into()),
            Value::Option(None),
            Value::Option(Some(Box::new(Value::Int(1)))),
            Value::Result(Ok(Box::new(Value::Int(1)))),
            Value::Result(Err(Box::new(Value::String("bad".into())))),
            Value::Error(Box::new(ErrorValue {
                message: "broken".into(),
                kind: "test".into(),
                code: 7,
                cause: None,
            })),
        ];
        for sample in samples {
            let rendered = sample.render_display();
            assert!(!rendered.is_empty(), "{sample:?}");
        }
    }

    #[test]
    fn config_round_trip_preserves_data_values() {
        let source = ConfigValue::Object(indexmap::IndexMap::from([
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
        let error = Value::Closure(Shared::from(closure))
            .try_into_config(&Span::dummy())
            .unwrap_err();
        assert!(error.to_string().contains("function values"));
    }
}
