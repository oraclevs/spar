use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeRegistry, Value};

use super::support::{string_arg, string_list_arg};

pub(crate) fn register(registry: &mut NativeRegistry) {
    let transforms: [(&str, fn(&str) -> String); 3] = [
        ("trim", |value: &str| value.trim().to_string()),
        ("lower", |value: &str| value.to_lowercase()),
        ("upper", |value: &str| value.to_uppercase()),
    ];
    for (name, transform) in transforms {
        registry.register(NativeFunction::sync(
            "nativeText", name, vec![("value", SparType::Str)], SparType::Str, true,
            move |_context, args| Ok(Value::String(transform(string_arg(args, 0, "value")?))),
        )).expect("nativeText unary registration must be unique");
    }
    let predicates: [(&str, fn(&str, &str) -> bool); 3] = [
        ("contains", |value: &str, needle: &str| value.contains(needle)),
        ("startsWith", |value: &str, needle: &str| value.starts_with(needle)),
        ("endsWith", |value: &str, needle: &str| value.ends_with(needle)),
    ];
    for (name, predicate) in predicates {
        registry.register(NativeFunction::sync(
            "nativeText", name,
            vec![("value", SparType::Str), ("needle", SparType::Str)],
            SparType::Bool,
            true,
            move |_context, args| Ok(Value::Bool(predicate(string_arg(args, 0, "value")?, string_arg(args, 1, "needle")?))),
        )).expect("nativeText predicate registration must be unique");
    }
    registry.register(NativeFunction::sync(
        "nativeText", "replace",
        vec![("value", SparType::Str), ("from", SparType::Str), ("to", SparType::Str)],
        SparType::Str, true,
        |_context, args| Ok(Value::String(string_arg(args, 0, "value")?.replace(string_arg(args, 1, "from")?, string_arg(args, 2, "to")?))),
    )).expect("nativeText::replace registration must be unique");
    registry.register(NativeFunction::sync(
        "nativeText", "split",
        vec![("value", SparType::Str), ("separator", SparType::Str)],
        SparType::List(Box::new(SparType::Str)), true,
        |_context, args| {
            let value = string_arg(args, 0, "value")?;
            let separator = string_arg(args, 1, "separator")?;
            Ok(Value::List(value.split(separator).map(|part| Value::String(part.to_string())).collect()))
        },
    )).expect("nativeText::split registration must be unique");
    registry.register(NativeFunction::sync(
        "nativeText", "join",
        vec![("values", SparType::List(Box::new(SparType::Str))), ("separator", SparType::Str)],
        SparType::Str, true,
        |_context, args| Ok(Value::String(string_list_arg(args, 0, "values")?.join(string_arg(args, 1, "separator")?))),
    )).expect("nativeText::join registration must be unique");
}
