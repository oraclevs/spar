//! Native extension packages: picks the artifact for the host target from a package's
//! `struct Native`, verifies its SHA-256, and loads it through the native module loader.
//!
//! Trust model: a package that declares `struct Native` runs machine code in the Spar process.
//! Loading is explicit (manifest-named path inside the package root, never a search), hash-checked
//! when the manifest carries a digest, and can be disabled entirely with `SPAR_NO_NATIVE=1`.

use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::native_module::{self, ModuleInfo};
use crate::package::error::PackageError;
use crate::package::lockfile::Lockfile;
use crate::package::manifest::{NativeSpec, PackageManifest};
use crate::package::metadata::PACKAGE_MANIFEST_FILE;
use crate::package::store::PackageStore;
use crate::runtime::NativeRegistry;
use spar_native_sys::*;

/// Target keys for this process, most specific first (`linux_x86_64_gnu`, `linux_x86_64`).
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
    let mut keys = Vec::new();
    if let Some(env) = env {
        keys.push(format!("{os}_{arch}_{env}"));
    }
    keys.push(format!("{os}_{arch}"));
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
            path.display(), info.abi_major, spec.abi
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
        for id in lock.packages.keys() {
            let snapshot = store.snapshot_path(id);
            if snapshot.is_dir() {
                loaded.extend(load_package_native(&snapshot, registry)?);
            }
        }
    }
    Ok(loaded)
}
