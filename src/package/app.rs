//! `spar run --app <name>` — locate a package by name and find the entry file
//! whose `main` should run.
//!
//! Lookup order (offline, nothing is fetched):
//! 1. the nearest project at or above `start` whose `Package.name` is `name`;
//! 2. a package in that project's lockfile whose dependency alias or package
//!    name is `name` (path dependencies run from their source directory,
//!    remote ones from the store snapshot).

use std::path::{Path, PathBuf};

use crate::package::error::PackageError;
use crate::package::lockfile::{LockedSource, Lockfile, PACKAGE_LOCK_FILE};
use crate::package::manifest::{PackageKind, PackageManifest};
use crate::package::metadata::PACKAGE_MANIFEST_FILE;
use crate::package::store::PackageStore;

#[derive(Debug, Clone)]
pub struct AppTarget {
    pub name: String,
    pub root: PathBuf,
    pub entry: PathBuf,
    pub manifest: PackageManifest,
}

pub fn resolve_app(
    name: &str,
    start: &Path,
    store: &PackageStore,
) -> Result<AppTarget, PackageError> {
    let project = start
        .ancestors()
        .find(|dir| dir.join(PACKAGE_MANIFEST_FILE).is_file());

    if let Some(project) = project {
        let manifest = read_manifest(project)?;
        if manifest.name == name {
            return into_target(project, manifest);
        }
        if let Some(root) = locked_root(project, name, store)? {
            let manifest = read_manifest(&root)?;
            return into_target(&root, manifest);
        }
    }

    let searched = project
        .map(|p| format!(" (searched project '{}' and its locked dependencies)", p.display()))
        .unwrap_or_else(|| " (no spar.package.spar found in this directory or its parents)".to_string());
    Err(PackageError::NotFound {
        message: format!("no package named '{name}'{searched}"),
    })
}

fn read_manifest(root: &Path) -> Result<PackageManifest, PackageError> {
    let path = root.join(PACKAGE_MANIFEST_FILE);
    let text = std::fs::read_to_string(&path).map_err(|e| PackageError::Io {
        message: format!("{}: {e}", path.display()),
    })?;
    PackageManifest::parse(&text, &path)
}

fn locked_root(
    project: &Path,
    name: &str,
    store: &PackageStore,
) -> Result<Option<PathBuf>, PackageError> {
    let lock_path = project.join(PACKAGE_LOCK_FILE);
    if !lock_path.is_file() {
        return Ok(None);
    }
    let lockfile = Lockfile::read(&lock_path)?;
    let id = lockfile.root.get(name).cloned().or_else(|| {
        lockfile
            .packages
            .iter()
            .find(|(_, package)| package.name == name)
            .map(|(id, _)| id.clone())
    });
    let Some(id) = id else {
        return Ok(None);
    };
    let package = &lockfile.packages[&id];
    let root = match &package.source {
        LockedSource::Path { path } => project.join(path),
        LockedSource::Github { .. } => store.snapshot_path(&id),
    };
    if !root.is_dir() {
        return Err(PackageError::NotFound {
            message: format!(
                "package '{name}' is locked but not on disk at {} — run `spar install`",
                root.display()
            ),
        });
    }
    Ok(Some(root))
}

fn into_target(root: &Path, manifest: PackageManifest) -> Result<AppTarget, PackageError> {
    if manifest.kind != PackageKind::Application {
        return Err(PackageError::Manifest {
            message: format!(
                "package '{}' is a {}, not an application: it has no `main` to run",
                manifest.name,
                manifest.kind.as_str()
            ),
        });
    }
    let entry = root.join(&manifest.entry);
    if !entry.is_file() {
        return Err(PackageError::NotFound {
            message: format!(
                "package '{}' entry file {} does not exist",
                manifest.name,
                entry.display()
            ),
        });
    }
    Ok(AppTarget {
        name: manifest.name.clone(),
        root: root.to_path_buf(),
        entry,
        manifest,
    })
}
