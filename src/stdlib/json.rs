use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeRegistry, Value};

use super::support::{error, serde_to_value, string_arg, value_to_serde};

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry.register(NativeFunction::sync(
        "nativeJson", "parse",
        vec![("text", SparType::Str)],
        SparType::TypeParameter("T".into()),
        true,
        |_context, args| {
            let parsed: serde_json::Value = serde_json::from_str(string_arg(args, 0, "text")?)
                .map_err(|e| error(format!("invalid JSON: {e}")))?;
            serde_to_value(parsed)
        },
    )).expect("nativeJson::parse registration must be unique");
    registry.register(NativeFunction::sync(
        "nativeJson", "stringify",
        vec![("value", SparType::TypeParameter("T".into()))],
        SparType::Str,
        true,
        |_context, args| {
            let value = args.first().ok_or_else(|| error("missing native argument 'value'"))?;
            serde_json::to_string(&value_to_serde(value)?)
                .map(Value::String)
                .map_err(|e| error(format!("JSON encoding failed: {e}")))
        },
    )).expect("nativeJson::stringify registration must be unique");
    registry.register(NativeFunction::sync(
        "nativeJson", "stringifyPretty",
        vec![("value", SparType::TypeParameter("T".into()))],
        SparType::Str,
        true,
        |_context, args| {
            let value = args.first().ok_or_else(|| error("missing native argument 'value'"))?;
            serde_json::to_string_pretty(&value_to_serde(value)?)
                .map(Value::String)
                .map_err(|e| error(format!("JSON encoding failed: {e}")))
        },
    )).expect("nativeJson::stringifyPretty registration must be unique");
}
