use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeRegistry, Value};

use super::support::{error, string_arg};

fn compile(pattern: &str) -> Result<regex::Regex, crate::SparError> {
    regex::Regex::new(pattern).map_err(|e| error(format!("invalid regular expression: {e}")))
}

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry
        .register(NativeFunction::sync(
            "nativeRegex",
            "isMatch",
            vec![("pattern", SparType::Str), ("text", SparType::Str)],
            SparType::Bool,
            true,
            |_context, args| {
                Ok(Value::Bool(
                    compile(string_arg(args, 0, "pattern")?)?
                        .is_match(string_arg(args, 1, "text")?),
                ))
            },
        ))
        .expect("nativeRegex::isMatch registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeRegex",
            "find",
            vec![("pattern", SparType::Str), ("text", SparType::Str)],
            SparType::Str,
            true,
            |_context, args| {
                let regex = compile(string_arg(args, 0, "pattern")?)?;
                let text = string_arg(args, 1, "text")?;
                Ok(Value::String(
                    regex
                        .find(text)
                        .map(|m| m.as_str().to_string())
                        .unwrap_or_default(),
                ))
            },
        ))
        .expect("nativeRegex::find registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeRegex",
            "findAll",
            vec![("pattern", SparType::Str), ("text", SparType::Str)],
            SparType::List(Box::new(SparType::Str)),
            true,
            |_context, args| {
                let regex = compile(string_arg(args, 0, "pattern")?)?;
                let text = string_arg(args, 1, "text")?;
                Ok(Value::List(
                    regex
                        .find_iter(text)
                        .map(|m| Value::String(m.as_str().to_string()))
                        .collect(),
                ))
            },
        ))
        .expect("nativeRegex::findAll registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeRegex",
            "replace",
            vec![
                ("pattern", SparType::Str),
                ("text", SparType::Str),
                ("replacement", SparType::Str),
            ],
            SparType::Str,
            true,
            |_context, args| {
                let regex = compile(string_arg(args, 0, "pattern")?)?;
                Ok(Value::String(
                    regex
                        .replace_all(
                            string_arg(args, 1, "text")?,
                            string_arg(args, 2, "replacement")?,
                        )
                        .into_owned(),
                ))
            },
        ))
        .expect("nativeRegex::replace registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeRegex",
            "split",
            vec![("pattern", SparType::Str), ("text", SparType::Str)],
            SparType::List(Box::new(SparType::Str)),
            true,
            |_context, args| {
                let regex = compile(string_arg(args, 0, "pattern")?)?;
                Ok(Value::List(
                    regex
                        .split(string_arg(args, 1, "text")?)
                        .map(|part| Value::String(part.to_string()))
                        .collect(),
                ))
            },
        ))
        .expect("nativeRegex::split registration must be unique");
}
