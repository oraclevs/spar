//! Native extension packages: picks the artifact for the host target from a package's
//! `struct Native`, verifies its SHA-256, and loads it through the native module loader.
//!
//! Trust model: a package that declares `struct Native` runs machine code in the Spar process.
//! Loading is explicit (manifest-named path inside the package root, never a search), hash-checked
//! when the manifest carries a digest, and can be disabled entirely with `SPAR_NO_NATIVE=1`.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::native_module::{self, ModuleInfo};
use crate::package::error::PackageError;
use crate::package::lockfile::Lockfile;
use crate::package::manifest::{NativeSpec, PackageManifest};
use crate::package::metadata::PACKAGE_MANIFEST_FILE;
use crate::package::store::PackageStore;
use crate::runtime::{NativeFunction, NativeRegistry};
use spar_native_sys::*;

/// Camel-case manifest target keys for this process, most specific first.
pub fn host_target_keys() -> Vec<String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let env = if cfg!(target_env = "musl") {
        Some("musl")
    } else if cfg!(target_env = "gnu") {
        Some("gnu")
    } else if cfg!(target_env = "msvc") {
        Some("msvc")
    } else {
        None
    };
    let arch = match arch {
        "x86_64" => "X8664".to_string(),
        "aarch64" => "Aarch64".to_string(),
        other => crate::naming::to_pascal_case(other),
    };
    let mut keys = Vec::new();
    if let Some(env) = env {
        let env = crate::naming::to_pascal_case(env);
        keys.push(format!("{os}{arch}{env}"));
    }
    keys.push(format!("{os}{arch}"));
    keys
}

fn capability_bit(name: &str) -> Option<u64> {
    Some(match name {
        "strings" => SPAR_CAP_STRINGS,
        "bytes" => SPAR_CAP_BYTES,
        "typed-arrays" => SPAR_CAP_TYPED_ARRAYS,
        "lists" => SPAR_CAP_LISTS,
        "records" => SPAR_CAP_RECORDS,
        "resources" => SPAR_CAP_NATIVE_RESOURCES,
        "callbacks" => SPAR_CAP_CALLBACKS,
        "async" => SPAR_CAP_ASYNC,
        "zero-copy-bytes" => SPAR_CAP_ZERO_COPY_BYTES,
        "direct-calls" => SPAR_CAP_DIRECT_CALLS,
        _ => return None,
    })
}

fn err(message: String) -> PackageError {
    PackageError::Io { message }
}

/// Resolves the artifact path for this host inside `root`, confined to the package directory.
pub fn resolve_artifact(
    spec: &NativeSpec,
    root: &Path,
) -> Result<(PathBuf, Option<String>), PackageError> {
    let keys = host_target_keys();
    let (key, (relative, sha)) =
        keys.iter()
            .find_map(|k| spec.artifacts.get(k).map(|v| (k, v)))
            .ok_or_else(|| {
                err(format!(
                "native package '{}' has no artifact for this target (looked for {}; declared: {})",
                spec.module,
                keys.join(", "),
                spec.artifacts.keys().cloned().collect::<Vec<_>>().join(", ")
            ))
            })?;
    let rel = Path::new(relative);
    if rel.is_absolute()
        || rel.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(err(format!("native artifact '{key}' path '{relative}' must be relative and stay inside the package")));
    }
    let full = root.join(rel);
    let canonical_root = root
        .canonicalize()
        .map_err(|e| err(format!("{}: {e}", root.display())))?;
    let canonical = full
        .canonicalize()
        .map_err(|e| err(format!("native artifact {}: {e}", full.display())))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(err(format!(
            "native artifact {} resolves outside the package",
            canonical.display()
        )));
    }
    Ok((canonical, sha.clone()))
}

fn sha256_hex(path: &Path) -> Result<String, PackageError> {
    let bytes = std::fs::read(path).map_err(|e| err(format!("{}: {e}", path.display())))?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeInterface {
    format_version: u32,
    module: String,
    types: Vec<String>,
    functions: Vec<InterfaceFunction>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InterfaceFunction {
    name: String,
    params: Vec<InterfaceParam>,
    ret: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InterfaceParam {
    name: String,
    ty: String,
}

fn read_interface(root: &Path, spec: &NativeSpec) -> Result<Option<NativeInterface>, PackageError> {
    let Some(relative) = &spec.interface else {
        return Ok(None);
    };
    let rel = Path::new(relative);
    if rel.is_absolute()
        || rel.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(err(format!(
            "native interface path '{relative}' must stay inside the package"
        )));
    }
    let canonical_root = root.canonicalize().map_err(|e| err(e.to_string()))?;
    let path = root
        .join(rel)
        .canonicalize()
        .map_err(|e| err(format!("native interface '{relative}': {e}")))?;
    if !path.starts_with(&canonical_root) {
        return Err(err(format!(
            "native interface '{relative}' resolves outside the package"
        )));
    }
    let size = std::fs::metadata(&path)
        .map_err(|e| err(format!("{}: {e}", path.display())))?
        .len();
    if size > 1_048_576 {
        return Err(err(format!(
            "native interface '{}' exceeds 1 MiB",
            path.display()
        )));
    }
    let source =
        std::fs::read_to_string(&path).map_err(|e| err(format!("{}: {e}", path.display())))?;
    let interface: NativeInterface = serde_json::from_str(&source)
        .map_err(|e| err(format!("native interface '{}': {e}", path.display())))?;
    if interface.format_version != 1 || interface.module != spec.module {
        return Err(err(format!(
            "native interface '{}' must use format version 1 and module '{}'",
            path.display(),
            spec.module
        )));
    }
    Ok(Some(interface))
}

fn register_interface(
    interface: &NativeInterface,
    registry: &mut NativeRegistry,
) -> Result<(), PackageError> {
    for name in &interface.types {
        if !crate::naming::is_pascal_case(name)
            || !name.chars().all(|ch| ch.is_ascii_alphanumeric())
        {
            return Err(err(format!(
                "native interface type '{name}' must use PascalCase"
            )));
        }
        registry.declare_type(name.clone());
    }
    for function in &interface.functions {
        if !crate::naming::is_camel_case(&function.name)
            || !function.name.chars().all(|ch| ch.is_ascii_alphanumeric())
        {
            return Err(err(format!(
                "native interface function '{}' must use camelCase",
                function.name
            )));
        }
        let params = function
            .params
            .iter()
            .map(|param| {
                if !crate::naming::is_camel_case(&param.name)
                    || !param.name.chars().all(|ch| ch.is_ascii_alphanumeric())
                {
                    return Err(err(format!(
                        "native interface parameter '{}' must use camelCase",
                        param.name
                    )));
                }
                let ty = crate::parser::Parser::parse_type_text(&param.ty).map_err(|e| {
                    err(format!("native interface parameter '{}': {e}", param.name))
                })?;
                Ok((param.name.as_str(), ty))
            })
            .collect::<Result<Vec<_>, PackageError>>()?;
        let ret = if function.ret == "void" {
            crate::ast::SparType::Void
        } else {
            crate::parser::Parser::parse_type_text(&function.ret).map_err(|e| {
                err(format!(
                    "native interface function '{}': {e}",
                    function.name
                ))
            })?
        };
        let native = NativeFunction::sync(
            interface.module.clone(),
            function.name.clone(),
            params,
            ret,
            false,
            |_ctx, _args| {
                Err(crate::SparError::EvalError {
                    message: "native interface is for analysis only".into(),
                    span: crate::Span::dummy(),
                })
            },
        );
        registry.register(native).map_err(|e| err(e.to_string()))?;
    }
    Ok(())
}

/// Reads a native package's static interface without loading or executing its binary.
pub fn load_package_native_interface(
    root: &Path,
    registry: &mut NativeRegistry,
) -> Result<(), PackageError> {
    let manifest_path = root.join(PACKAGE_MANIFEST_FILE);
    if !manifest_path.is_file() {
        return Ok(());
    }
    let source = std::fs::read_to_string(&manifest_path).map_err(|e| err(e.to_string()))?;
    // Source-only projects do not need their manifest parsed for native editor
    // metadata. In particular, an incomplete manifest must not obscure the
    // package import diagnostics of an unrelated source file.
    let has_native = crate::Lexer::new(&source)
        .tokenize()
        .map(|tokens| {
            tokens.windows(2).any(|pair| {
                matches!(&pair[0].token, crate::token::Token::KwStruct)
                    && matches!(&pair[1].token, crate::token::Token::Ident(name) if name == "Native")
            })
        })
        .unwrap_or_else(|_| source.contains("Native"));
    if !has_native {
        return Ok(());
    }
    let manifest = PackageManifest::parse(&source, &manifest_path)?;
    if let Some(spec) = &manifest.native {
        if let Some(interface) = read_interface(root, spec)? {
            register_interface(&interface, registry)?;
        }
    }
    Ok(())
}

/// Reads static interfaces for the root package and locked dependencies.
pub fn load_project_native_interfaces(
    project_dir: &Path,
    lockfile: Option<(&Lockfile, &PackageStore)>,
    registry: &mut NativeRegistry,
) -> Result<(), PackageError> {
    load_package_native_interface(project_dir, registry)?;
    if let Some((lock, store)) = lockfile {
        for (id, package) in &lock.packages {
            let root = match &package.source {
                crate::package::lockfile::LockedSource::Github { .. } => store.snapshot_path(id),
                crate::package::lockfile::LockedSource::Path { path } => project_dir.join(path),
            };
            if root.is_dir() {
                load_package_native_interface(&root, registry)?;
            }
        }
    }
    Ok(())
}

/// Loads the native module declared by the package rooted at `root` (a directory containing
/// `spar.package.spar`). Returns `Ok(None)` when the package declares none.
pub fn load_package_native(
    root: &Path,
    registry: &mut NativeRegistry,
) -> Result<Option<ModuleInfo>, PackageError> {
    let manifest_path = root.join(PACKAGE_MANIFEST_FILE);
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let source = std::fs::read_to_string(&manifest_path)
        .map_err(|e| err(format!("{}: {e}", manifest_path.display())))?;
    let manifest = PackageManifest::parse(&source, &manifest_path)?;
    let Some(spec) = manifest.native else {
        return Ok(None);
    };
    if !crate::runtime_config::native_enabled() {
        return Err(err(format!(
            "package '{}' contains native code but native extensions are disabled (SPAR_NO_NATIVE)",
            manifest.name
        )));
    }
    let (path, sha) = resolve_artifact(&spec, root)?;
    if let Some(expected) = sha {
        let actual = sha256_hex(&path)?;
        if actual != expected {
            return Err(err(format!(
                "native artifact {} failed its integrity check (expected sha256 {expected}, found {actual})",
                path.display()
            )));
        }
    }
    let module = native_module::load_module(&path).map_err(|e| err(e.to_string()))?;
    let info = module.info().clone();
    let declared_major = if spec.abi == "spar-native-0" { 0 } else { 1 };
    if info.abi_major != declared_major {
        return Err(err(format!(
            "native artifact {} declares ABI {} but the manifest says '{}'",
            path.display(),
            info.abi_major,
            spec.abi
        )));
    }
    if info.name != spec.module {
        return Err(err(format!(
            "native artifact {} declares module '{}' but the manifest says '{}'",
            path.display(),
            info.name,
            spec.module
        )));
    }
    if !spec.capabilities.is_empty() {
        let mut declared = 0u64;
        for c in &spec.capabilities {
            declared |= capability_bit(c)
                .ok_or_else(|| err(format!("unknown capability '{c}' in `struct Native`")))?;
        }
        let extra = info.required_capabilities & !declared;
        if extra != 0 {
            return Err(err(format!(
                "native module '{}' requires capabilities 0x{extra:x} that its manifest does not declare",
                info.name
            )));
        }
    }
    if let Some(interface) = read_interface(root, &spec)? {
        let mut declared = NativeRegistry::new();
        register_interface(&interface, &mut declared)?;
        let mut actual = NativeRegistry::new();
        module
            .register(&mut actual)
            .map_err(|e| err(e.to_string()))?;
        let declared_types: std::collections::BTreeSet<_> =
            declared.declared_types().iter().cloned().collect();
        let actual_types: std::collections::BTreeSet<_> =
            actual.declared_types().iter().cloned().collect();
        if declared_types != actual_types {
            return Err(err(format!(
                "native interface for '{}' has types that differ from its binary",
                info.name
            )));
        }
        let declared_signatures = declared.signatures();
        let actual_signatures = actual.signatures();
        if declared_signatures.len() != actual_signatures.len()
            || declared_signatures.iter().any(|(key, expected)| {
                actual_signatures.get(key).is_none_or(|found| {
                    found.params != expected.params || found.ret != expected.ret
                })
            })
        {
            return Err(err(format!(
                "native interface for '{}' has functions that differ from its binary",
                info.name
            )));
        }
    }
    module.register(registry).map_err(|e| err(e.to_string()))?;
    Ok(Some(info))
}

/// Loads the root project's native module (if any) and every native dependency in the lockfile.
pub fn load_project_natives(
    project_dir: &Path,
    lockfile: Option<(&Lockfile, &PackageStore)>,
    registry: &mut NativeRegistry,
) -> Result<Vec<ModuleInfo>, PackageError> {
    let mut loaded = Vec::new();
    loaded.extend(load_package_native(project_dir, registry)?);
    if let Some((lock, store)) = lockfile {
        for (id, package) in &lock.packages {
            let root = match &package.source {
                crate::package::lockfile::LockedSource::Github { .. } => store.snapshot_path(id),
                crate::package::lockfile::LockedSource::Path { path } => project_dir.join(path),
            };
            if root.is_dir() {
                loaded.extend(load_package_native(&root, registry)?);
            }
        }
    }
    Ok(loaded)
}
