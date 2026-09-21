use super::{Schema, SchemaInferenceError, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct TableValue {
    rows: Vec<Value>,
    schema: Schema,
}

impl TableValue {
    pub fn from_records(rows: Vec<Value>) -> Result<Self, SchemaInferenceError> {
        let schema = Schema::infer_records(&rows)?;
        Ok(Self { rows, schema })
    }

    pub fn with_schema(rows: Vec<Value>, schema: Schema) -> Self {
        Self { rows, schema }
    }

    pub fn rows(&self) -> &[Value] {
        &self.rows
    }

    pub fn into_rows(self) -> Vec<Value> {
        self.rows
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn take(&self, count: usize) -> Self {
        Self {
            rows: self.rows.iter().take(count).cloned().collect(),
            schema: self.schema.clone(),
        }
    }

    pub fn skip(&self, count: usize) -> Self {
        Self {
            rows: self.rows.iter().skip(count).cloned().collect(),
            schema: self.schema.clone(),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    fn row(name: &str, age: i64) -> Value {
        Value::Object(indexmap::IndexMap::from([
            ("name".into(), Value::String(name.into())),
            ("age".into(), Value::Int(age)),
        ]))
    }

    #[test]
    fn table_preserves_schema_through_take_and_skip() {
        let table = TableValue::from_records(vec![row("Obi", 24), row("Ada", 31)]).unwrap();
        let take = table.take(1);
        let skip = table.skip(1);

        assert_eq!(take.rows(), &[row("Obi", 24)]);
        assert_eq!(skip.rows(), &[row("Ada", 31)]);
        assert_eq!(take.schema(), table.schema());
        assert_eq!(skip.schema(), table.schema());
    }
}
