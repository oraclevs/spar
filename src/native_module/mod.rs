//! Spar Native ABI host: loads native extension modules (`.so`/`.dylib`/`.dll`) and exposes their
//! functions through the ordinary `NativeRegistry`. See `spar-native-sys/docs/native-api`.

mod buffer;
mod env;
mod host;
mod loader;

use crate::async_runtime::RuntimeFault;
use crate::error::Span;
use crate::runtime::{RuntimeContext, Value};

/// Interpreter access handed to native functions declared with `SPAR_FN_CALLS`.
pub(crate) trait CallbackHost {
    fn call_callable(&mut self, callable: &Value, args: Vec<Value>, span: &Span) -> Result<Value, RuntimeFault>;
    fn context_ptr(&mut self) -> *mut RuntimeContext;
}

pub use buffer::{NativeBuffer, NativeResource};
pub use host::live_persistent_refs;
pub(crate) use loader::call_external;
pub use loader::{
    api_table, load_into_registry, load_module, loaded_modules, shutdown_all, LoadedModule, ModuleInfo, NativeLoadError,
    RUNTIME_CAPABILITIES,
};

#[cfg(test)]
mod tests;
