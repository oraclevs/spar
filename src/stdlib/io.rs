use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeRegistry, Value};

use super::support::{error, owned_bytes_arg, string_arg};

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry
        .register(NativeFunction::sync(
            "nativeIo",
            "print",
            vec![("message", SparType::Str)],
            SparType::Void,
            true,
            |context, args| {
                context
                    .write_stdout(string_arg(args, 0, "message")?.as_bytes())
                    .map_err(|error_value| error(format!("stdout write failed: {error_value}")))?;
                Ok(Value::Void)
            },
        ))
        .expect("nativeIo::print registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeIo",
            "println",
            vec![("message", SparType::Str)],
            SparType::Void,
            true,
            |context, args| {
                let message = string_arg(args, 0, "message")?;
                context
                    .write_stdout(format!("{message}\n").as_bytes())
                    .map_err(|error_value| error(format!("stdout write failed: {error_value}")))?;
                Ok(Value::Void)
            },
        ))
        .expect("nativeIo::println registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeIo",
            "eprint",
            vec![("message", SparType::Str)],
            SparType::Void,
            true,
            |context, args| {
                context
                    .write_stderr(string_arg(args, 0, "message")?.as_bytes())
                    .map_err(|error_value| error(format!("stderr write failed: {error_value}")))?;
                Ok(Value::Void)
            },
        ))
        .expect("nativeIo::eprint registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeIo",
            "eprintln",
            vec![("message", SparType::Str)],
            SparType::Void,
            true,
            |context, args| {
                let message = string_arg(args, 0, "message")?;
                context
                    .write_stderr(format!("{message}\n").as_bytes())
                    .map_err(|error_value| error(format!("stderr write failed: {error_value}")))?;
                Ok(Value::Void)
            },
        ))
        .expect("nativeIo::eprintln registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeIo",
            "readAll",
            vec![],
            SparType::Str,
            true,
            |context, _args| {
                let bytes = context
                    .read_stdin_remaining()
                    .map_err(|error_value| error(format!("stdin read failed: {error_value}")))?;
                let text = String::from_utf8(bytes)
                    .map_err(|_| error("stdin contains bytes that are not valid UTF-8"))?;
                Ok(Value::String(text))
            },
        ))
        .expect("nativeIo::readAll registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeIo",
            "readLine",
            vec![],
            SparType::Str,
            true,
            |context, _args| {
                let bytes = context
                    .read_stdin_line()
                    .map_err(|error_value| error(format!("stdin read failed: {error_value}")))?;
                let text = String::from_utf8(bytes)
                    .map_err(|_| error("stdin contains bytes that are not valid UTF-8"))?;
                Ok(Value::String(
                    text.trim_end_matches(&['\r', '\n'][..]).to_string(),
                ))
            },
        ))
        .expect("nativeIo::readLine registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeIo",
            "readBytes",
            vec![],
            SparType::Named("Bytes".into()),
            true,
            |context, _args| {
                context
                    .read_stdin_remaining()
                    .map(Value::Bytes)
                    .map_err(|error_value| error(format!("stdin read failed: {error_value}")))
            },
        ))
        .expect("nativeIo::readBytes registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeIo",
            "writeBytes",
            vec![("content", SparType::Named("Bytes".into()))],
            SparType::Void,
            true,
            |context, args| {
                context
                    .write_stdout(&owned_bytes_arg(args, 0, "content")?)
                    .map_err(|error_value| error(format!("stdout write failed: {error_value}")))?;
                Ok(Value::Void)
            },
        ))
        .expect("nativeIo::writeBytes registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeIo",
            "writeErrorBytes",
            vec![("content", SparType::Named("Bytes".into()))],
            SparType::Void,
            true,
            |context, args| {
                context
                    .write_stderr(&owned_bytes_arg(args, 0, "content")?)
                    .map_err(|error_value| error(format!("stderr write failed: {error_value}")))?;
                Ok(Value::Void)
            },
        ))
        .expect("nativeIo::writeErrorBytes registration must be unique");
}
