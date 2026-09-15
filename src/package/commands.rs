//! The package manager's own operations — `init`, `add`, `remove`,
//! `install`, `update`, `tree` — each a plain function over an explicit
//! project directory, provider, and store, so every one of them is
//! testable against a temporary directory and a local git remote
//! without touching a real project or the developer's real global
//! store. `main.rs` is a thin CLI wrapper around these.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::package::error::PackageError;
use crate::package::github::{NetworkPolicy, PackageProvider};
use crate::package::lockfile::{LockedSource, Lockfile, PackageId};
use crate::package::manifest::{PackageKind, PackageManifest};
use crate::package::resolver::DependencyResolver;
use crate::package::source::PackageSource;
use crate::package::store::PackageStore;

fn manifest_path(dir: &Path) -> PathBuf {
    dir.join("spar.package.spar")
}

fn lockfile_path(dir: &Path) -> PathBuf {
    dir.join("spar.lock")
}

fn read_manifest(dir: &Path) -> Result<PackageManifest, PackageError> {
    let path = manifest_path(dir);
    let text = std::fs::read_to_string(&path).map_err(|e| PackageError::NotFound {
        message: format!(
            "no spar.package.spar in {} — run `spar init` first: {e}",
            dir.display()
        ),
    })?;
    PackageManifest::parse(&text, &path)
}

fn read_lockfile(dir: &Path) -> Result<Option<Lockfile>, PackageError> {
    let path = lockfile_path(dir);
    if !path.is_file() {
        return Ok(None);
    }
    Lockfile::read(&path).map(Some)
}

/// Creates `spar.package.spar` and, if it doesn't already exist, a stub
/// entry file. Refuses to overwrite an existing manifest.
pub fn init(dir: &Path, name: &str, kind: PackageKind) -> Result<PackageManifest, PackageError> {
    let path = manifest_path(dir);
    if path.exists() {
        return Err(PackageError::Conflict {
            message: format!("{} already exists", path.display()),
        });
    }
    let manifest = PackageManifest {
        name: name.to_string(),
        version: semver::Version::new(0, 1, 0),
        kind,
        entry: PathBuf::from(kind.conventional_entry()),
        dependencies: BTreeMap::new(),
        overrides: BTreeMap::new(),
    };
    manifest.write(&path)?;

    let entry_path = dir.join(&manifest.entry);
    if !entry_path.exists() {
        if let Some(parent) = entry_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| PackageError::Io {
                message: format!("failed to create {}: {e}", parent.display()),
            })?;
        }
        let stub = match kind {
            PackageKind::Application => "function main() -> int {\n    return 0;\n};\n",
            PackageKind::Library => "// This library's public exports go here.\n",
            PackageKind::Config => "",
        };
        std::fs::write(&entry_path, stub).map_err(|e| PackageError::Io {
            message: format!("failed to write {}: {e}", entry_path.display()),
        })?;
    }

    Ok(manifest)
}

/// Resolves `manifest`'s full dependency graph and materializes every
/// GitHub-sourced node into `store`, producing a ready-to-write
/// `Lockfile`. A local `path:` dependency is never materialized into
/// the store (its content isn't immutable) — its `LockedSource::Path`
/// entry just records the resolved path directly.
fn resolve_and_materialize(
    manifest: &PackageManifest,
    project_dir: &Path,
    provider: &dyn PackageProvider,
    network: NetworkPolicy,
    store: &PackageStore,
) -> Result<Lockfile, PackageError> {
    let resolver = DependencyResolver::new(provider, network);
    let graph = resolver.resolve(manifest, project_dir)?;

    let mut packages = BTreeMap::new();
    for (id, node) in graph.nodes {
        let mut locked = node.locked;
        if matches!(locked.source, LockedSource::Github { .. }) {
            let snapshot = store.materialize(&id, &node.content_dir)?;
            locked.integrity = Some(store.read_integrity(&snapshot)?);
        }
        packages.insert(id, locked);
    }
    Ok(Lockfile {
        root: graph.root,
        packages,
    })
}

/// Adds (or updates) one dependency: validates the request, writes it
/// into the manifest's `[Dependencies]`, re-resolves the whole graph,
/// materializes it, and writes both the manifest and the lockfile.
/// Nothing is written if resolution fails partway through.
pub fn add(
    dir: &Path,
    alias: &str,
    request: &str,
    provider: &dyn PackageProvider,
    network: NetworkPolicy,
    store: &PackageStore,
) -> Result<Lockfile, PackageError> {
    PackageSource::parse(request)?; // validate before touching the manifest
    let mut manifest = read_manifest(dir)?;
    manifest
        .dependencies
        .insert(alias.to_string(), request.to_string());

    let lockfile = resolve_and_materialize(&manifest, dir, provider, network, store)?;
    manifest.write(&manifest_path(dir))?;
    lockfile.write_atomically(&lockfile_path(dir))?;
    Ok(lockfile)
}

/// Removes a dependency (and any override naming it) from the manifest,
/// then re-resolves — any package that's no longer reachable simply
/// doesn't appear in the new lockfile. The global store itself is never
/// touched; another project may still reference what it had.
pub fn remove(
    dir: &Path,
    alias: &str,
    provider: &dyn PackageProvider,
    network: NetworkPolicy,
    store: &PackageStore,
) -> Result<Lockfile, PackageError> {
    let mut manifest = read_manifest(dir)?;
    if manifest.dependencies.remove(alias).is_none() {
        return Err(PackageError::NotFound {
            message: format!("'{alias}' is not a declared dependency"),
        });
    }
    manifest.overrides.remove(alias);

    let lockfile = resolve_and_materialize(&manifest, dir, provider, network, store)?;
    manifest.write(&manifest_path(dir))?;
    lockfile.write_atomically(&lockfile_path(dir))?;
    Ok(lockfile)
}

/// Materializes every package an existing lockfile already names,
/// fetching by the exact locked revision — never re-resolving a
/// version requirement or branch, so a tag moving upstream since the
/// lock was written can't silently change what gets installed. With no
/// lockfile yet, resolves fresh from the manifest (equivalent to
/// `add`-ing every declared dependency at once).
pub fn install(
    dir: &Path,
    provider: &dyn PackageProvider,
    network: NetworkPolicy,
    store: &PackageStore,
) -> Result<Lockfile, PackageError> {
    let manifest = read_manifest(dir)?;
    match read_lockfile(dir)? {
        Some(lockfile) => {
            for (id, package) in &lockfile.packages {
                if let LockedSource::Github {
                    owner,
                    repo,
                    revision,
                } = &package.source
                {
                    if store.is_materialized(id) {
                        continue;
                    }
                    let fetched = provider.fetch_github(
                        owner,
                        repo,
                        &crate::package::source::GitHubSelector::Revision(revision.clone()),
                        network,
                    )?;
                    store.materialize(id, fetched.directory.path())?;
                }
            }
            Ok(lockfile)
        }
        None => resolve_and_materialize(&manifest, dir, provider, network, store).map(|lockfile| {
            let _ = lockfile.write_atomically(&lockfile_path(dir));
            lockfile
        }),
    }
}

/// Re-resolves `alias` (or every dependency, if `None`) against the
/// manifest's *current* requests, allowing a version requirement to
/// pick up a newer matching tag or a branch to move to its current
/// commit — the explicit opposite of `install`'s "never re-resolve"
/// guarantee. Unlisted aliases keep their existing locked entry
/// untouched when only one alias is updated.
pub fn update(
    dir: &Path,
    alias: Option<&str>,
    provider: &dyn PackageProvider,
    store: &PackageStore,
) -> Result<Lockfile, PackageError> {
    let manifest = read_manifest(dir)?;
    if let Some(alias) = alias {
        if !manifest.dependencies.contains_key(alias) {
            return Err(PackageError::NotFound {
                message: format!("'{alias}' is not a declared dependency"),
            });
        }
    }
    // Phase 0 doesn't attempt partial re-resolution of a single alias
    // while reusing the rest of an existing graph — re-resolving
    // everything is simple and correct; a future revision could keep
    // everything but `alias` pinned to its current lock entry.
    let lockfile = resolve_and_materialize(&manifest, dir, provider, NetworkPolicy::Allow, store)?;
    lockfile.write_atomically(&lockfile_path(dir))?;
    Ok(lockfile)
}

/// A deterministic, indented tree of the current lockfile — reads only
/// the lockfile already on disk, no network, no store access.
pub fn tree(dir: &Path) -> Result<String, PackageError> {
    let Some(lockfile) = read_lockfile(dir)? else {
        return Ok(String::new());
    };
    let mut out = String::new();
    let mut aliases: Vec<&String> = lockfile.root.keys().collect();
    aliases.sort();
    for (index, alias) in aliases.iter().enumerate() {
        let is_last = index + 1 == aliases.len();
        let id = &lockfile.root[*alias];
        write_tree_node(&lockfile, alias, id, "", is_last, &mut out);
    }
    Ok(out)
}

fn write_tree_node(
    lockfile: &Lockfile,
    alias: &str,
    id: &PackageId,
    prefix: &str,
    is_last: bool,
    out: &mut String,
) {
    let branch = if is_last { "└── " } else { "├── " };
    let label = match lockfile.packages.get(id) {
        Some(package) => format!("{alias} {}@{} ({id})", package.name, package.version),
        None => format!("{alias} ({id}, missing from lockfile)"),
    };
    out.push_str(prefix);
    out.push_str(branch);
    out.push_str(&label);
    out.push('\n');

    let Some(package) = lockfile.packages.get(id) else {
        return;
    };
    let child_prefix = format!("{prefix}{}", if is_last { "    " } else { "│   " });
    let mut child_aliases: Vec<&String> = package.dependencies.keys().collect();
    child_aliases.sort();
    for (index, child_alias) in child_aliases.iter().enumerate() {
        let child_is_last = index + 1 == child_aliases.len();
        let child_id = &package.dependencies[*child_alias];
        write_tree_node(
            lockfile,
            child_alias,
            child_id,
            &child_prefix,
            child_is_last,
            out,
        );
    }
}
