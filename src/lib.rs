pub mod ast;
pub(crate) mod async_runtime;
pub mod command;
pub mod compiled;
pub mod compiler;
pub mod de;
pub mod depgraph;
pub mod dotenv;
pub mod emit;
pub mod engine;
pub mod error;
pub mod evaluator;
pub mod formatter;
pub mod host;
pub mod lexer;
pub mod loader;
pub(crate) mod lowerer;
pub mod naming;
pub mod package;
pub mod parser;
pub mod renderer;
pub mod resolver;
pub mod runner;
pub(crate) mod runtime;
pub mod session;
mod shell_lang;
pub(crate) mod stdlib;
pub mod structured_codec;
pub mod structured_input;
pub mod task_lowering;
#[cfg(test)]
pub mod tests;
pub mod token;
pub mod typechecker;

pub use ast::Program;
pub use command::parse_shell_plan;
pub use compiled::CompiledProgram;
pub use compiler::{Compilation, CompileOptions, Compiler};
pub use de::{from_eval, from_str, SparDeserError};
pub use emit::{emit_to_json, emit_to_toml, emit_to_yaml, EmitFormat};
pub use engine::{Engine, ExecutionOutcome};
pub use error::{Span, SparError};
pub use evaluator::{
    execute_shell_plan, execute_shell_plan_with_options, ConfigValue, EvalResult, Evaluator,
    PromiseHandle, ShellPlanOutcome,
};
pub use host::{HostError, HostFunction, HostRegistry};
pub use lexer::Lexer;
pub use parser::Parser;
pub use renderer::ErrorRenderer;
pub use resolver::{Resolver, SymbolTable};
pub use runtime::{
    NativeExecutionKind, NativeFunction, NativeFunctionId, NativeMethod, NativeMethodId,
    NativeMethodSignature, NativeRegistry, ResourceId, RuntimeContext, RuntimeInput, RuntimeOutput,
    Schema, SchemaField, SchemaInferenceError, SchemaType, StreamResource, StreamState, TableValue,
    Value,
};
pub use session::{
    input_completeness, InputCompleteness, InteractiveEvalResult, InteractivePresentation,
    InteractivePreviewResult, InteractiveRuntimeValue, Session,
};
pub use structured_codec::{
    CodecDescriptor, CodecMode, StructuredFormat, StructuredFormatRegistry, StructuredParser,
    StructuredSerializer,
};
pub use structured_input::{
    structured_decoder_descriptors, DecoderCapabilities, DecoderDescriptor, DecoderKind,
    DecoderNamespace, DecoderOptionKind, DecoderOptionSpec, DecoderOutputShape, StreamingMode,
    StructuredInputRegistry,
};
pub use token::{SpannedToken, Token};
pub use typechecker::TypeChecker;

/// Editor/tooling-safe view of the bundled Spar standard library.
///
/// These helpers deliberately expose only stable read-only module identity
/// information. Tooling such as `spar-ls` should not need access to the
/// compiler's private stdlib implementation modules.
pub fn bundled_stdlib_module_names() -> Vec<String> {
    let root = stdlib::bundled_root().join("src");
    let mut modules = std::fs::read_dir(root)
        .into_iter()
        .flat_map(|entries| entries.flatten())
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("spar") {
                return None;
            }
            let stem = path.file_stem()?.to_str()?;
            Some(if stem == "lib" {
                "std".to_string()
            } else {
                format!("std/{stem}")
            })
        })
        .collect::<Vec<_>>();
    modules.sort();
    modules.dedup();
    modules
}

/// Resolve `std` or `std/<module>` to the bundled source used by this Spar
/// build. Returns `None` for non-stdlib requests.
pub fn resolve_bundled_stdlib_import(request: &str) -> Option<std::path::PathBuf> {
    stdlib::resolve_bundled_import(request)
}

/// True when `path` belongs to the bundled standard library shipped with this
/// Spar build.
pub fn is_bundled_stdlib_path(path: &std::path::Path) -> bool {
    stdlib::is_bundled_std_path(path)
}
