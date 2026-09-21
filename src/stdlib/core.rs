use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeMethod, NativeRegistry, Value};

use super::support::error;

fn type_parameter(name: &str) -> SparType {
    SparType::TypeParameter(name.into())
}

fn applied(name: &str, arguments: Vec<SparType>) -> SparType {
    SparType::Applied {
        name: name.into(),
        arguments,
    }
}

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry
        .register(NativeFunction::sync(
            "nativeCore",
            "len",
            vec![("value", SparType::TypeParameter("T".into()))],
            SparType::Int,
            true,
            |_context, args| length_value(args),
        ))
        .expect("nativeCore::len registration must be unique");

    register_sum_type_constructors(registry);
    register_collection_methods(registry);
    register_record_methods(registry);
    register_option_methods(registry);
    register_result_methods(registry);
    register_table_methods(registry);
}

fn register_sum_type_constructors(registry: &mut NativeRegistry) {
    let t = type_parameter("T");
    let e = type_parameter("E");
    let option_t = applied("Option", vec![t.clone()]);
    let result_te = applied("Result", vec![t.clone(), e.clone()]);

    registry
        .register(NativeFunction::sync(
            "nativeCore",
            "some",
            vec![("value", t.clone())],
            option_t.clone(),
            true,
            |_context, args| {
                let value = args
                    .first()
                    .cloned()
                    .ok_or_else(|| error("missing native argument 'value'"))?;
                Ok(Value::Option(Some(Box::new(value))))
            },
        ))
        .expect("nativeCore::some registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeCore",
            "none",
            vec![],
            option_t,
            true,
            |_context, _args| Ok(Value::Option(None)),
        ))
        .expect("nativeCore::none registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeCore",
            "ok",
            vec![("value", t)],
            result_te.clone(),
            true,
            |_context, args| {
                let value = args
                    .first()
                    .cloned()
                    .ok_or_else(|| error("missing native argument 'value'"))?;
                Ok(Value::Result(Ok(Box::new(value))))
            },
        ))
        .expect("nativeCore::ok registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeCore",
            "err",
            vec![("error", e)],
            result_te,
            true,
            |_context, args| {
                let value = args
                    .first()
                    .cloned()
                    .ok_or_else(|| error("missing native argument 'error'"))?;
                Ok(Value::Result(Err(Box::new(value))))
            },
        ))
        .expect("nativeCore::err registration must be unique");
}

fn register_collection_methods(registry: &mut NativeRegistry) {
    registry
        .register_method(NativeMethod::sync(
            "str",
            "length",
            SparType::Str,
            vec![],
            SparType::Int,
            false,
            |_context, args| length_value(args),
        ))
        .expect("str.length registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "str",
            "isEmpty",
            SparType::Str,
            vec![],
            SparType::Bool,
            false,
            |_context, args| empty_value(args),
        ))
        .expect("str.isEmpty registration must be unique");

    let t = type_parameter("T");
    let list_t = SparType::List(Box::new(t.clone()));
    registry
        .register_method(NativeMethod::sync(
            "List",
            "length",
            list_t.clone(),
            vec![],
            SparType::Int,
            false,
            |_context, args| length_value(args),
        ))
        .expect("List.length registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "List",
            "isEmpty",
            list_t,
            vec![],
            SparType::Bool,
            false,
            |_context, args| empty_value(args),
        ))
        .expect("List.isEmpty registration must be unique");

    let bytes = SparType::Named("Bytes".into());
    registry
        .register_method(NativeMethod::sync(
            "Bytes",
            "length",
            bytes.clone(),
            vec![],
            SparType::Int,
            false,
            |_context, args| length_value(args),
        ))
        .expect("Bytes.length registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "Bytes",
            "isEmpty",
            bytes,
            vec![],
            SparType::Bool,
            false,
            |_context, args| empty_value(args),
        ))
        .expect("Bytes.isEmpty registration must be unique");

    let k = type_parameter("K");
    let v = type_parameter("V");
    let map = applied("Map", vec![k.clone(), v.clone()]);
    registry
        .register_method(NativeMethod::sync(
            "Map",
            "length",
            map.clone(),
            vec![],
            SparType::Int,
            false,
            |_context, args| length_value(args),
        ))
        .expect("Map.length registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "Map",
            "isEmpty",
            map.clone(),
            vec![],
            SparType::Bool,
            false,
            |_context, args| empty_value(args),
        ))
        .expect("Map.isEmpty registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "Map",
            "keys",
            map.clone(),
            vec![],
            SparType::List(Box::new(k.clone())),
            false,
            |_context, args| map_keys(args),
        ))
        .expect("Map.keys registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "Map",
            "values",
            map.clone(),
            vec![],
            SparType::List(Box::new(v)),
            false,
            |_context, args| map_values(args),
        ))
        .expect("Map.values registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "Map",
            "containsKey",
            map,
            vec![("key", k)],
            SparType::Bool,
            false,
            |_context, args| map_contains_key(args),
        ))
        .expect("Map.containsKey registration must be unique");
}

fn register_option_methods(registry: &mut NativeRegistry) {
    let t = type_parameter("T");
    let option_t = applied("Option", vec![t.clone()]);
    let methods = [
        NativeMethod::sync(
            "Option",
            "isSome",
            option_t.clone(),
            vec![],
            SparType::Bool,
            false,
            |_context, args| Ok(Value::Bool(option_receiver(args)?.is_some())),
        ),
        NativeMethod::sync(
            "Option",
            "isNone",
            option_t.clone(),
            vec![],
            SparType::Bool,
            false,
            |_context, args| Ok(Value::Bool(option_receiver(args)?.is_none())),
        ),
        NativeMethod::sync(
            "Option",
            "unwrap",
            option_t.clone(),
            vec![],
            t.clone(),
            false,
            |_context, args| {
                option_receiver(args)?
                    .as_deref()
                    .cloned()
                    .ok_or_else(|| error("cannot unwrap None"))
            },
        ),
        NativeMethod::sync(
            "Option",
            "unwrapOr",
            option_t,
            vec![("fallback", t.clone())],
            t,
            false,
            |_context, args| {
                let fallback = args
                    .get(1)
                    .cloned()
                    .ok_or_else(|| error("missing Option.unwrapOr fallback"))?;
                Ok(option_receiver(args)?
                    .as_deref()
                    .cloned()
                    .unwrap_or(fallback))
            },
        ),
    ];

    for method in methods {
        let name = method.name.clone();
        registry
            .register_method(method)
            .unwrap_or_else(|_| panic!("Option.{name} registration must be unique"));
    }
}

fn register_result_methods(registry: &mut NativeRegistry) {
    let t = type_parameter("T");
    let e = type_parameter("E");
    let result_te = applied("Result", vec![t.clone(), e.clone()]);
    let methods = [
        NativeMethod::sync(
            "Result",
            "isOk",
            result_te.clone(),
            vec![],
            SparType::Bool,
            false,
            |_context, args| Ok(Value::Bool(result_receiver(args)?.is_ok())),
        ),
        NativeMethod::sync(
            "Result",
            "isErr",
            result_te.clone(),
            vec![],
            SparType::Bool,
            false,
            |_context, args| Ok(Value::Bool(result_receiver(args)?.is_err())),
        ),
        NativeMethod::sync(
            "Result",
            "unwrap",
            result_te.clone(),
            vec![],
            t.clone(),
            false,
            |_context, args| match result_receiver(args)? {
                Ok(value) => Ok(value.as_ref().clone()),
                Err(_) => Err(error("cannot unwrap Err")),
            },
        ),
        NativeMethod::sync(
            "Result",
            "unwrapErr",
            result_te.clone(),
            vec![],
            e,
            false,
            |_context, args| match result_receiver(args)? {
                Ok(_) => Err(error("cannot unwrapErr Ok")),
                Err(value) => Ok(value.as_ref().clone()),
            },
        ),
        NativeMethod::sync(
            "Result",
            "unwrapOr",
            result_te,
            vec![("fallback", t.clone())],
            t,
            false,
            |_context, args| {
                let fallback = args
                    .get(1)
                    .cloned()
                    .ok_or_else(|| error("missing Result.unwrapOr fallback"))?;
                match result_receiver(args)? {
                    Ok(value) => Ok(value.as_ref().clone()),
                    Err(_) => Ok(fallback),
                }
            },
        ),
    ];

    for method in methods {
        let name = method.name.clone();
        registry
            .register_method(method)
            .unwrap_or_else(|_| panic!("Result.{name} registration must be unique"));
    }
}

fn register_table_methods(registry: &mut NativeRegistry) {
    let table_t = applied("Table", vec![type_parameter("T")]);

    registry
        .register_method(NativeMethod::sync(
            "Table",
            "length",
            table_t.clone(),
            vec![],
            SparType::Int,
            false,
            |_context, args| table_length(args),
        ))
        .expect("Table.length registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Table",
            "isEmpty",
            table_t.clone(),
            vec![],
            SparType::Bool,
            false,
            |_context, args| table_is_empty(args),
        ))
        .expect("Table.isEmpty registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Table",
            "rows",
            table_t.clone(),
            vec![],
            SparType::List(Box::new(type_parameter("T"))),
            false,
            |_context, args| table_rows(args),
        ))
        .expect("Table.rows registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Table",
            "columns",
            table_t,
            vec![],
            SparType::List(Box::new(SparType::Str)),
            false,
            |_context, args| table_columns(args),
        ))
        .expect("Table.columns registration must be unique");
}

fn length_value(args: &[Value]) -> Result<Value, crate::SparError> {
    let value = args
        .first()
        .ok_or_else(|| error("missing method receiver"))?;
    let length = match value {
        Value::String(value) => value.chars().count(),
        Value::Bytes(value) => value.len(),
        Value::List(value) => value.len(),
        Value::Object(value) => value.len(),
        Value::Map(value) => value.len(),
        Value::Table(value) => value.len(),
        other => {
            return Err(error(format!(
                "length() does not support runtime value {}",
                other.type_name()
            )))
        }
    };
    i64::try_from(length)
        .map(Value::Int)
        .map_err(|_| error("value length exceeds Spar int range"))
}

fn empty_value(args: &[Value]) -> Result<Value, crate::SparError> {
    let value = args
        .first()
        .ok_or_else(|| error("missing method receiver"))?;
    let empty = match value {
        Value::String(value) => value.is_empty(),
        Value::Bytes(value) => value.is_empty(),
        Value::List(value) => value.is_empty(),
        Value::Object(value) => value.is_empty(),
        Value::Map(value) => value.is_empty(),
        Value::Table(value) => value.is_empty(),
        other => {
            return Err(error(format!(
                "isEmpty() does not support runtime value {}",
                other.type_name()
            )))
        }
    };
    Ok(Value::Bool(empty))
}

fn map_receiver(args: &[Value]) -> Result<&[(Value, Value)], crate::SparError> {
    match args.first() {
        Some(Value::Map(entries)) => Ok(entries),
        Some(other) => Err(error(format!(
            "Map method received runtime value {}",
            other.type_name()
        ))),
        None => Err(error("missing Map method receiver")),
    }
}

fn map_keys(args: &[Value]) -> Result<Value, crate::SparError> {
    Ok(Value::List(
        map_receiver(args)?
            .iter()
            .map(|(key, _)| key.clone())
            .collect(),
    ))
}

fn map_values(args: &[Value]) -> Result<Value, crate::SparError> {
    Ok(Value::List(
        map_receiver(args)?
            .iter()
            .map(|(_, value)| value.clone())
            .collect(),
    ))
}

fn map_contains_key(args: &[Value]) -> Result<Value, crate::SparError> {
    let key = args
        .get(1)
        .ok_or_else(|| error("missing Map.containsKey key"))?;
    Ok(Value::Bool(
        map_receiver(args)?
            .iter()
            .any(|(existing, _)| existing == key),
    ))
}

fn option_receiver(args: &[Value]) -> Result<&std::option::Option<Box<Value>>, crate::SparError> {
    match args.first() {
        Some(Value::Option(value)) => Ok(value),
        Some(other) => Err(error(format!(
            "Option method received runtime value {}",
            other.type_name()
        ))),
        None => Err(error("missing Option method receiver")),
    }
}

fn result_receiver(
    args: &[Value],
) -> Result<&std::result::Result<Box<Value>, Box<Value>>, crate::SparError> {
    match args.first() {
        Some(Value::Result(value)) => Ok(value),
        Some(other) => Err(error(format!(
            "Result method received runtime value {}",
            other.type_name()
        ))),
        None => Err(error("missing Result method receiver")),
    }
}

fn table_receiver(args: &[Value]) -> Result<&crate::runtime::TableValue, crate::SparError> {
    match args.first() {
        Some(Value::Table(table)) => Ok(table),
        Some(other) => Err(error(format!(
            "Table method received runtime value {}",
            other.type_name()
        ))),
        None => Err(error("missing table method receiver")),
    }
}

fn table_length(args: &[Value]) -> Result<Value, crate::SparError> {
    i64::try_from(table_receiver(args)?.len())
        .map(Value::Int)
        .map_err(|_| error("table length exceeds Spar int range"))
}

fn table_is_empty(args: &[Value]) -> Result<Value, crate::SparError> {
    Ok(Value::Bool(table_receiver(args)?.is_empty()))
}

fn table_rows(args: &[Value]) -> Result<Value, crate::SparError> {
    Ok(Value::List(table_receiver(args)?.rows().to_vec()))
}

fn table_columns(args: &[Value]) -> Result<Value, crate::SparError> {
    Ok(Value::List(
        table_receiver(args)?
            .schema()
            .columns()
            .map(|column| Value::String(column.to_string()))
            .collect(),
    ))
}

/// `Record` is the dynamic bridge type: a field read from a Record is itself a
/// Record-typed dynamic value, and these methods turn dynamic values into
/// statically typed ones. Every conversion is checked at runtime.
fn register_record_methods(registry: &mut NativeRegistry) {
    let record = SparType::Named("Record".into());

    registry
        .register_method(NativeMethod::sync(
            "Record",
            "has",
            record.clone(),
            vec![("name", SparType::Str)],
            SparType::Bool,
            false,
            |_context, args| {
                let name = super::support::string_arg(args, 1, "name")?;
                match args.first() {
                    Some(Value::Object(fields)) => Ok(Value::Bool(fields.contains_key(name))),
                    Some(other) => Err(error(format!(
                        "Record.has expected an object, found {}",
                        other.type_name()
                    ))),
                    None => Err(error("Record.has is missing its receiver")),
                }
            },
        ))
        .expect("Record.has registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Record",
            "keys",
            record.clone(),
            vec![],
            SparType::List(Box::new(SparType::Str)),
            false,
            |_context, args| match args.first() {
                Some(Value::Object(fields)) => {
                    let mut keys = fields.keys().cloned().collect::<Vec<_>>();
                    keys.sort();
                    Ok(Value::List(keys.into_iter().map(Value::String).collect()))
                }
                Some(other) => Err(error(format!(
                    "Record.keys expected an object, found {}",
                    other.type_name()
                ))),
                None => Err(error("Record.keys is missing its receiver")),
            },
        ))
        .expect("Record.keys registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Record",
            "asStr",
            record.clone(),
            vec![],
            SparType::Str,
            false,
            |_context, args| match args.first() {
                Some(value @ Value::String(_)) => Ok(value.clone()),
                other => Err(dynamic_mismatch("asStr", "str", other)),
            },
        ))
        .expect("Record.asStr registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Record",
            "asInt",
            record.clone(),
            vec![],
            SparType::Int,
            false,
            |_context, args| match args.first() {
                Some(value @ Value::Int(_)) => Ok(value.clone()),
                other => Err(dynamic_mismatch("asInt", "int", other)),
            },
        ))
        .expect("Record.asInt registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Record",
            "asFloat",
            record.clone(),
            vec![],
            SparType::Float,
            false,
            |_context, args| match args.first() {
                Some(value @ Value::Float(_)) => Ok(value.clone()),
                // An integer widens to a float; the reverse never happens.
                Some(Value::Int(value)) => Ok(Value::Float(*value as f64)),
                other => Err(dynamic_mismatch("asFloat", "float", other)),
            },
        ))
        .expect("Record.asFloat registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Record",
            "asBool",
            record,
            vec![],
            SparType::Bool,
            false,
            |_context, args| match args.first() {
                Some(value @ Value::Bool(_)) => Ok(value.clone()),
                other => Err(dynamic_mismatch("asBool", "bool", other)),
            },
        ))
        .expect("Record.asBool registration must be unique");
}

fn dynamic_mismatch(method: &str, expected: &str, found: Option<&Value>) -> crate::SparError {
    error(format!(
        "{method}() expected a {expected} value, found {}",
        found.map_or("nothing", Value::type_name)
    ))
}
