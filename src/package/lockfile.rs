//! `spar.lock` — the exact, deterministic dependency graph Spar resolved
//! last time, so normal execution (`check`/`emit`/`exec`/task
//! runs/ordinary imports) never needs the network: it reads this file
//! and the global store, nothing else. Generated/managed by Spar; not
//! meant to be hand-edited, but it's plain TOML with sorted keys so a
//! diff is readable and stable regardless of resolution order.

use std::collections::BTreeMap;
use std::path::Path;

use crate::package::error::PackageError;

/// A package's stable identity within one lockfile — how root
/// dependencies and inter-package edges refer to a `LockedPackage`.
/// Not the same as a human dependency request (`github:owner/repo@1.0`):
/// this is the resolved, revision-pinned identity.
pub type PackageId = String;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LockedSource {
    Github {
        owner: String,
        repo: String,
        /// The exact resolved commit — a tag or branch in the original
        /// request is a human selector; this is what execution actually
        /// uses, and what makes a moved tag not silently alter a locked
        /// project.
        revision: String,
    },
    Path {
        /// Relative to the lockfile's own directory.
        path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,
    pub source: LockedSource,
    /// `"sha256:<hex>"` — absent for `LockedSource::Path` packages, whose
    /// content isn't immutable (Task 13 store dedup only applies to
    /// immutable remote revisions).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integrity: Option<String>,
    /// The package's public entry module, relative to its own root.
    pub entry: String,
    /// alias → dependency `PackageId`, sorted by alias.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, PackageId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Lockfile {
    /// alias → `PackageId`, for the root project's own `[Dependencies]`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub root: BTreeMap<String, PackageId>,
    /// Every resolved package in the graph (root's direct and transitive
    /// dependencies), keyed by `PackageId`. Two versions/revisions of
    /// "the same" dependency are two distinct entries here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub packages: BTreeMap<PackageId, LockedPackage>,
}

impl Lockfile {
    pub fn to_toml(&self) -> Result<String, PackageError> {
        toml::to_string_pretty(self).map_err(|e| PackageError::Io {
            message: format!("failed to serialize lockfile: {e}"),
        })
    }

    pub fn parse_toml(text: &str) -> Result<Self, PackageError> {
        toml::from_str(text).map_err(|e| PackageError::Io {
            message: format!("failed to parse lockfile: {e}"),
        })
    }

    pub fn read(path: &Path) -> Result<Self, PackageError> {
        let text = std::fs::read_to_string(path).map_err(|e| PackageError::Io {
            message: format!("failed to read lockfile at {}: {e}", path.display()),
        })?;
        Self::parse_toml(&text)
    }

    /// Writes via a temporary sibling file, then renames it into place —
    /// so a reader never observes a partially-written lockfile.
    pub fn write_atomically(&self, path: &Path) -> Result<(), PackageError> {
        let text = self.to_toml()?;
        let tmp = path.with_extension("lock.tmp");
        std::fs::write(&tmp, text).map_err(|e| PackageError::Io {
            message: format!("failed to write {}: {e}", tmp.display()),
        })?;
        std::fs::rename(&tmp, path).map_err(|e| PackageError::Io {
            message: format!("failed to finalize {}: {e}", path.display()),
        })
    }
}

/// A `PackageId` for a resolved GitHub revision: stable, filesystem-safe,
/// and distinct per (owner, repo, revision) — two different revisions of
/// the same repo are two different ids, which is exactly what lets them
/// coexist in the store.
pub fn github_package_id(owner: &str, repo: &str, revision: &str) -> PackageId {
    let short = &revision[..revision.len().min(12)];
    format!("github-{owner}-{repo}-{short}")
}

/// A `PackageId` for a local path dependency, derived from its
/// canonicalized (or, failing that, lexically normalized) absolute path
/// — stable across two manifests requesting the same directory.
pub fn local_package_id(canonical_path: &Path) -> PackageId {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(canonical_path.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    format!("path-{:x}", digest)[..21].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_package(name: &str) -> LockedPackage {
        LockedPackage {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            source: LockedSource::Github {
                owner: "owner".into(),
                repo: name.into(),
                revision: "0123456789abcdef0123456789abcdef01234567".into(),
            },
            integrity: Some("sha256:deadbeef".into()),
            entry: "src/lib.spar".into(),
            dependencies: BTreeMap::new(),
        }
    }

    fn lockfile_in_order(names: &[&str]) -> Lockfile {
        let mut lockfile = Lockfile::default();
        for name in names {
            lockfile
                .root
                .insert(name.to_string(), format!("pkg-{name}"));
            lockfile
                .packages
                .insert(format!("pkg-{name}"), sample_package(name));
        }
        lockfile
    }

    #[test]
    fn lockfile_serialization_is_stable_regardless_of_insertion_order() {
        let forward = lockfile_in_order(&["a", "b"]).to_toml().unwrap();
        let backward = lockfile_in_order(&["b", "a"]).to_toml().unwrap();
        assert_eq!(forward, backward);
    }

    #[test]
    fn lockfile_round_trips_through_toml() {
        let original = lockfile_in_order(&["a", "b"]);
        let text = original.to_toml().unwrap();
        let parsed = Lockfile::parse_toml(&text).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn write_atomically_never_leaves_a_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spar.lock");
        lockfile_in_order(&["a"]).write_atomically(&path).unwrap();
        assert!(path.exists());
        assert!(!path.with_extension("lock.tmp").exists());
        let parsed = Lockfile::read(&path).unwrap();
        assert_eq!(parsed, lockfile_in_order(&["a"]));
    }

    #[test]
    fn github_ids_differ_per_revision_and_local_ids_are_stable() {
        let a = github_package_id("owner", "repo", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let b = github_package_id("owner", "repo", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_ne!(a, b);

        let p1 = local_package_id(Path::new("/tmp/http"));
        let p2 = local_package_id(Path::new("/tmp/http"));
        let p3 = local_package_id(Path::new("/tmp/other"));
        assert_eq!(p1, p2);
        assert_ne!(p1, p3);
    }
}
