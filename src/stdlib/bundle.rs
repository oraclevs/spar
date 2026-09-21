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

/// Source-backed stdlib remains the authoritative fallback and is also what
/// the LSP uses for source navigation.
pub fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("stdlib")
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
}
