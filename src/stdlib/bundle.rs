use std::path::{Path, PathBuf};

/// Binary stdlib cache format. Increment this whenever the serialized
/// representation of a precompiled std module changes incompatibly.
pub const STDLIB_CACHE_FORMAT_VERSION: u32 = 1;

/// Identity used by release tooling to decide whether a precompiled stdlib
/// artifact can be reused by this compiler/runtime build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StdlibBundleIdentity {
    pub cache_format: u32,
    pub spar_version: &'static str,
    pub package_version: &'static str,
}

impl StdlibBundleIdentity {
    pub fn current() -> Self {
        Self {
            cache_format: STDLIB_CACHE_FORMAT_VERSION,
            spar_version: env!("CARGO_PKG_VERSION"),
            package_version: env!("CARGO_PKG_VERSION"),
        }
    }

    pub fn cache_key(&self) -> String {
        format!(
            "spar-stdlib-v{}-compiler-{}-package-{}",
            self.cache_format, self.spar_version, self.package_version
        )
    }
}

/// Optional release-time precompiled bundle location.
///
/// Development and diagnostics always retain source fallback. Merely setting
/// this path never disables source loading; a future serializer/deserializer
/// may consume the artifact only after validating `StdlibBundleIdentity`.
pub fn configured_precompiled_bundle() -> Option<PathBuf> {
    std::env::var_os("SPAR_STDLIB_PRECOMPILED")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Source identity retained for diagnostics and LSP navigation. Runtime reads
/// use the embedded snapshot, even if this checkout changes or disappears.
pub fn source_root() -> PathBuf {
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("SPA_HOME").filter(|value| !value.is_empty()) {
        candidates.push(PathBuf::from(home).join("stdlib"));
    }
    if let Some(data) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        candidates.push(PathBuf::from(data).join("spar/stdlib"));
    } else if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        candidates.push(PathBuf::from(home).join(".local/share/spar/stdlib"));
    }
    candidates.push(PathBuf::from("/usr/local/share/spar/stdlib"));
    candidates.push(PathBuf::from("/usr/share/spar/stdlib"));
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("stdlib"));
    candidates
        .into_iter()
        .find(|root| root.join("src/lib.spar").is_file())
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("stdlib"))
}

pub fn source_module_path(module: &str) -> Option<PathBuf> {
    let source_root = source_root().join("src");
    if module.is_empty() {
        return Some(source_root.join("lib.spar"));
    }
    let relative = Path::new(module);
    if relative.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return None;
    }
    let mut path = source_root.join(relative);
    if path.extension().is_none() {
        path.set_extension("spar");
    }
    Some(path)
}

/// Sources compiled into this binary, paired with its parser and native ABI.
/// Filesystem paths remain source identities for diagnostics and navigation.
pub fn embedded_source(path: &Path) -> Option<&'static str> {
    let root = source_root().join("src");
    let relative = path.strip_prefix(&root).ok()?;
    if relative
        .components()
        .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return None;
    }
    match relative.to_str()? {
        "async.spar" => Some(include_str!("../../stdlib/src/async.spar")),
        "data.spar" => Some(include_str!("../../stdlib/src/data.spar")),
        "env.spar" => Some(include_str!("../../stdlib/src/env.spar")),
        "fs.spar" => Some(include_str!("../../stdlib/src/fs.spar")),
        "http.spar" => Some(include_str!("../../stdlib/src/http.spar")),
        "io.spar" => Some(include_str!("../../stdlib/src/io.spar")),
        "json.spar" => Some(include_str!("../../stdlib/src/json.spar")),
        "lib.spar" => Some(include_str!("../../stdlib/src/lib.spar")),
        "math.spar" => Some(include_str!("../../stdlib/src/math.spar")),
        "path.spar" => Some(include_str!("../../stdlib/src/path.spar")),
        "prelude.spar" => Some(include_str!("../../stdlib/src/prelude.spar")),
        "process.spar" => Some(include_str!("../../stdlib/src/process.spar")),
        "random.spar" => Some(include_str!("../../stdlib/src/random.spar")),
        "regex.spar" => Some(include_str!("../../stdlib/src/regex.spar")),
        "terminal.spar" => Some(include_str!("../../stdlib/src/terminal.spar")),
        "text.spar" => Some(include_str!("../../stdlib/src/text.spar")),
        "time.spar" => Some(include_str!("../../stdlib/src/time.spar")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_identity_is_versioned() {
        let identity = StdlibBundleIdentity::current();
        assert_eq!(identity.cache_format, STDLIB_CACHE_FORMAT_VERSION);
        assert!(identity.cache_key().contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn source_module_resolution_rejects_escape() {
        assert!(source_module_path("fs")
            .unwrap()
            .ends_with("stdlib/src/fs.spar"));
        assert!(source_module_path("../secret").is_none());
    }

    #[test]
    fn imports_read_the_source_compiled_with_this_binary() {
        for module in ["lib", "data", "fs", "io", "http", "json", "prelude"] {
            let path = source_module_path(module).unwrap();
            let embedded = embedded_source(&path).expect("bundled module must be embedded");
            assert_eq!(crate::stdlib::read_module_source(&path).unwrap(), embedded);
            assert!(crate::stdlib::module_source_exists(&path));
            let tokens = crate::Lexer::new(embedded).tokenize().unwrap();
            crate::Parser::new(tokens).parse().unwrap();
        }
    }

    #[test]
    fn embedded_sources_do_not_capture_external_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fs.spar");
        std::fs::write(&path, "export var marker = 42;").unwrap();
        assert!(embedded_source(&path).is_none());
        assert_eq!(
            crate::stdlib::read_module_source(&path).unwrap(),
            "export var marker = 42;"
        );
        assert!(embedded_source(&source_root().join("src/../secret.spar")).is_none());
    }
}
