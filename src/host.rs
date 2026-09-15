//! Foundation for registering native Rust functions under a namespace
//! (`fs::read`, `process::exit`, ...) that Spar source calls with the
//! same `ns::fn(param: value)` syntax as a functionGroup or cross-file
//! function — no new keyword, no parser change.
//!
//! This is deliberately generic: it doesn't know about filesystems or
//! processes. An embedding application registers whatever it wants to
//! expose; a future first-party stdlib package would be built the same
//! way, just shipped by Anthropic instead of the embedder.

use std::collections::HashMap;
use std::sync::Arc;

use crate::ast::SparType;
use crate::evaluator::ConfigValue;

pub type HostCallback = Arc<dyn Fn(&[ConfigValue]) -> Result<ConfigValue, String> + Send + Sync>;

/// One registered native function, reachable from Spar as `namespace::name(...)`.
#[derive(Clone)]
pub struct HostFunction {
    pub namespace: String,
    pub name: String,
    /// Declared in call order — a Spar call site may use named arguments
    /// in any order, but the callback always receives them positionally
    /// in this order.
    pub params: Vec<(String, SparType)>,
    pub ret: SparType,
    callback: HostCallback,
}

impl HostFunction {
    pub fn new(
        namespace: impl Into<String>,
        name: impl Into<String>,
        params: Vec<(&str, SparType)>,
        ret: SparType,
        callback: impl Fn(&[ConfigValue]) -> Result<ConfigValue, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            namespace: namespace.into(),
            name: name.into(),
            params: params
                .into_iter()
                .map(|(n, t)| (n.to_string(), t))
                .collect(),
            ret,
            callback: Arc::new(callback),
        }
    }

    pub(crate) fn call(&self, args: &[ConfigValue]) -> Result<ConfigValue, HostError> {
        (self.callback)(args).map_err(|message| HostError::Callback {
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            message,
        })
    }
}

/// Just the type-checkable shape of a `HostFunction`, with no callback —
/// this is what `SymbolTable` carries, so a type-checker holding only a
/// `&SymbolTable` (never a live `HostRegistry`) can still type a host
/// call's return value and check its parameter names.
#[derive(Clone, Debug)]
pub struct HostSignature {
    pub params: Vec<(String, SparType)>,
    pub ret: SparType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    Duplicate {
        namespace: String,
        name: String,
    },
    Unknown {
        namespace: String,
        name: String,
    },
    Callback {
        namespace: String,
        name: String,
        message: String,
    },
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostError::Duplicate { namespace, name } => {
                write!(
                    f,
                    "host function '{namespace}::{name}' is already registered"
                )
            }
            HostError::Unknown { namespace, name } => {
                write!(f, "no host function registered as '{namespace}::{name}'")
            }
            HostError::Callback {
                namespace,
                name,
                message,
            } => write!(f, "host function '{namespace}::{name}' failed: {message}"),
        }
    }
}

impl std::error::Error for HostError {}

/// A registry of native functions grouped by namespace, cheap to clone
/// (an `Arc`-backed map) so an `Engine`/`Session` can carry one around by
/// value without re-registering anything.
#[derive(Clone, Default)]
pub struct HostRegistry {
    functions: HashMap<(String, String), HostFunction>,
}

impl HostRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, function: HostFunction) -> Result<(), HostError> {
        let key = (function.namespace.clone(), function.name.clone());
        if self.functions.contains_key(&key) {
            return Err(HostError::Duplicate {
                namespace: function.namespace,
                name: function.name,
            });
        }
        self.functions.insert(key, function);
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.functions.is_empty()
    }

    pub fn get(&self, namespace: &str, name: &str) -> Option<&HostFunction> {
        self.functions
            .get(&(namespace.to_string(), name.to_string()))
    }

    pub fn call(
        &self,
        namespace: &str,
        name: &str,
        args: &[ConfigValue],
    ) -> Result<ConfigValue, HostError> {
        let function = self
            .get(namespace, name)
            .ok_or_else(|| HostError::Unknown {
                namespace: namespace.to_string(),
                name: name.to_string(),
            })?;
        function.call(args)
    }

    /// Type-checkable signatures only, for `SymbolTable` — no callbacks.
    pub(crate) fn signatures(&self) -> HashMap<(String, String), HostSignature> {
        self.functions
            .iter()
            .map(|(key, f)| {
                (
                    key.clone(),
                    HostSignature {
                        params: f.params.clone(),
                        ret: f.ret.clone(),
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop(namespace: &str, name: &str) -> HostFunction {
        HostFunction::new(namespace, name, vec![], SparType::Void, |_| {
            Ok(ConfigValue::Int(0))
        })
    }

    #[test]
    fn registering_the_same_namespace_and_name_twice_errors() {
        let mut hosts = HostRegistry::new();
        hosts.register(noop("log", "write")).unwrap();
        let error = hosts.register(noop("log", "write")).unwrap_err();
        assert_eq!(
            error,
            HostError::Duplicate {
                namespace: "log".into(),
                name: "write".into(),
            }
        );
    }

    #[test]
    fn calling_an_unregistered_function_errors() {
        let hosts = HostRegistry::new();
        let error = hosts.call("log", "write", &[]).unwrap_err();
        assert_eq!(
            error,
            HostError::Unknown {
                namespace: "log".into(),
                name: "write".into(),
            }
        );
    }

    #[test]
    fn calling_a_registered_function_runs_its_callback() {
        let mut hosts = HostRegistry::new();
        hosts
            .register(HostFunction::new(
                "math",
                "double",
                vec![("n", SparType::Int)],
                SparType::Int,
                |args| match &args[0] {
                    ConfigValue::Int(n) => Ok(ConfigValue::Int(n * 2)),
                    _ => Err("expected int".into()),
                },
            ))
            .unwrap();
        assert_eq!(
            hosts
                .call("math", "double", &[ConfigValue::Int(21)])
                .unwrap(),
            ConfigValue::Int(42)
        );
    }
}

impl std::fmt::Debug for HostRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut names: Vec<&(String, String)> = self.functions.keys().collect();
        names.sort();
        f.debug_struct("HostRegistry")
            .field(
                "functions",
                &names
                    .iter()
                    .map(|(ns, name)| format!("{ns}::{name}"))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}
