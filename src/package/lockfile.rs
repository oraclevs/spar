//! `spar.package.lock.spar` — the exact, deterministic dependency graph Spar resolved
//! last time, so normal execution (`check`/`emit`/`exec`/task
//! runs/ordinary imports) never needs the network: it reads this file
//! and the global store, nothing else. Generated/managed by Spar; not
//! meant to be hand-edited. It is restricted, typed Spar source with
//! sorted entries, so editors understand it and diffs remain stable.

use std::collections::BTreeMap;
use std::path::Path;

use crate::ast::{Expr, FieldValue, Literal, SectionItem, StringPart, TopLevelItem};
use crate::package::error::PackageError;

pub const PACKAGE_LOCK_FILE: &str = "spar.package.lock.spar";
pub const LEGACY_PACKAGE_LOCK_FILE: &str = "spar.lock";

/// A package's stable identity within one lockfile — how root
/// dependencies and inter-package edges refer to a `LockedPackage`.
/// Not the same as a human dependency request (`github:owner/repo@1.0`):
/// this is the resolved, revision-pinned identity.
pub type PackageId = String;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LockedSource {
    Github {
        owner: String,
        repo: String,
        /// The exact resolved commit — a tag or branch in the original
        /// request is a human selector; this is what execution actually
        /// uses, and what makes a moved tag not silently alter a locked
        /// project.
        revision: String,
    },
    Path {
        /// Relative to the lockfile's own directory.
        path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,
    pub source: LockedSource,
    /// `"sha256:<hex>"` — absent for `LockedSource::Path` packages, whose
    /// content isn't immutable (Task 13 store dedup only applies to
    /// immutable remote revisions).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integrity: Option<String>,
    /// The package's public entry module, relative to its own root.
    pub entry: String,
    /// alias → dependency `PackageId`, sorted by alias.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, PackageId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Lockfile {
    /// alias → `PackageId`, for the root project's own `[Dependencies]`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub root: BTreeMap<String, PackageId>,
    /// Every resolved package in the graph (root's direct and transitive
    /// dependencies), keyed by `PackageId`. Two versions/revisions of
    /// "the same" dependency are two distinct entries here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub packages: BTreeMap<PackageId, LockedPackage>,
}

impl Lockfile {
    pub fn to_spar(&self) -> Result<String, PackageError> {
        let mut out = String::from(concat!(
            "struct Lock: SparPackageLock {\n",
            "    formatVersion = 1;\n",
            "    root: List<SparLockedDependency> = [\n",
        ));
        for (alias, package_id) in &self.root {
            out.push_str(&format!(
                "        {{ alias: \"{}\"; packageId: \"{}\"; }},\n",
                escape(alias),
                escape(package_id)
            ));
        }
        out.push_str("    ];\n    packages: List<SparLockedPackage> = [\n");
        for (id, package) in &self.packages {
            let (source_kind, source_location, revision) = match &package.source {
                LockedSource::Github {
                    owner,
                    repo,
                    revision,
                } => ("github", format!("{owner}/{repo}"), revision.as_str()),
                LockedSource::Path { path } => ("path", path.clone(), ""),
            };
            out.push_str("        {\n");
            for (name, value) in [
                ("id", id.as_str()),
                ("name", package.name.as_str()),
                ("version", package.version.as_str()),
                ("sourceKind", source_kind),
                ("sourceLocation", source_location.as_str()),
                ("revision", revision),
                ("integrity", package.integrity.as_deref().unwrap_or("")),
                ("entry", package.entry.as_str()),
            ] {
                out.push_str(&format!("            {name}: \"{}\";\n", escape(value)));
            }
            out.push_str("            dependencies: [\n");
            for (alias, dependency_id) in &package.dependencies {
                out.push_str(&format!(
                    "                {{ alias: \"{}\"; packageId: \"{}\"; }},\n",
                    escape(alias),
                    escape(dependency_id)
                ));
            }
            out.push_str("            ];\n        },\n");
        }
        out.push_str("    ];\n};\n");
        Ok(out)
    }

    pub fn parse_spar(text: &str, path: &Path) -> Result<Self, PackageError> {
        let tokens = crate::Lexer::new(text)
            .tokenize()
            .map_err(|error| lock_err(path, &error.to_string()))?;
        let program = crate::Parser::new(tokens)
            .parse()
            .map_err(|error| lock_err(path, &error.to_string()))?;
        let [TopLevelItem::Section(section)] = program.items.as_slice() else {
            return Err(lock_err(
                path,
                "lockfile must contain exactly one `struct Lock: SparPackageLock` declaration",
            ));
        };
        if section.path.as_slice() != ["Lock"]
            || section
                .type_binding
                .as_ref()
                .and_then(|binding| match &binding.ty {
                    crate::ast::SparType::Named(name) => Some(name.as_str()),
                    _ => None,
                })
                != Some("SparPackageLock")
        {
            return Err(lock_err(
                path,
                "lockfile must use `struct Lock: SparPackageLock { ... };`",
            ));
        }

        let fields = field_map(&section.items, path, "[Lock]")?;
        expect_fields(
            &fields,
            &["formatVersion", "root", "packages"],
            path,
            "[Lock]",
        )?;
        if int_field(&fields, "formatVersion", path, "[Lock]")? != 1 {
            return Err(lock_err(path, "unsupported lock formatVersion; expected 1"));
        }

        let mut root = BTreeMap::new();
        for edge in list_field(&fields, "root", path, "[Lock]")? {
            let (alias, package_id) = parse_edge(edge, path)?;
            if root.insert(alias.clone(), package_id).is_some() {
                return Err(lock_err(path, &format!("duplicate root alias '{alias}'")));
            }
        }

        let mut packages = BTreeMap::new();
        for package_expr in list_field(&fields, "packages", path, "[Lock]")? {
            let Expr::Object(items, _) = package_expr else {
                return Err(lock_err(
                    path,
                    "each packages item must be an object literal",
                ));
            };
            let package_fields = field_map(items, path, "locked package")?;
            expect_fields(
                &package_fields,
                &[
                    "id",
                    "name",
                    "version",
                    "sourceKind",
                    "sourceLocation",
                    "revision",
                    "integrity",
                    "entry",
                    "dependencies",
                ],
                path,
                "locked package",
            )?;
            let id = string_field(&package_fields, "id", path, "locked package")?.to_string();
            let name = string_field(&package_fields, "name", path, "locked package")?.to_string();
            let version =
                string_field(&package_fields, "version", path, "locked package")?.to_string();
            semver::Version::parse(&version).map_err(|error| {
                lock_err(
                    path,
                    &format!("package '{id}' has invalid version '{version}': {error}"),
                )
            })?;
            let source_kind = string_field(&package_fields, "sourceKind", path, "locked package")?;
            let source_location =
                string_field(&package_fields, "sourceLocation", path, "locked package")?;
            let revision = string_field(&package_fields, "revision", path, "locked package")?;
            let integrity_text =
                string_field(&package_fields, "integrity", path, "locked package")?;
            if id.is_empty() || name.is_empty() {
                return Err(lock_err(
                    path,
                    "locked package id and name must not be empty",
                ));
            }
            let source = match source_kind {
                "github" => {
                    let Some((owner, repo)) = source_location.split_once('/') else {
                        return Err(lock_err(
                            path,
                            &format!(
                                "package '{id}' GitHub sourceLocation must be owner/repository"
                            ),
                        ));
                    };
                    if owner.is_empty() || repo.is_empty() {
                        return Err(lock_err(
                            path,
                            &format!("package '{id}' GitHub source requires owner and repository"),
                        ));
                    }
                    if !is_full_git_commit(revision) {
                        return Err(lock_err(
                            path,
                            &format!("package '{id}' revision must be a full hexadecimal commit"),
                        ));
                    }
                    if !is_sha256_integrity(integrity_text) {
                        return Err(lock_err(
                            path,
                            &format!("package '{id}' must contain a complete SHA-256 integrity"),
                        ));
                    }
                    LockedSource::Github {
                        owner: owner.to_string(),
                        repo: repo.to_string(),
                        revision: revision.to_string(),
                    }
                }
                "path" => {
                    if source_location.is_empty()
                        || !revision.is_empty()
                        || !integrity_text.is_empty()
                    {
                        return Err(lock_err(
                            path,
                            &format!("package '{id}' path source requires a path and empty revision/integrity"),
                        ));
                    }
                    LockedSource::Path {
                        path: source_location.to_string(),
                    }
                }
                other => {
                    return Err(lock_err(
                        path,
                        &format!("package '{id}' has invalid sourceKind '{other}'"),
                    ))
                }
            };
            let mut dependencies = BTreeMap::new();
            for edge in list_field(&package_fields, "dependencies", path, "locked package")? {
                let (alias, package_id) = parse_edge(edge, path)?;
                if dependencies.insert(alias.clone(), package_id).is_some() {
                    return Err(lock_err(
                        path,
                        &format!("package '{id}' has duplicate dependency alias '{alias}'"),
                    ));
                }
            }
            let package = LockedPackage {
                name,
                version,
                source,
                integrity: (!integrity_text.is_empty()).then(|| integrity_text.to_string()),
                entry: string_field(&package_fields, "entry", path, "locked package")?.to_string(),
                dependencies,
            };
            if package.entry.is_empty() {
                return Err(lock_err(
                    path,
                    &format!("package '{id}' entry must not be empty"),
                ));
            }
            if packages.insert(id.clone(), package).is_some() {
                return Err(lock_err(path, &format!("duplicate package id '{id}'")));
            }
        }

        for (alias, package_id) in &root {
            if !packages.contains_key(package_id) {
                return Err(lock_err(
                    path,
                    &format!("root dependency '{alias}' targets missing package '{package_id}'"),
                ));
            }
        }
        for (id, package) in &packages {
            for (alias, package_id) in &package.dependencies {
                if !packages.contains_key(package_id) {
                    return Err(lock_err(
                        path,
                        &format!("package '{id}' dependency '{alias}' targets missing package '{package_id}'"),
                    ));
                }
            }
        }
        Ok(Self { root, packages })
    }

    pub fn read(path: &Path) -> Result<Self, PackageError> {
        let text = std::fs::read_to_string(path).map_err(|e| PackageError::Io {
            message: format!("failed to read lockfile at {}: {e}", path.display()),
        })?;
        Self::parse_spar(&text, path)
    }

    /// Writes via a temporary sibling file, then renames it into place —
    /// so a reader never observes a partially-written lockfile.
    pub fn write_atomically(&self, path: &Path) -> Result<(), PackageError> {
        let text = self.to_spar()?;
        let tmp = path.with_extension("spar.tmp");
        std::fs::write(&tmp, text).map_err(|e| PackageError::Io {
            message: format!("failed to write {}: {e}", tmp.display()),
        })?;
        std::fs::rename(&tmp, path).map_err(|e| PackageError::Io {
            message: format!("failed to finalize {}: {e}", path.display()),
        })
    }
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn lock_err(path: &Path, message: &str) -> PackageError {
    PackageError::Lockfile {
        message: format!("{}: {message}", path.display()),
    }
}

fn field_map<'a>(
    items: &'a [SectionItem],
    path: &Path,
    context: &str,
) -> Result<BTreeMap<String, &'a Expr>, PackageError> {
    let mut fields = BTreeMap::new();
    for item in items {
        let SectionItem::Field(field) = item else {
            return Err(lock_err(
                path,
                &format!("{context} may not contain spreads"),
            ));
        };
        let Some(FieldValue::Expr(value)) = &field.value else {
            return Err(lock_err(
                path,
                &format!("{context}.{} must have a literal value", field.name),
            ));
        };
        if fields.insert(field.name.clone(), value).is_some() {
            return Err(lock_err(
                path,
                &format!("{context} has duplicate field '{}'", field.name),
            ));
        }
    }
    Ok(fields)
}

fn expect_fields(
    fields: &BTreeMap<String, &Expr>,
    expected: &[&str],
    path: &Path,
    context: &str,
) -> Result<(), PackageError> {
    for name in expected {
        if !fields.contains_key(*name) {
            return Err(lock_err(
                path,
                &format!("{context} is missing required field '{name}'"),
            ));
        }
    }
    if let Some(name) = fields
        .keys()
        .find(|name| !expected.contains(&name.as_str()))
    {
        return Err(lock_err(
            path,
            &format!("{context} contains unknown field '{name}'"),
        ));
    }
    Ok(())
}

fn string_field<'a>(
    fields: &'a BTreeMap<String, &Expr>,
    name: &str,
    path: &Path,
    context: &str,
) -> Result<&'a str, PackageError> {
    let Some(Expr::String(string)) = fields.get(name).copied() else {
        return Err(lock_err(
            path,
            &format!("{context}.{name} must be a literal string"),
        ));
    };
    match string.parts.as_slice() {
        [] => Ok(""),
        [StringPart::Literal(text)] => Ok(text),
        _ => Err(lock_err(
            path,
            &format!("{context}.{name} may not use interpolation"),
        )),
    }
}

fn int_field(
    fields: &BTreeMap<String, &Expr>,
    name: &str,
    path: &Path,
    context: &str,
) -> Result<i64, PackageError> {
    match fields.get(name).copied() {
        Some(Expr::Literal(Literal::Int(value))) => Ok(*value),
        _ => Err(lock_err(
            path,
            &format!("{context}.{name} must be an integer"),
        )),
    }
}

fn list_field<'a>(
    fields: &'a BTreeMap<String, &Expr>,
    name: &str,
    path: &Path,
    context: &str,
) -> Result<&'a [Expr], PackageError> {
    match fields.get(name).copied() {
        Some(Expr::List(values, _)) => Ok(values),
        _ => Err(lock_err(path, &format!("{context}.{name} must be a list"))),
    }
}

fn parse_edge(expr: &Expr, path: &Path) -> Result<(String, PackageId), PackageError> {
    let Expr::Object(items, _) = expr else {
        return Err(lock_err(
            path,
            "each dependency edge must be an object literal",
        ));
    };
    let fields = field_map(items, path, "dependency edge")?;
    expect_fields(&fields, &["alias", "packageId"], path, "dependency edge")?;
    let alias = string_field(&fields, "alias", path, "dependency edge")?;
    let package_id = string_field(&fields, "packageId", path, "dependency edge")?;
    if alias.is_empty() || package_id.is_empty() {
        return Err(lock_err(
            path,
            "dependency edge alias and packageId must not be empty",
        ));
    }
    Ok((alias.to_string(), package_id.to_string()))
}

fn is_full_git_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_sha256_integrity(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

/// A `PackageId` for a resolved GitHub revision: stable, filesystem-safe,
/// and distinct per (owner, repo, revision) — two different revisions of
/// the same repo are two different ids, which is exactly what lets them
/// coexist in the store.
pub fn github_package_id(owner: &str, repo: &str, revision: &str) -> PackageId {
    let short = &revision[..revision.len().min(12)];
    format!("github-{owner}-{repo}-{short}")
}

/// A `PackageId` for a local path dependency, derived from its
/// canonicalized (or, failing that, lexically normalized) absolute path
/// — stable across two manifests requesting the same directory.
pub fn local_package_id(canonical_path: &Path) -> PackageId {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(canonical_path.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    format!("path-{:x}", digest)[..21].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_package(name: &str) -> LockedPackage {
        LockedPackage {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            source: LockedSource::Github {
                owner: "owner".into(),
                repo: name.into(),
                revision: "0123456789abcdef0123456789abcdef01234567".into(),
            },
            integrity: Some(format!("sha256:{}", "d".repeat(64))),
            entry: "src/lib.spar".into(),
            dependencies: BTreeMap::new(),
        }
    }

    fn lockfile_in_order(names: &[&str]) -> Lockfile {
        let mut lockfile = Lockfile::default();
        for name in names {
            lockfile
                .root
                .insert(name.to_string(), format!("pkg-{name}"));
            lockfile
                .packages
                .insert(format!("pkg-{name}"), sample_package(name));
        }
        lockfile
    }

    #[test]
    fn lockfile_serialization_is_stable_regardless_of_insertion_order() {
        let forward = lockfile_in_order(&["a", "b"]).to_spar().unwrap();
        let backward = lockfile_in_order(&["b", "a"]).to_spar().unwrap();
        assert_eq!(forward, backward);
    }

    #[test]
    fn lockfile_round_trips_through_spar() {
        let original = lockfile_in_order(&["a", "b"]);
        let path = Path::new("spar.package.lock.spar");
        let text = original.to_spar().unwrap();
        assert!(text.starts_with("struct Lock: SparPackageLock {"));
        assert!(text.contains("\n    formatVersion = 1;\n    root: List<SparLockedDependency> ="));
        assert!(!text.contains("[[packages]]"));
        let parsed = Lockfile::parse_spar(&text, path).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn write_atomically_never_leaves_a_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spar.package.lock.spar");
        lockfile_in_order(&["a"]).write_atomically(&path).unwrap();
        assert!(path.exists());
        assert!(!path.with_extension("spar.tmp").exists());
        let parsed = Lockfile::read(&path).unwrap();
        assert_eq!(parsed, lockfile_in_order(&["a"]));
    }

    #[test]
    fn github_ids_differ_per_revision_and_local_ids_are_stable() {
        let a = github_package_id("owner", "repo", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let b = github_package_id("owner", "repo", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_ne!(a, b);

        let p1 = local_package_id(Path::new("/tmp/http"));
        let p2 = local_package_id(Path::new("/tmp/http"));
        let p3 = local_package_id(Path::new("/tmp/other"));
        assert_eq!(p1, p2);
        assert_ne!(p1, p3);
    }

    #[test]
    fn lockfile_rejects_non_immutable_github_identity() {
        let path = Path::new("spar.package.lock.spar");
        let source = lockfile_in_order(&["http"])
            .to_spar()
            .unwrap()
            .replace("0123456789abcdef0123456789abcdef01234567", "moving-branch");
        let error = Lockfile::parse_spar(&source, path).unwrap_err();
        assert!(error.to_string().contains("full hexadecimal commit"));

        let source = lockfile_in_order(&["http"])
            .to_spar()
            .unwrap()
            .replace(&format!("sha256:{}", "d".repeat(64)), "sha256:bad");
        let error = Lockfile::parse_spar(&source, path).unwrap_err();
        assert!(error.to_string().contains("SHA-256 integrity"));
    }
}
