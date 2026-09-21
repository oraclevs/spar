use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeMethod, NativeRegistry, Value};

use super::support::{string_arg, string_list_arg};

pub(crate) fn register(registry: &mut NativeRegistry) {
    register_unary(registry, "trim", trim_impl);
    register_unary(registry, "lower", lower_impl);
    register_unary(registry, "upper", upper_impl);

    register_predicate(registry, "contains", contains_impl);
    register_predicate(registry, "startsWith", starts_with_impl);
    register_predicate(registry, "endsWith", ends_with_impl);

    registry
        .register(NativeFunction::sync(
            "nativeText",
            "replace",
            vec![
                ("value", SparType::Str),
                ("from", SparType::Str),
                ("to", SparType::Str),
            ],
            SparType::Str,
            true,
            replace_impl,
        ))
        .expect("nativeText::replace registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "str",
            "replace",
            SparType::Str,
            vec![("from", SparType::Str), ("to", SparType::Str)],
            SparType::Str,
            false,
            replace_impl,
        ))
        .expect("str.replace registration must be unique");

    registry
        .register(NativeFunction::sync(
            "nativeText",
            "split",
            vec![("value", SparType::Str), ("separator", SparType::Str)],
            SparType::List(Box::new(SparType::Str)),
            true,
            split_impl,
        ))
        .expect("nativeText::split registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "str",
            "split",
            SparType::Str,
            vec![("separator", SparType::Str)],
            SparType::List(Box::new(SparType::Str)),
            false,
            split_impl,
        ))
        .expect("str.split registration must be unique");

    let string_list = SparType::List(Box::new(SparType::Str));
    registry
        .register(NativeFunction::sync(
            "nativeText",
            "join",
            vec![
                ("values", string_list.clone()),
                ("separator", SparType::Str),
            ],
            SparType::Str,
            true,
            join_impl,
        ))
        .expect("nativeText::join registration must be unique");
    registry
        .register_method(NativeMethod::sync(
            "List",
            "join",
            string_list,
            vec![("separator", SparType::Str)],
            SparType::Str,
            false,
            join_impl,
        ))
        .expect("List<str>.join registration must be unique");
}

fn register_unary(
    registry: &mut NativeRegistry,
    name: &str,
    callback: fn(&mut crate::runtime::RuntimeContext, &[Value]) -> Result<Value, crate::SparError>,
) {
    registry
        .register(NativeFunction::sync(
            "nativeText",
            name,
            vec![("value", SparType::Str)],
            SparType::Str,
            true,
            callback,
        ))
        .unwrap_or_else(|_| panic!("nativeText::{name} registration must be unique"));
    registry
        .register_method(NativeMethod::sync(
            "str",
            name,
            SparType::Str,
            vec![],
            SparType::Str,
            false,
            callback,
        ))
        .unwrap_or_else(|_| panic!("str.{name} registration must be unique"));
}

fn register_predicate(
    registry: &mut NativeRegistry,
    name: &str,
    callback: fn(&mut crate::runtime::RuntimeContext, &[Value]) -> Result<Value, crate::SparError>,
) {
    registry
        .register(NativeFunction::sync(
            "nativeText",
            name,
            vec![("value", SparType::Str), ("needle", SparType::Str)],
            SparType::Bool,
            true,
            callback,
        ))
        .unwrap_or_else(|_| panic!("nativeText::{name} registration must be unique"));
    registry
        .register_method(NativeMethod::sync(
            "str",
            name,
            SparType::Str,
            vec![("needle", SparType::Str)],
            SparType::Bool,
            false,
            callback,
        ))
        .unwrap_or_else(|_| panic!("str.{name} registration must be unique"));
}

fn trim_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    Ok(Value::String(
        string_arg(args, 0, "value")?.trim().to_string(),
    ))
}

fn lower_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    Ok(Value::String(string_arg(args, 0, "value")?.to_lowercase()))
}

fn upper_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    Ok(Value::String(string_arg(args, 0, "value")?.to_uppercase()))
}

fn contains_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    Ok(Value::Bool(
        string_arg(args, 0, "value")?.contains(string_arg(args, 1, "needle")?),
    ))
}

fn starts_with_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    Ok(Value::Bool(
        string_arg(args, 0, "value")?.starts_with(string_arg(args, 1, "needle")?),
    ))
}

fn ends_with_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    Ok(Value::Bool(
        string_arg(args, 0, "value")?.ends_with(string_arg(args, 1, "needle")?),
    ))
}

fn replace_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    Ok(Value::String(string_arg(args, 0, "value")?.replace(
        string_arg(args, 1, "from")?,
        string_arg(args, 2, "to")?,
    )))
}

fn split_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    let value = string_arg(args, 0, "value")?;
    let separator = string_arg(args, 1, "separator")?;
    Ok(Value::List(
        value
            .split(separator)
            .map(|part| Value::String(part.to_string()))
            .collect(),
    ))
}

fn join_impl(
    _context: &mut crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<Value, crate::SparError> {
    Ok(Value::String(
        string_list_arg(args, 0, "values")?.join(string_arg(args, 1, "separator")?),
    ))
}
