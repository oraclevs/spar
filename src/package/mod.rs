//! Spar's package manager: manifests, lockfiles, the global store, and
//! GitHub/local dependency resolution. Package-namespace imports use explicit `import pkg` syntax and resolve
//! through the current package scope in the lock graph; ordinary `import`
//! is reserved for source modules/files.

pub mod commands;
pub mod error;
pub mod github;
pub mod local;
pub mod locator;
pub mod lockfile;
pub mod manifest;
pub mod metadata;
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
pub use metadata::{metadata_kind, MetadataFileKind, PACKAGE_MANIFEST_FILE};
pub use resolver::{DependencyResolver, ResolvedGraph, ResolvedNode};
pub use source::{GitHubSelector, PackageSource};
pub use store::{PackageStore, StorePaths};
