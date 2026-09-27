use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeIntrinsic, NativeRegistry, Value};

use super::support::{error, value_to_serde};

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry
        .register(NativeFunction::sync_intrinsic(
            "nativeJson",
            "parse",
            vec![("text", SparType::Str)],
            SparType::TypeParameter("T".into()),
            true,
            NativeIntrinsic::JsonParse,
        ))
        .expect("nativeJson::parse registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeJson",
            "stringify",
            vec![("value", SparType::Any)],
            SparType::Str,
            true,
            |_context, args| {
                let value = args
                    .first()
                    .ok_or_else(|| error("missing native argument 'value'"))?;
                serde_json::to_string(&value_to_serde(value)?)
                    .map(Value::String)
                    .map_err(|e| error(format!("JSON encoding failed: {e}")))
            },
        ))
        .expect("nativeJson::stringify registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeJson",
            "stringifyPretty",
            vec![("value", SparType::Any)],
            SparType::Str,
            true,
            |_context, args| {
                let value = args
                    .first()
                    .ok_or_else(|| error("missing native argument 'value'"))?;
                serde_json::to_string_pretty(&value_to_serde(value)?)
                    .map(Value::String)
                    .map_err(|e| error(format!("JSON encoding failed: {e}")))
            },
        ))
        .expect("nativeJson::stringifyPretty registration must be unique");
}
