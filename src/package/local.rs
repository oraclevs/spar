//! Local filesystem package sources (`path:../http` requests).

use std::path::{Path, PathBuf};

use crate::package::error::PackageError;
use crate::package::manifest::PackageManifest;

/// A local path dependency, resolved relative to the manifest that
/// declared it, canonicalized, and validated to actually be a package
/// (a `spar.package.spar` whose declared entry file exists).
#[derive(Debug)]
pub struct LocalSource {
    pub canonical_path: PathBuf,
    pub manifest: PackageManifest,
}

impl LocalSource {
    pub fn resolve(declaring_manifest_dir: &Path, relative: &Path) -> Result<Self, PackageError> {
        let joined = declaring_manifest_dir.join(relative);
        let canonical_path = joined.canonicalize().map_err(|e| PackageError::NotFound {
            message: format!(
                "local dependency path '{}' does not exist: {e}",
                joined.display()
            ),
        })?;
        if !canonical_path.is_dir() {
            return Err(PackageError::NotFound {
                message: format!(
                    "local dependency path '{}' is not a directory",
                    canonical_path.display()
                ),
            });
        }

        let manifest_path = canonical_path.join("spar.package.spar");
        let manifest_text =
            std::fs::read_to_string(&manifest_path).map_err(|e| PackageError::NotFound {
                message: format!(
                    "local dependency at '{}' has no spar.package.spar: {e}",
                    canonical_path.display()
                ),
            })?;
        let manifest = PackageManifest::parse(&manifest_text, &manifest_path)?;

        let entry_path = canonical_path.join(&manifest.entry);
        if !entry_path.is_file() {
            return Err(PackageError::NotFound {
                message: format!(
                    "local dependency '{}' declares entry '{}' but that file doesn't exist at {}",
                    manifest.name,
                    manifest.entry.display(),
                    entry_path.display()
                ),
            });
        }

        Ok(Self {
            canonical_path,
            manifest,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_local_package(dir: &Path, name: &str) {
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("spar.package.spar"),
            format!(
                r#"
                [Package] {{
                    name: str = "{name}";
                    version: str = "1.0.0";
                    kind: str = "library";
                }};
                "#
            ),
        )
        .unwrap();
        fs::write(dir.join("src/lib.spar"), "export var x: int = 1;").unwrap();
    }

    #[test]
    fn resolves_a_valid_local_package_relative_to_the_declaring_manifest() {
        let root = tempfile::tempdir().unwrap();
        let declaring = root.path().join("app");
        fs::create_dir_all(&declaring).unwrap();
        let dep = root.path().join("http");
        write_local_package(&dep, "http");

        let resolved = LocalSource::resolve(&declaring, Path::new("../http")).unwrap();
        assert_eq!(resolved.manifest.name, "http");
        assert_eq!(resolved.canonical_path, dep.canonicalize().unwrap());
    }

    #[test]
    fn missing_local_path_is_a_clear_not_found_error() {
        let root = tempfile::tempdir().unwrap();
        let error = LocalSource::resolve(root.path(), Path::new("missing")).unwrap_err();
        assert!(matches!(error, PackageError::NotFound { .. }));
    }

    #[test]
    fn local_package_missing_its_declared_entry_file_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let dep = root.path().join("broken");
        fs::create_dir_all(&dep).unwrap();
        fs::write(
            dep.join("spar.package.spar"),
            r#"
            [Package] {
                name: str = "broken";
                version: str = "1.0.0";
                kind: str = "library";
            };
            "#,
        )
        .unwrap();
        // No src/lib.spar written — declared entry is missing.
        let error = LocalSource::resolve(root.path(), Path::new("broken")).unwrap_err();
        assert!(matches!(error, PackageError::NotFound { .. }));
    }
}
