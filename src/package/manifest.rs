//! `spar.package.spar` — the project manifest, written in Spar itself.
//!
//! Deliberately restricted: exactly `[Package]`, `[Dependencies]`, and an
//! optional `[Overrides]` section, each holding only literal string
//! fields (no interpolation, no function calls, no imports). Parsing a
//! manifest never runs the resolver, type checker, or evaluator — it's a
//! plain AST walk over the lexer/parser output, so it's bootstrap-safe
//! and deterministic without needing the rest of the language pipeline.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::ast::{Expr, FieldValue, Program, SectionDecl, SectionItem, StringPart, TopLevelItem};
use crate::package::error::PackageError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageKind {
    Application,
    Library,
    Config,
}

impl PackageKind {
    pub fn conventional_entry(self) -> &'static str {
        match self {
            PackageKind::Application => "src/main.spar",
            PackageKind::Library => "src/lib.spar",
            PackageKind::Config => "src/config.spar",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PackageKind::Application => "application",
            PackageKind::Library => "library",
            PackageKind::Config => "config",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PackageManifest {
    pub name: String,
    pub version: semver::Version,
    pub kind: PackageKind,
    /// Always populated — either the manifest's explicit `entry` field or
    /// `kind`'s conventional default. Explicit is authoritative.
    pub entry: PathBuf,
    /// alias → raw dependency request string, e.g. `"github:owner/http@1.4.0"`.
    pub dependencies: BTreeMap<String, String>,
    /// alias → raw local-development override request, e.g. `"path:../http"`.
    pub overrides: BTreeMap<String, String>,
}

impl PackageManifest {
    pub fn parse(source: &str, path: &Path) -> Result<Self, PackageError> {
        let tokens = crate::Lexer::new(source)
            .tokenize()
            .map_err(|e| manifest_err(path, &e.to_string()))?;
        let program = crate::Parser::new(tokens)
            .parse()
            .map_err(|e| manifest_err(path, &e.to_string()))?;
        Self::from_program(&program, path)
    }

    fn from_program(program: &Program, path: &Path) -> Result<Self, PackageError> {
        let mut package_fields: Option<BTreeMap<String, String>> = None;
        let mut dependencies = BTreeMap::new();
        let mut overrides = BTreeMap::new();
        let mut saw_dependencies = false;
        let mut saw_overrides = false;

        for item in &program.items {
            let TopLevelItem::Section(section) = item else {
                return Err(unsupported(path));
            };
            match section.path.as_slice() {
                [name] if name == "Package" => {
                    if package_fields.is_some() {
                        return Err(manifest_err(path, "duplicate [Package] section"));
                    }
                    package_fields = Some(literal_fields(section, path)?);
                }
                [name] if name == "Dependencies" => {
                    if saw_dependencies {
                        return Err(manifest_err(path, "duplicate [Dependencies] section"));
                    }
                    saw_dependencies = true;
                    dependencies = literal_fields(section, path)?;
                }
                [name] if name == "Overrides" => {
                    if saw_overrides {
                        return Err(manifest_err(path, "duplicate [Overrides] section"));
                    }
                    saw_overrides = true;
                    overrides = literal_fields(section, path)?;
                }
                _ => return Err(unsupported(path)),
            }
        }

        let fields =
            package_fields.ok_or_else(|| manifest_err(path, "missing [Package] section"))?;

        let name = fields
            .get("name")
            .cloned()
            .ok_or_else(|| manifest_err(path, "[Package] is missing required field 'name'"))?;
        if !is_valid_package_name(&name) {
            return Err(manifest_err(
                path,
                &format!(
                    "[Package] name '{name}' is invalid — use lowercase letters, digits, '-', \
                     or '_', starting with a letter or digit"
                ),
            ));
        }

        let version_text = fields
            .get("version")
            .cloned()
            .ok_or_else(|| manifest_err(path, "[Package] is missing required field 'version'"))?;
        let version = semver::Version::parse(&version_text).map_err(|e| {
            manifest_err(
                path,
                &format!("[Package] version '{version_text}' is not valid SemVer: {e}"),
            )
        })?;

        let kind_text = fields
            .get("kind")
            .cloned()
            .ok_or_else(|| manifest_err(path, "[Package] is missing required field 'kind'"))?;
        let kind = match kind_text.as_str() {
            "application" => PackageKind::Application,
            "library" => PackageKind::Library,
            "config" => PackageKind::Config,
            other => {
                return Err(manifest_err(
                    path,
                    &format!(
                        "[Package] kind '{other}' is invalid — must be 'application', \
                         'library', or 'config'"
                    ),
                ))
            }
        };

        let entry = match fields.get("entry") {
            Some(explicit) => PathBuf::from(explicit),
            None => PathBuf::from(kind.conventional_entry()),
        };

        for (alias, request) in dependencies.iter().chain(overrides.iter()) {
            if alias.is_empty() {
                return Err(manifest_err(path, "a dependency alias cannot be empty"));
            }
            crate::package::source::PackageSource::parse(request).map_err(|e| {
                manifest_err(
                    path,
                    &format!("dependency '{alias}' has an invalid request: {e}"),
                )
            })?;
        }
        for alias in overrides.keys() {
            if !dependencies.contains_key(alias) {
                return Err(manifest_err(
                    path,
                    &format!(
                        "[Overrides] names '{alias}', which has no matching entry in \
                         [Dependencies] to override"
                    ),
                ));
            }
        }

        Ok(PackageManifest {
            name,
            version,
            kind,
            entry,
            dependencies,
            overrides,
        })
    }

    /// Renders this manifest back to canonical Spar source — the CLI's
    /// `init`/`add`/`remove` write manifests this way rather than
    /// text-patching an existing file, so the result is always a valid,
    /// literal-only manifest regardless of how the file looked before.
    /// This does not preserve comments or formatting a human added by
    /// hand; manifests are tool-managed the same way `spar.lock` is.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("[Package] {\n");
        out.push_str(&format!("    name: str = \"{}\";\n", escape(&self.name)));
        out.push_str(&format!("    version: str = \"{}\";\n", self.version));
        out.push_str(&format!("    kind: str = \"{}\";\n", self.kind.as_str()));
        out.push_str(&format!(
            "    entry: str = \"{}\";\n",
            escape(&self.entry.to_string_lossy())
        ));
        out.push_str("};\n");

        if !self.dependencies.is_empty() {
            out.push_str("\n[Dependencies] {\n");
            for (alias, request) in &self.dependencies {
                out.push_str(&format!("    {alias}: str = \"{}\";\n", escape(request)));
            }
            out.push_str("};\n");
        }

        if !self.overrides.is_empty() {
            out.push_str("\n[Overrides] {\n");
            for (alias, request) in &self.overrides {
                out.push_str(&format!("    {alias}: str = \"{}\";\n", escape(request)));
            }
            out.push_str("};\n");
        }

        out
    }

    pub fn write(&self, path: &Path) -> Result<(), PackageError> {
        std::fs::write(path, self.render()).map_err(|e| PackageError::Io {
            message: format!("failed to write {}: {e}", path.display()),
        })
    }
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn is_valid_package_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn unsupported(path: &Path) -> PackageError {
    manifest_err(
        path,
        "manifest may contain only literal [Package], [Dependencies], and [Overrides] sections",
    )
}

fn manifest_err(path: &Path, message: &str) -> PackageError {
    PackageError::Manifest {
        message: format!("{}: {message}", path.display()),
    }
}

fn literal_fields(
    section: &SectionDecl,
    path: &Path,
) -> Result<BTreeMap<String, String>, PackageError> {
    let mut out = BTreeMap::new();
    for item in &section.items {
        let SectionItem::Field(field) = item else {
            return Err(unsupported(path));
        };
        let Some(FieldValue::Expr(expr)) = &field.value else {
            return Err(unsupported(path));
        };
        let Some(text) = literal_str(expr) else {
            return Err(manifest_err(
                path,
                &format!(
                    "field '{}' must be a literal string with no interpolation, function \
                     calls, or references to other values",
                    field.name
                ),
            ));
        };
        if out.insert(field.name.clone(), text.to_string()).is_some() {
            return Err(manifest_err(
                path,
                &format!("duplicate field '{}'", field.name),
            ));
        }
    }
    Ok(out)
}

fn literal_str(expr: &Expr) -> Option<&str> {
    let Expr::String(s) = expr else {
        return None;
    };
    match s.parts.as_slice() {
        [] => Some(""),
        [StringPart::Literal(text)] => Some(text.as_str()),
        _ => None,
    }
}
