//! Parses a dependency request string — the right-hand side of a
//! `[Dependencies]`/`[Overrides]` manifest field — into a `PackageSource`.
//! Only GitHub and local-path sources exist in Phase 0; the match here is
//! the one place a future provider (`git:`, `gitlab:`, a registry) would
//! be added, behind the same enum.

use std::path::PathBuf;

use crate::package::error::PackageError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitHubSelector {
    /// A version requirement, always normalized so a bare tag like
    /// `@1.4.0` means exactly that version (`=1.4.0`), never "compatible
    /// with" — a locked dependency request should pin, not range, unless
    /// the author wrote an explicit range like `@^1.4`.
    VersionReq(semver::VersionReq),
    /// A branch name or exact commit, from `#revision`. Resolving this
    /// still locks to one concrete commit (Task 14) — this variant just
    /// says how the *request* selects it, not what ends up in the lock.
    Revision(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageSource {
    GitHub {
        owner: String,
        repo: String,
        selector: GitHubSelector,
    },
    LocalPath(PathBuf),
}

impl PackageSource {
    pub fn parse(request: &str) -> Result<Self, PackageError> {
        if let Some(rest) = request.strip_prefix("path:") {
            if rest.is_empty() {
                return Err(invalid(request, "'path:' needs a path after it"));
            }
            return Ok(PackageSource::LocalPath(PathBuf::from(rest)));
        }

        if let Some(rest) = request.strip_prefix("github:") {
            return parse_github(request, rest);
        }

        Err(invalid(
            request,
            "expected 'github:owner/repo@version', 'github:owner/repo#revision', or 'path:...'",
        ))
    }
}

fn parse_github(request: &str, rest: &str) -> Result<PackageSource, PackageError> {
    let (owner_repo, selector) = if let Some((base, version)) = rest.split_once('@') {
        let req = parse_version_selector(version)
            .map_err(|e| invalid(request, &format!("invalid version requirement: {e}")))?;
        (base, GitHubSelector::VersionReq(req))
    } else if let Some((base, revision)) = rest.split_once('#') {
        if revision.is_empty() {
            return Err(invalid(request, "'#' needs a branch or commit after it"));
        }
        (base, GitHubSelector::Revision(revision.to_string()))
    } else {
        return Err(invalid(
            request,
            "must include either '@version' or '#revision'",
        ));
    };

    let Some((owner, repo)) = owner_repo.split_once('/') else {
        return Err(invalid(request, "expected 'owner/repo' before '@'/'#'"));
    };
    if owner.is_empty() || repo.is_empty() {
        return Err(invalid(request, "owner and repo must not be empty"));
    }

    Ok(PackageSource::GitHub {
        owner: owner.to_string(),
        repo: repo.to_string(),
        selector,
    })
}

/// A bare version (`1.4.0`) means exactly that version; anything starting
/// with a comparison operator (`^1.4`, `>=1.0, <2.0`, `~1.2`) is passed
/// through to `semver::VersionReq` unchanged.
fn parse_version_selector(raw: &str) -> Result<semver::VersionReq, semver::Error> {
    match raw.chars().next() {
        Some(c) if c.is_ascii_digit() => semver::VersionReq::parse(&format!("={raw}")),
        _ => semver::VersionReq::parse(raw),
    }
}

fn invalid(request: &str, why: &str) -> PackageError {
    PackageError::InvalidRequest {
        message: format!("invalid dependency request '{request}': {why}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exact_version_local_path_and_revision_requests() {
        assert_eq!(
            PackageSource::parse("github:owner/repo@1.4.0").unwrap(),
            PackageSource::GitHub {
                owner: "owner".into(),
                repo: "repo".into(),
                selector: GitHubSelector::VersionReq(semver::VersionReq::parse("=1.4.0").unwrap()),
            }
        );
        assert_eq!(
            PackageSource::parse("github:owner/repo@^1.4").unwrap(),
            PackageSource::GitHub {
                owner: "owner".into(),
                repo: "repo".into(),
                selector: GitHubSelector::VersionReq(semver::VersionReq::parse("^1.4").unwrap()),
            }
        );
        assert_eq!(
            PackageSource::parse("github:owner/repo#main").unwrap(),
            PackageSource::GitHub {
                owner: "owner".into(),
                repo: "repo".into(),
                selector: GitHubSelector::Revision("main".into()),
            }
        );
        assert_eq!(
            PackageSource::parse("path:../local").unwrap(),
            PackageSource::LocalPath(PathBuf::from("../local"))
        );
    }

    #[test]
    fn exact_and_range_requests_are_distinct() {
        let PackageSource::GitHub {
            selector: GitHubSelector::VersionReq(exact),
            ..
        } = PackageSource::parse("github:o/r@1.4.0").unwrap()
        else {
            panic!("expected a version requirement");
        };
        assert!(exact.matches(&semver::Version::parse("1.4.0").unwrap()));
        assert!(!exact.matches(&semver::Version::parse("1.4.1").unwrap()));

        let PackageSource::GitHub {
            selector: GitHubSelector::VersionReq(range),
            ..
        } = PackageSource::parse("github:o/r@^1.4").unwrap()
        else {
            panic!("expected a version requirement");
        };
        assert!(range.matches(&semver::Version::parse("1.9.0").unwrap()));
        assert!(!range.matches(&semver::Version::parse("2.0.0").unwrap()));
    }

    #[test]
    fn rejects_malformed_requests() {
        for bad in [
            "owner/repo@1.0.0",
            "github:owner-only",
            "github:/repo@1.0.0",
            "github:owner/@1.0.0",
            "path:",
            "svn:owner/repo",
        ] {
            assert!(
                PackageSource::parse(bad).is_err(),
                "expected '{bad}' to be rejected"
            );
        }
    }
}
