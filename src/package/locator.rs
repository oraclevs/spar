//! Routes an explicit `import pkg` request through a
//! project's already-resolved `Lockfile` and `PackageStore` — the piece
//! that makes `import pkg "http" as http;` resolve the `http` dependency
//! declared by the current package scope. Ordinary `import` never reaches
//! this locator; it remains a source-module/file import.
//!
//! Deliberately offline: `resolve_import` only ever reads the lockfile
//! and store already on disk. It never resolves a version requirement,
//! never contacts a provider, and never materializes anything — if the
//! store doesn't have a dependency's snapshot yet, the caller sees a
//! plain "file not found" once it tries to read the path this returns,
//! the same as any other missing import.

use std::path::{Path, PathBuf};

use crate::package::lockfile::{LockedSource, Lockfile, PackageId};
use crate::package::store::PackageStore;

#[derive(Clone, Debug)]
pub struct ModuleLocator {
    /// The package whose dependency-alias edges apply to an explicit
    /// `import pkg` resolved through this locator — `None` for the root
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
    /// dependency (`id`), so its own `import pkg` requests follow its own
    /// `[Dependencies]` edges, not the root project's.
    pub fn for_package(id: PackageId, lockfile: Lockfile, store: PackageStore) -> Self {
        Self {
            current: Some(id),
            lockfile,
            store,
        }
    }

    /// Return the dependency aliases visible from the current package scope.
    ///
    /// This is intentionally read-only and offline. Editor tooling can use it
    /// for `import pkg` completion without reaching into lockfile internals or
    /// triggering package resolution/network access.
    pub fn visible_import_aliases(&self) -> Vec<String> {
        let edges = match &self.current {
            None => &self.lockfile.root,
            Some(id) => {
                let Some(package) = self.lockfile.packages.get(id) else {
                    return Vec::new();
                };
                &package.dependencies
            }
        };
        let mut aliases = edges.keys().cloned().collect::<Vec<_>>();
        aliases.sort();
        aliases
    }

    /// Resolves a dependency import request to a concrete Spar module.
    /// `alias` resolves to the dependency entry module. `alias/sub/module`
    /// resolves relative to the entry module's directory and gains `.spar`
    /// when omitted. This gives package imports a stable module root without
    /// exposing the global store layout to source code.
    pub fn resolve_import(&self, base_dir: &Path, request: &str) -> Option<PathBuf> {
        self.resolve_import_scoped(base_dir, request)
            .map(|(path, _)| path)
    }

    /// Like `resolve_import`, but also returns a locator scoped to the
    /// resolved dependency. Imported package code must use this scoped
    /// locator for its own `import pkg ...` statements so transitive
    /// dependencies are resolved from that package's lockfile edges rather
    /// than from the root project's aliases.
    pub fn resolve_import_scoped(
        &self,
        _base_dir: &Path,
        request: &str,
    ) -> Option<(PathBuf, ModuleLocator)> {
        let (alias, submodule) = match request.split_once('/') {
            Some((alias, rest)) => (alias, Some(rest)),
            None => (request, None),
        };
        if alias.is_empty() {
            return None;
        }
        let edges = match &self.current {
            None => &self.lockfile.root,
            Some(id) => &self.lockfile.packages.get(id)?.dependencies,
        };
        let target_id = edges.get(alias)?.clone();
        let package = self.lockfile.packages.get(&target_id)?;
        let package_root = match &package.source {
            LockedSource::Github { .. } => self.store.snapshot_path(&target_id),
            LockedSource::Path { path } => PathBuf::from(path),
        };

        let path = match submodule {
            None => package_root.join(&package.entry),
            Some(submodule) => {
                if submodule.is_empty() {
                    return None;
                }
                let relative = Path::new(submodule);
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

                let entry = Path::new(&package.entry);
                let module_root = entry.parent().unwrap_or_else(|| Path::new(""));
                let mut module = module_root.join(relative);
                if module.extension().is_none() {
                    module.set_extension("spar");
                }
                package_root.join(module)
            }
        };

        Some((path, self.for_dependency(target_id)))
    }

    /// A locator scoped to one of the current package's own
    /// dependencies, for recursing into a fetched package's own `import pkg`
    /// statements with the right edges.
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
    fn package_submodule_resolves_beside_the_entry_and_adds_spar_extension() {
        let locator = ModuleLocator::for_root(sample_lockfile(), test_store());
        let resolved = locator
            .resolve_import(Path::new("."), "http/client")
            .expect("http/client must resolve");
        assert_eq!(
            resolved,
            PathBuf::from("/data/spar/store/github-owner-http-abc123/src/client.spar")
        );
    }

    #[test]
    fn package_submodule_cannot_escape_the_package_root() {
        let locator = ModuleLocator::for_root(sample_lockfile(), test_store());
        assert!(locator
            .resolve_import(Path::new("."), "http/../secret")
            .is_none());
    }

    #[test]
    fn unknown_alias_resolves_to_nothing() {
        let locator = ModuleLocator::for_root(sample_lockfile(), test_store());
        assert!(locator
            .resolve_import(Path::new("."), "nonexistent")
            .is_none());
    }

    #[test]
    fn local_path_import_uses_the_live_source_instead_of_the_immutable_store() {
        let mut lockfile = Lockfile::default();
        lockfile
            .root
            .insert("toolkit".to_string(), "path-toolkit".to_string());
        lockfile.packages.insert(
            "path-toolkit".to_string(),
            LockedPackage {
                name: "toolkit".into(),
                version: "1.0.0".into(),
                source: LockedSource::Path {
                    path: "/workspace/toolkit".into(),
                },
                integrity: None,
                entry: "src/lib.spar".into(),
                dependencies: BTreeMap::new(),
            },
        );
        let locator = ModuleLocator::for_root(lockfile, test_store());
        assert_eq!(
            locator.resolve_import(Path::new("."), "toolkit"),
            Some(PathBuf::from("/workspace/toolkit/src/lib.spar"))
        );
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
    #[test]
    fn visible_import_aliases_are_sorted_and_offline() {
        let mut lockfile = sample_lockfile();
        lockfile
            .root
            .insert("alpha".to_string(), "github-owner-http-abc123".to_string());
        let locator = ModuleLocator::for_root(lockfile, test_store());
        assert_eq!(
            locator.visible_import_aliases(),
            vec!["alpha".to_string(), "http".to_string()]
        );
    }
}
