//! `spar.package.spar` — the project manifest, written in Spar itself.
//!
//! Deliberately restricted: exactly `struct Package`, `struct Dependencies`, and an
//! optional `struct Overrides` declaration, each holding only literal string
//! fields (no interpolation, no function calls, no imports). Parsing a
//! manifest never runs the resolver, type checker, or evaluator — it's a
//! plain AST walk over the lexer/parser output, so it's bootstrap-safe
//! and deterministic without needing the rest of the language pipeline.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::ast::{Expr, FieldValue, Program, StructDecl, ObjectItem, StringPart, TopLevelItem};
use crate::package::error::PackageError;
use crate::runtime_config::RuntimeSettings;

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

/// Native extension declared by a package (`struct Native { ... }`).
///
/// ```spar
/// struct Native {
///     module: str = "fastArray";
///     abi: str = "spar-native-0";
///     capabilities: str = "typed-arrays,strings";
///     linux_x86_64_gnu: str = "native/linux-x86_64-gnu/libfastarray.so";
///     linux_x86_64_gnu_sha256: str = "<hex>";
/// };
/// ```
/// Artifact keys are `<os>_<arch>[_<env>]` (and `..._sha256`); paths are relative to the package
/// root and may not escape it. Nothing is ever searched for: only the path named here is loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSpec {
    pub module: String,
    pub abi: String,
    pub capabilities: Vec<String>,
    /// target key (`linux_x86_64_gnu`) -> (relative path, optional lowercase hex sha256)
    pub artifacts: BTreeMap<String, (String, Option<String>)>,
}

pub const NATIVE_ABI_NAME: &str = "spar-native-0";

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
    pub native: Option<NativeSpec>,
    /// `struct Runtime` — settings that replace the `SPAR_*` environment switches.
    pub runtime: RuntimeSettings,
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
        let mut native = None;
        let mut runtime: Option<RuntimeSettings> = None;

        for item in &program.items {
            let TopLevelItem::Struct(structure) = item else {
                return Err(unsupported(path));
            };
            match structure.name.as_str() {
                "Package" => {
                    if package_fields.is_some() {
                        return Err(manifest_err(path, "duplicate `struct Package` declaration"));
                    }
                    package_fields = Some(literal_fields(structure, path)?);
                }
                "Dependencies" => {
                    if saw_dependencies {
                        return Err(manifest_err(path, "duplicate `struct Dependencies` declaration"));
                    }
                    saw_dependencies = true;
                    dependencies = literal_fields(structure, path)?;
                }
                "Overrides" => {
                    if saw_overrides {
                        return Err(manifest_err(path, "duplicate `struct Overrides` declaration"));
                    }
                    saw_overrides = true;
                    overrides = literal_fields(structure, path)?;
                }
                "Native" => {
                    if native.is_some() {
                        return Err(manifest_err(path, "duplicate `struct Native` declaration"));
                    }
                    native = Some(native_spec(literal_fields(structure, path)?, path)?);
                }
                "Runtime" => {
                    if runtime.is_some() {
                        return Err(manifest_err(path, "duplicate `struct Runtime` declaration"));
                    }
                    runtime = Some(runtime_settings(structure, path)?);
                }
                _ => return Err(unsupported(path)),
            }
        }
        let runtime = runtime.unwrap_or_default();

        let fields =
            package_fields.ok_or_else(|| manifest_err(path, "missing Package declaration"))?;
        if let Some(field) = fields
            .keys()
            .find(|field| !matches!(field.as_str(), "name" | "version" | "kind" | "entry"))
        {
            return Err(manifest_err(
                path,
                &format!("`struct Package` contains unknown field '{field}'"),
            ));
        }

        let name = fields
            .get("name")
            .cloned()
            .ok_or_else(|| manifest_err(path, "`struct Package` is missing required field 'name'"))?;
        if !is_valid_package_name(&name) {
            return Err(manifest_err(
                path,
                &format!(
                    "`struct Package` name '{name}' is invalid — use lowercase letters, digits, '-', \
                     or '_', starting with a letter or digit"
                ),
            ));
        }

        let version_text = fields
            .get("version")
            .cloned()
            .ok_or_else(|| manifest_err(path, "`struct Package` is missing required field 'version'"))?;
        let version = semver::Version::parse(&version_text).map_err(|e| {
            manifest_err(
                path,
                &format!("`struct Package` version '{version_text}' is not valid SemVer: {e}"),
            )
        })?;

        let kind_text = fields
            .get("kind")
            .cloned()
            .ok_or_else(|| manifest_err(path, "`struct Package` is missing required field 'kind'"))?;
        let kind = match kind_text.as_str() {
            "application" => PackageKind::Application,
            "library" => PackageKind::Library,
            "config" => PackageKind::Config,
            other => {
                return Err(manifest_err(
                    path,
                    &format!(
                        "`struct Package` kind '{other}' is invalid — must be 'application', \
                         'library', or 'config'"
                    ),
                ))
            }
        };

        let entry = match fields.get("entry") {
            Some(explicit) => PathBuf::from(explicit),
            None => PathBuf::from(kind.conventional_entry()),
        };

        if dependencies.contains_key(crate::stdlib::STD_PACKAGE_NAME)
            || overrides.contains_key(crate::stdlib::STD_PACKAGE_NAME)
        {
            return Err(manifest_err(
                path,
                "dependency alias 'std' is reserved for Spar's bundled standard library and must not be declared",
            ));
        }

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
                        "`struct Overrides` names '{alias}', which has no matching entry in \
                         `struct Dependencies` to override"
                    ),
                ));
            }
            if !matches!(
                crate::package::source::PackageSource::parse(&overrides[alias]),
                Ok(crate::package::source::PackageSource::LocalPath(_))
            ) {
                return Err(manifest_err(
                    path,
                    &format!("`struct Overrides` entry '{alias}' must use a local 'path:' source"),
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
            native,
            runtime,
        })
    }

    /// Renders this manifest back to canonical Spar source — the CLI's
    /// `init`/`add`/`remove` write manifests this way rather than
    /// text-patching an existing file, so the result is always a valid,
    /// literal-only manifest regardless of how the file looked before.
    /// This does not preserve comments or formatting a human added by
    /// hand; manifests are tool-managed the same way
    /// `spar.package.lock.spar` is.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("struct Package {\n");
        out.push_str(&format!("    name: str = \"{}\";\n", escape(&self.name)));
        out.push_str(&format!("    version: str = \"{}\";\n", self.version));
        out.push_str(&format!("    kind: str = \"{}\";\n", self.kind.as_str()));
        out.push_str(&format!(
            "    entry: str = \"{}\";\n",
            escape(&self.entry.to_string_lossy())
        ));
        out.push_str("};\n");

        if !self.dependencies.is_empty() {
            out.push_str("\nstruct Dependencies {\n");
            for (alias, request) in &self.dependencies {
                out.push_str(&format!("    {alias}: str = \"{}\";\n", escape(request)));
            }
            out.push_str("};\n");
        }

        if !self.overrides.is_empty() {
            out.push_str("\nstruct Overrides {\n");
            for (alias, request) in &self.overrides {
                out.push_str(&format!("    {alias}: str = \"{}\";\n", escape(request)));
            }
            out.push_str("};\n");
        }

        if let Some(native) = &self.native {
            out.push_str("\nstruct Native {\n");
            out.push_str(&format!("    module: str = \"{}\";\n", escape(&native.module)));
            out.push_str(&format!("    abi: str = \"{}\";\n", escape(&native.abi)));
            if !native.capabilities.is_empty() {
                out.push_str(&format!("    capabilities: str = \"{}\";\n", escape(&native.capabilities.join(","))));
            }
            for (key, (path, sha)) in &native.artifacts {
                out.push_str(&format!("    {key}: str = \"{}\";\n", escape(path)));
                if let Some(sha) = sha {
                    out.push_str(&format!("    {key}_sha256: str = \"{sha}\";\n"));
                }
            }
            out.push_str("};\n");
        }

        let runtime = render_runtime(&self.runtime);
        if !runtime.is_empty() {
            out.push_str("\nstruct Runtime {\n");
            out.push_str(&runtime);
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

fn native_spec(mut fields: BTreeMap<String, String>, path: &Path) -> Result<NativeSpec, PackageError> {
    let module = fields
        .remove("module")
        .ok_or_else(|| manifest_err(path, "`struct Native` is missing required field 'module'"))?;
    let abi = fields
        .remove("abi")
        .ok_or_else(|| manifest_err(path, "`struct Native` is missing required field 'abi'"))?;
    if abi != NATIVE_ABI_NAME {
        return Err(manifest_err(path, &format!("`struct Native` abi '{abi}' is not supported (expected '{NATIVE_ABI_NAME}')")));
    }
    let capabilities = fields
        .remove("capabilities")
        .map(|c| c.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    let mut artifacts: BTreeMap<String, (String, Option<String>)> = BTreeMap::new();
    let keys: Vec<String> = fields.keys().cloned().collect();
    for key in keys {
        if key.ends_with("_sha256") {
            continue;
        }
        let target_ok = key.split('_').count() >= 2 && key.split('_').all(|p| !p.is_empty());
        if !target_ok {
            return Err(manifest_err(path, &format!("`struct Native` field '{key}' is not a target key like 'linux_x86_64_gnu'")));
        }
        let artifact = fields.remove(&key).unwrap();
        let sha = fields.remove(&format!("{key}_sha256"));
        if let Some(sha) = &sha {
            if sha.len() != 64 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(manifest_err(path, &format!("`struct Native` field '{key}_sha256' must be 64 hex characters")));
            }
        }
        artifacts.insert(key, (artifact, sha.map(|s| s.to_ascii_lowercase())));
    }
    if let Some(stray) = fields.keys().next() {
        return Err(manifest_err(path, &format!("`struct Native` field '{stray}' has no matching artifact path")));
    }
    Ok(NativeSpec { module, abi, capabilities, artifacts })
}

/// `struct Runtime`: typed literal fields, each key's type fixed by `runtime_config::KEYS`.
fn runtime_settings(structure: &StructDecl, path: &Path) -> Result<RuntimeSettings, PackageError> {
    use crate::ast::{Literal, SparType};
    if !structure.type_parameters.is_empty() {
        return Err(manifest_err(path, "manifest declarations cannot be generic"));
    }
    let mut settings = RuntimeSettings::default();
    let mut seen = std::collections::BTreeSet::new();
    for item in &structure.items {
        let ObjectItem::Field(field) = item else {
            return Err(unsupported(path));
        };
        let Some((_, expected, _, _)) = crate::runtime_config::KEYS
            .iter()
            .find(|(key, ..)| *key == field.name)
        else {
            return Err(manifest_err(
                path,
                &format!("`struct Runtime`: {}", crate::runtime_config::unknown_key(&field.name)),
            ));
        };
        if !seen.insert(field.name.clone()) {
            return Err(manifest_err(path, &format!("duplicate field '{}'", field.name)));
        }
        let declared = match &field.ty {
            Some(SparType::Bool) => "bool",
            Some(SparType::Int) => "int",
            Some(SparType::Str) => "str",
            _ => "",
        };
        if declared != *expected {
            return Err(manifest_err(
                path,
                &format!("`struct Runtime` field '{}' must be typed `{expected}`", field.name),
            ));
        }
        let Some(FieldValue::Expr(expr)) = &field.value else {
            return Err(unsupported(path));
        };
        let value = match expr {
            Expr::Literal(Literal::Bool(b)) => b.to_string(),
            Expr::Literal(Literal::Int(n)) => n.to_string(),
            other => match literal_str(other) {
                Some(text) => text.to_string(),
                None => {
                    return Err(manifest_err(
                        path,
                        &format!("`struct Runtime` field '{}' must be a plain literal", field.name),
                    ))
                }
            },
        };
        settings
            .set(&field.name, &value)
            .map_err(|message| manifest_err(path, &format!("`struct Runtime`: {message}")))?;
    }
    Ok(settings)
}

fn render_runtime(runtime: &RuntimeSettings) -> String {
    let mut out = String::new();
    let mut bool_field = |name: &str, value: Option<bool>| {
        if let Some(v) = value {
            out.push_str(&format!("    {name}: bool = {v};\n"));
        }
    };
    bool_field("vm", runtime.vm);
    bool_field("bytecode", runtime.bytecode);
    bool_field("jit", runtime.jit);
    bool_field("native", runtime.native);
    bool_field("nativeDebug", runtime.native_debug);
    if let Some(n) = runtime.async_workers {
        out.push_str(&format!("    asyncWorkers: int = {n};\n"));
    }
    if let Some(modules) = &runtime.native_modules {
        out.push_str(&format!("    nativeModules: str = \"{}\";\n", escape(modules)));
    }
    out
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
        "manifest may contain only literal Package, Dependencies, and Overrides declarations",
    )
}

fn manifest_err(path: &Path, message: &str) -> PackageError {
    PackageError::Manifest {
        message: format!("{}: {message}", path.display()),
    }
}

fn literal_fields(
    structure: &StructDecl,
    path: &Path,
) -> Result<BTreeMap<String, String>, PackageError> {
    if !structure.type_parameters.is_empty() {
        return Err(manifest_err(path, "manifest declarations cannot be generic"));
    }
    let mut out = BTreeMap::new();
    for item in &structure.items {
        let ObjectItem::Field(field) = item else {
            return Err(unsupported(path));
        };
        if field.ty != Some(crate::ast::SparType::Str) {
            return Err(manifest_err(path, "manifest fields must be explicitly typed as str"));
        }
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
