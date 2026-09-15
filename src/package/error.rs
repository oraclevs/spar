//! Error type shared by every `package::*` module.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageError {
    /// A `spar.package.spar` manifest is malformed, missing a required
    /// field, or uses a construct manifests don't allow (anything beyond
    /// literal `[Package]`/`[Dependencies]`/`[Overrides]` string fields).
    Manifest { message: String },
    /// A generated `spar.package.lock.spar` file is malformed or contains
    /// an internally inconsistent dependency graph.
    Lockfile { message: String },
    /// A dependency request string (`github:owner/repo@1.0.0`, `path:..`)
    /// doesn't parse.
    InvalidRequest { message: String },
    /// A filesystem operation on the store, cache, or a local dependency
    /// failed.
    Io { message: String },
    /// Something a lockfile or manifest names doesn't exist where
    /// expected (a local override path, a store snapshot, ...).
    NotFound { message: String },
    /// A store snapshot's recomputed hash doesn't match its recorded
    /// integrity value.
    IntegrityMismatch { message: String },
    /// A network operation was needed but not permitted
    /// (`NetworkPolicy::Offline`) or itself failed.
    Network { message: String },
    /// The dependency graph contains a cycle.
    Cycle { message: String },
    /// Two dependency requirements for the same package can't both be
    /// satisfied, or a lockfile write raced another writer.
    Conflict { message: String },
}

impl std::fmt::Display for PackageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            PackageError::Manifest { message }
            | PackageError::Lockfile { message }
            | PackageError::InvalidRequest { message }
            | PackageError::Io { message }
            | PackageError::NotFound { message }
            | PackageError::IntegrityMismatch { message }
            | PackageError::Network { message }
            | PackageError::Cycle { message }
            | PackageError::Conflict { message } => message,
        };
        f.write_str(message)
    }
}

impl std::error::Error for PackageError {}
