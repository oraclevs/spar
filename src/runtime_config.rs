//! Runtime switches — one place instead of scattered `std::env` reads.
//!
//! Each setting can come from four layers. Highest wins:
//! CLI flag (`--runtime key=value`) > environment variable > manifest
//! (`struct Runtime` in `spar.package.spar`) > built-in default.
//!
//! The CLI resolves the layers once at startup and [`install`]s the result;
//! embedders that never call `install` get environment variables plus defaults,
//! which is exactly the behaviour before this module existed.

use std::sync::OnceLock;

/// One layer of settings. `None` means "this layer has no opinion".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeSettings {
    pub vm: Option<bool>,
    pub bytecode: Option<bool>,
    pub jit: Option<bool>,
    pub native: Option<bool>,
    pub native_debug: Option<bool>,
    pub async_workers: Option<usize>,
    /// `:`-separated shared-library paths (`PATH`-style) loaded as native modules.
    pub native_modules: Option<String>,
}

/// Every key accepted in `struct Runtime` and `--runtime key=value`, with its env var and doc.
pub const KEYS: &[(&str, &str, &str, &str)] = &[
    ("vm", "bool", "SPAR_DISABLE_VM", "Tier-1 register VM for eligible functions. `false` falls back to the tree walker (default true)."),
    ("bytecode", "bool", "SPAR_DISABLE_BYTECODE", "Bytecode tier for eligible functions. `false` falls back to the tree walker (default true)."),
    ("jit", "bool", "SPAR_NO_JIT", "Native JIT for VM functions. `false` keeps the bytecode interpreter (default true)."),
    ("native", "bool", "SPAR_NO_NATIVE", "Allow packages that ship native (C/C++/Rust) extensions (default true)."),
    ("nativeDebug", "bool", "SPAR_NATIVE_DEBUG", "Trace native module calls on stderr (default false)."),
    ("asyncWorkers", "int", "SPAR_ASYNC_WORKERS", "Async worker threads, must be > 0 (default min(cpus * 4, 64))."),
    ("nativeModules", "str", "SPAR_NATIVE_MODULES", "`:`-separated shared libraries to load as native modules (development hook)."),
];

impl RuntimeSettings {
    /// Set one key from text (`--runtime key=value`). Errors name the valid keys/values.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let bool_value = || match value {
            "true" | "on" | "1" => Ok(true),
            "false" | "off" | "0" => Ok(false),
            _ => Err(format!("runtime setting '{key}' expects true/false, got '{value}'")),
        };
        match key {
            "vm" => self.vm = Some(bool_value()?),
            "bytecode" => self.bytecode = Some(bool_value()?),
            "jit" => self.jit = Some(bool_value()?),
            "native" => self.native = Some(bool_value()?),
            "nativeDebug" => self.native_debug = Some(bool_value()?),
            "asyncWorkers" => {
                let n: usize = value.parse().ok().filter(|n| *n > 0).ok_or_else(|| {
                    format!("runtime setting 'asyncWorkers' expects a positive integer, got '{value}'")
                })?;
                self.async_workers = Some(n);
            }
            "nativeModules" => self.native_modules = Some(value.to_owned()),
            _ => return Err(unknown_key(key)),
        }
        Ok(())
    }

    /// `self` wins; anything it leaves unset comes from `lower`.
    pub fn over(self, lower: RuntimeSettings) -> RuntimeSettings {
        RuntimeSettings {
            vm: self.vm.or(lower.vm),
            bytecode: self.bytecode.or(lower.bytecode),
            jit: self.jit.or(lower.jit),
            native: self.native.or(lower.native),
            native_debug: self.native_debug.or(lower.native_debug),
            async_workers: self.async_workers.or(lower.async_workers),
            native_modules: self.native_modules.or(lower.native_modules),
        }
    }

    /// The environment layer, with each variable's historical meaning.
    pub fn from_env() -> RuntimeSettings {
        let has = |name: &str| std::env::var_os(name).is_some();
        RuntimeSettings {
            vm: has("SPAR_DISABLE_VM").then_some(false),
            bytecode: has("SPAR_DISABLE_BYTECODE").then_some(false),
            jit: has("SPAR_NO_JIT").then_some(false),
            native: std::env::var_os("SPAR_NO_NATIVE")
                .filter(|v| v != "0")
                .map(|_| false),
            native_debug: has("SPAR_NATIVE_DEBUG").then_some(true),
            async_workers: std::env::var("SPAR_ASYNC_WORKERS")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|n| *n > 0),
            native_modules: std::env::var_os("SPAR_NATIVE_MODULES")
                .map(|v| v.to_string_lossy().into_owned()),
        }
    }
}

pub fn unknown_key(key: &str) -> String {
    let valid: Vec<&str> = KEYS.iter().map(|k| k.0).collect();
    format!("unknown runtime setting '{key}' — valid settings: {}", valid.join(", "))
}

static INSTALLED: OnceLock<RuntimeSettings> = OnceLock::new();

/// Fix the process-wide settings. First call wins; later calls are ignored.
/// Pass the already-merged result (`cli.over(env.over(manifest))`).
pub fn install(settings: RuntimeSettings) {
    let _ = INSTALLED.set(settings);
}

/// Merge CLI and manifest layers with the environment in between, then install.
pub fn install_layers(cli: RuntimeSettings, manifest: RuntimeSettings) {
    install(cli.over(RuntimeSettings::from_env().over(manifest)));
}

fn get<T>(pick: impl Fn(&RuntimeSettings) -> T) -> T {
    match INSTALLED.get() {
        Some(settings) => pick(settings),
        None => pick(&RuntimeSettings::from_env()),
    }
}

pub fn vm_enabled() -> bool {
    get(|s| s.vm).unwrap_or(true)
}
pub fn bytecode_enabled() -> bool {
    get(|s| s.bytecode).unwrap_or(true)
}
pub fn jit_enabled() -> bool {
    get(|s| s.jit).unwrap_or(true)
}
pub fn native_enabled() -> bool {
    get(|s| s.native).unwrap_or(true)
}
pub fn native_debug() -> bool {
    get(|s| s.native_debug).unwrap_or(false)
}
pub fn async_workers() -> Option<usize> {
    get(|s| s.async_workers)
}
pub fn native_modules() -> Option<String> {
    get(|s| s.native_modules.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_beats_env_beats_manifest() {
        let cli = RuntimeSettings { jit: Some(true), ..Default::default() };
        let env = RuntimeSettings { jit: Some(false), vm: Some(false), ..Default::default() };
        let manifest = RuntimeSettings { jit: Some(false), vm: Some(true), bytecode: Some(false), ..Default::default() };
        let merged = cli.over(env.over(manifest));
        assert_eq!(merged.jit, Some(true));
        assert_eq!(merged.vm, Some(false));
        assert_eq!(merged.bytecode, Some(false));
        assert_eq!(merged.native, None);
    }

    #[test]
    fn set_validates_keys_and_values() {
        let mut s = RuntimeSettings::default();
        s.set("jit", "off").unwrap();
        s.set("asyncWorkers", "3").unwrap();
        assert_eq!((s.jit, s.async_workers), (Some(false), Some(3)));
        assert!(s.set("asyncWorkers", "0").is_err());
        assert!(s.set("jit", "maybe").is_err());
        assert!(s.set("nope", "1").unwrap_err().contains("valid settings: vm,"));
    }
}
