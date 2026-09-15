//! GitHub package source resolution — shells out to the system `git`
//! binary for every operation (`ls-remote`, `fetch`, `checkout`); no
//! HTTP client, no GitHub API. Kept behind the `PackageProvider` trait
//! so tests substitute a local bare repo instead of touching the real
//! network — `GitCommandProvider::with_remote_base` points every
//! `owner/repo` request at `<base>/owner/repo` instead of
//! `https://github.com/owner/repo.git`.

use std::path::Path;
use std::process::Command;

use crate::package::error::PackageError;
use crate::package::source::GitHubSelector;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkPolicy {
    Allow,
    Offline,
}

pub struct FetchedRevision {
    pub commit: String,
    /// That commit's full tree (no `.git`), in a directory that lives as
    /// long as this value does.
    pub directory: tempfile::TempDir,
}

pub trait PackageProvider {
    fn fetch_github(
        &self,
        owner: &str,
        repo: &str,
        selector: &GitHubSelector,
        network: NetworkPolicy,
    ) -> Result<FetchedRevision, PackageError>;
}

#[derive(Default)]
pub struct GitCommandProvider {
    /// `None` → `https://github.com/{owner}/{repo}.git`. `Some(base)` →
    /// `{base}/{owner}/{repo}` — how tests point at a local repo.
    remote_base: Option<String>,
}

impl GitCommandProvider {
    pub fn with_remote_base(base: impl Into<String>) -> Self {
        Self {
            remote_base: Some(base.into()),
        }
    }

    fn remote_url(&self, owner: &str, repo: &str) -> String {
        match &self.remote_base {
            Some(base) => format!("{}/{owner}/{repo}", base.trim_end_matches('/')),
            None => format!("https://github.com/{owner}/{repo}.git"),
        }
    }
}

impl PackageProvider for GitCommandProvider {
    fn fetch_github(
        &self,
        owner: &str,
        repo: &str,
        selector: &GitHubSelector,
        network: NetworkPolicy,
    ) -> Result<FetchedRevision, PackageError> {
        if network == NetworkPolicy::Offline {
            return Err(PackageError::Network {
                message: format!(
                    "resolving 'github:{owner}/{repo}' requires network access, which is \
                     disabled (offline mode) — run `spar update` with network allowed first"
                ),
            });
        }
        let url = self.remote_url(owner, repo);
        let commit = resolve_ref(&url, selector)?;
        let directory = fetch_commit(&url, &commit)?;
        Ok(FetchedRevision { commit, directory })
    }
}

fn run_git(args: &[&str]) -> Result<String, PackageError> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| PackageError::Network {
            message: format!("failed to run `git {}`: {e}", args.join(" ")),
        })?;
    if !output.status.success() {
        return Err(PackageError::Network {
            message: format!(
                "`git {}` failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn resolve_ref(url: &str, selector: &GitHubSelector) -> Result<String, PackageError> {
    match selector {
        GitHubSelector::Revision(rev) => {
            let output = run_git(&["ls-remote", url, rev])?;
            if let Some(sha) = output
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().next())
            {
                return Ok(sha.to_string());
            }
            if rev.len() >= 7 && rev.len() <= 40 && rev.chars().all(|c| c.is_ascii_hexdigit()) {
                // Looks like an exact commit already — `ls-remote` only
                // finds refs (branches/tags), not arbitrary commits.
                return Ok(rev.to_string());
            }
            Err(PackageError::NotFound {
                message: format!("no branch, tag, or commit named '{rev}' found at {url}"),
            })
        }
        GitHubSelector::VersionReq(req) => {
            let output = run_git(&["ls-remote", "--tags", url])?;
            let mut candidates: Vec<(semver::Version, String)> = Vec::new();
            for line in output.lines() {
                let mut parts = line.split_whitespace();
                let (Some(sha), Some(refname)) = (parts.next(), parts.next()) else {
                    continue;
                };
                let Some(tag) = refname.strip_prefix("refs/tags/") else {
                    continue;
                };
                // Prefer the dereferenced commit an annotated tag points
                // at (`^{}`) over the tag object's own sha.
                let tag = tag.strip_suffix("^{}").unwrap_or(tag);
                let version_text = tag.strip_prefix('v').unwrap_or(tag);
                if let Ok(version) = semver::Version::parse(version_text) {
                    if req.matches(&version) {
                        candidates.push((version, sha.to_string()));
                    }
                }
            }
            candidates.sort_by(|a, b| a.0.cmp(&b.0));
            candidates
                .into_iter()
                .next_back()
                .map(|(_, sha)| sha)
                .ok_or_else(|| PackageError::NotFound {
                    message: format!("no tag at {url} satisfies version requirement '{req}'"),
                })
        }
    }
}

fn fetch_commit(url: &str, commit: &str) -> Result<tempfile::TempDir, PackageError> {
    let dir = tempfile::tempdir().map_err(|e| PackageError::Io {
        message: e.to_string(),
    })?;
    let dir_str = dir.path().to_string_lossy().into_owned();
    run_git(&["init", "--quiet", &dir_str])?;
    run_git(&[
        "-C", &dir_str, "fetch", "--quiet", "--depth", "1", url, commit,
    ])?;
    run_git(&["-C", &dir_str, "checkout", "--quiet", "FETCH_HEAD"])?;
    remove_git_metadata(dir.path())?;
    Ok(dir)
}

fn remove_git_metadata(dir: &Path) -> Result<(), PackageError> {
    let git_dir = dir.join(".git");
    if git_dir.exists() {
        std::fs::remove_dir_all(&git_dir).map_err(|e| PackageError::Io {
            message: format!("failed to remove {}: {e}", git_dir.display()),
        })?;
    }
    Ok(())
}
