use std::collections::BTreeMap;

use super::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaType {
    Void,
    Int,
    Float,
    Bool,
    Str,
    Bytes,
    List(Box<SchemaType>),
    Record,
    Map,
    Table,
    Schema,
    Error,
    Shell,
    Promise,
    Resource,
    Function,
    Dynamic,
}

impl SchemaType {
    pub fn display_name(&self) -> String {
        match self {
            Self::Void => "void".into(),
            Self::Int => "int".into(),
            Self::Float => "float".into(),
            Self::Bool => "bool".into(),
            Self::Str => "str".into(),
            Self::Bytes => "Bytes".into(),
            Self::List(inner) => format!("List<{}>", inner.display_name()),
            Self::Record => "Record".into(),
            Self::Map => "Map".into(),
            Self::Table => "Table".into(),
            Self::Schema => "Schema".into(),
            Self::Error => "error".into(),
            Self::Shell => "shell".into(),
            Self::Promise => "Promise".into(),
            Self::Resource => "resource".into(),
            Self::Function => "fn".into(),
            Self::Dynamic => "dynamic".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaField {
    pub name: String,
    pub ty: SchemaType,
    pub optional: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Schema {
    pub fields: Vec<SchemaField>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaInferenceError {
    pub row_index: usize,
    pub actual_type: &'static str,
}

impl std::fmt::Display for SchemaInferenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "schema row {} must be a Record, found {}",
            self.row_index, self.actual_type
        )
    }
}

impl std::error::Error for SchemaInferenceError {}

impl Schema {
    /// Infer a deterministic schema from dynamic record rows.
    ///
    /// Columns are sorted by name because `Value::Object` intentionally uses
    /// `HashMap` storage and therefore does not carry insertion order. A field
    /// is optional when at least one row omits it. Conflicting observed value
    /// types are represented as `dynamic` rather than discarding the column.
    pub fn infer_records(rows: &[Value]) -> Result<Self, SchemaInferenceError> {
        #[derive(Clone)]
        struct Accumulator {
            seen: usize,
            ty: SchemaType,
        }

        let mut fields: BTreeMap<String, Accumulator> = BTreeMap::new();
        for (row_index, row) in rows.iter().enumerate() {
            let Value::Object(values) = row else {
                return Err(SchemaInferenceError {
                    row_index,
                    actual_type: row.type_name(),
                });
            };
            for (name, value) in values {
                let observed = schema_type_of(value);
                fields
                    .entry(name.clone())
                    .and_modify(|field| {
                        field.seen += 1;
                        field.ty = merge_types(&field.ty, &observed);
                    })
                    .or_insert(Accumulator {
                        seen: 1,
                        ty: observed,
                    });
            }
        }

        Ok(Self {
            fields: fields
                .into_iter()
                .map(|(name, field)| SchemaField {
                    name,
                    ty: field.ty,
                    optional: field.seen != rows.len(),
                })
                .collect(),
        })
    }

    pub fn columns(&self) -> impl Iterator<Item = &str> {
        self.fields.iter().map(|field| field.name.as_str())
    }
}

pub fn schema_type_of(value: &Value) -> SchemaType {
    match value {
        Value::Void => SchemaType::Void,
        Value::Int(_) => SchemaType::Int,
        Value::Float(_) => SchemaType::Float,
        Value::Bool(_) => SchemaType::Bool,
        Value::String(_) => SchemaType::Str,
        Value::Bytes(_) => SchemaType::Bytes,
        Value::List(values) => {
            let mut iter = values.iter();
            let Some(first) = iter.next() else {
                return SchemaType::List(Box::new(SchemaType::Dynamic));
            };
            let mut item_ty = schema_type_of(first);
            for value in iter {
                item_ty = merge_types(&item_ty, &schema_type_of(value));
            }
            SchemaType::List(Box::new(item_ty))
        }
        Value::Object(_) => SchemaType::Record,
        Value::Map(_) => SchemaType::Map,
        Value::Option(_) | Value::Result(_) => SchemaType::Dynamic,
        Value::Table(_) => SchemaType::Table,
        Value::Schema(_) => SchemaType::Schema,
        Value::Error { .. } => SchemaType::Error,
        Value::Shell(_) | Value::MixedShell(_) | Value::ShellProgram(_) => SchemaType::Shell,
        Value::Promise(_) => SchemaType::Promise,
        Value::Resource(_) => SchemaType::Resource,
        Value::Closure(_) | Value::Function(_) => SchemaType::Function,
    }
}

fn merge_types(left: &SchemaType, right: &SchemaType) -> SchemaType {
    if left == right {
        return left.clone();
    }
    match (left, right) {
        (SchemaType::List(left), SchemaType::List(right)) => {
            SchemaType::List(Box::new(merge_types(left, right)))
        }
        _ => SchemaType::Dynamic,
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn heterogeneous_records_infer_optional_fields_in_deterministic_order() {
        let rows = vec![
            Value::Object(indexmap::IndexMap::from([
                ("name".into(), Value::String("Obi".into())),
                ("age".into(), Value::Int(24)),
            ])),
            Value::Object(indexmap::IndexMap::from([
                ("name".into(), Value::String("Ada".into())),
                ("active".into(), Value::Bool(true)),
            ])),
        ];

        let schema = Schema::infer_records(&rows).unwrap();
        assert_eq!(
            schema.fields,
            vec![
                SchemaField {
                    name: "active".into(),
                    ty: SchemaType::Bool,
                    optional: true,
                },
                SchemaField {
                    name: "age".into(),
                    ty: SchemaType::Int,
                    optional: true,
                },
                SchemaField {
                    name: "name".into(),
                    ty: SchemaType::Str,
                    optional: false,
                },
            ]
        );
    }

    #[test]
    fn conflicting_field_types_become_dynamic() {
        let rows = vec![
            Value::Object(indexmap::IndexMap::from([("value".into(), Value::Int(1))])),
            Value::Object(indexmap::IndexMap::from([(
                "value".into(),
                Value::String("one".into()),
            )])),
        ];
        let schema = Schema::infer_records(&rows).unwrap();
        assert_eq!(schema.fields[0].ty, SchemaType::Dynamic);
        assert!(!schema.fields[0].optional);
    }

    #[test]
    fn empty_rows_have_an_empty_schema() {
        assert_eq!(Schema::infer_records(&[]).unwrap(), Schema::default());
    }
}
