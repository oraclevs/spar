use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeRegistry, Value};

use super::support::error;

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry
        .register(NativeFunction::sync(
            "nativeCore",
            "len",
            vec![("value", SparType::TypeParameter("T".into()))],
            SparType::Int,
            true,
            |_context, args| {
                let value = args.first().ok_or_else(|| error("missing native argument 'value'"))?;
                let length = match value {
                    Value::String(value) => value.chars().count(),
                    Value::Bytes(value) => value.len(),
                    Value::List(value) => value.len(),
                    Value::Object(value) => value.len(),
                    other => {
                        return Err(error(format!(
                            "len() does not support runtime value {}",
                            other.type_name()
                        )))
                    }
                };
                i64::try_from(length)
                    .map(Value::Int)
                    .map_err(|_| error("value length exceeds Spar int range"))
            },
        ))
        .expect("nativeCore::len registration must be unique");
}
