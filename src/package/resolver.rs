//! Deterministic dependency-graph resolution over `PackageProvider` (for
//! GitHub) and `LocalSource` (for `path:` requests), producing a
//! `ResolvedGraph` ready to be turned into a `Lockfile` and materialized
//! into the store.
//!
//! What this module deliberately does NOT do: decide when resolution is
//! allowed to happen. That's a caller concern (spec section 28) — normal
//! execution (check/emit/exec/task runs/ordinary imports) never
//! constructs a `DependencyResolver` at all, reading only an existing
//! `Lockfile` and the store instead. Only explicit commands (`add`,
//! `update`) call this.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::package::error::PackageError;
use crate::package::github::{NetworkPolicy, PackageProvider};
use crate::package::local::LocalSource;
use crate::package::lockfile::{
    github_package_id, local_package_id, LockedPackage, LockedSource, PackageId,
};
use crate::package::manifest::PackageManifest;
use crate::package::source::PackageSource;

#[derive(Debug)]
pub struct ResolvedNode {
    pub manifest: PackageManifest,
    pub locked: LockedPackage,
    /// Where this node's file content currently lives — the canonical
    /// path for a local dependency, or a fetched GitHub revision's temp
    /// directory (kept alive by the `DependencyResolver` that produced
    /// this graph; materialize it into the store before dropping the
    /// resolver if you need it to persist).
    pub content_dir: PathBuf,
}

#[derive(Debug)]
pub struct ResolvedGraph {
    /// alias → `PackageId`, for the resolved root project's own
    /// `[Dependencies]` (after applying its own `[Overrides]`).
    pub root: BTreeMap<String, PackageId>,
    pub nodes: BTreeMap<PackageId, ResolvedNode>,
}

pub struct DependencyResolver<'a> {
    provider: &'a dyn PackageProvider,
    network: NetworkPolicy,
    // Keeps every fetched GitHub revision's TempDir alive for as long as
    // the resolver itself lives, since `ResolvedNode::content_dir` points
    // into it.
    fetches: RefCell<Vec<tempfile::TempDir>>,
}

struct Outcome {
    id: PackageId,
    manifest: PackageManifest,
    content_dir: PathBuf,
    locked_source: LockedSource,
}

impl<'a> DependencyResolver<'a> {
    pub fn new(provider: &'a dyn PackageProvider, network: NetworkPolicy) -> Self {
        Self {
            provider,
            network,
            fetches: RefCell::new(Vec::new()),
        }
    }

    pub fn resolve(
        &self,
        root_manifest: &PackageManifest,
        root_dir: &Path,
    ) -> Result<ResolvedGraph, PackageError> {
        let mut nodes = BTreeMap::new();
        let mut in_progress: Vec<(PackageId, String)> = Vec::new();
        let root = self.resolve_children(root_manifest, root_dir, &mut in_progress, &mut nodes)?;
        Ok(ResolvedGraph { root, nodes })
    }

    fn resolve_children(
        &self,
        manifest: &PackageManifest,
        manifest_dir: &Path,
        in_progress: &mut Vec<(PackageId, String)>,
        nodes: &mut BTreeMap<PackageId, ResolvedNode>,
    ) -> Result<BTreeMap<String, PackageId>, PackageError> {
        let mut edges = BTreeMap::new();
        let mut aliases: Vec<&String> = manifest.dependencies.keys().collect();
        aliases.sort();

        for alias in aliases {
            let raw = manifest
                .overrides
                .get(alias)
                .unwrap_or(&manifest.dependencies[alias]);
            let source = PackageSource::parse(raw)?;
            let outcome = self.fetch_one(&source, manifest_dir)?;
            edges.insert(alias.clone(), outcome.id.clone());

            if let Some(cycle_start) = in_progress.iter().position(|(id, _)| *id == outcome.id) {
                let mut labels: Vec<&str> = in_progress[cycle_start..]
                    .iter()
                    .map(|(_, label)| label.as_str())
                    .collect();
                labels.push(alias.as_str());
                return Err(PackageError::Cycle {
                    message: format!("dependency cycle: {}", labels.join(" > ")),
                });
            }
            if nodes.contains_key(&outcome.id) {
                continue; // diamond dependency — already fully resolved elsewhere
            }

            in_progress.push((outcome.id.clone(), alias.clone()));
            let child_edges =
                self.resolve_children(&outcome.manifest, &outcome.content_dir, in_progress, nodes)?;
            in_progress.pop();

            nodes.insert(
                outcome.id.clone(),
                ResolvedNode {
                    locked: LockedPackage {
                        name: outcome.manifest.name.clone(),
                        version: outcome.manifest.version.to_string(),
                        source: outcome.locked_source,
                        integrity: None, // computed when materializing into the store
                        entry: outcome.manifest.entry.to_string_lossy().into_owned(),
                        dependencies: child_edges,
                    },
                    manifest: outcome.manifest,
                    content_dir: outcome.content_dir,
                },
            );
        }
        Ok(edges)
    }

    fn fetch_one(
        &self,
        source: &PackageSource,
        declaring_dir: &Path,
    ) -> Result<Outcome, PackageError> {
        match source {
            PackageSource::LocalPath(relative) => {
                let local = LocalSource::resolve(declaring_dir, relative)?;
                let id = local_package_id(&local.canonical_path);
                let locked_source = LockedSource::Path {
                    path: local.canonical_path.to_string_lossy().into_owned(),
                };
                Ok(Outcome {
                    id,
                    content_dir: local.canonical_path,
                    manifest: local.manifest,
                    locked_source,
                })
            }
            PackageSource::GitHub {
                owner,
                repo,
                selector,
            } => {
                let fetched = self
                    .provider
                    .fetch_github(owner, repo, selector, self.network)?;
                let id = github_package_id(owner, repo, &fetched.commit);
                let manifest_path = fetched.directory.path().join("spar.package.spar");
                let manifest_text = std::fs::read_to_string(&manifest_path).map_err(|e| {
                    PackageError::NotFound {
                        message: format!(
                            "github:{owner}/{repo}@{}: no spar.package.spar: {e}",
                            fetched.commit
                        ),
                    }
                })?;
                let manifest = PackageManifest::parse(&manifest_text, &manifest_path)?;
                let content_dir = fetched.directory.path().to_path_buf();
                self.fetches.borrow_mut().push(fetched.directory);
                Ok(Outcome {
                    id,
                    content_dir,
                    manifest,
                    locked_source: LockedSource::Github {
                        owner: owner.clone(),
                        repo: repo.clone(),
                        revision: fetched.commit,
                    },
                })
            }
        }
    }
}
