//! Spar's package manager: manifests, lockfiles, the global store, and
//! GitHub/local dependency resolution. Reuses ordinary Spar imports for
//! package-aware code (`import "http" as http;` resolves through the
//! current project's lockfile when `"http"` isn't a filesystem path) —
//! no new syntax anywhere in this subsystem.

pub mod commands;
pub mod error;
pub mod github;
pub mod local;
pub mod locator;
pub mod lockfile;
pub mod manifest;
pub mod resolver;
pub mod source;
pub mod store;

pub use error::PackageError;
pub use github::{FetchedRevision, GitCommandProvider, NetworkPolicy, PackageProvider};
pub use local::LocalSource;
pub use locator::ModuleLocator;
pub use lockfile::{
    github_package_id, local_package_id, LockedPackage, LockedSource, Lockfile, PackageId,
    LEGACY_PACKAGE_LOCK_FILE, PACKAGE_LOCK_FILE,
};
pub use manifest::{PackageKind, PackageManifest};
pub use resolver::{DependencyResolver, ResolvedGraph, ResolvedNode};
pub use source::{GitHubSelector, PackageSource};
pub use store::{PackageStore, StorePaths};
