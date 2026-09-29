mod async_runtime;
// Release-time precompiled-bundle identity is reserved for tooling and not wired yet.
#[allow(dead_code)]
mod bundle;
mod core;
mod data;
pub(crate) use data::FUNCTION_NAMES as DATA_FUNCTIONS;
mod env;
mod fs;
mod http;
mod io;
mod json;
mod math;
mod path;
mod process;
mod random;
mod regex;
pub(crate) mod support;
mod terminal;
mod text;
mod time;

use std::path::{Path, PathBuf};

pub const STD_PACKAGE_NAME: &str = "std";

/// Source identity used for bundled imports and editor navigation.
/// Execution reads the matching sources embedded in the binary.
pub fn bundled_root() -> PathBuf {
    bundle::source_root()
}

/// Resolve `std` or `std/<module>` as a reserved bundled package import.
/// Returns `None` for non-std requests or attempts to escape the package.
pub fn resolve_bundled_import(request: &str) -> Option<PathBuf> {
    let rest = if request == STD_PACKAGE_NAME {
        None
    } else {
        request.strip_prefix("std/").map(Some)?
    };

    match rest {
        None => bundle::source_module_path(""),
        Some(module) if !module.is_empty() => bundle::source_module_path(module),
        Some(_) => None,
    }
}

const PRELUDE_NAMES: &[&str] = &[
    "print",
    "println",
    "len",
    "range",
    "rangeFrom",
    "assert",
    "panic",
    "some",
    "none",
    "ok",
    "err",
];

/// Inject Spar-written prelude functions into an ordinary source module.
/// Explicit imports of the same canonical std symbols suppress implicit
/// injection; direct user declarations with reserved prelude names are
/// rejected so behavior never depends on accidental shadowing.
pub(crate) fn inject_prelude(
    program: &mut crate::ast::Program,
) -> Result<(), Vec<crate::SparError>> {
    use crate::ast::{ImportKind, TopLevelItem};

    let mut errors = Vec::new();
    let mut explicitly_imported = std::collections::HashSet::<String>::new();

    for item in &program.items {
        match item {
            TopLevelItem::Function(function) if PRELUDE_NAMES.contains(&function.name.as_str()) => {
                errors.push(crate::SparError::ResolveError {
                    message: format!(
                        "'{}' is a reserved Spar prelude name and cannot be redeclared",
                        function.name
                    ),
                    hint: Some("rename the function; the canonical implementation is provided by the bundled std package".into()),
                    span: function.name_span.clone(),
                });
            }
            TopLevelItem::Import(decl) => {
                let items = match &decl.kind {
                    ImportKind::Selective(items) | ImportKind::TypeSelective(items) => items,
                    _ => continue,
                };
                for item in items {
                    let final_name = item.alias.as_deref().unwrap_or(&item.name);
                    if !PRELUDE_NAMES.contains(&final_name) {
                        continue;
                    }
                    let canonical_std = decl.package
                        && matches!(decl.path.as_str(), "std" | "std/io" | "std/prelude");
                    if canonical_std {
                        explicitly_imported.insert(final_name.to_string());
                    } else {
                        errors.push(crate::SparError::ResolveError {
                            message: format!(
                                "imported name '{final_name}' would shadow a reserved Spar prelude binding"
                            ),
                            hint: Some("alias the imported symbol or use the bundled std implementation".into()),
                            span: item.span.clone(),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/stdlib/src/prelude.spar"
    ));
    let tokens = crate::Lexer::new(source)
        .tokenize()
        .map_err(|error| vec![error])?;
    let mut prelude = crate::Parser::new(tokens)
        .parse()
        .map_err(|error| vec![error])?;
    crate::loader::mark_program_trusted_native(&mut prelude);

    program
        .items
        .extend(prelude.items.into_iter().filter(|item| match item {
            TopLevelItem::Function(function) => !explicitly_imported.contains(&function.name),
            _ => false,
        }));
    Ok(())
}

pub fn is_bundled_std_path(path: &Path) -> bool {
    let root = bundled_root();
    let root = root.canonicalize().unwrap_or(root);
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    path.starts_with(root)
}

/// Native capability registry used by the bundled standard library.
/// Public Spar programs do not receive direct access to private entries; the
/// resolver enforces the trust marker on stdlib-originating functions.
pub fn native_registry() -> crate::runtime::NativeRegistry {
    let mut registry = crate::runtime::NativeRegistry::new();
    async_runtime::register(&mut registry);
    core::register(&mut registry);
    data::register(&mut registry);
    io::register(&mut registry);
    fs::register(&mut registry);
    path::register(&mut registry);
    env::register(&mut registry);
    time::register(&mut registry);
    terminal::register(&mut registry);
    random::register(&mut registry);
    text::register(&mut registry);
    math::register(&mut registry);
    json::register(&mut registry);
    regex::register(&mut registry);
    process::register(&mut registry);
    http::register(&mut registry);
    #[cfg(not(target_arch = "wasm32"))]
    load_env_native_modules(&mut registry);
    registry
}

/// Developer hook: `SPAR_NATIVE_MODULES=/path/a.so:/path/b.so` loads native modules into every
/// registry this process builds. Package manifests are the supported way to declare native
/// dependencies; this exists for tests, benchmarks and quick experiments. Load failures are
/// reported once on stderr and the module's functions stay unresolved.
#[cfg(not(target_arch = "wasm32"))]
fn load_env_native_modules(registry: &mut crate::runtime::NativeRegistry) {
    let Some(list) = crate::runtime_config::native_modules() else {
        return;
    };
    for path in std::env::split_paths(&list).filter(|p| !p.as_os_str().is_empty()) {
        if let Err(error) = crate::native_module::load_into_registry(&path, registry) {
            eprintln!("spar: native module error: {error}");
        }
    }
}

/// Read a compiler-owned stdlib snapshot or an ordinary filesystem module.
pub(crate) fn read_module_source(path: &Path) -> std::io::Result<String> {
    if let Some(source) = bundle::embedded_source(path) {
        return Ok(source.to_owned());
    }
    if is_bundled_std_path(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "module is not included in this compiler's standard library",
        ));
    }
    std::fs::read_to_string(path)
}

pub(crate) fn module_source_exists(path: &Path) -> bool {
    if is_bundled_std_path(path) {
        bundle::embedded_source(path).is_some()
    } else {
        path.exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_std_root_and_submodule() {
        assert_eq!(
            resolve_bundled_import("std"),
            Some(bundled_root().join("src/lib.spar"))
        );
        assert_eq!(
            resolve_bundled_import("std/fs"),
            Some(bundled_root().join("src/fs.spar"))
        );
    }

    #[test]
    fn rejects_std_escape() {
        assert!(resolve_bundled_import("std/../secret").is_none());
        assert!(resolve_bundled_import("std//").is_none());
    }
}
