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
pub use session::Session;
pub use token::{SpannedToken, Token};
pub use typechecker::TypeChecker;
