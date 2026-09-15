//! Spar's package manager: manifests, lockfiles, the global store, and
//! GitHub/local dependency resolution. Reuses ordinary Spar imports for
//! package-aware code (`import "http" as http;` resolves through the
//! current project's lockfile when `"http"` isn't a filesystem path) —
//! no new syntax anywhere in this subsystem.

pub mod error;
pub mod manifest;
pub mod source;

pub use error::PackageError;
pub use manifest::{PackageKind, PackageManifest};
pub use source::{GitHubSelector, PackageSource};
