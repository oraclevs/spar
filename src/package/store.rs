//! The durable, deduplicated global package store — resolved packages
//! belong to the machine, not to individual projects (spec section 23).
//! `StorePaths` follows XDG: immutable package contents live under
//! `$XDG_DATA_HOME/spar/store` (so a cache cleaner can't delete them),
//! disposable VCS/download metadata under `$XDG_CACHE_HOME/spar/`.
//!
//! Tests always inject explicit roots via `StorePaths::new` — never call
//! `StorePaths::from_env` from a test, or it touches the developer's real
//! global store.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::package::error::PackageError;

const INTEGRITY_MARKER: &str = ".spar-integrity";

#[derive(Debug, Clone)]
pub struct StorePaths {
    data_root: PathBuf,
    cache_root: PathBuf,
}

impl StorePaths {
    pub fn new(data_root: PathBuf, cache_root: PathBuf) -> Self {
        Self {
            data_root,
            cache_root,
        }
    }

    /// Real XDG roots (`$XDG_DATA_HOME`/`$XDG_CACHE_HOME`, falling back to
    /// `~/.local/share` and `~/.cache`). Only ever called by the actual
    /// CLI, never by a test.
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let data_root = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        let cache_root = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cache"));
        Self::new(data_root, cache_root)
    }

    pub fn store(&self) -> PathBuf {
        self.data_root.join("spar/store")
    }

    pub fn git_cache(&self) -> PathBuf {
        self.cache_root.join("spar/git")
    }

    pub fn download_cache(&self) -> PathBuf {
        self.cache_root.join("spar/downloads")
    }

    pub fn temporary(&self) -> PathBuf {
        self.cache_root.join("spar/temporary")
    }
}

#[derive(Debug, Clone)]
pub struct PackageStore {
    paths: StorePaths,
}

impl PackageStore {
    pub fn new(paths: StorePaths) -> Self {
        Self { paths }
    }

    pub fn paths(&self) -> &StorePaths {
        &self.paths
    }

    /// Where a package with this id lives (or would live) in the store —
    /// doesn't imply it's actually materialized yet.
    pub fn snapshot_path(&self, id: &str) -> PathBuf {
        self.paths.store().join(sanitize_id(id))
    }

    pub fn is_materialized(&self, id: &str) -> bool {
        self.snapshot_path(id).is_dir()
    }

    /// Copies `source_dir`'s contents into the store under `id`, records
    /// its content hash inside the snapshot, and returns the final path.
    /// If `id` is already materialized, returns the existing snapshot
    /// without touching it or re-copying anything — this is the
    /// dedication mechanism: many projects requesting the same immutable
    /// revision share one physical copy.
    pub fn materialize(&self, id: &str, source_dir: &Path) -> Result<PathBuf, PackageError> {
        let final_path = self.snapshot_path(id);
        if final_path.is_dir() {
            return Ok(final_path);
        }

        let store_root = self.paths.store();
        std::fs::create_dir_all(&store_root).map_err(|e| io_err(&store_root, e))?;

        let tmp = store_root.join(format!(".tmp-{}", sanitize_id(id)));
        if tmp.exists() {
            std::fs::remove_dir_all(&tmp).map_err(|e| io_err(&tmp, e))?;
        }
        copy_dir_recursive(source_dir, &tmp)?;

        let integrity = compute_integrity(&tmp)?;
        std::fs::write(tmp.join(INTEGRITY_MARKER), &integrity).map_err(|e| io_err(&tmp, e))?;

        std::fs::rename(&tmp, &final_path).map_err(|e| io_err(&final_path, e))?;
        Ok(final_path)
    }

    /// Recomputes a materialized snapshot's content hash and compares it
    /// against the one recorded when it was materialized.
    pub fn verify(&self, snapshot: &Path) -> Result<(), PackageError> {
        let recorded_path = snapshot.join(INTEGRITY_MARKER);
        let recorded =
            std::fs::read_to_string(&recorded_path).map_err(|e| PackageError::NotFound {
                message: format!(
                    "no recorded integrity marker at {}: {e}",
                    recorded_path.display()
                ),
            })?;
        let actual = compute_integrity(snapshot)?;
        if recorded.trim() != actual {
            return Err(PackageError::IntegrityMismatch {
                message: format!(
                    "snapshot at {} failed its integrity check (recorded {}, actual {actual})",
                    snapshot.display(),
                    recorded.trim()
                ),
            });
        }
        Ok(())
    }
}

fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn io_err(path: &Path, error: std::io::Error) -> PackageError {
    PackageError::Io {
        message: format!("{}: {error}", path.display()),
    }
}

fn copy_dir_recursive(from: &Path, to: &Path) -> Result<(), PackageError> {
    std::fs::create_dir_all(to).map_err(|e| io_err(to, e))?;
    for entry in std::fs::read_dir(from).map_err(|e| io_err(from, e))? {
        let entry = entry.map_err(|e| io_err(from, e))?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let src = entry.path();
        let dst = to.join(&name);
        if src.is_dir() {
            copy_dir_recursive(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| io_err(&src, e))?;
        }
    }
    Ok(())
}

/// Sorted relative file paths + contents, excluding VCS metadata and the
/// integrity marker itself (so writing the marker after computing this
/// doesn't change what a later `verify` recomputes).
fn compute_integrity(dir: &Path) -> Result<String, PackageError> {
    let mut files = BTreeMap::new();
    collect_files(dir, dir, &mut files)?;

    let mut hasher = Sha256::new();
    for (relative, absolute) in &files {
        let bytes = std::fs::read(absolute).map_err(|e| io_err(absolute, e))?;
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0u8]);
        hasher.update(&bytes);
        hasher.update([0u8]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn collect_files(
    root: &Path,
    current: &Path,
    out: &mut BTreeMap<PathBuf, PathBuf>,
) -> Result<(), PackageError> {
    for entry in std::fs::read_dir(current).map_err(|e| io_err(current, e))? {
        let entry = entry.map_err(|e| io_err(current, e))?;
        let name = entry.file_name();
        if name == ".git" || name == INTEGRITY_MARKER {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .expect("path is under root by construction")
                .to_path_buf();
            out.insert(relative, path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_paths_use_injected_xdg_roots() {
        let paths = StorePaths::new(PathBuf::from("/data"), PathBuf::from("/cache"));
        assert_eq!(paths.store(), Path::new("/data/spar/store"));
        assert_eq!(paths.git_cache(), Path::new("/cache/spar/git"));
        assert_eq!(paths.download_cache(), Path::new("/cache/spar/downloads"));
    }

    fn temp_store() -> (tempfile::TempDir, PackageStore) {
        let dir = tempfile::tempdir().unwrap();
        let paths = StorePaths::new(dir.path().join("data"), dir.path().join("cache"));
        (dir, PackageStore::new(paths))
    }

    fn write_package(dir: &Path, files: &[(&str, &str)]) {
        for (name, contents) in files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, contents).unwrap();
        }
    }

    #[test]
    fn same_identity_reuses_one_snapshot_and_two_revisions_coexist() {
        let (_root, store) = temp_store();
        let source = tempfile::tempdir().unwrap();
        write_package(source.path(), &[("src/lib.spar", "export var x: int = 1;")]);

        let first = store.materialize("colors-1.0", source.path()).unwrap();
        let repeated = store.materialize("colors-1.0", source.path()).unwrap();
        assert_eq!(first, repeated);

        let source_two = tempfile::tempdir().unwrap();
        write_package(
            source_two.path(),
            &[("src/lib.spar", "export var x: int = 2;")],
        );
        let second = store.materialize("colors-2.0", source_two.path()).unwrap();
        assert_ne!(first, second);

        assert!(store.is_materialized("colors-1.0"));
        assert!(store.is_materialized("colors-2.0"));
    }

    #[test]
    fn altered_snapshot_fails_sha256_integrity_check() {
        let (_root, store) = temp_store();
        let source = tempfile::tempdir().unwrap();
        write_package(source.path(), &[("src/lib.spar", "export var x: int = 1;")]);
        let snapshot = store.materialize("pkg", source.path()).unwrap();

        store
            .verify(&snapshot)
            .expect("freshly materialized snapshot must verify");

        std::fs::write(snapshot.join("src/lib.spar"), "export var x: int = 999;").unwrap();
        assert!(matches!(
            store.verify(&snapshot),
            Err(PackageError::IntegrityMismatch { .. })
        ));
    }

    #[test]
    fn integrity_is_stable_across_materialize_and_verify() {
        let (_root, store) = temp_store();
        let source = tempfile::tempdir().unwrap();
        write_package(
            source.path(),
            &[
                ("src/lib.spar", "export var x: int = 1;"),
                ("src/nested/other.spar", "export var y: int = 2;"),
            ],
        );
        let snapshot = store.materialize("pkg", source.path()).unwrap();
        store.verify(&snapshot).unwrap();
    }
}
