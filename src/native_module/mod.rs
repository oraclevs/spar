//! Spar Native ABI host: loads native extension modules (`.so`/`.dylib`/`.dll`) and exposes their
//! functions through the ordinary `NativeRegistry`. See `spar-native-sys/docs/native-api`.

mod buffer;
mod env;
mod host;
mod loader;

pub use buffer::{NativeBuffer, NativeResource};
pub use host::live_persistent_refs;
pub use loader::{
    api_table, load_into_registry, load_module, loaded_modules, shutdown_all, LoadedModule, ModuleInfo, NativeLoadError,
    RUNTIME_CAPABILITIES,
};
