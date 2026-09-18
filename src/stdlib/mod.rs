mod async_runtime;
mod bundle;
mod core;
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
mod support;
mod terminal;
mod text;
mod time;

use std::path::{Path, PathBuf};

pub use bundle::{configured_precompiled_bundle, StdlibBundleIdentity, STDLIB_CACHE_FORMAT_VERSION};

pub const STD_PACKAGE_NAME: &str = "std";

/// Root of the bundled standard-library sources for development builds.
/// Release builds may replace this source-backed resolver with the versioned
/// precompiled bundle while preserving the same package/module identities.
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


const PRELUDE_NAMES: &[&str] = &["print", "println", "len", "assert", "panic"];

/// Inject Spar-written prelude functions into an ordinary source module.
/// Explicit imports of the same canonical std symbols suppress implicit
/// injection; direct user declarations with reserved prelude names are
/// rejected so behavior never depends on accidental shadowing.
pub(crate) fn inject_prelude(program: &mut crate::ast::Program) -> Result<(), Vec<crate::SparError>> {
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

    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/stdlib/src/prelude.spar"));
    let tokens = crate::Lexer::new(source).tokenize().map_err(|error| vec![error])?;
    let mut prelude = crate::Parser::new(tokens).parse().map_err(|error| vec![error])?;
    crate::loader::mark_program_trusted_native(&mut prelude);

    program.items.extend(prelude.items.into_iter().filter(|item| match item {
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
    registry
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
