use std::fs::{self, OpenOptions};
use std::io::Write;

use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeRegistry, Value};

use super::support::{bool_arg, error, object, owned_bytes_arg, string_arg};

pub(crate) fn register(registry: &mut NativeRegistry) {
    register_read_write(registry);
    register_queries(registry);
    register_mutations(registry);
}

fn register_read_write(registry: &mut NativeRegistry) {
    registry
        .register(NativeFunction::sync(
            "nativeFs",
            "readText",
            vec![("path", SparType::Str)],
            SparType::Str,
            true,
            |context, args| {
                let path = context.resolve_path(string_arg(args, 0, "path")?);
                fs::read_to_string(&path)
                    .map(Value::String)
                    .map_err(|e| error(format!("cannot read '{}': {e}", path.display())))
            },
        ))
        .expect("nativeFs::readText registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeFs",
            "writeText",
            vec![("path", SparType::Str), ("content", SparType::Str)],
            SparType::Void,
            true,
            |context, args| {
                let path = context.resolve_path(string_arg(args, 0, "path")?);
                fs::write(&path, string_arg(args, 1, "content")?)
                    .map(|_| Value::Void)
                    .map_err(|e| error(format!("cannot write '{}': {e}", path.display())))
            },
        ))
        .expect("nativeFs::writeText registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeFs",
            "appendText",
            vec![("path", SparType::Str), ("content", SparType::Str)],
            SparType::Void,
            true,
            |context, args| {
                let path = context.resolve_path(string_arg(args, 0, "path")?);
                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .map_err(|e| {
                        error(format!("cannot open '{}' for append: {e}", path.display()))
                    })?;
                file.write_all(string_arg(args, 1, "content")?.as_bytes())
                    .map(|_| Value::Void)
                    .map_err(|e| error(format!("cannot append '{}': {e}", path.display())))
            },
        ))
        .expect("nativeFs::appendText registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeFs",
            "readBytes",
            vec![("path", SparType::Str)],
            SparType::Named("Bytes".into()),
            true,
            |context, args| {
                let path = context.resolve_path(string_arg(args, 0, "path")?);
                fs::read(&path)
                    .map(Value::Bytes)
                    .map_err(|e| error(format!("cannot read '{}': {e}", path.display())))
            },
        ))
        .expect("nativeFs::readBytes registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeFs",
            "writeBytes",
            vec![
                ("path", SparType::Str),
                ("content", SparType::Named("Bytes".into())),
            ],
            SparType::Void,
            true,
            |context, args| {
                let path = context.resolve_path(string_arg(args, 0, "path")?);
                let bytes = owned_bytes_arg(args, 1, "content")?;
                fs::write(&path, bytes)
                    .map(|_| Value::Void)
                    .map_err(|e| error(format!("cannot write '{}': {e}", path.display())))
            },
        ))
        .expect("nativeFs::writeBytes registration must be unique");
}

fn register_queries(registry: &mut NativeRegistry) {
    type Predicate = fn(&fs::Metadata) -> bool;
    let predicates: [(&str, Predicate); 3] = [
        ("exists", |_metadata: &fs::Metadata| true),
        ("isFile", |metadata: &fs::Metadata| metadata.is_file()),
        ("isDir", |metadata: &fs::Metadata| metadata.is_dir()),
    ];
    for (name, predicate) in predicates {
        registry
            .register(NativeFunction::sync(
                "nativeFs",
                name,
                vec![("path", SparType::Str)],
                SparType::Bool,
                true,
                move |context, args| {
                    let path = context.resolve_path(string_arg(args, 0, "path")?);
                    match fs::metadata(path) {
                        Ok(metadata) => Ok(Value::Bool(predicate(&metadata))),
                        Err(error_value) if error_value.kind() == std::io::ErrorKind::NotFound => {
                            Ok(Value::Bool(false))
                        }
                        Err(error_value) => {
                            Err(error(format!("filesystem metadata failed: {error_value}")))
                        }
                    }
                },
            ))
            .expect("nativeFs query registration must be unique");
    }
    registry
        .register(NativeFunction::sync(
            "nativeFs",
            "metadata",
            vec![("path", SparType::Str)],
            SparType::Named("FileMetadata".into()),
            true,
            |context, args| {
                let path = context.resolve_path(string_arg(args, 0, "path")?);
                let metadata = fs::metadata(&path)
                    .map_err(|e| error(format!("cannot inspect '{}': {e}", path.display())))?;
                let modified_ms = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .and_then(|duration| i64::try_from(duration.as_millis()).ok())
                    .unwrap_or(0);
                Ok(object([
                    (
                        "size",
                        Value::Int(i64::try_from(metadata.len()).unwrap_or(i64::MAX)),
                    ),
                    ("isFile", Value::Bool(metadata.is_file())),
                    ("isDir", Value::Bool(metadata.is_dir())),
                    ("modifiedMillis", Value::Int(modified_ms)),
                ]))
            },
        ))
        .expect("nativeFs::metadata registration must be unique");
}

fn register_mutations(registry: &mut NativeRegistry) {
    registry
        .register(NativeFunction::sync(
            "nativeFs",
            "createDir",
            vec![("path", SparType::Str), ("recursive", SparType::Bool)],
            SparType::Void,
            true,
            |context, args| {
                let path = context.resolve_path(string_arg(args, 0, "path")?);
                let recursive = bool_arg(args, 1, "recursive")?;
                let result = if recursive {
                    fs::create_dir_all(&path)
                } else {
                    fs::create_dir(&path)
                };
                result.map(|_| Value::Void).map_err(|e| {
                    error(format!("cannot create directory '{}': {e}", path.display()))
                })
            },
        ))
        .expect("nativeFs::createDir registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeFs",
            "removeFile",
            vec![("path", SparType::Str)],
            SparType::Void,
            true,
            |context, args| {
                let path = context.resolve_path(string_arg(args, 0, "path")?);
                fs::remove_file(&path)
                    .map(|_| Value::Void)
                    .map_err(|e| error(format!("cannot remove file '{}': {e}", path.display())))
            },
        ))
        .expect("nativeFs::removeFile registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeFs",
            "removeDir",
            vec![("path", SparType::Str), ("recursive", SparType::Bool)],
            SparType::Void,
            true,
            |context, args| {
                let path = context.resolve_path(string_arg(args, 0, "path")?);
                let recursive = bool_arg(args, 1, "recursive")?;
                let result = if recursive {
                    fs::remove_dir_all(&path)
                } else {
                    fs::remove_dir(&path)
                };
                result.map(|_| Value::Void).map_err(|e| {
                    error(format!("cannot remove directory '{}': {e}", path.display()))
                })
            },
        ))
        .expect("nativeFs::removeDir registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeFs",
            "copy",
            vec![("from", SparType::Str), ("to", SparType::Str)],
            SparType::Int,
            true,
            |context, args| {
                let from = context.resolve_path(string_arg(args, 0, "from")?);
                let to = context.resolve_path(string_arg(args, 1, "to")?);
                fs::copy(&from, &to)
                    .map(|bytes| Value::Int(i64::try_from(bytes).unwrap_or(i64::MAX)))
                    .map_err(|e| {
                        error(format!(
                            "cannot copy '{}' to '{}': {e}",
                            from.display(),
                            to.display()
                        ))
                    })
            },
        ))
        .expect("nativeFs::copy registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeFs",
            "move",
            vec![("from", SparType::Str), ("to", SparType::Str)],
            SparType::Void,
            true,
            |context, args| {
                let from = context.resolve_path(string_arg(args, 0, "from")?);
                let to = context.resolve_path(string_arg(args, 1, "to")?);
                fs::rename(&from, &to).map(|_| Value::Void).map_err(|e| {
                    error(format!(
                        "cannot move '{}' to '{}': {e}",
                        from.display(),
                        to.display()
                    ))
                })
            },
        ))
        .expect("nativeFs::move registration must be unique");
}
