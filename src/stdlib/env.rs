use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeRegistry, Value};

use super::support::string_arg;

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry.register(NativeFunction::sync(
        "nativeEnv", "get", vec![("name", SparType::Str)], SparType::Str, true,
        |context, args| Ok(Value::String(context.env_get(string_arg(args, 0, "name")?).unwrap_or("").to_string())),
    )).expect("nativeEnv::get registration must be unique");
    registry.register(NativeFunction::sync(
        "nativeEnv", "has", vec![("name", SparType::Str)], SparType::Bool, true,
        |context, args| Ok(Value::Bool(context.env_contains(string_arg(args, 0, "name")?))),
    )).expect("nativeEnv::has registration must be unique");
    registry.register(NativeFunction::sync(
        "nativeEnv", "set", vec![("name", SparType::Str), ("value", SparType::Str)], SparType::Void, true,
        |context, args| {
            context.env_set(string_arg(args, 0, "name")?, string_arg(args, 1, "value")?);
            Ok(Value::Void)
        },
    )).expect("nativeEnv::set registration must be unique");
    registry.register(NativeFunction::sync(
        "nativeEnv", "unset", vec![("name", SparType::Str)], SparType::Void, true,
        |context, args| {
            context.env_unset(string_arg(args, 0, "name")?);
            Ok(Value::Void)
        },
    )).expect("nativeEnv::unset registration must be unique");
    registry.register(NativeFunction::sync(
        "nativeEnv", "keys", vec![], SparType::List(Box::new(SparType::Str)), true,
        |context, _args| {
            let mut keys = context.environment().keys().cloned().collect::<Vec<_>>();
            keys.sort();
            Ok(Value::List(keys.into_iter().map(Value::String).collect()))
        },
    )).expect("nativeEnv::keys registration must be unique");
}
