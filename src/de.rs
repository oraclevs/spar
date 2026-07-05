use std::collections::HashMap;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};

use crate::evaluator::{ConfigValue, EvalResult};

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum SparDeserError {
    Pipeline(Vec<crate::error::SparError>),
    Serde(String),
}

impl SparDeserError {
    pub fn messages(&self) -> Vec<String> {
        match self {
            SparDeserError::Pipeline(es) => es.iter().map(|e| e.to_string()).collect(),
            SparDeserError::Serde(s)     => vec![s.clone()],
        }
    }
}

impl de::Error for SparDeserError {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        SparDeserError::Serde(msg.to_string())
    }
}

impl std::fmt::Display for SparDeserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SparDeserError::Pipeline(es) => {
                for e in es { writeln!(f, "{e}")?; }
                Ok(())
            }
            SparDeserError::Serde(s) => write!(f, "deserialization error: {s}"),
        }
    }
}

impl std::error::Error for SparDeserError {}

// ── Public API ────────────────────────────────────────────────────────────────

pub fn from_str<T: serde::de::DeserializeOwned>(src: &str) -> Result<T, SparDeserError> {
    use crate::{Lexer, Parser};
    use crate::resolver::Resolver;
    use crate::typechecker::TypeChecker;
    use crate::evaluator::Evaluator;

    let tokens  = Lexer::new(src).tokenize()
                    .map_err(|e| SparDeserError::Pipeline(vec![e]))?;
    let program = Parser::new(tokens).parse()
                    .map_err(|e| SparDeserError::Pipeline(vec![e]))?;
    let symbols = Resolver::new().resolve(&program, &[])
                    .map_err(SparDeserError::Pipeline)?;
    TypeChecker::check(&program, &symbols)
                    .map_err(SparDeserError::Pipeline)?;
    let result  = Evaluator::evaluate(&program, &symbols)
                    .map_err(SparDeserError::Pipeline)?;
    from_eval(&result)
}

pub fn from_eval<T: serde::de::DeserializeOwned>(result: &EvalResult) -> Result<T, SparDeserError> {
    T::deserialize(SparDeserializer {
        globals:  &result.globals,
        sections: &result.sections,
    })
}

// ── SparDeserializer (top-level) ────────────────────────────────────────────────

struct SparDeserializer<'de> {
    globals:  &'de HashMap<String, ConfigValue>,
    sections: &'de HashMap<Vec<String>, HashMap<String, ConfigValue>>,
}

impl<'de> de::Deserializer<'de> for SparDeserializer<'de> {
    type Error = SparDeserError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        self.deserialize_map(visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        visitor.visit_map(RootMapAccess::new(self.globals, self.sections))
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name:   &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, SparDeserError> {
        self.deserialize_map(visitor)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct enum identifier ignored_any
    }
}

// ── StringDeserializer (map keys) ─────────────────────────────────────────────

struct StringDeserializer<'de> {
    value: &'de str,
}

impl<'de> de::Deserializer<'de> for StringDeserializer<'de> {
    type Error = SparDeserError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        visitor.visit_str(self.value)
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        visitor.visit_str(self.value)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        visitor.visit_str(self.value)
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        visitor.visit_str(self.value)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum ignored_any
    }
}

// ── ValueDeserializer (wraps a ConfigValue) ───────────────────────────────────

struct ValueDeserializer<'de> {
    value: &'de ConfigValue,
}

impl<'de> de::Deserializer<'de> for ValueDeserializer<'de> {
    type Error = SparDeserError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        match self.value {
            ConfigValue::Str(s)   => visitor.visit_str(s),
            ConfigValue::Int(n)   => visitor.visit_i64(*n),
            ConfigValue::Float(f) => visitor.visit_f64(*f),
            ConfigValue::Bool(b)  => visitor.visit_bool(*b),
            ConfigValue::List(vs) => visitor.visit_seq(ListSeqAccess { iter: vs.iter() }),
            ConfigValue::Section(map) => visitor.visit_map(SectionMapAccess {
                iter: map.iter(),
                next_value: None,
            }),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, SparDeserError> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_i32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        if let ConfigValue::Int(n) = self.value {
            visitor.visit_i32(*n as i32)
        } else {
            self.deserialize_any(visitor)
        }
    }

    fn deserialize_i64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        if let ConfigValue::Int(n) = self.value {
            visitor.visit_i64(*n)
        } else {
            self.deserialize_any(visitor)
        }
    }

    fn deserialize_u64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        if let ConfigValue::Int(n) = self.value {
            visitor.visit_u64(*n as u64)
        } else {
            self.deserialize_any(visitor)
        }
    }

    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        match self.value {
            ConfigValue::Float(f) => visitor.visit_f64(*f),
            ConfigValue::Int(n)   => visitor.visit_f64(*n as f64),
            _                     => self.deserialize_any(visitor),
        }
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        if let ConfigValue::Str(s) = self.value {
            visitor.visit_str(s)
        } else {
            self.deserialize_any(visitor)
        }
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        self.deserialize_str(visitor)
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        if let ConfigValue::Bool(b) = self.value {
            visitor.visit_bool(*b)
        } else {
            self.deserialize_any(visitor)
        }
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        if let ConfigValue::List(vs) = self.value {
            visitor.visit_seq(ListSeqAccess { iter: vs.iter() })
        } else {
            self.deserialize_any(visitor)
        }
    }

    serde::forward_to_deserialize_any! {
        i8 i16 i128 u8 u16 u32 u128 f32 char
        bytes byte_buf unit unit_struct tuple tuple_struct map struct enum
        identifier ignored_any
    }
}

// ── SectionDeserializer ───────────────────────────────────────────────────────

struct SectionDeserializer<'de> {
    fields: &'de HashMap<String, ConfigValue>,
}

impl<'de> de::Deserializer<'de> for SectionDeserializer<'de> {
    type Error = SparDeserError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        self.deserialize_map(visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        visitor.visit_map(SectionMapAccess {
            iter:       self.fields.iter(),
            next_value: None,
        })
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name:   &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, SparDeserError> {
        self.deserialize_map(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, SparDeserError> {
        visitor.visit_some(self)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct newtype_struct seq tuple
        tuple_struct enum identifier ignored_any
    }
}

// ── RootMapAccess ─────────────────────────────────────────────────────────────

enum RootEntry<'de> {
    Value(&'de ConfigValue),
    Section(&'de HashMap<String, ConfigValue>),
}

struct RootMapAccess<'de> {
    entries: Vec<(&'de str, RootEntry<'de>)>,
    index:   usize,
}

impl<'de> RootMapAccess<'de> {
    fn new(
        globals:  &'de HashMap<String, ConfigValue>,
        sections: &'de HashMap<Vec<String>, HashMap<String, ConfigValue>>,
    ) -> Self {
        let mut entries: Vec<(&'de str, RootEntry<'de>)> = Vec::new();

        for (k, v) in globals {
            entries.push((k.as_str(), RootEntry::Value(v)));
        }
        for (path, fields) in sections {
            if path.len() == 1 {
                entries.push((path[0].as_str(), RootEntry::Section(fields)));
            }
            // Multi-segment sections not exposed at root — dot-joined keys
            // don't map to struct field names.
        }

        RootMapAccess { entries, index: 0 }
    }
}

impl<'de> MapAccess<'de> for RootMapAccess<'de> {
    type Error = SparDeserError;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, SparDeserError> {
        if self.index >= self.entries.len() {
            return Ok(None);
        }
        let key = self.entries[self.index].0;
        seed.deserialize(StringDeserializer { value: key }).map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, SparDeserError> {
        let entry = &self.entries[self.index];
        self.index += 1;
        match &entry.1 {
            RootEntry::Value(cv)    => seed.deserialize(ValueDeserializer { value: cv }),
            RootEntry::Section(fds) => seed.deserialize(SectionDeserializer { fields: fds }),
        }
    }
}

// ── SectionMapAccess ──────────────────────────────────────────────────────────

struct SectionMapAccess<'de> {
    iter:       std::collections::hash_map::Iter<'de, String, ConfigValue>,
    next_value: Option<&'de ConfigValue>,
}

impl<'de> MapAccess<'de> for SectionMapAccess<'de> {
    type Error = SparDeserError;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, SparDeserError> {
        match self.iter.next() {
            None         => Ok(None),
            Some((k, v)) => {
                self.next_value = Some(v);
                seed.deserialize(StringDeserializer { value: k.as_str() }).map(Some)
            }
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, SparDeserError> {
        let val = self.next_value.take().expect("next_value_seed called before next_key_seed");
        seed.deserialize(ValueDeserializer { value: val })
    }
}

// ── ListSeqAccess ─────────────────────────────────────────────────────────────

struct ListSeqAccess<'de> {
    iter: std::slice::Iter<'de, ConfigValue>,
}

impl<'de> SeqAccess<'de> for ListSeqAccess<'de> {
    type Error = SparDeserError;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, SparDeserError> {
        match self.iter.next() {
            None    => Ok(None),
            Some(v) => seed.deserialize(ValueDeserializer { value: v }).map(Some),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize, Debug, PartialEq)]
    struct Full { port: i64, name: String, active: bool }

    #[derive(Deserialize, Debug, PartialEq)]
    struct WithFloat { ratio: f64 }

    #[derive(Deserialize, Debug, PartialEq)]
    struct WithIntList { ports: Vec<i64> }

    #[derive(Deserialize, Debug, PartialEq)]
    struct WithStrList { names: Vec<String> }

    #[derive(Deserialize, Debug, PartialEq)]
    struct WithSection { port: i64, #[serde(rename = "Database")] database: DbConfig }

    #[derive(Deserialize, Debug, PartialEq)]
    struct DbConfig { host: String, pool: i64 }

    #[derive(Deserialize, Debug, PartialEq)]
    struct WithOpt { required: String, maybe: Option<i64> }

    #[derive(Deserialize, Debug, PartialEq)]
    struct BoolField { flag: bool }

    #[derive(Deserialize, Debug, PartialEq)]
    struct TimeoutField { timeout: i64 }

    #[derive(Deserialize, Debug, PartialEq)]
    struct ModeField { mode: String }

    #[test]
    fn test_simple_struct() {
        let src = r#"
            var port: int   = 3000;
            var name: str   = "keel";
            var active: bool = true;
        "#;
        let cfg: Full = from_str(src).expect("should deserialize");
        assert_eq!(cfg.port, 3000);
        assert_eq!(cfg.name, "keel");
        assert!(cfg.active);
    }

    #[test]
    fn test_float_field() {
        let cfg: WithFloat = from_str("var ratio: float = 2.5;").unwrap();
        assert!((cfg.ratio - 2.5).abs() < 1e-10);
    }

    #[test]
    fn test_int_list() {
        let cfg: WithIntList = from_str("var ports: [int] = [3000, 8080, 9090];").unwrap();
        assert_eq!(cfg.ports, vec![3000i64, 8080, 9090]);
    }

    #[test]
    fn test_str_list() {
        let cfg: WithStrList = from_str(r#"var names: [str] = ["alice", "bob"];"#).unwrap();
        assert_eq!(cfg.names, vec!["alice", "bob"]);
    }

    #[test]
    fn test_nested_section() {
        let src = r#"
            var port: int = 8080;
            [Database]{ host: str = "localhost"; pool: int = 5; };
        "#;
        let cfg: WithSection = from_str(src).unwrap();
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.database.host, "localhost");
        assert_eq!(cfg.database.pool, 5);
    }

    #[test]
    fn test_optional_field_present() {
        let src = r#"var required: str = "hello"; var maybe: int = 42;"#;
        let cfg: WithOpt = from_str(src).unwrap();
        assert_eq!(cfg.required, "hello");
        assert_eq!(cfg.maybe, Some(42));
    }

    #[test]
    fn test_optional_field_absent() {
        let src = r#"var required: str = "hello";"#;
        let cfg: WithOpt = from_str(src).unwrap();
        assert_eq!(cfg.required, "hello");
        assert_eq!(cfg.maybe, None);
    }

    #[test]
    fn test_evaluated_expression() {
        let cfg: TimeoutField = from_str("var timeout: int = 30 * 3;").unwrap();
        assert_eq!(cfg.timeout, 90);
    }

    #[test]
    fn test_env_fallback() {
        std::env::remove_var("SPAR_DE_MISSING");
        let src = r#"var mode: str = env("SPAR_DE_MISSING") ?? "production";"#;
        let cfg: ModeField = from_str(src).unwrap();
        assert_eq!(cfg.mode, "production");
    }

    #[test]
    fn test_bool_false() {
        let cfg: BoolField = from_str("var flag: bool = false;").unwrap();
        assert!(!cfg.flag);
    }

    #[test]
    fn test_from_eval_direct() {
        use crate::evaluator::ConfigValue;
        let mut globals = std::collections::HashMap::new();
        globals.insert("port".into(), ConfigValue::Int(9000));
        let result = crate::evaluator::EvalResult {
            globals,
            sections: std::collections::HashMap::new(),
            warnings: vec![],
        };
        #[derive(Deserialize)]
        struct P { port: i64 }
        let p: P = from_eval(&result).unwrap();
        assert_eq!(p.port, 9000);
    }

    #[test]
    fn test_parse_error_propagated() {
        let result = from_str::<Full>("var port int = 3000;");
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_error_propagated() {
        let result = from_str::<Full>("var port: int = undefined_var;");
        assert!(result.is_err());
    }

    #[test]
    fn test_interpolated_string() {
        let src = r#"
            var host: str = "localhost";
            var url: str  = "http://${global::host}";
        "#;
        #[derive(Deserialize)]
        struct U { url: String }
        let u: U = from_str(src).unwrap();
        assert_eq!(u.url, "http://localhost");
    }

    #[test]
    fn test_error_messages_non_empty() {
        let err = from_str::<Full>("BROKEN SOURCE ???").unwrap_err();
        assert!(!err.messages().is_empty());
    }
}
