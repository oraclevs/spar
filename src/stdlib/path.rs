use std::path::{Component, Path, PathBuf};

use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeRegistry, Value};

use super::support::string_arg;

pub(crate) fn register(registry: &mut NativeRegistry) {
    let unary = [
        ("basename", basename as fn(&Path) -> String),
        ("dirname", dirname),
        ("extension", extension),
        ("normalize", normalize),
    ];
    for (name, function) in unary {
        registry
            .register(NativeFunction::sync(
                "nativePath",
                name,
                vec![("path", SparType::Str)],
                SparType::Str,
                true,
                move |_context, args| {
                    Ok(Value::String(function(Path::new(string_arg(
                        args, 0, "path",
                    )?))))
                },
            ))
            .expect("nativePath unary registration must be unique");
    }
    registry
        .register(NativeFunction::sync(
            "nativePath",
            "join",
            vec![("left", SparType::Str), ("right", SparType::Str)],
            SparType::Str,
            true,
            |_context, args| {
                let joined =
                    Path::new(string_arg(args, 0, "left")?).join(string_arg(args, 1, "right")?);
                Ok(Value::String(joined.to_string_lossy().into_owned()))
            },
        ))
        .expect("nativePath::join registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativePath",
            "isAbsolute",
            vec![("path", SparType::Str)],
            SparType::Bool,
            true,
            |_context, args| {
                Ok(Value::Bool(
                    Path::new(string_arg(args, 0, "path")?).is_absolute(),
                ))
            },
        ))
        .expect("nativePath::isAbsolute registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativePath",
            "absolute",
            vec![("path", SparType::Str)],
            SparType::Str,
            true,
            |context, args| {
                Ok(Value::String(normalize(
                    &context.resolve_path(string_arg(args, 0, "path")?),
                )))
            },
        ))
        .expect("nativePath::absolute registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativePath",
            "relative",
            vec![("from", SparType::Str), ("to", SparType::Str)],
            SparType::Str,
            true,
            |context, args| {
                let from = normalize_path(&context.resolve_path(string_arg(args, 0, "from")?));
                let to = normalize_path(&context.resolve_path(string_arg(args, 1, "to")?));
                Ok(Value::String(relative(&from, &to)))
            },
        ))
        .expect("nativePath::relative registration must be unique");
}

fn basename(path: &Path) -> String {
    path.file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default()
}
fn dirname(path: &Path) -> String {
    path.parent()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default()
}
fn extension(path: &Path) -> String {
    path.extension()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default()
}
fn normalize(path: &Path) -> String {
    normalize_path(path).to_string_lossy().into_owned()
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let can_pop = matches!(output.components().next_back(), Some(Component::Normal(_)));
                if can_pop {
                    output.pop();
                } else if !output.has_root() {
                    output.push("..");
                }
            }
            Component::Prefix(prefix) => output.push(prefix.as_os_str()),
            Component::RootDir => output.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::Normal(part) => output.push(part),
        }
    }
    if output.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        output
    }
}

fn relative(from: &Path, to: &Path) -> String {
    let from_components = from.components().collect::<Vec<_>>();
    let to_components = to.components().collect::<Vec<_>>();
    let common = from_components
        .iter()
        .zip(&to_components)
        .take_while(|(left, right)| left == right)
        .count();

    if common == 0 && (from.is_absolute() || to.is_absolute()) {
        return to.to_string_lossy().into_owned();
    }

    let mut result = PathBuf::new();
    for _ in common..from_components.len() {
        result.push("..");
    }
    for component in &to_components[common..] {
        result.push(component.as_os_str());
    }
    if result.as_os_str().is_empty() {
        ".".into()
    } else {
        result.to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_preserves_unresolved_parent_components() {
        assert_eq!(normalize(Path::new("../a/./b")), "../a/b");
        assert_eq!(normalize(Path::new("a/b/../c")), "a/c");
        assert_eq!(normalize(Path::new(".")), ".");
    }
}
