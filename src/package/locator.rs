//! Routes a bare (non-filesystem-looking) import string through a
//! project's already-resolved `Lockfile` and `PackageStore` — the piece
//! that makes `import "http" as http;` mean "the `http` dependency this
//! project's manifest declared" instead of "a file literally named
//! `http`". Filesystem-like imports (`./x`, `../x`, `/x`, anything with
//! a `.spar` suffix) never reach this — `loader.rs` keeps resolving
//! those exactly as it always has.
//!
//! Deliberately offline: `resolve_import` only ever reads the lockfile
//! and store already on disk. It never resolves a version requirement,
//! never contacts a provider, and never materializes anything — if the
//! store doesn't have a dependency's snapshot yet, the caller sees a
//! plain "file not found" once it tries to read the path this returns,
//! the same as any other missing import.

use std::path::{Path, PathBuf};

use crate::package::lockfile::{Lockfile, PackageId};
use crate::package::store::PackageStore;

#[derive(Clone, Debug)]
pub struct ModuleLocator {
    /// The package whose `[Dependencies]` alias edges apply to a bare
    /// import resolved through this locator — `None` for the root
    /// project itself, whose edges live at `lockfile.root` rather than
    /// under some `PackageId` in `lockfile.packages`.
    current: Option<PackageId>,
    lockfile: Lockfile,
    store: PackageStore,
}

impl ModuleLocator {
    pub fn for_root(lockfile: Lockfile, store: PackageStore) -> Self {
        Self {
            current: None,
            lockfile,
            store,
        }
    }

    /// A locator for resolving imports *inside* an already-resolved
    /// dependency (`id`), so its own bare imports follow its own
    /// `[Dependencies]` edges, not the root project's.
    pub fn for_package(id: PackageId, lockfile: Lockfile, store: PackageStore) -> Self {
        Self {
            current: Some(id),
            lockfile,
            store,
        }
    }

    /// Resolves `alias` (already known not to look like a filesystem
    /// path) to that dependency's entry module inside its store
    /// snapshot. `None` when nothing in the current package's edges
    /// matches — the caller falls back to treating `alias` as an
    /// ordinary (and here, nonexistent) filesystem path, which produces
    /// the same "cannot find import file" diagnostic bare imports
    /// always have, rather than a separate "unknown package" message.
    pub fn resolve_import(&self, _base_dir: &Path, alias: &str) -> Option<PathBuf> {
        let edges = match &self.current {
            None => &self.lockfile.root,
            Some(id) => &self.lockfile.packages.get(id)?.dependencies,
        };
        let target_id = edges.get(alias)?;
        let package = self.lockfile.packages.get(target_id)?;
        let snapshot = self.store.snapshot_path(target_id);
        Some(snapshot.join(&package.entry))
    }

    /// A locator scoped to one of the current package's own
    /// dependencies, for recursing into a fetched package's own bare
    /// imports with the right edges.
    pub fn for_dependency(&self, target_id: PackageId) -> Self {
        Self {
            current: Some(target_id),
            lockfile: self.lockfile.clone(),
            store: self.store.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::lockfile::{LockedPackage, LockedSource};
    use crate::package::store::StorePaths;
    use std::collections::BTreeMap;

    fn sample_lockfile() -> Lockfile {
        let mut lockfile = Lockfile::default();
        lockfile
            .root
            .insert("http".to_string(), "github-owner-http-abc123".to_string());
        lockfile.packages.insert(
            "github-owner-http-abc123".to_string(),
            LockedPackage {
                name: "http".into(),
                version: "1.4.0".into(),
                source: LockedSource::Github {
                    owner: "owner".into(),
                    repo: "http".into(),
                    revision: "abc123abc123abc123abc123abc123abc123abcd".into(),
                },
                integrity: Some("sha256:deadbeef".into()),
                entry: "src/lib.spar".into(),
                dependencies: BTreeMap::new(),
            },
        );
        lockfile
    }

    fn test_store() -> PackageStore {
        PackageStore::new(StorePaths::new(
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        ))
    }

    #[test]
    fn bare_import_uses_current_packages_lock_edges() {
        let locator = ModuleLocator::for_root(sample_lockfile(), test_store());
        let resolved = locator
            .resolve_import(Path::new("."), "http")
            .expect("http alias must resolve");
        assert_eq!(
            resolved,
            PathBuf::from("/data/spar/store/github-owner-http-abc123/src/lib.spar")
        );
    }

    #[test]
    fn unknown_alias_resolves_to_nothing() {
        let locator = ModuleLocator::for_root(sample_lockfile(), test_store());
        assert!(locator
            .resolve_import(Path::new("."), "nonexistent")
            .is_none());
    }

    #[test]
    fn a_dependencys_own_edges_are_scoped_to_that_dependency_not_the_root() {
        let mut lockfile = sample_lockfile();
        lockfile
            .packages
            .get_mut("github-owner-http-abc123")
            .unwrap()
            .dependencies
            .insert("json".to_string(), "github-owner-json-def456".to_string());
        lockfile.packages.insert(
            "github-owner-json-def456".to_string(),
            LockedPackage {
                name: "json".into(),
                version: "2.0.0".into(),
                source: LockedSource::Github {
                    owner: "owner".into(),
                    repo: "json".into(),
                    revision: "def456def456def456def456def456def456defa".into(),
                },
                integrity: Some("sha256:cafebabe".into()),
                entry: "src/lib.spar".into(),
                dependencies: BTreeMap::new(),
            },
        );

        let root_locator = ModuleLocator::for_root(lockfile, test_store());
        // "json" is not one of the root's own edges.
        assert!(root_locator
            .resolve_import(Path::new("."), "json")
            .is_none());

        let http_locator = root_locator.for_dependency("github-owner-http-abc123".to_string());
        let resolved = http_locator
            .resolve_import(Path::new("."), "json")
            .expect("http's own dependency edge to json must resolve");
        assert_eq!(
            resolved,
            PathBuf::from("/data/spar/store/github-owner-json-def456/src/lib.spar")
        );
    }
}
