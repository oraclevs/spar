use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::sync::{Arc, Mutex};

pub(crate) mod bytecode;
pub(crate) mod context;
pub(crate) mod native;
pub(crate) mod record;
pub(crate) mod resource;
pub(crate) mod scheduler;
pub(crate) mod schema;
pub(crate) mod stream;
pub(crate) mod table;
pub(crate) mod value;

pub use context::{RuntimeContext, RuntimeInput, RuntimeOutput};
pub use native::{
    NativeExecutionKind, NativeFunction, NativeFunctionId, NativeIntrinsic, NativeMethod,
    NativeMethodId, NativeMethodSignature, NativeRegistry, NativeSignature, ReceiverMode,
};
pub use record::Record;
pub use resource::ResourceId;
pub use schema::{Schema, SchemaField, SchemaInferenceError, SchemaType};
pub use stream::{StreamResource, StreamState};
pub use table::TableValue;
pub use value::{ErrorValue, Shared, Value};

use crate::ast::SparType;
use crate::async_runtime::{RuntimeFault, TaskInvocation, TaskStatus};
use crate::compiled::{
    CompiledExpression, CompiledMethodTarget, CompiledObjectItem, CompiledProgram,
    CompiledShellCommand, CompiledShellExpr, CompiledShellMixedPipeline, CompiledShellRedirect,
    CompiledShellStep, CompiledShellWord, CompiledShellWordPart, CompiledStatement,
    CompiledStringPart, FunctionId, LocalSlot, ModuleId, TypedOperation,
};
use crate::error::{Span, SparError};
use crate::evaluator::ConfigValue;

#[derive(Clone)]
pub(crate) struct Frame {
    slots: Vec<Option<Value>>,
    /// Reified generic bindings for the current function invocation. Spar's
    /// ordinary runtime remains type-erased; only operations that explicitly
    /// require a checked type (typed JSON/HTTP decoding) consult this map.
    type_bindings: Option<Box<HashMap<String, RuntimeTypeBinding>>>,
}

#[derive(Clone, Debug)]
struct RuntimeTypeBinding {
    ty: SparType,
    /// Module where unqualified named types in `ty` were resolved at the call
    /// site. This lets a generic stdlib wrapper decode caller-owned structs.
    module: ModuleId,
}

#[derive(Clone)]
pub struct ClosureValue {
    captured: Frame,
    parameter_slots: Vec<LocalSlot>,
    body: Vec<CompiledStatement>,
    module: crate::compiled::ModuleId,
    span: Span,
}

impl std::fmt::Debug for ClosureValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Closure")
            .field("span", &self.span)
            .finish()
    }
}

impl PartialEq for ClosureValue {
    fn eq(&self, other: &Self) -> bool {
        self.span == other.span
    }
}

#[derive(Clone)]
pub struct ShellProgramValue {
    body: Vec<CompiledStatement>,
    captured: Frame,
    module: crate::compiled::ModuleId,
    span: Span,
}

impl std::fmt::Debug for ShellProgramValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShellProgram")
            .field("span", &self.span)
            .finish()
    }
}

impl PartialEq for ShellProgramValue {
    fn eq(&self, other: &Self) -> bool {
        self.span == other.span
    }
}

#[derive(Clone)]
pub struct MixedShellValue {
    plan: CompiledShellExpr,
    captured: Frame,
    module: crate::compiled::ModuleId,
    span: Span,
}

impl std::fmt::Debug for MixedShellValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MixedShell")
            .field("span", &self.span)
            .finish()
    }
}

impl PartialEq for MixedShellValue {
    fn eq(&self, other: &Self) -> bool {
        self.span == other.span
    }
}

impl Frame {
    pub fn new(slot_count: usize) -> Self {
        Self {
            slots: vec![None; slot_count],
            type_bindings: None,
        }
    }

    pub fn read(&self, slot: LocalSlot, span: &Span) -> Result<&Value, SparError> {
        self.slots
            .get(slot.0 as usize)
            .ok_or_else(|| internal_slot_error(slot, "is invalid", span))?
            .as_ref()
            .ok_or_else(|| internal_slot_error(slot, "is uninitialized", span))
    }

    pub fn write(&mut self, slot: LocalSlot, value: Value, span: &Span) -> Result<(), SparError> {
        let destination = self
            .slots
            .get_mut(slot.0 as usize)
            .ok_or_else(|| internal_slot_error(slot, "is invalid", span))?;
        *destination = Some(value);
        Ok(())
    }

    /// Moves a slot's value out, leaving it temporarily empty. Every caller
    /// of this must write a value back into the same slot before control
    /// returns to anywhere that might read it again (including on an error
    /// path) — see the `MethodCall` fast path in `Runtime::eval_expression`,
    /// the only intended caller.
    pub fn take(&mut self, slot: LocalSlot, span: &Span) -> Result<Value, SparError> {
        let destination = self
            .slots
            .get_mut(slot.0 as usize)
            .ok_or_else(|| internal_slot_error(slot, "is invalid", span))?;
        destination
            .take()
            .ok_or_else(|| internal_slot_error(slot, "is uninitialized", span))
    }
}

fn bind_runtime_type_parameters(
    pattern: &SparType,
    concrete: &SparType,
    origin_module: ModuleId,
    bindings: &mut HashMap<String, RuntimeTypeBinding>,
) {
    match (pattern, concrete) {
        (SparType::TypeParameter(name), concrete) => {
            bindings
                .entry(name.clone())
                .or_insert_with(|| RuntimeTypeBinding {
                    ty: concrete.clone(),
                    module: origin_module,
                });
        }
        (SparType::List(pattern), SparType::List(concrete)) => {
            bind_runtime_type_parameters(pattern, concrete, origin_module, bindings);
        }
        (
            SparType::Applied {
                name: pattern_name,
                arguments: pattern_arguments,
            },
            SparType::Applied {
                name: concrete_name,
                arguments: concrete_arguments,
            },
        ) if pattern_name == concrete_name
            && pattern_arguments.len() == concrete_arguments.len() =>
        {
            for (pattern, concrete) in pattern_arguments.iter().zip(concrete_arguments) {
                bind_runtime_type_parameters(pattern, concrete, origin_module, bindings);
            }
        }
        (
            SparType::Function {
                params: pattern_params,
                return_type: pattern_return,
            },
            SparType::Function {
                params: concrete_params,
                return_type: concrete_return,
            },
        ) if pattern_params.len() == concrete_params.len() => {
            for (pattern, concrete) in pattern_params.iter().zip(concrete_params) {
                bind_runtime_type_parameters(&pattern.ty, &concrete.ty, origin_module, bindings);
            }
            bind_runtime_type_parameters(pattern_return, concrete_return, origin_module, bindings);
        }
        _ => {}
    }
}

fn resolve_runtime_type(
    ty: &SparType,
    frame: &Frame,
    default_module: ModuleId,
) -> RuntimeTypeBinding {
    fn resolve(ty: &SparType, frame: &Frame) -> (SparType, Option<ModuleId>) {
        match ty {
            SparType::TypeParameter(name) => frame
                .type_bindings
                .as_ref()
                .and_then(|bindings| bindings.get(name))
                .map(|binding| (binding.ty.clone(), Some(binding.module)))
                .unwrap_or_else(|| (ty.clone(), None)),
            SparType::List(inner) => {
                let (inner, origin) = resolve(inner, frame);
                (SparType::List(Box::new(inner)), origin)
            }
            SparType::Applied { name, arguments } => {
                let mut origin = None;
                let arguments = arguments
                    .iter()
                    .map(|argument| {
                        let (argument, argument_origin) = resolve(argument, frame);
                        if origin.is_none() {
                            origin = argument_origin;
                        }
                        argument
                    })
                    .collect();
                (
                    SparType::Applied {
                        name: name.clone(),
                        arguments,
                    },
                    origin,
                )
            }
            SparType::Function {
                params,
                return_type,
            } => {
                let mut origin = None;
                let params = params
                    .iter()
                    .map(|parameter| {
                        let (ty, parameter_origin) = resolve(&parameter.ty, frame);
                        if origin.is_none() {
                            origin = parameter_origin;
                        }
                        crate::ast::CallableParamType {
                            name: parameter.name.clone(),
                            ty,
                        }
                    })
                    .collect();
                let (return_type, return_origin) = resolve(return_type, frame);
                if origin.is_none() {
                    origin = return_origin;
                }
                (
                    SparType::Function {
                        params,
                        return_type: Box::new(return_type),
                    },
                    origin,
                )
            }
            other => (other.clone(), None),
        }
    }

    let (ty, origin) = resolve(ty, frame);
    RuntimeTypeBinding {
        ty,
        module: origin.unwrap_or(default_module),
    }
}

fn internal_slot_error(slot: LocalSlot, detail: &str, span: &Span) -> SparError {
    SparError::EvalError {
        message: format!("internal runtime error: local slot {} {detail}", slot.0),
        span: span.clone(),
    }
}

use crate::recursion::MAX_CALL_DEPTH;

#[cfg(test)]
pub(crate) fn execute_self_contained_entry(
    program: &Arc<CompiledProgram>,
) -> Result<Value, Vec<SparError>> {
    let entry = program.entry_main.ok_or_else(|| {
        vec![SparError::ResolveError {
            message: "no 'main' function found".into(),
            hint: None,
            span: Span::dummy(),
        }]
    })?;
    let mut runtime = Runtime::new_entry_runtime(
        Arc::clone(program),
        RuntimeContext::for_base_dir(&program.options.base_dir),
    );
    // Unlike the other 3 entry points, this test-only helper never calls
    // `ensure_module` — it drives `run_entry` directly against whatever
    // self-contained program the caller compiled, with no module state.
    runtime.state = None;
    runtime
        .run_entry(entry)
        .map_err(|fault| vec![fault.into_error()])
}

#[derive(Clone)]
enum DataSequenceShape {
    List,
    Table(Schema),
    Stream(crate::ast::SparType),
}

pub(crate) struct Runtime<'a> {
    program: Arc<CompiledProgram>,
    call_depth: usize,
    vm_state: crate::vm::VmState,
    state: Option<ModuleState>,
    scheduler: Arc<scheduler::Scheduler>,
    is_entry: bool,
    shell_depth: usize,
    shell_outcome: Option<crate::evaluator::ShellPlanOutcome>,
    jobs: Vec<spar_process::Job>,
    last_job: Option<Value>,
    shell_exit: bool,
    shell_cwd: Option<std::path::PathBuf>,
    context: RuntimeContext,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl Drop for Runtime<'_> {
    fn drop(&mut self) {
        // Worker-side `Runtime`s (built fresh per task by the scheduler's
        // `run` callback) must NOT shut the scheduler down when they finish
        // one task — only the entry `Runtime` (the one the top-level
        // `execute_*` free function owns) does, and only once every other
        // pending/in-flight task has had a chance to run to completion.
        if self.is_entry {
            self.scheduler.shutdown();
        }
        self.context.shutdown();
    }
}

impl<'a> Runtime<'a> {
    /// Builds the top-level `Runtime` for one `execute_*` call, plus the
    /// `Scheduler` backing every async task it (transitively) spawns. Each
    /// spawned task runs on a worker thread through a fresh, short-lived
    /// `Runtime` built by the closure below, sharing this one's `program`
    /// (`Arc`-cloned, cheap) and `ModuleState` (also `Arc`-shared — see
    /// `ModuleState::share`) but owning its own call stack and a
    /// `RuntimeContext` snapshot captured at spawn time.
    fn new_entry_runtime(program: Arc<CompiledProgram>, context: RuntimeContext) -> Self {
        let module_state = ModuleState::new(&program);
        let run_program = Arc::clone(&program);
        let run_module_state = module_state.share();
        // The `run` closure needs a handle to the very `Scheduler` it's
        // being installed into (to build each worker's `Runtime.scheduler`
        // field) — filled immediately after `Scheduler::new` returns, well
        // before any task can possibly run. This must be a `Weak`, not an
        // `Arc`: the closure is stored *inside* the `Scheduler` itself (as
        // its `run` field), so an `Arc` here would be a reference cycle —
        // Scheduler -> run closure -> Arc<Scheduler> -> back to Scheduler —
        // that never frees, leaking the whole task table, the compiled
        // program, and the module cache on every execution, including
        // purely synchronous ones. A `Weak` breaks the cycle; it's always
        // upgradable here because nothing can be running a task without
        // the Scheduler itself still being alive somewhere up the stack.
        let scheduler_cell: Arc<std::sync::OnceLock<std::sync::Weak<scheduler::Scheduler>>> =
            Arc::new(std::sync::OnceLock::new());
        let run_scheduler_cell = Arc::clone(&scheduler_cell);
        let run: Arc<dyn Fn(TaskInvocation) -> Result<Value, RuntimeFault> + Send + Sync> =
            Arc::new(move |invocation| {
                let scheduler = run_scheduler_cell
                    .get()
                    .expect("scheduler cell filled before any task can run")
                    .upgrade()
                    .expect("scheduler outlives every task it is currently running");
                let mut worker_runtime = Runtime {
                    program: Arc::clone(&run_program),
                    call_depth: invocation.call_depth,
                    vm_state: Default::default(),
                    state: Some(run_module_state.share()),
                    scheduler,
                    is_entry: false,
                    shell_depth: 0,
                    shell_outcome: None,
                    jobs: Vec::new(),
                    last_job: None,
                    shell_exit: false,
                    shell_cwd: None,
                    context: invocation.context,
                    _marker: std::marker::PhantomData,
                };
                let result =
                    worker_runtime.call_function(invocation.function, invocation.arguments);
                // `exit(code:)` inside this task's own call chain set
                // `requested_exit` on this task's *isolated* context
                // (`spawn_child` gives every task its own copy) — nothing
                // about that is visible to whichever `Runtime` awaits this
                // task's promise unless it's carried back explicitly.
                // `RuntimeFault::Exit` is that side channel: it overrides
                // whatever `call_function` itself returned (matching the
                // original single-threaded semantics, where a real exit
                // request always wins over a computed return value — see
                // `call_function_with_context`'s own
                // `requested_exit().unwrap_or(exit_code)`), and
                // `Runtime::apply_exit_and_unwrap` on the awaiting side
                // reapplies it to that side's own context.
                match worker_runtime.context.requested_exit() {
                    Some(code) => Err(RuntimeFault::Exit(code)),
                    None => result,
                }
            });
        let pool_scheduler =
            scheduler::Scheduler::new(scheduler::Scheduler::default_pool_size(), run);
        let _ = scheduler_cell.set(Arc::downgrade(&pool_scheduler));
        Runtime {
            program,
            call_depth: 0,
            vm_state: Default::default(),
            state: Some(module_state),
            scheduler: pool_scheduler,
            is_entry: true,
            shell_depth: 0,
            shell_outcome: None,
            jobs: Vec::new(),
            last_job: None,
            shell_exit: false,
            shell_cwd: None,
            context,
            _marker: std::marker::PhantomData,
        }
    }
}

struct ModuleState {
    results: std::sync::Arc<
        std::sync::Mutex<HashMap<crate::compiled::ModuleId, crate::evaluator::EvalResult>>,
    >,
    hosts: crate::HostRegistry,
    natives: std::sync::Arc<NativeRegistry>,
    effect_ledger: Option<crate::session::EffectLedger>,
}

impl ModuleState {
    fn new(program: &CompiledProgram) -> Self {
        Self {
            results: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            hosts: program.options.hosts.clone(),
            natives: std::sync::Arc::new(program.options.natives.clone()),
            effect_ledger: program.options.effect_ledger.clone(),
        }
    }

    /// Used by Task 5 so every per-task `Runtime` sharing one top-level
    /// execution reads/writes the same results cache instead of each
    /// getting its own empty one.
    fn share(&self) -> Self {
        Self {
            results: self.results.clone(),
            hosts: self.hosts.clone(),
            natives: self.natives.clone(),
            effect_ledger: self.effect_ledger.clone(),
        }
    }
}

enum MixedDecoderState {
    Codec(crate::structured_codec::StructuredParser),
    ScocStreaming {
        parser: Box<dyn scoc::ScocStreamParser>,
        span: Span,
    },
    ScocBuffered {
        parser_name: String,
        options: scoc::ParseOptions,
        output_shape: scoc::OutputShape,
        bytes: Vec<u8>,
        span: Span,
    },
}

impl MixedDecoderState {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, SparError> {
        match self {
            Self::Codec(parser) => parser.push(bytes),
            Self::ScocStreaming { parser, span } => parser
                .push(bytes)
                .map_err(|error| crate::structured_input::scoc_error(error, span))
                .and_then(crate::structured_input::scoc_stream_to_values),
            Self::ScocBuffered { bytes: buffer, .. } => {
                buffer.extend_from_slice(bytes);
                Ok(Vec::new())
            }
        }
    }

    fn finish(self) -> Result<Vec<Value>, SparError> {
        match self {
            Self::Codec(parser) => parser.finish(),
            Self::ScocStreaming { parser, span } => parser
                .finish()
                .map_err(|error| crate::structured_input::scoc_error(error, &span))
                .and_then(crate::structured_input::scoc_stream_to_values),
            Self::ScocBuffered {
                parser_name,
                options,
                output_shape,
                bytes,
                span,
            } => {
                let value = scoc::parse(&parser_name, &bytes, &options)
                    .map_err(|error| crate::structured_input::scoc_error(error, &span))?;
                crate::structured_input::scoc_batch_to_values(value, output_shape, &span)
            }
        }
    }
}

struct MixedInputState {
    stream: Option<spar_process::ProcessStream>,
    decoder: Option<MixedDecoderState>,
    pending: VecDeque<Value>,
    stderr: Vec<u8>,
    status: Option<spar_process::PipelineStatus>,
    finished: bool,
    cancelled: bool,
}

fn mixed_input_lock_error() -> SparError {
    SparError::EvalError {
        message: "mixed pipeline process state lock is poisoned".into(),
        span: Span::dummy(),
    }
}

fn mixed_input_next(shared: &Arc<Mutex<MixedInputState>>) -> Result<Option<Value>, SparError> {
    let mut state = shared.lock().map_err(|_| mixed_input_lock_error())?;
    if let Some(value) = state.pending.pop_front() {
        return Ok(Some(value));
    }
    if state.finished {
        return Ok(None);
    }

    loop {
        let chunk = state
            .stream
            .as_ref()
            .ok_or_else(|| SparError::EvalError {
                message: "mixed pipeline process stream is unavailable".into(),
                span: Span::dummy(),
            })?
            .next_chunk()
            .map_err(|error| SparError::EvalError {
                message: format!("could not read mixed pipeline process output: {error}"),
                span: Span::dummy(),
            })?;

        match chunk {
            Some(spar_process::ProcessOutputChunk::Stdout(bytes)) => {
                let values = state
                    .decoder
                    .as_mut()
                    .ok_or_else(|| SparError::EvalError {
                        message: "mixed pipeline decoder is unavailable".into(),
                        span: Span::dummy(),
                    })?
                    .push(&bytes)?;
                state.pending.extend(values);
                if let Some(value) = state.pending.pop_front() {
                    return Ok(Some(value));
                }
            }
            Some(spar_process::ProcessOutputChunk::Stderr(bytes)) => {
                state.stderr.extend_from_slice(&bytes);
            }
            None => {
                let mut process = state.stream.take().ok_or_else(|| SparError::EvalError {
                    message: "mixed pipeline process stream is unavailable".into(),
                    span: Span::dummy(),
                })?;
                let status = process.wait().map_err(|error| SparError::EvalError {
                    message: format!("could not wait for mixed pipeline process: {error}"),
                    span: Span::dummy(),
                })?;
                state.status = Some(status);
                let decoder = state.decoder.take().ok_or_else(|| SparError::EvalError {
                    message: "mixed pipeline decoder is unavailable".into(),
                    span: Span::dummy(),
                })?;
                state.pending.extend(decoder.finish()?);
                state.finished = true;
                return Ok(state.pending.pop_front());
            }
        }
    }
}

fn mixed_input_cancel(shared: &Arc<Mutex<MixedInputState>>) {
    let Ok(mut state) = shared.lock() else {
        return;
    };
    if state.finished {
        return;
    }
    state.cancelled = true;
    if let Some(mut stream) = state.stream.take() {
        let _ = stream.cancel();
        state.status = stream.wait().ok();
        // The process group is gone, so the readers hit EOF. Keep whatever
        // stderr was already in flight; only stdout is discarded.
        while let Ok(Some(chunk)) = stream.next_chunk() {
            if let spar_process::ProcessOutputChunk::Stderr(bytes) = chunk {
                state.stderr.extend_from_slice(&bytes);
            }
        }
    }
    state.decoder.take();
    state.pending.clear();
    state.finished = true;
}

fn decoder_shape_element_type(
    shape: crate::structured_input::DecoderOutputShape,
) -> crate::ast::SparType {
    match shape {
        crate::structured_input::DecoderOutputShape::Scalar => crate::ast::SparType::Str,
        crate::structured_input::DecoderOutputShape::List => {
            crate::ast::SparType::List(Box::new(crate::ast::SparType::Named("Record".into())))
        }
        crate::structured_input::DecoderOutputShape::Record
        | crate::structured_input::DecoderOutputShape::Table => {
            crate::ast::SparType::Named("Record".into())
        }
    }
}

fn mixed_decoder_element_type(
    descriptor: &crate::structured_input::DecoderDescriptor,
    raw: bool,
    streaming: bool,
) -> crate::ast::SparType {
    let shape = if streaming {
        descriptor
            .stream_item
            .unwrap_or(descriptor.output_shape(raw))
    } else {
        match descriptor.output_shape(raw) {
            crate::structured_input::DecoderOutputShape::Table => {
                crate::structured_input::DecoderOutputShape::Record
            }
            shape => shape,
        }
    };
    decoder_shape_element_type(shape)
}

fn runtime_value_to_scoc_option(value: Value, span: &Span) -> Result<scoc::OptionValue, SparError> {
    match value {
        Value::Bool(value) => Ok(scoc::OptionValue::Bool(value)),
        Value::Int(value) => Ok(scoc::OptionValue::Integer(value)),
        Value::Float(value) => Ok(scoc::OptionValue::Float(value)),
        Value::String(value) => Ok(scoc::OptionValue::String(value)),
        other => Err(SparError::EvalError {
            message: format!(
                "SCOC decoder options must evaluate to bool, int, float, or str; got {}",
                other.type_name()
            ),
            span: span.clone(),
        }),
    }
}

#[allow(dead_code)] // context-free entry point for embedders
pub(crate) fn execute_program(program: &Arc<CompiledProgram>) -> Result<Value, Vec<SparError>> {
    execute_program_with_context(
        program,
        RuntimeContext::for_base_dir(&program.options.base_dir),
    )
}

pub(crate) fn execute_program_with_context(
    program: &Arc<CompiledProgram>,
    context: RuntimeContext,
) -> Result<Value, Vec<SparError>> {
    let entry = program.entry_main.ok_or_else(|| {
        vec![SparError::ResolveError {
            message: "no 'main' function found — Execute mode requires a zero-argument 'main' returning 'int', 'void', or 'shell'".into(),
            hint: None,
            span: Span::dummy(),
        }]
    })?;
    let mut runtime = Runtime::new_entry_runtime(Arc::clone(program), context);
    let value = runtime
        .ensure_module(program.entry)
        .and_then(|()| runtime.run_entry(entry))
        .map_err(|fault| vec![fault.into_error()])?;
    match value {
        Value::MixedShell(shell) => runtime
            .execute_mixed_shell(&shell)
            .map(|outcome| Value::Int(i64::from(outcome.exit_code)))
            .map_err(|fault| vec![fault.into_error()]),
        Value::ShellProgram(program) => runtime
            .execute_shell_program(&program)
            .map(|outcome| Value::Int(i64::from(outcome.exit_code)))
            .map_err(|fault| vec![fault.into_error()]),
        Value::Shell(plan) => {
            let span = runtime
                .entry_function_span(entry)
                .unwrap_or_else(Span::dummy);
            runtime
                .execute_native_shell_plan(&plan, &span)
                .map(|outcome| Value::Int(i64::from(outcome.exit_code)))
                .map_err(|fault| vec![fault.into_error()])
        }
        other => Ok(other),
    }
}

/// Calls `function` (from `program`) with `arguments`, in `context`. If it
/// returns a shell-typed value (`Value::Shell`/`MixedShell`/`ShellProgram`),
/// executes it live — the same per-step engine `execute_program_with_context`
/// uses for `-> shell main()` — so `$!`, `$?`, and `lastJob` observe real
/// job state, and calls to other `-> shell` functions from inside that body
/// share it (same `Runtime` instance, `last_job` is instance-scoped, not
/// call-frame-scoped). Returns the resulting exit status; an explicit
/// `exit(code: N)` inside the block wins over the executed plan's own exit
/// code, matching `-> shell main()`'s behavior.
pub(crate) fn call_function_with_context(
    program: &Arc<CompiledProgram>,
    function: FunctionId,
    arguments: Vec<Value>,
    context: RuntimeContext,
) -> Result<i32, Vec<SparError>> {
    let entry_module = program.entry;
    let mut runtime = Runtime::new_entry_runtime(Arc::clone(program), context);
    let called: Result<Value, RuntimeFault> = (|| {
        runtime.ensure_module(entry_module)?;
        let result = if runtime.function_is_async(function)? {
            let spawn_context = runtime.context.spawn_child();
            let spawn_depth = runtime.call_depth;
            let handle = runtime
                .scheduler
                .spawn(function, arguments, spawn_context, spawn_depth);
            runtime.drive_promise(handle, &Span::dummy())
        } else {
            runtime.call_function(function, arguments)
        };
        if result.is_ok() {
            if let Some(fatal) = runtime.scheduler.settle_and_take_fatal() {
                return Err(fatal);
            }
        }
        result
    })();
    let value = called.map_err(|fault| vec![fault.into_error()])?;
    let exit_code = match value {
        Value::MixedShell(shell) => runtime
            .execute_mixed_shell(&shell)
            .map(|outcome| outcome.exit_code)
            .map_err(|fault| vec![fault.into_error()]),
        Value::ShellProgram(shell_program) => runtime
            .execute_shell_program(&shell_program)
            .map(|outcome| outcome.exit_code)
            .map_err(|fault| vec![fault.into_error()]),
        Value::Shell(plan) => {
            let span = runtime
                .entry_function_span(function)
                .unwrap_or_else(Span::dummy);
            runtime
                .execute_native_shell_plan(&plan, &span)
                .map(|outcome| outcome.exit_code)
                .map_err(|fault| vec![fault.into_error()])
        }
        Value::Int(code) => Ok(code as i32),
        _ => Ok(0),
    }?;
    Ok(runtime.context.requested_exit().unwrap_or(exit_code))
}

pub(crate) enum InteractiveRuntimeExecution {
    Value(crate::session::InteractiveRuntimeValue),
    Process(crate::evaluator::ShellPlanOutcome),
}

pub(crate) fn execute_interactive_preview_with_context(
    program: &Arc<CompiledProgram>,
    function_name: &str,
    context: RuntimeContext,
    preview_limit: usize,
    await_result: bool,
) -> Result<(InteractiveRuntimeExecution, crate::evaluator::EvalResult), Vec<SparError>> {
    let mut runtime = Runtime::new_entry_runtime(Arc::clone(program), context);
    runtime
        .ensure_module(program.entry)
        .map_err(|fault| vec![fault.into_error()])?;
    let function = program
        .modules
        .get(program.entry.0 as usize)
        .and_then(|module| {
            module
                .functions
                .iter()
                .find(|function| function.key.group.is_none() && function.name == function_name)
        })
        .map(|function| function.id)
        .ok_or_else(|| {
            vec![SparError::EvalError {
                message: format!("interactive preview function '{function_name}' is unavailable"),
                span: Span::dummy(),
            }]
        })?;
    let mut value = runtime
        .call_function(function, Vec::new())
        .map_err(|fault| vec![fault.into_error()])?;
    if await_result {
        // The wrapper was async: wait for its promise to resolve.
        if let Value::Promise(handle) = value {
            value = runtime
                .drive_promise(handle, &Span::dummy())
                .map_err(|fault| vec![fault.into_error()])?;
        }
    }
    if let Some(fatal) = runtime.scheduler.settle_and_take_fatal() {
        return Err(vec![fatal.into_error()]);
    }
    let execution = match value {
        Value::MixedShell(shell) => {
            // A lone mixed pipeline typed at a terminal returns its structured
            // result for Sparsh to render; anything longer keeps writing bytes.
            let single = matches!(
                shell.plan.steps.as_slice(),
                [(_, CompiledShellStep::MixedPipeline(_))]
            );
            let capture = single && runtime.context.structured_terminal();
            runtime.context.set_capture_mixed(capture);
            let outcome = runtime
                .execute_mixed_shell(&shell)
                .map_err(|fault| vec![fault.into_error()])?;
            runtime.context.set_capture_mixed(false);
            match runtime.context.take_mixed_capture() {
                Some(capture) if outcome.success => {
                    InteractiveRuntimeExecution::Value(crate::session::InteractiveRuntimeValue {
                        value: capture.value,
                        stream_preview: false,
                        truncated: false,
                        presentation: capture.format.map_or(
                            crate::session::InteractivePresentation::Pipeline,
                            crate::session::InteractivePresentation::Encoded,
                        ),
                    })
                }
                _ => InteractiveRuntimeExecution::Process(outcome),
            }
        }
        Value::ShellProgram(program) => InteractiveRuntimeExecution::Process(
            runtime
                .execute_shell_program(&program)
                .map_err(|fault| vec![fault.into_error()])?,
        ),
        value => InteractiveRuntimeExecution::Value(
            runtime
                .materialize_interactive_preview(value, preview_limit)
                .map_err(|fault| vec![fault.into_error()])?,
        ),
    };
    let result = runtime
        .state
        .as_mut()
        .and_then(|state| state.results.lock().unwrap().remove(&program.entry))
        .ok_or_else(|| {
            vec![SparError::EvalError {
                message: "interactive runtime state is unavailable".into(),
                span: Span::dummy(),
            }]
        })?;
    Ok((execution, result))
}

enum RuntimeFlow {
    Normal,
    Break,
    Continue,
    Return(Value),
}

#[cfg(not(target_arch = "wasm32"))]
impl crate::native_module::CallbackHost for Runtime<'_> {
    fn call_callable(
        &mut self,
        callable: &Value,
        args: Vec<Value>,
        span: &Span,
    ) -> Result<Value, RuntimeFault> {
        self.invoke_data_callable(callable, args, span)
    }
    fn context_ptr(&mut self) -> *mut RuntimeContext {
        &mut self.context as *mut RuntimeContext
    }
    fn scheduler(&self) -> Arc<scheduler::Scheduler> {
        self.scheduler.clone()
    }
}

impl Runtime<'_> {
    fn run_entry(&mut self, entry: FunctionId) -> Result<Value, RuntimeFault> {
        let result = if self.function_is_async(entry)? {
            let spawn_context = self.context.spawn_child();
            let spawn_depth = self.call_depth;
            let handle = self
                .scheduler
                .spawn(entry, Vec::new(), spawn_context, spawn_depth);
            self.drive_promise(handle, &Span::dummy())
        } else {
            self.call_function(entry, Vec::new())
        };
        // A spawned-but-never-awaited task's panic must still abort the
        // program (see Scheduler::settle_and_take_fatal) — only check once
        // the entry call itself succeeded; an already-failing entry call
        // doesn't need a second error layered on top.
        if result.is_ok() {
            if let Some(fatal) = self.scheduler.settle_and_take_fatal() {
                return Err(fatal);
            }
        }
        result
    }

    fn function_is_async(&self, id: FunctionId) -> Result<bool, RuntimeFault> {
        self.program
            .function(id)
            .map(|function| function.is_async)
            .ok_or_else(|| {
                RuntimeFault::Fatal(Box::new(runtime_error(
                    &format!("unknown function ID {}", id.0),
                    &Span::dummy(),
                )))
            })
    }

    fn entry_function_span(&self, id: FunctionId) -> Option<Span> {
        self.program
            .function(id)
            .map(|function| function.span.clone())
    }

    fn drive_promise(
        &mut self,
        handle: crate::PromiseHandle,
        span: &Span,
    ) -> Result<Value, RuntimeFault> {
        let result = self.scheduler.await_handle(handle, span);
        self.apply_exit_and_unwrap(result)
    }

    /// Translates a completed task's `RuntimeFault::Exit(code)` (see
    /// `new_entry_runtime`'s `run` closure) into this `Runtime`'s own
    /// `requested_exit`, and the value the rest of the language already
    /// expects to see: exactly what an ordinary `exit(code:)` call produces
    /// when it's *this* context that made it. Every place that consumes a
    /// task's raw `Ready` result (`drive_promise`, `race`, `timeout`) must
    /// route through this — otherwise `exit()` inside an awaited task is
    /// silently lost the moment it crosses a task boundary.
    fn apply_exit_and_unwrap(
        &mut self,
        result: Result<Value, RuntimeFault>,
    ) -> Result<Value, RuntimeFault> {
        match result {
            Err(RuntimeFault::Exit(code)) => {
                self.context.request_exit(code);
                Ok(Value::Int(i64::from(code)))
            }
            other => other,
        }
    }

    fn call_closure(
        &mut self,
        closure: &ClosureValue,
        arguments: Vec<Value>,
        call_span: &Span,
    ) -> Result<Value, RuntimeFault> {
        crate::recursion::with_stack(|| self.call_closure_inner(closure, arguments, call_span))
    }

    #[inline(never)]
    fn call_closure_inner(
        &mut self,
        closure: &ClosureValue,
        arguments: Vec<Value>,
        call_span: &Span,
    ) -> Result<Value, RuntimeFault> {
        if self.call_depth >= MAX_CALL_DEPTH {
            return Err(runtime_error(
                &format!("maximum function call depth ({MAX_CALL_DEPTH}) exceeded"),
                call_span,
            )
            .into());
        }
        if arguments.len() != closure.parameter_slots.len() {
            return Err(runtime_error(
                &format!(
                    "closure expects {} argument{}, found {}",
                    closure.parameter_slots.len(),
                    if closure.parameter_slots.len() == 1 {
                        ""
                    } else {
                        "s"
                    },
                    arguments.len()
                ),
                call_span,
            )
            .into());
        }
        self.call_depth += 1;
        let result = (|| {
            let mut frame = closure.captured.clone();
            for (slot, value) in closure.parameter_slots.iter().copied().zip(arguments) {
                frame.write(slot, value, &closure.span)?;
            }
            match self.execute_statements(&closure.body, &mut frame, closure.module)? {
                RuntimeFlow::Return(value) => Ok(value),
                RuntimeFlow::Normal => Ok(Value::Void),
                RuntimeFlow::Break | RuntimeFlow::Continue => {
                    Err(runtime_error("loop control escaped a closure", &closure.span).into())
                }
            }
        })();
        self.call_depth -= 1;
        result
    }

    fn call_function(
        &mut self,
        id: FunctionId,
        arguments: Vec<Value>,
    ) -> Result<Value, RuntimeFault> {
        self.call_function_typed(id, arguments, None)
    }

    fn call_function_typed(
        &mut self,
        id: FunctionId,
        arguments: Vec<Value>,
        return_type: Option<RuntimeTypeBinding>,
    ) -> Result<Value, RuntimeFault> {
        self.call_function_with_frame_typed(id, arguments, return_type)
            .map(|(value, _, _)| value)
    }

    fn call_function_with_frame(
        &mut self,
        id: FunctionId,
        arguments: Vec<Value>,
    ) -> Result<(Value, Frame, Option<LocalSlot>), RuntimeFault> {
        self.call_function_with_frame_typed(id, arguments, None)
    }

    fn call_function_with_frame_typed(
        &mut self,
        id: FunctionId,
        arguments: Vec<Value>,
        return_type: Option<RuntimeTypeBinding>,
    ) -> Result<(Value, Frame, Option<LocalSlot>), RuntimeFault> {
        crate::recursion::with_stack(|| {
            self.call_function_with_frame_typed_inner(id, arguments, return_type)
        })
    }

    #[inline(never)]
    fn call_function_with_frame_typed_inner(
        &mut self,
        id: FunctionId,
        arguments: Vec<Value>,
        return_type: Option<RuntimeTypeBinding>,
    ) -> Result<(Value, Frame, Option<LocalSlot>), RuntimeFault> {
        if self.call_depth >= MAX_CALL_DEPTH {
            return Err(runtime_error(
                &format!("maximum function call depth ({MAX_CALL_DEPTH}) exceeded"),
                &Span::dummy(),
            )
            .into());
        }
        let program = Arc::clone(&self.program);
        let function = program.function(id).ok_or_else(|| {
            runtime_error(&format!("unknown function ID {}", id.0), &Span::dummy())
        })?;
        let parameter_slots = &function.parameter_slots;
        let module = function.key.module;
        let default_values = &function.default_values;
        let slot_count = function.slot_count;
        let body = &function.body;
        let declared_return_type = &function.return_type;
        let function_span = &function.span;
        if arguments.len() > parameter_slots.len() {
            return Err(runtime_error("too many direct-call arguments", function_span).into());
        }
        self.call_depth += 1;
        let result: Result<(Value, Frame, Option<LocalSlot>), RuntimeFault> = (|| {
            let mut frame = Frame::new(slot_count);
            if let Some(actual_return) = return_type.as_ref() {
                bind_runtime_type_parameters(
                    declared_return_type,
                    &actual_return.ty,
                    actual_return.module,
                    &mut **frame.type_bindings.get_or_insert_with(Default::default),
                );
            }
            let mut supplied = arguments.into_iter();
            for (index, slot) in parameter_slots.iter().copied().enumerate() {
                match supplied.next() {
                    Some(Value::Void) => {
                        let default = default_values
                            .get(index)
                            .and_then(Option::as_ref)
                            .ok_or_else(|| {
                                runtime_error(
                                    "missing required direct-call argument",
                                    function_span,
                                )
                            })?;
                        let value = self.eval_expression(default, &mut frame, module)?;
                        frame.write(slot, value, function_span)?;
                    }
                    Some(value) => {
                        frame.write(slot, value, function_span)?;
                    }
                    None => {
                        let default = default_values
                            .get(index)
                            .and_then(Option::as_ref)
                            .ok_or_else(|| {
                                runtime_error(
                                    "missing required direct-call argument",
                                    function_span,
                                )
                            })?;
                        let value = self.eval_expression(default, &mut frame, module)?;
                        frame.write(slot, value, function_span)?;
                    }
                }
            }
            let value = match self.run_body(&program, id, body, &mut frame, module)? {
                RuntimeFlow::Return(value) => value,
                RuntimeFlow::Normal => Value::Void,
                RuntimeFlow::Break | RuntimeFlow::Continue => {
                    return Err(runtime_error(
                        "loop control escaped a compiled function",
                        function_span,
                    )
                    .into())
                }
            };
            Ok((value, frame, parameter_slots.first().copied()))
        })();
        self.call_depth -= 1;
        result
    }

    /// Runs a bytecode-lowered function. Arguments are evaluated in the
    /// caller's frame, converted to register bits, and the result converted
    /// back according to the function's declared primitive return type.
    fn call_vm(
        &mut self,
        program: &CompiledProgram,
        id: FunctionId,
        vm_function: &crate::vm::VmFunction,
        arguments: &[CompiledExpression],
        caller: &mut Frame,
        caller_module: ModuleId,
    ) -> Result<Value, RuntimeFault> {
        use crate::vm::Prim;
        let mut bits = [0u64; 8];
        for (index, (argument, prim)) in arguments.iter().zip(&vm_function.params).enumerate() {
            let value = self.eval_expression(argument, caller, caller_module)?;
            bits[index] = match (prim, value) {
                (Prim::Int, Value::Int(v)) => v as u64,
                (Prim::Float, Value::Float(v)) => v.to_bits(),
                (Prim::Bool, Value::Bool(v)) => v as u64,
                _ => {
                    return Err(runtime_error(
                        "bytecode call received an argument of the wrong primitive type",
                        &Span::dummy(),
                    )
                    .into())
                }
            };
        }
        let result = program.vm.run(
            &mut self.vm_state,
            id,
            &bits[..arguments.len()],
            self.call_depth,
        )?;
        Ok(match vm_function.ret {
            Prim::Int => Value::Int(result as i64),
            Prim::Float => Value::Float(f64::from_bits(result)),
            Prim::Bool => Value::Bool(result != 0),
            Prim::Void => Value::Void,
        })
    }

    /// Calls a statically known, non-async function, evaluating the call's
    /// argument expressions straight into the callee frame's parameter slots.
    /// Equivalent to `call_function_typed` on pre-evaluated arguments, minus
    /// the intermediate `Vec<Value>`.
    fn call_direct_inline(
        &mut self,
        id: FunctionId,
        arguments: &[CompiledExpression],
        caller: &mut Frame,
        caller_module: ModuleId,
        return_type: Option<&SparType>,
    ) -> Result<Value, RuntimeFault> {
        crate::recursion::with_stack(|| {
            self.call_direct_inline_inner(id, arguments, caller, caller_module, return_type)
        })
    }

    #[inline(never)]
    fn call_direct_inline_inner(
        &mut self,
        id: FunctionId,
        arguments: &[CompiledExpression],
        caller: &mut Frame,
        caller_module: ModuleId,
        return_type: Option<&SparType>,
    ) -> Result<Value, RuntimeFault> {
        if self.call_depth >= MAX_CALL_DEPTH {
            return Err(runtime_error(
                &format!("maximum function call depth ({MAX_CALL_DEPTH}) exceeded"),
                &Span::dummy(),
            )
            .into());
        }
        // SAFETY: `self.program` is an `Arc` that is never reassigned or dropped
        // while this `Runtime` is alive, and the reference below is only used
        // inside this call, so the pointee outlives it. Detaching the borrow
        // from `&self` lets `self` be borrowed mutably for the body without an
        // atomic refcount increment/decrement on every call.
        let program: &CompiledProgram = unsafe { &*Arc::as_ptr(&self.program) };
        let function = program.function(id).ok_or_else(|| {
            runtime_error(&format!("unknown function ID {}", id.0), &Span::dummy())
        })?;
        let parameter_slots = &function.parameter_slots;
        let function_span = &function.span;
        if arguments.len() > parameter_slots.len() {
            return Err(runtime_error("too many direct-call arguments", function_span).into());
        }
        if let Some(vm_function) = program.vm.get(id) {
            if arguments.len() == vm_function.params.len()
                && arguments.len() <= 8
                && !arguments
                    .iter()
                    .any(|a| matches!(a, CompiledExpression::DefaultArgument(_)))
            {
                return self.call_vm(program, id, vm_function, arguments, caller, caller_module);
            }
        }
        let module = function.key.module;
        let mut frame = Frame::new(function.slot_count);
        // Generic reification only matters when the callee's declared return
        // type mentions a type parameter; skip the per-call type clone otherwise.
        if let Some(site_type) = return_type {
            if crate::typechecker::mentions_type_parameter(&function.return_type) {
                let actual_return = resolve_runtime_type(site_type, caller, caller_module);
                bind_runtime_type_parameters(
                    &function.return_type,
                    &actual_return.ty,
                    actual_return.module,
                    &mut **frame.type_bindings.get_or_insert_with(Default::default),
                );
            }
        }
        // Pass 1: supplied arguments, evaluated in the caller's frame. A
        // `Void` placeholder (skipped named parameter) falls through to the
        // default in pass 2, exactly as with a pre-evaluated argument list.
        for (argument, slot) in arguments.iter().zip(parameter_slots.iter().copied()) {
            match self.eval_expression(argument, caller, caller_module)? {
                Value::Void => {}
                value => frame.write(slot, value, function_span)?,
            }
        }
        self.call_depth += 1;
        let result = (|| {
            // Pass 2: defaults for every parameter left unset.
            for (index, slot) in parameter_slots.iter().copied().enumerate() {
                if frame.slots[slot.0 as usize].is_some() {
                    continue;
                }
                let default = function
                    .default_values
                    .get(index)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| {
                        runtime_error("missing required direct-call argument", function_span)
                    })?;
                let value = self.eval_expression(default, &mut frame, module)?;
                frame.write(slot, value, function_span)?;
            }
            match self.run_body(program, id, &function.body, &mut frame, module)? {
                RuntimeFlow::Return(value) => Ok(value),
                RuntimeFlow::Normal => Ok(Value::Void),
                RuntimeFlow::Break | RuntimeFlow::Continue => Err(runtime_error(
                    "loop control escaped a compiled function",
                    function_span,
                )
                .into()),
            }
        })();
        self.call_depth -= 1;
        result
    }

    fn execute_statements(
        &mut self,
        statements: &[CompiledStatement],
        frame: &mut Frame,
        module: crate::compiled::ModuleId,
    ) -> Result<RuntimeFlow, RuntimeFault> {
        for statement in statements {
            let flow = match statement {
                CompiledStatement::TupleBinding { slots, value, span } => {
                    let value = self.eval_expression(value, frame, module)?;
                    let Value::List(items) = value else {
                        return Err(type_error("tuple", &value, span).into());
                    };
                    if items.len() != slots.len() {
                        return Err(runtime_error("tuple binding length mismatch", span).into());
                    }
                    for (slot, item) in slots.iter().copied().zip(items.into_inner()) {
                        frame.write(slot, item, span)?;
                    }
                    RuntimeFlow::Normal
                }
                CompiledStatement::StoreLocal { slot, value, span } => {
                    let value = self.eval_expression(value, frame, module)?;
                    frame.write(*slot, value, span)?;
                    RuntimeFlow::Normal
                }
                CompiledStatement::StoreGlobal { name, value, span } => {
                    self.eval_store_global(module, name, value, frame, span)?;
                    RuntimeFlow::Normal
                }
                CompiledStatement::StoreFieldLocal {
                    slot,
                    fields,
                    value,
                    span,
                } => {
                    let value = self.eval_expression(value, frame, module)?;
                    let mut target = frame.read(*slot, span)?.clone();
                    assign_field_path(&mut target, fields, value, span)?;
                    frame.write(*slot, target, span)?;
                    RuntimeFlow::Normal
                }
                CompiledStatement::StoreFieldGlobal {
                    name,
                    fields,
                    value,
                    span,
                } => {
                    let value = self.eval_expression(value, frame, module)?;
                    let mut target = self.read_global(module, name, span)?;
                    assign_field_path(&mut target, fields, value, span)?;
                    self.write_global(module, name, target, span)?;
                    RuntimeFlow::Normal
                }
                CompiledStatement::Expression(expression, statement_span) => {
                    let value = self.eval_expression(expression, frame, module)?;
                    if self.shell_depth > 0 {
                        let outcome = match value {
                            Value::Shell(plan) => {
                                Some(self.execute_native_shell_plan(&plan, statement_span)?)
                            }
                            Value::MixedShell(shell) => Some(self.execute_mixed_shell(&shell)?),
                            Value::ShellProgram(program) => {
                                Some(self.execute_shell_program(&program)?)
                            }
                            _ => None,
                        };
                        if let Some(outcome) = outcome {
                            self.shell_outcome = Some(outcome);
                        }
                    }
                    RuntimeFlow::Normal
                }
                CompiledStatement::If {
                    condition,
                    then_body,
                    else_body,
                    span,
                } => match self.eval_expression(condition, frame, module)? {
                    Value::Bool(true) => self.execute_statements(then_body, frame, module)?,
                    Value::Bool(false) => self.execute_statements(else_body, frame, module)?,
                    value => return Err(type_error("bool", &value, span).into()),
                },
                CompiledStatement::For {
                    index_slot,
                    value_slot,
                    iterable,
                    body,
                    span,
                } => {
                    let Value::List(items) = self.eval_expression(iterable, frame, module)? else {
                        return Err(runtime_error("checked loop received a non-list", span).into());
                    };
                    let mut loop_flow = RuntimeFlow::Normal;
                    for (index, value) in items.into_iter().enumerate() {
                        if let Some(index_slot) = index_slot {
                            frame.write(*index_slot, Value::Int(index as i64), span)?;
                        }
                        frame.write(*value_slot, value, span)?;
                        match self.execute_statements(body, frame, module)? {
                            RuntimeFlow::Normal | RuntimeFlow::Continue => {}
                            RuntimeFlow::Break => break,
                            flow @ RuntimeFlow::Return(_) => {
                                loop_flow = flow;
                                break;
                            }
                        }
                    }
                    loop_flow
                }
                CompiledStatement::While {
                    condition,
                    body,
                    span,
                } => {
                    let mut loop_flow = RuntimeFlow::Normal;
                    loop {
                        if let Some(condition) = condition {
                            match self.eval_expression(condition, frame, module)? {
                                Value::Bool(true) => {}
                                Value::Bool(false) => break,
                                value => return Err(type_error("bool", &value, span).into()),
                            }
                        }
                        match self.execute_statements(body, frame, module)? {
                            RuntimeFlow::Normal | RuntimeFlow::Continue => {}
                            RuntimeFlow::Break => break,
                            flow @ RuntimeFlow::Return(_) => {
                                loop_flow = flow;
                                break;
                            }
                        }
                    }
                    loop_flow
                }
                CompiledStatement::Return(value, _) => RuntimeFlow::Return(match value {
                    Some(value) => self.eval_expression(value, frame, module)?,
                    None => Value::Void,
                }),
                CompiledStatement::Break(_) => RuntimeFlow::Break,
                CompiledStatement::Continue(_) => RuntimeFlow::Continue,
                CompiledStatement::Try {
                    body,
                    catch_slot,
                    handler,
                    span,
                } => match self.execute_statements(body, frame, module) {
                    Ok(flow) => flow,
                    Err(RuntimeFault::Raised(error)) => {
                        let caught = Value::Error(Box::new(value::ErrorValue {
                            message: error.to_string(),
                            kind: "runtime".into(),
                            code: 1,
                            cause: None,
                        }));
                        if let Some(catch_slot) = catch_slot {
                            frame.write(*catch_slot, caught, span)?;
                        }
                        self.execute_statements(handler, frame, module)?
                    }
                    // A `Fatal` runtime error always propagates unconditionally
                    // past `try`/`catch` — never caught by a user handler.
                    // `Exit` gets the same treatment: an `exit(code:)` inside a
                    // `try` block must not be swallowed by its `catch`, exactly
                    // as it isn't gated by any other control-flow construct.
                    Err(fault @ (RuntimeFault::Fatal(_) | RuntimeFault::Exit(_))) => {
                        return Err(fault)
                    }
                },
            };
            if let Some(code) = self.context.requested_exit() {
                return Ok(RuntimeFlow::Return(Value::Int(i64::from(code))));
            }
            if self.shell_exit && self.shell_depth > 0 {
                return Ok(RuntimeFlow::Normal);
            }
            if !matches!(flow, RuntimeFlow::Normal) {
                return Ok(flow);
            }
        }
        Ok(RuntimeFlow::Normal)
    }

    fn eval_expression(
        &mut self,
        expression: &CompiledExpression,
        frame: &mut Frame,
        module: crate::compiled::ModuleId,
    ) -> Result<Value, RuntimeFault> {
        match expression {
            CompiledExpression::Constant(value, _) => Ok(match value {
                // Scalars are the hot case; skip the 160-byte ConfigValue clone.
                ConfigValue::Int(value) => Value::Int(*value),
                ConfigValue::Float(value) => Value::Float(*value),
                ConfigValue::Bool(value) => Value::Bool(*value),
                other => Value::from_config(other.clone()),
            }),
            CompiledExpression::Local(slot, span) => Ok(frame.read(*slot, span)?.clone()),
            CompiledExpression::Global(name, span) => self.read_global(module, name, span),
            CompiledExpression::DefaultArgument(_) => Ok(Value::Void),
            CompiledExpression::ImportedValue { module, path, span } => {
                self.read_path(*module, path, span)
            }
            CompiledExpression::FunctionRef { function, .. } => Ok(Value::Function(*function)),
            CompiledExpression::Closure {
                captures,
                parameter_slots,
                slot_count,
                body,
                module: closure_module,
                span,
            } => {
                let mut captured = Frame::new(*slot_count);
                captured.type_bindings = frame.type_bindings.clone();
                for (slot, expression) in captures {
                    let value = self.eval_expression(expression, frame, module)?;
                    captured.write(*slot, value, span)?;
                }
                Ok(Value::Closure(Shared::from(ClosureValue {
                    captured,
                    parameter_slots: parameter_slots.clone(),
                    body: body.clone(),
                    module: *closure_module,
                    span: span.clone(),
                })))
            }
            CompiledExpression::Invoke {
                callee,
                arguments,
                span,
            } => {
                let callee = self.eval_expression(callee, frame, module)?;
                let values = arguments
                    .iter()
                    .map(|argument| self.eval_expression(argument, frame, module))
                    .collect::<Result<Vec<_>, _>>()?;
                match callee {
                    Value::Closure(closure) => self.call_closure(&closure, values, span),
                    Value::Function(function) => {
                        if self.function_is_async(function)? {
                            Ok(Value::Promise(self.scheduler.spawn(
                                function,
                                values,
                                self.context.spawn_child(),
                                self.call_depth,
                            )))
                        } else {
                            self.call_function(function, values)
                        }
                    }
                    other => Err(type_error("fn", &other, span).into()),
                }
            }
            CompiledExpression::HostCall {
                namespace,
                name,
                arguments,
                span,
            } => {
                let values = arguments
                    .iter()
                    .map(|argument| {
                        self.eval_expression(argument, frame, module)?
                            .try_into_config(span)
                            .map_err(RuntimeFault::from)
                    })
                    .collect::<Result<Vec<_>, RuntimeFault>>()?;
                self.state
                    .as_ref()
                    .ok_or_else(|| module_state_error(span))?
                    .hosts
                    .call(namespace, name, &values)
                    .map(Value::from_config)
                    .map_err(|error| SparError::EvalError {
                        message: error.to_string(),
                        span: span.clone(),
                    })
                    .map_err(Into::into)
            }
            CompiledExpression::NativeCall {
                function,
                arguments,
                return_type,
                span,
            } => {
                let values = arguments
                    .iter()
                    .map(|argument| self.eval_expression(argument, frame, module))
                    .collect::<Result<Vec<_>, _>>()?;
                let natives = self
                    .state
                    .as_ref()
                    .ok_or_else(|| module_state_error(span))?
                    .natives
                    .clone();
                if let Some(intrinsic) = natives.intrinsic(*function) {
                    let requested_type = return_type
                        .as_ref()
                        .map(|ty| resolve_runtime_type(ty, frame, module));
                    return self.execute_native_intrinsic(
                        intrinsic,
                        &values,
                        requested_type.as_ref(),
                        span,
                    );
                }
                natives
                    .call(*function, &mut self.context, &values, span)
                    .map_err(Into::into)
            }
            CompiledExpression::Panic { message, span } => {
                let message = self.eval_expression(message, frame, module)?;
                let Value::String(message) = message else {
                    return Err(type_error("str", &message, span).into());
                };
                Err(RuntimeFault::Fatal(Box::new(runtime_error(&message, span))))
            }
            CompiledExpression::ExecShell(shell) => {
                // Evaluate the compiled shell first so `${...}` interpolation in
                // arguments, environment and redirect targets is applied.
                let plan = self.eval_shell_plan(shell, frame, module)?;
                self.execute_shell(&plan, &shell.span)
                    .map(Value::from_config)
            }
            CompiledExpression::Await { promise, span } => {
                let value = self.eval_expression(promise, frame, module)?;
                let Value::Promise(handle) = value else {
                    return Err(type_error("Promise", &value, span).into());
                };
                self.drive_promise(handle, span)
            }
            CompiledExpression::StructConstruct {
                module: source_module,
                name,
                overrides,
                span,
            } => {
                let mut value = self.read_path(*source_module, std::slice::from_ref(name), span)?;
                let Value::Object(fields) = &mut value else {
                    return Err(runtime_error(
                        &format!("struct '{name}' did not evaluate to an object"),
                        span,
                    )
                    .into());
                };
                for (field, expression) in overrides {
                    let override_value = self.eval_expression(expression, frame, module)?;
                    fields.insert(field.as_str(), override_value);
                }
                Ok(value)
            }
            CompiledExpression::MethodCall {
                target,
                receiver,
                receiver_lvalue,
                arguments,
                mutates_receiver,
                span,
            } => {
                // Fast path: `x.method(...)` on a plain local variable,
                // calling a *native* method. The general path below reads
                // the receiver through `eval_expression`, which always
                // clones — for a container (`Map`/`List`/...) that clone
                // costs O(n) on every single call regardless of what the
                // method itself does, which dominates real workloads (see
                // SPAR_RUNTIME_FINDINGS.md Finding 19 in the
                // `researchgraph` project: 8,000 calls to `.length()` on a
                // *stable* 8,000-entry map cost 5.4s). Here we move the
                // value out of its slot instead, and always move it back
                // before returning — including on error, since arguments
                // are evaluated first (the only step that can fail before
                // we touch the slot) and a native call never drops the
                // receiver even if it errors (it's held by an owned local /
                // `&mut` borrow the whole time, never moved away).
                //
                // Scoped to native methods only: a user-defined `impl`
                // method's `self` is moved into a fresh callee frame, and if
                // that callee errors partway there is no value left to
                // restore. Natives don't have that problem, and every hot
                // container call in practice (`.insert`, `.get`, `.append`,
                // `.getOr`, ...) is a native method.
                if let (
                    Some(CompiledExpression::Local(slot, _)),
                    CompiledMethodTarget::Native(method),
                ) = (receiver.as_deref(), target)
                {
                    let slot = *slot;
                    let mut args = Vec::with_capacity(arguments.len());
                    for argument in arguments {
                        args.push(self.eval_expression(argument, frame, module)?);
                    }
                    let mut receiver_value = frame.take(slot, span)?;
                    let natives = self
                        .state
                        .as_ref()
                        .ok_or_else(|| module_state_error(span))?
                        .natives
                        .clone();
                    let outcome: Result<Value, RuntimeFault> = if *mutates_receiver {
                        natives
                            .call_method_mut(
                                *method,
                                &mut self.context,
                                &mut receiver_value,
                                &args,
                                span,
                            )
                            .map_err(Into::into)
                    } else {
                        let mut call_values = Vec::with_capacity(1 + args.len());
                        call_values.push(receiver_value);
                        call_values.append(&mut args);
                        let outcome = if let Some(intrinsic) = natives.method_intrinsic(*method) {
                            self.execute_native_intrinsic(intrinsic, &call_values, None, span)
                        } else {
                            natives
                                .call_method(*method, &mut self.context, &call_values, span)
                                .map_err(Into::into)
                        };
                        receiver_value = call_values.swap_remove(0);
                        outcome
                    };
                    frame.write(slot, receiver_value, span)?;
                    return outcome;
                }

                let mut values = Vec::new();
                if let Some(receiver) = receiver {
                    values.push(self.eval_expression(receiver, frame, module)?);
                }
                for argument in arguments {
                    values.push(self.eval_expression(argument, frame, module)?);
                }
                let (result, updated_receiver) = match target {
                    CompiledMethodTarget::Function(function) => {
                        // Mirror CompiledExpression::DirectCall: an async
                        // `impl` method must hand back a real Promise for
                        // `await` to consume, the same as an async free
                        // function — this had no such check at all, so
                        // `await receiver.asyncMethod()` typechecked (once
                        // infer_method_call wrapped its type in Promise<T>
                        // to match) but crashed at eval with "expected
                        // Promise, received <T>". A self-mutating async
                        // method can't also write back an updated receiver
                        // here (the call is deferred, not run inline), but
                        // no stdlib method combines the two today.
                        if self.function_is_async(*function)? {
                            return Ok(Value::Promise(self.scheduler.spawn(
                                *function,
                                values,
                                self.context.spawn_child(),
                                self.call_depth,
                            )));
                        }
                        let (result, method_frame, parameter_slots) =
                            self.call_function_with_frame(*function, values)?;
                        let updated = if *mutates_receiver {
                            let self_slot = parameter_slots.ok_or_else(|| {
                                runtime_error("mutable method is missing self parameter", span)
                            })?;
                            Some(method_frame.read(self_slot, span)?.clone())
                        } else {
                            None
                        };
                        (result, updated)
                    }
                    CompiledMethodTarget::Native(method) => {
                        let natives = self
                            .state
                            .as_ref()
                            .ok_or_else(|| module_state_error(span))?
                            .natives
                            .clone();
                        if *mutates_receiver {
                            let mut iter = values.into_iter();
                            let mut receiver_value = iter.next().ok_or_else(|| {
                                runtime_error("mutable native method is missing receiver", span)
                            })?;
                            let args = iter.collect::<Vec<_>>();
                            let result = natives.call_method_mut(
                                *method,
                                &mut self.context,
                                &mut receiver_value,
                                &args,
                                span,
                            )?;
                            (result, Some(receiver_value))
                        } else {
                            let result = if let Some(intrinsic) = natives.method_intrinsic(*method)
                            {
                                self.execute_native_intrinsic(intrinsic, &values, None, span)?
                            } else {
                                natives.call_method(*method, &mut self.context, &values, span)?
                            };
                            (result, None)
                        }
                    }
                };
                if let Some(updated) = updated_receiver {
                    let target = receiver_lvalue.as_ref().ok_or_else(|| {
                        runtime_error("mutable method receiver has no writeback target", span)
                    })?;
                    self.write_compiled_lvalue(frame, module, target, updated, span)?;
                }
                Ok(result)
            }
            CompiledExpression::DirectCall {
                function,
                arguments,
                return_type,
                span: _,
            } => {
                if !self.function_is_async(*function)? {
                    return self.call_direct_inline(
                        *function,
                        arguments,
                        frame,
                        module,
                        return_type.as_ref(),
                    );
                }
                let values = arguments
                    .iter()
                    .map(|argument| self.eval_expression(argument, frame, module))
                    .collect::<Result<Vec<_>, _>>()?;
                // Async generic reification is not currently consumed by a
                // native reflection API. Ordinary promise execution remains
                // type-erased; sync generic wrappers preserve their target.
                Ok(Value::Promise(self.scheduler.spawn(
                    *function,
                    values,
                    self.context.spawn_child(),
                    self.call_depth,
                )))
            }
            CompiledExpression::List(items, _) => Ok(Value::List(Shared::from(
                items
                    .iter()
                    .map(|item| self.eval_expression(item, frame, module))
                    .collect::<Result<Vec<_>, _>>()?,
            ))),
            CompiledExpression::Object(items, span) => {
                let mut object = Record::new();
                for item in items {
                    match item {
                        CompiledObjectItem::Field { name, value } => {
                            object
                                .insert(name.clone(), self.eval_expression(value, frame, module)?);
                        }
                        CompiledObjectItem::Spread(value) => {
                            let Value::Object(fields) =
                                self.eval_expression(value, frame, module)?
                            else {
                                return Err(runtime_error(
                                    "checked Record spread received a non-Record value",
                                    span,
                                )
                                .into());
                            };
                            object.extend(fields);
                        }
                    }
                }
                Ok(Value::Object(Shared::from(object)))
            }
            CompiledExpression::Map(items, span) => {
                let mut entries = crate::runtime::value::MapValue::new();
                for item in items {
                    match item {
                        CompiledObjectItem::Field { name, value } => {
                            let value = self.eval_expression(value, frame, module)?;
                            entries.insert(Value::String(name.clone()), value);
                        }
                        CompiledObjectItem::Spread(value) => {
                            let Value::Map(values) = self.eval_expression(value, frame, module)?
                            else {
                                return Err(runtime_error(
                                    "checked Map spread received a non-Map value",
                                    span,
                                )
                                .into());
                            };
                            for (key, value) in values {
                                entries.insert(key, value);
                            }
                        }
                    }
                }
                Ok(Value::Map((Shared::from(entries)).into_inner()))
            }
            CompiledExpression::Operation {
                operation,
                operands,
                span,
            } => {
                if *operation == TypedOperation::Fallback {
                    let [left, right] = operands.as_slice() else {
                        return Err(runtime_error(
                            "checked fallback operation has invalid arity",
                            span,
                        )
                        .into());
                    };
                    return self
                        .eval_expression(left, frame, module)
                        .or_else(|_| self.eval_expression(right, frame, module));
                }
                // `&&`/`||` must short-circuit: the right operand can be
                // unsafe to evaluate when the left already decides the
                // result (e.g. `xs.length() > 0 && xs[0] > 0`).
                if matches!(operation, TypedOperation::BoolAnd | TypedOperation::BoolOr) {
                    let [left, right] = operands.as_slice() else {
                        return Err(runtime_error(
                            "checked boolean operation has invalid arity",
                            span,
                        )
                        .into());
                    };
                    let left_value = self.eval_expression(left, frame, module)?;
                    let Value::Bool(left_bool) = left_value else {
                        return Err(runtime_error(
                            "boolean operator received a non-bool operand",
                            span,
                        )
                        .into());
                    };
                    let short_circuits = match operation {
                        TypedOperation::BoolAnd => !left_bool,
                        _ => left_bool,
                    };
                    if short_circuits {
                        return Ok(Value::Bool(left_bool));
                    }
                    return self.eval_expression(right, frame, module);
                }
                match operands.as_slice() {
                    [operand] => {
                        let values = [self.eval_expression(operand, frame, module)?];
                        Ok(eval_operation(*operation, &values, span)?)
                    }
                    [left, right] => {
                        let left = self.eval_expression(left, frame, module)?;
                        let right = self.eval_expression(right, frame, module)?;
                        if let (Value::Int(a), Value::Int(b)) = (&left, &right) {
                            if let Some(fast) = eval_int_binary(*operation, *a, *b) {
                                return Ok(fast);
                            }
                        }
                        let values = [left, right];
                        Ok(eval_operation(*operation, &values, span)?)
                    }
                    _ => {
                        let values = operands
                            .iter()
                            .map(|operand| self.eval_expression(operand, frame, module))
                            .collect::<Result<Vec<_>, _>>()?;
                        Ok(eval_operation(*operation, &values, span)?)
                    }
                }
            }
            CompiledExpression::Index {
                source,
                index,
                span,
            } => {
                let source = self.eval_expression(source, frame, module)?;
                let index = self.eval_expression(index, frame, module)?;
                match (source, index) {
                    (Value::Bytes(items), Value::Int(index)) if index >= 0 => {
                        Ok(Value::Int(i64::from(
                            *items.get(index as usize).ok_or_else(|| {
                                runtime_error("byte index is out of bounds", span)
                            })?,
                        )))
                    }
                    (Value::List(items), Value::Int(index)) if index >= 0 => Ok(items
                        .get(index as usize)
                        .cloned()
                        .ok_or_else(|| runtime_error("list index is out of bounds", span))?),
                    (Value::Object(mut fields), Value::Int(index)) if index >= 0 => {
                        let Some(Value::List(items)) = fields.shift_remove("values") else {
                            return Err(runtime_error(
                                "checked index received a non-list object",
                                span,
                            )
                            .into());
                        };
                        Ok(items
                            .get(index as usize)
                            .cloned()
                            .ok_or_else(|| runtime_error("byte index is out of bounds", span))?)
                    }
                    (source, index) => Err(runtime_error(
                        &format!(
                            "checked index received {} and {}",
                            source.type_name(),
                            index.type_name()
                        ),
                        span,
                    )
                    .into()),
                }
            }
            CompiledExpression::Field { base, field, span } => {
                let base = self.eval_expression(base, frame, module)?;
                field_of_value(base, field, span)
            }
            CompiledExpression::Interpolation(parts, span) => {
                let mut output = String::new();
                for part in parts {
                    match part {
                        CompiledStringPart::Literal(value) => output.push_str(value),
                        CompiledStringPart::Expression(value) => {
                            let value = self.eval_expression(value, frame, module)?;
                            match value {
                                Value::String(value) => output.push_str(&value),
                                Value::Int(value) => output.push_str(&value.to_string()),
                                Value::Float(value) => output.push_str(&value.to_string()),
                                Value::Bool(value) => output.push_str(&value.to_string()),
                                other => return Err(type_error("primitive", &other, span).into()),
                            }
                        }
                    }
                }
                Ok(Value::String(output))
            }
            CompiledExpression::Comprehension {
                binding,
                source,
                body,
                span,
            } => {
                let Value::List(items) = self.eval_expression(source, frame, module)? else {
                    return Err(
                        runtime_error("checked comprehension received a non-list", span).into(),
                    );
                };
                let mut output = Vec::with_capacity(items.len());
                for item in items {
                    frame.write(*binding, item, span)?;
                    output.push(self.eval_expression(body, frame, module)?);
                }
                Ok(Value::List(Shared::from(output)))
            }
            CompiledExpression::Shell(shell) => Ok(Value::Shell(Shared::from(
                self.eval_shell_plan(shell, frame, module)?,
            ))),
            CompiledExpression::MixedShell(shell) => {
                Ok(Value::MixedShell(Shared::from(MixedShellValue {
                    plan: shell.clone(),
                    captured: frame.clone(),
                    module,
                    span: shell.span.clone(),
                })))
            }
            CompiledExpression::CommandSubstitution(shell) => Ok(Value::String(
                self.execute_command_substitution(shell, frame, module)?,
            )),
            CompiledExpression::ShellProgram { body, span } => {
                Ok(Value::ShellProgram(Shared::from(ShellProgramValue {
                    body: body.clone(),
                    captured: frame.clone(),
                    module,
                    span: span.clone(),
                })))
            }
        }
    }

    fn execute_native_intrinsic(
        &mut self,
        intrinsic: NativeIntrinsic,
        args: &[Value],
        requested_type: Option<&RuntimeTypeBinding>,
        span: &Span,
    ) -> Result<Value, RuntimeFault> {
        match intrinsic {
            #[cfg(not(target_arch = "wasm32"))]
            NativeIntrinsic::External(index) => {
                crate::native_module::call_external(index, self, args, span)
            }
            #[cfg(target_arch = "wasm32")]
            NativeIntrinsic::External(_) => {
                Err(runtime_error("native modules are not available on this target", span).into())
            }
            NativeIntrinsic::JsonParse => {
                let target = requested_type.ok_or_else(|| {
                    runtime_error("typed JSON parse is missing its reified target type", span)
                })?;
                if matches!(target.ty, SparType::TypeParameter(_)) {
                    return Err(runtime_error(
                        "typed JSON parse target was not resolved from the generic call site",
                        span,
                    )
                    .into());
                }
                let text = match args.first() {
                    Some(Value::String(text)) => text,
                    Some(other) => {
                        return Err(type_error("str", other, span).into());
                    }
                    None => {
                        return Err(
                            runtime_error("JSON parse requires a text argument", span).into()
                        );
                    }
                };
                let parsed: serde_json::Value = serde_json::from_str(text)
                    .map_err(|error| runtime_error(&format!("invalid JSON: {error}"), span))?;

                self.ensure_module(target.module)?;
                let target_symbols = self
                    .program
                    .modules
                    .iter()
                    .find(|module| module.id == target.module)
                    .map(|module| module.checked.symbols.clone())
                    .ok_or_else(|| {
                        runtime_error("typed JSON target module is unavailable", span)
                    })?;
                let imports = self
                    .program
                    .modules
                    .iter()
                    .find(|module| module.id == target.module)
                    .unwrap()
                    .import_modules
                    .clone();
                let functions = self
                    .program
                    .modules
                    .iter()
                    .flat_map(|module| {
                        module
                            .functions
                            .iter()
                            .map(|function| (function.key.clone(), function.id))
                    })
                    .collect();
                let parameters = self
                    .program
                    .modules
                    .iter()
                    .flat_map(|module| {
                        crate::compiled::function_declarations(&module.checked.program)
                            .into_iter()
                            .zip(&module.functions)
                            .map(|(declaration, function)| {
                                (
                                    function.id,
                                    declaration
                                        .params
                                        .iter()
                                        .map(|parameter| {
                                            (parameter.name.clone(), parameter.ty.clone())
                                        })
                                        .collect(),
                                )
                            })
                    })
                    .collect();
                let mut default_fault = None;
                let mut evaluate_default = |expression: &crate::ast::Expr, ty: &SparType| {
                    let (expression, slots) = crate::lowerer::lower_default(
                        expression,
                        ty,
                        &target_symbols,
                        crate::lowerer::LoweringContext {
                            module: target.module,
                            imports: &imports,
                            functions: &functions,
                            parameters: &parameters,
                        },
                    )?;
                    self.eval_expression(&expression, &mut Frame::new(slots), target.module)
                        .map_err(|fault| {
                            default_fault = Some(fault.clone());
                            fault.into_error()
                        })
                };
                let mut environment = crate::stdlib::support::JsonDecodeEnvironment {
                    symbols: &target_symbols,
                    evaluate_default: &mut evaluate_default,
                };
                let decoded =
                    crate::stdlib::support::decode_json_typed(parsed, &target.ty, &mut environment);
                match default_fault {
                    Some(fault) => Err(fault),
                    None => decoded.map_err(Into::into),
                }
            }
            NativeIntrinsic::PromiseRace => {
                let promises = match args.first() {
                    Some(Value::List(values)) => values
                        .iter()
                        .map(|value| match value {
                            Value::Promise(handle) => Ok(*handle),
                            other => Err(runtime_error(
                                &format!(
                                    "race expected Promise<T> values, received {}",
                                    other.type_name()
                                ),
                                span,
                            )),
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    Some(other) => {
                        return Err(runtime_error(
                            &format!(
                                "race expected a list of promises, received {}",
                                other.type_name()
                            ),
                            span,
                        )
                        .into())
                    }
                    None => {
                        return Err(runtime_error("race requires a promises argument", span).into())
                    }
                };
                if promises.is_empty() {
                    return Err(runtime_error("race requires at least one promise", span).into());
                }
                loop {
                    let mut pending = false;
                    for handle in &promises {
                        match self.scheduler.status_snapshot(*handle) {
                            TaskStatus::Ready(result) => return self.apply_exit_and_unwrap(result),
                            TaskStatus::Pending => pending = true,
                            TaskStatus::Running => pending = true,
                            TaskStatus::Cancelled => {
                                return Err(runtime_error(
                                    "race encountered a cancelled promise",
                                    span,
                                )
                                .into())
                            }
                            TaskStatus::Unknown => {
                                return Err(RuntimeFault::Fatal(Box::new(runtime_error(
                                    "race received an unknown promise",
                                    span,
                                ))))
                            }
                        }
                    }
                    if !pending {
                        return Err(RuntimeFault::Fatal(Box::new(runtime_error(
                            "race scheduler made no progress",
                            span,
                        ))));
                    }
                    self.scheduler
                        .wait_for_any_change(std::time::Duration::from_millis(50));
                }
            }
            NativeIntrinsic::PromiseTimeout => {
                let handle = match args.first() {
                    Some(Value::Promise(handle)) => *handle,
                    Some(other) => {
                        return Err(runtime_error(
                            &format!("timeout expected a promise, received {}", other.type_name()),
                            span,
                        )
                        .into())
                    }
                    None => {
                        return Err(
                            runtime_error("timeout requires a promise argument", span).into()
                        )
                    }
                };
                let millis = match args.get(1) {
                    Some(Value::Int(value)) if *value >= 0 => *value as u64,
                    Some(Value::Int(_)) => {
                        return Err(
                            runtime_error("timeout duration cannot be negative", span).into()
                        )
                    }
                    Some(other) => {
                        return Err(runtime_error(
                            &format!(
                                "timeout expected an int duration, received {}",
                                other.type_name()
                            ),
                            span,
                        )
                        .into())
                    }
                    None => {
                        return Err(runtime_error("timeout requires a millis argument", span).into())
                    }
                };
                let started = self
                    .scheduler
                    .created_at(handle)
                    .unwrap_or_else(std::time::Instant::now);
                let limit = std::time::Duration::from_millis(millis);
                loop {
                    match self.scheduler.status_snapshot(handle) {
                        TaskStatus::Ready(result) => {
                            if started.elapsed() > limit {
                                return Err(runtime_error(
                                    &format!("promise timed out after {millis} ms"),
                                    span,
                                )
                                .into());
                            }
                            return self.apply_exit_and_unwrap(result);
                        }
                        // `Running` now legitimately means "a worker thread
                        // is actively executing it right now" (real
                        // concurrency), not a same-stack cycle the way it
                        // did under the old single-threaded pump — so it
                        // waits exactly like `Pending`.
                        TaskStatus::Pending | TaskStatus::Running => {}
                        TaskStatus::Cancelled => {
                            return Err(runtime_error("promise was cancelled", span).into())
                        }
                        TaskStatus::Unknown => {
                            return Err(RuntimeFault::Fatal(Box::new(runtime_error(
                                "unknown promise handle",
                                span,
                            ))))
                        }
                    }
                    if started.elapsed() >= limit {
                        return Err(runtime_error(
                            &format!("promise timed out after {millis} ms"),
                            span,
                        )
                        .into());
                    }
                    self.scheduler
                        .wait_for_any_change(std::time::Duration::from_millis(50));
                }
            }
            intrinsic @ (NativeIntrinsic::DataMap
            | NativeIntrinsic::DataFilter
            | NativeIntrinsic::DataTake
            | NativeIntrinsic::DataSkip
            | NativeIntrinsic::DataFirst
            | NativeIntrinsic::DataLast
            | NativeIntrinsic::DataCollect
            | NativeIntrinsic::DataCollectTable
            | NativeIntrinsic::DataCount
            | NativeIntrinsic::DataSortBy
            | NativeIntrinsic::DataGroupBy
            | NativeIntrinsic::DataUnique
            | NativeIntrinsic::DataUniqueBy
            | NativeIntrinsic::DataFlatten
            | NativeIntrinsic::DataGet
            | NativeIntrinsic::DataSelect
            | NativeIntrinsic::DataSchema
            | NativeIntrinsic::DataInspect
            | NativeIntrinsic::DataFind
            | NativeIntrinsic::DataFindIndex
            | NativeIntrinsic::DataAny
            | NativeIntrinsic::DataEvery) => self.execute_data_intrinsic(intrinsic, args, span),
            NativeIntrinsic::CoreMapGetOrElse
            | NativeIntrinsic::CoreOptionUnwrapOrElse
            | NativeIntrinsic::CoreOptionMap
            | NativeIntrinsic::CoreOptionFilter
            | NativeIntrinsic::CoreOptionAndThen
            | NativeIntrinsic::CoreOptionOrElse
            | NativeIntrinsic::CoreResultMap
            | NativeIntrinsic::CoreResultMapErr
            | NativeIntrinsic::CoreResultAndThen
            | NativeIntrinsic::CoreResultOrElse => {
                self.execute_core_callable_intrinsic(intrinsic, args, span)
            }
        }
    }

    fn execute_data_intrinsic(
        &mut self,
        intrinsic: NativeIntrinsic,
        args: &[Value],
        span: &Span,
    ) -> Result<Value, RuntimeFault> {
        let source = args
            .first()
            .ok_or_else(|| runtime_error("structured data operation requires a source", span))?;
        match intrinsic {
            NativeIntrinsic::DataMap => {
                let callable = args
                    .get(1)
                    .ok_or_else(|| runtime_error("map requires a transform callable", span))?
                    .clone();
                match source {
                    Value::List(values) => values
                        .iter()
                        .cloned()
                        .map(|value| self.invoke_data_callable(&callable, vec![value], span))
                        .collect::<Result<Vec<_>, _>>()
                        .map(|items| Value::List(Shared::from(items))),
                    Value::Table(table) => {
                        let rows = table
                            .rows()
                            .iter()
                            .cloned()
                            .map(|value| self.invoke_data_callable(&callable, vec![value], span))
                            .collect::<Result<Vec<_>, _>>()?;
                        // Rows that are not Records (a scalar projection) leave the
                        // table and come back as a plain list.
                        if !rows.iter().all(|row| matches!(row, Value::Object(_))) {
                            return Ok(Value::List(Shared::from(rows)));
                        }
                        let table = TableValue::from_records(rows).map_err(|error| {
                            runtime_error(
                                &format!("map over Table must produce Record rows: {error}"),
                                span,
                            )
                        })?;
                        Ok(Value::Table(Shared::from(table)))
                    }
                    Value::Resource(_) => {
                        let stream = self.take_stream_resource(source, span)?;
                        let stream = stream
                            .map_lazy(crate::ast::SparType::TypeParameter("U".into()), callable);
                        Ok(Value::Resource(self.context.insert_stream(stream)))
                    }
                    other => Err(evaluation_error(
                        &format!(
                            "`map` needs a list, table or stream, but got {}",
                            sequence_kind(other)
                        ),
                        span,
                    )
                    .into()),
                }
            }
            NativeIntrinsic::DataFilter => {
                let predicate = args
                    .get(1)
                    .ok_or_else(|| runtime_error("filter requires a predicate callable", span))?
                    .clone();
                match source {
                    Value::List(values) => {
                        let mut output = Vec::new();
                        for value in values.iter().cloned() {
                            if self.invoke_predicate(&predicate, value.clone(), span)? {
                                output.push(value);
                            }
                        }
                        Ok(Value::List(Shared::from(output)))
                    }
                    Value::Table(table) => {
                        let mut rows = Vec::new();
                        for value in table.rows().iter().cloned() {
                            if self.invoke_predicate(&predicate, value.clone(), span)? {
                                rows.push(value);
                            }
                        }
                        Ok(Value::Table(Shared::from(TableValue::with_schema(
                            rows,
                            table.schema().clone(),
                        ))))
                    }
                    Value::Resource(_) => {
                        let stream = self.take_stream_resource(source, span)?;
                        let stream = stream.filter_lazy(predicate);
                        Ok(Value::Resource(self.context.insert_stream(stream)))
                    }
                    other => Err(evaluation_error(
                        &format!(
                            "`filter` needs a list, table or stream, but got {}",
                            sequence_kind(other)
                        ),
                        span,
                    )
                    .into()),
                }
            }
            NativeIntrinsic::DataTake => {
                let count = self.data_count_arg(args, 1, span)?;
                match source {
                    Value::List(values) => {
                        Ok(Value::List(values.iter().take(count).cloned().collect()))
                    }
                    Value::Table(table) => Ok(Value::Table(Shared::from(table.take(count)))),
                    Value::Resource(_) => {
                        let stream = self.take_stream_resource(source, span)?.take_lazy(count);
                        Ok(Value::Resource(self.context.insert_stream(stream)))
                    }
                    other => Err(evaluation_error(
                        &format!(
                            "`take` needs a list, table or stream, but got {}",
                            sequence_kind(other)
                        ),
                        span,
                    )
                    .into()),
                }
            }
            NativeIntrinsic::DataSkip => {
                let count = self.data_count_arg(args, 1, span)?;
                match source {
                    Value::List(values) => {
                        Ok(Value::List(values.iter().skip(count).cloned().collect()))
                    }
                    Value::Table(table) => Ok(Value::Table(Shared::from(table.skip(count)))),
                    Value::Resource(_) => {
                        let stream = self.take_stream_resource(source, span)?.skip_lazy(count);
                        Ok(Value::Resource(self.context.insert_stream(stream)))
                    }
                    other => Err(evaluation_error(
                        &format!(
                            "`skip` needs a list, table or stream, but got {}",
                            sequence_kind(other)
                        ),
                        span,
                    )
                    .into()),
                }
            }
            NativeIntrinsic::DataFirst => match source {
                Value::List(values) => values.first().cloned().ok_or_else(|| {
                    runtime_error("first cannot read an empty sequence", span).into()
                }),
                Value::Table(table) => table.rows().first().cloned().ok_or_else(|| {
                    runtime_error("first cannot read an empty sequence", span).into()
                }),
                Value::Resource(_) => {
                    let mut stream = self.take_stream_resource(source, span)?;
                    let result = self.pull_stream(&mut stream, span)?.ok_or_else(|| {
                        runtime_error("first cannot read an empty sequence", span)
                    })?;
                    stream.cancel();
                    Ok(result)
                }
                other => Err(evaluation_error(
                    &format!(
                        "`first` needs a list, table or stream, but got {}",
                        sequence_kind(other)
                    ),
                    span,
                )
                .into()),
            },
            NativeIntrinsic::DataLast => match source {
                Value::List(values) => values.last().cloned().ok_or_else(|| {
                    runtime_error("last cannot read an empty sequence", span).into()
                }),
                Value::Table(table) => table.rows().last().cloned().ok_or_else(|| {
                    runtime_error("last cannot read an empty sequence", span).into()
                }),
                Value::Resource(_) => {
                    let mut stream = self.take_stream_resource(source, span)?;
                    let mut last = None;
                    while let Some(value) = self.pull_stream(&mut stream, span)? {
                        last = Some(value);
                    }
                    last.ok_or_else(|| {
                        runtime_error("last cannot read an empty sequence", span).into()
                    })
                }
                other => Err(evaluation_error(
                    &format!(
                        "`last` needs a list, table or stream, but got {}",
                        sequence_kind(other)
                    ),
                    span,
                )
                .into()),
            },
            NativeIntrinsic::DataCollect => {
                let (_, values) = self.materialize_data_sequence(source, span)?;
                Ok(Value::List(Shared::from(values)))
            }
            NativeIntrinsic::DataCollectTable => match source {
                Value::Table(table) => Ok(Value::Table(table.clone())),
                _ => {
                    let (_, rows) = self.materialize_data_sequence(source, span)?;
                    let table = TableValue::from_records(rows).map_err(|error| {
                        runtime_error(&format!("collectTable requires Record rows: {error}"), span)
                    })?;
                    Ok(Value::Table(Shared::from(table)))
                }
            },
            NativeIntrinsic::DataCount => match source {
                Value::List(values) => Ok(Value::Int(values.len() as i64)),
                Value::Table(table) => Ok(Value::Int(table.len() as i64)),
                Value::Resource(_) => {
                    let (_, values) = self.materialize_data_sequence(source, span)?;
                    Ok(Value::Int(values.len() as i64))
                }
                other => Err(evaluation_error(
                    &format!(
                        "`count` needs a list, table or stream, but got {}",
                        sequence_kind(other)
                    ),
                    span,
                )
                .into()),
            },
            NativeIntrinsic::DataSortBy => {
                let key = args
                    .get(1)
                    .ok_or_else(|| runtime_error("sortBy requires a key callable", span))?
                    .clone();
                let (shape, values) = self.materialize_data_sequence(source, span)?;
                let mut keyed = Vec::with_capacity(values.len());
                for value in values {
                    let sort_key = self.invoke_data_callable(&key, vec![value.clone()], span)?;
                    if sort_key.data_ordering(&sort_key).is_none() {
                        return Err(runtime_error(
                            &format!(
                                "sortBy key must return int, float, str, or bool; found {}",
                                sort_key.type_name()
                            ),
                            span,
                        )
                        .into());
                    }
                    keyed.push((value, sort_key));
                }
                if let Some((_, first_key)) = keyed.first() {
                    for (_, other_key) in keyed.iter().skip(1) {
                        if first_key.data_ordering(other_key).is_none() {
                            return Err(runtime_error(
                                "sortBy keys must all have the same orderable type",
                                span,
                            )
                            .into());
                        }
                    }
                }
                keyed.sort_by(|left, right| {
                    left.1
                        .data_ordering(&right.1)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                self.rebuild_data_sequence(
                    shape,
                    keyed.into_iter().map(|(value, _)| value).collect(),
                    span,
                )
            }
            NativeIntrinsic::DataGroupBy => {
                let key = args
                    .get(1)
                    .ok_or_else(|| runtime_error("groupBy requires a key callable", span))?
                    .clone();
                let (_, values) = self.materialize_data_sequence(source, span)?;
                let mut groups: Vec<(Value, Vec<Value>)> = Vec::new();
                for value in values {
                    let group_key = self.invoke_data_callable(&key, vec![value.clone()], span)?;
                    self.ensure_data_comparable(&group_key, "groupBy key", span)?;
                    if let Some(index) = groups
                        .iter()
                        .position(|(existing, _)| existing == &group_key)
                    {
                        groups[index].1.push(value);
                    } else {
                        groups.push((group_key, vec![value]));
                    }
                }
                let mut output = Vec::with_capacity(groups.len());
                for (key, rows) in groups {
                    let table = TableValue::from_records(rows).map_err(|error| {
                        runtime_error(&format!("groupBy requires Record rows: {error}"), span)
                    })?;
                    output.push((key, Value::Table(Shared::from(table))));
                }
                Ok(Value::Map(output.into()))
            }
            NativeIntrinsic::DataUnique => match source {
                Value::Resource(_) => {
                    let stream = self.take_stream_resource(source, span)?.unique_lazy();
                    Ok(Value::Resource(self.context.insert_stream(stream)))
                }
                _ => {
                    let (shape, values) = self.materialize_data_sequence(source, span)?;
                    let mut output = Vec::new();
                    for value in values {
                        self.ensure_data_comparable(&value, "unique value", span)?;
                        if !output.iter().any(|existing| existing == &value) {
                            output.push(value);
                        }
                    }
                    self.rebuild_data_sequence(shape, output, span)
                }
            },
            NativeIntrinsic::DataUniqueBy => {
                let key = args
                    .get(1)
                    .ok_or_else(|| runtime_error("uniqueBy requires a key callable", span))?
                    .clone();
                match source {
                    Value::Resource(_) => {
                        let stream = self.take_stream_resource(source, span)?.unique_by_lazy(key);
                        Ok(Value::Resource(self.context.insert_stream(stream)))
                    }
                    _ => {
                        let (shape, values) = self.materialize_data_sequence(source, span)?;
                        let mut seen = Vec::new();
                        let mut output = Vec::new();
                        for value in values {
                            let computed =
                                self.invoke_data_callable(&key, vec![value.clone()], span)?;
                            self.ensure_data_comparable(&computed, "uniqueBy key", span)?;
                            if seen.iter().any(|existing| existing == &computed) {
                                continue;
                            }
                            seen.push(computed);
                            output.push(value);
                        }
                        self.rebuild_data_sequence(shape, output, span)
                    }
                }
            }
            NativeIntrinsic::DataFlatten => match source {
                Value::List(values) => {
                    let mut output = Vec::new();
                    for value in values {
                        let Value::List(items) = value else {
                            return Err(runtime_error(
                                "flatten expects every sequence element to be a list",
                                span,
                            )
                            .into());
                        };
                        output.extend(items.iter().cloned());
                    }
                    Ok(Value::List(Shared::from(output)))
                }
                Value::Resource(_) => {
                    let stream = self
                        .take_stream_resource(source, span)?
                        .flatten_lazy(crate::ast::SparType::TypeParameter("T".into()));
                    Ok(Value::Resource(self.context.insert_stream(stream)))
                }
                Value::Table(_) => Err(runtime_error(
                    "flatten is not defined for Table rows; use select/map to project a list first",
                    span,
                )
                .into()),
                other => Err(runtime_error(
                    &format!(
                        "flatten expected List or Stream; found {}",
                        other.type_name()
                    ),
                    span,
                )
                .into()),
            },
            NativeIntrinsic::DataGet => {
                let key = args
                    .get(1)
                    .ok_or_else(|| runtime_error("get requires a key/index", span))?;
                match source {
                    Value::List(values) => {
                        let index = self.data_index(key, span)?;
                        values
                            .get(index)
                            .cloned()
                            .ok_or_else(|| runtime_error("get index is out of bounds", span).into())
                    }
                    Value::Table(table) => {
                        let index = self.data_index(key, span)?;
                        table
                            .rows()
                            .get(index)
                            .cloned()
                            .ok_or_else(|| runtime_error("get index is out of bounds", span).into())
                    }
                    Value::Resource(_) => {
                        let index = self.data_index(key, span)?;
                        let mut stream = self.take_stream_resource(source, span)?;
                        for current in 0..=index {
                            let Some(value) = self.pull_stream(&mut stream, span)? else {
                                return Err(
                                    runtime_error("get index is out of bounds", span).into()
                                );
                            };
                            if current == index {
                                stream.cancel();
                                return Ok(value);
                            }
                        }
                        unreachable!()
                    }
                    Value::Map(entries) => {
                        self.ensure_data_comparable(key, "map key", span)?;
                        entries.get(key).cloned().ok_or_else(|| {
                            runtime_error("map does not contain the requested key", span).into()
                        })
                    }
                    other => Err(runtime_error(
                        &format!(
                            "get expected List, Table, Stream, or Map; found {}",
                            other.type_name()
                        ),
                        span,
                    )
                    .into()),
                }
            }
            NativeIntrinsic::DataSelect => {
                let fields = self.data_fields_arg(args, 1, span)?;
                match source {
                    Value::List(values) => values
                        .iter()
                        .cloned()
                        .map(|value| self.project_data_record(value, &fields, span))
                        .collect::<Result<Vec<_>, _>>()
                        .map(|items| Value::List(Shared::from(items))),
                    Value::Table(table) => {
                        let rows = table
                            .rows()
                            .iter()
                            .cloned()
                            .map(|value| self.project_data_record(value, &fields, span))
                            .collect::<Result<Vec<_>, _>>()?;
                        let table = TableValue::from_records(rows).map_err(|error| {
                            runtime_error(
                                &format!("select could not infer output schema: {error}"),
                                span,
                            )
                        })?;
                        Ok(Value::Table(Shared::from(table)))
                    }
                    Value::Resource(_) => {
                        let stream = self.take_stream_resource(source, span)?.select_lazy(fields);
                        Ok(Value::Resource(self.context.insert_stream(stream)))
                    }
                    other => Err(evaluation_error(
                        &format!(
                            "`select` needs a list, table or stream, but got {}",
                            sequence_kind(other)
                        ),
                        span,
                    )
                    .into()),
                }
            }
            NativeIntrinsic::DataSchema => match source {
                Value::Table(table) => Ok(Value::Schema(Shared::from(table.schema().clone()))),
                Value::List(values) => Schema::infer_records(values)
                    .map(|schema| Value::Schema(Shared::from(schema)))
                    .map_err(|error| {
                        runtime_error(&format!("schema requires Record rows: {error}"), span).into()
                    }),
                Value::Resource(_) => {
                    let (_, rows) = self.materialize_data_sequence(source, span)?;
                    Schema::infer_records(&rows)
                        .map(|schema| Value::Schema(Shared::from(schema)))
                        .map_err(|error| {
                            runtime_error(&format!("schema requires Record rows: {error}"), span)
                                .into()
                        })
                }
                other => Err(evaluation_error(
                    &format!(
                        "`schema` needs a list, table or stream, but got {}",
                        sequence_kind(other)
                    ),
                    span,
                )
                .into()),
            },
            NativeIntrinsic::DataFind => {
                let predicate = args
                    .get(1)
                    .ok_or_else(|| runtime_error("find requires a predicate callable", span))?
                    .clone();
                let Value::List(values) = source else {
                    return Err(type_error("List", source, span).into());
                };
                for value in values.iter().cloned() {
                    if self.invoke_predicate(&predicate, value.clone(), span)? {
                        return Ok(Value::Option(Some(Box::new(value))));
                    }
                }
                Ok(Value::Option(None))
            }
            NativeIntrinsic::DataFindIndex => {
                let predicate = args
                    .get(1)
                    .ok_or_else(|| runtime_error("findIndex requires a predicate callable", span))?
                    .clone();
                let Value::List(values) = source else {
                    return Err(type_error("List", source, span).into());
                };
                for (index, value) in values.iter().cloned().enumerate() {
                    if self.invoke_predicate(&predicate, value, span)? {
                        let index = i64::try_from(index).map_err(|_| {
                            runtime_error("list index exceeds Spar int range", span)
                        })?;
                        return Ok(Value::Option(Some(Box::new(Value::Int(index)))));
                    }
                }
                Ok(Value::Option(None))
            }
            NativeIntrinsic::DataAny => {
                let predicate = args
                    .get(1)
                    .ok_or_else(|| runtime_error("any requires a predicate callable", span))?
                    .clone();
                let Value::List(values) = source else {
                    return Err(type_error("List", source, span).into());
                };
                for value in values.iter().cloned() {
                    if self.invoke_predicate(&predicate, value, span)? {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
            NativeIntrinsic::DataEvery => {
                let predicate = args
                    .get(1)
                    .ok_or_else(|| runtime_error("every requires a predicate callable", span))?
                    .clone();
                let Value::List(values) = source else {
                    return Err(type_error("List", source, span).into());
                };
                for value in values.iter().cloned() {
                    if !self.invoke_predicate(&predicate, value, span)? {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }
            NativeIntrinsic::DataInspect => Ok(source.clone()),
            NativeIntrinsic::JsonParse
            | NativeIntrinsic::PromiseRace
            | NativeIntrinsic::PromiseTimeout
            | NativeIntrinsic::CoreMapGetOrElse
            | NativeIntrinsic::CoreOptionUnwrapOrElse
            | NativeIntrinsic::CoreOptionMap
            | NativeIntrinsic::CoreOptionFilter
            | NativeIntrinsic::CoreOptionAndThen
            | NativeIntrinsic::CoreOptionOrElse
            | NativeIntrinsic::CoreResultMap
            | NativeIntrinsic::CoreResultMapErr
            | NativeIntrinsic::CoreResultAndThen
            | NativeIntrinsic::CoreResultOrElse
            | NativeIntrinsic::External(_) => unreachable!(),
        }
    }

    fn execute_core_callable_intrinsic(
        &mut self,
        intrinsic: NativeIntrinsic,
        args: &[Value],
        span: &Span,
    ) -> Result<Value, RuntimeFault> {
        let receiver = args
            .first()
            .ok_or_else(|| runtime_error("core method intrinsic requires a receiver", span))?;
        match intrinsic {
            NativeIntrinsic::CoreMapGetOrElse => {
                let Value::Map(entries) = receiver else {
                    return Err(type_error("Map", receiver, span).into());
                };
                let key = args
                    .get(1)
                    .ok_or_else(|| runtime_error("getOrElse requires a key", span))?;
                if let Some(value) = entries.get(key) {
                    return Ok(value.clone());
                }
                let fallback = args
                    .get(2)
                    .ok_or_else(|| runtime_error("getOrElse requires a fallback callable", span))?;
                self.invoke_data_callable(fallback, vec![], span)
            }
            NativeIntrinsic::CoreOptionUnwrapOrElse => {
                let Value::Option(value) = receiver else {
                    return Err(type_error("Option", receiver, span).into());
                };
                if let Some(value) = value {
                    return Ok(value.as_ref().clone());
                }
                let fallback = args.get(1).ok_or_else(|| {
                    runtime_error("unwrapOrElse requires a fallback callable", span)
                })?;
                self.invoke_data_callable(fallback, vec![], span)
            }
            NativeIntrinsic::CoreOptionMap => {
                let Value::Option(value) = receiver else {
                    return Err(type_error("Option", receiver, span).into());
                };
                let Some(value) = value else {
                    return Ok(Value::Option(None));
                };
                let transform = args.get(1).ok_or_else(|| {
                    runtime_error("Option.map requires a transform callable", span)
                })?;
                let mapped =
                    self.invoke_data_callable(transform, vec![value.as_ref().clone()], span)?;
                Ok(Value::Option(Some(Box::new(mapped))))
            }
            NativeIntrinsic::CoreOptionFilter => {
                let Value::Option(value) = receiver else {
                    return Err(type_error("Option", receiver, span).into());
                };
                let Some(value) = value else {
                    return Ok(Value::Option(None));
                };
                let predicate = args.get(1).ok_or_else(|| {
                    runtime_error("Option.filter requires a predicate callable", span)
                })?;
                if self.invoke_predicate(predicate, value.as_ref().clone(), span)? {
                    Ok(Value::Option(Some(Box::new(value.as_ref().clone()))))
                } else {
                    Ok(Value::Option(None))
                }
            }
            NativeIntrinsic::CoreOptionAndThen => {
                let Value::Option(value) = receiver else {
                    return Err(type_error("Option", receiver, span).into());
                };
                let Some(value) = value else {
                    return Ok(Value::Option(None));
                };
                let transform = args.get(1).ok_or_else(|| {
                    runtime_error("Option.andThen requires a transform callable", span)
                })?;
                let mapped =
                    self.invoke_data_callable(transform, vec![value.as_ref().clone()], span)?;
                if matches!(mapped, Value::Option(_)) {
                    Ok(mapped)
                } else {
                    Err(type_error("Option", &mapped, span).into())
                }
            }
            NativeIntrinsic::CoreOptionOrElse => {
                let Value::Option(value) = receiver else {
                    return Err(type_error("Option", receiver, span).into());
                };
                if value.is_some() {
                    return Ok(receiver.clone());
                }
                let fallback = args.get(1).ok_or_else(|| {
                    runtime_error("Option.orElse requires a fallback callable", span)
                })?;
                let mapped = self.invoke_data_callable(fallback, vec![], span)?;
                if matches!(mapped, Value::Option(_)) {
                    Ok(mapped)
                } else {
                    Err(type_error("Option", &mapped, span).into())
                }
            }
            NativeIntrinsic::CoreResultMap => {
                let Value::Result(value) = receiver else {
                    return Err(type_error("Result", receiver, span).into());
                };
                match value {
                    Ok(value) => {
                        let transform = args.get(1).ok_or_else(|| {
                            runtime_error("Result.map requires a transform callable", span)
                        })?;
                        let mapped = self.invoke_data_callable(
                            transform,
                            vec![value.as_ref().clone()],
                            span,
                        )?;
                        Ok(Value::Result(Ok(Box::new(mapped))))
                    }
                    Err(error) => Ok(Value::Result(Err(Box::new(error.as_ref().clone())))),
                }
            }
            NativeIntrinsic::CoreResultMapErr => {
                let Value::Result(value) = receiver else {
                    return Err(type_error("Result", receiver, span).into());
                };
                match value {
                    Ok(value) => Ok(Value::Result(Ok(Box::new(value.as_ref().clone())))),
                    Err(error) => {
                        let transform = args.get(1).ok_or_else(|| {
                            runtime_error("Result.mapErr requires a transform callable", span)
                        })?;
                        let mapped = self.invoke_data_callable(
                            transform,
                            vec![error.as_ref().clone()],
                            span,
                        )?;
                        Ok(Value::Result(Err(Box::new(mapped))))
                    }
                }
            }
            NativeIntrinsic::CoreResultAndThen => {
                let Value::Result(value) = receiver else {
                    return Err(type_error("Result", receiver, span).into());
                };
                match value {
                    Ok(value) => {
                        let transform = args.get(1).ok_or_else(|| {
                            runtime_error("Result.andThen requires a transform callable", span)
                        })?;
                        let mapped = self.invoke_data_callable(
                            transform,
                            vec![value.as_ref().clone()],
                            span,
                        )?;
                        if matches!(mapped, Value::Result(_)) {
                            Ok(mapped)
                        } else {
                            Err(type_error("Result", &mapped, span).into())
                        }
                    }
                    Err(error) => Ok(Value::Result(Err(Box::new(error.as_ref().clone())))),
                }
            }
            NativeIntrinsic::CoreResultOrElse => {
                let Value::Result(value) = receiver else {
                    return Err(type_error("Result", receiver, span).into());
                };
                match value {
                    Ok(value) => Ok(Value::Result(Ok(Box::new(value.as_ref().clone())))),
                    Err(error) => {
                        let fallback = args.get(1).ok_or_else(|| {
                            runtime_error("Result.orElse requires a fallback callable", span)
                        })?;
                        let mapped = self.invoke_data_callable(
                            fallback,
                            vec![error.as_ref().clone()],
                            span,
                        )?;
                        if matches!(mapped, Value::Result(_)) {
                            Ok(mapped)
                        } else {
                            Err(type_error("Result", &mapped, span).into())
                        }
                    }
                }
            }
            _ => unreachable!(),
        }
    }

    fn invoke_data_callable(
        &mut self,
        callable: &Value,
        arguments: Vec<Value>,
        span: &Span,
    ) -> Result<Value, RuntimeFault> {
        match callable {
            Value::Closure(closure) => self.call_closure(closure, arguments, span),
            Value::Function(function) => {
                if self.function_is_async(*function)? {
                    return Err(runtime_error(
                        "structured data callbacks must be synchronous",
                        span,
                    )
                    .into());
                }
                self.call_function(*function, arguments)
            }
            other => Err(type_error("fn", other, span).into()),
        }
    }

    fn invoke_predicate(
        &mut self,
        callable: &Value,
        argument: Value,
        span: &Span,
    ) -> Result<bool, RuntimeFault> {
        match self.invoke_data_callable(callable, vec![argument], span)? {
            Value::Bool(value) => Ok(value),
            other => Err(runtime_error(
                &format!(
                    "structured predicate must return bool, found {}",
                    other.type_name()
                ),
                span,
            )
            .into()),
        }
    }

    fn take_stream_resource(
        &mut self,
        source: &Value,
        span: &Span,
    ) -> Result<StreamResource, RuntimeFault> {
        let Value::Resource(id) = source else {
            return Err(type_error("Stream", source, span).into());
        };
        self.context
            .resources_mut()
            .remove::<StreamResource>(*id)
            .ok_or_else(|| runtime_error("stream handle is no longer valid", span).into())
    }

    fn materialize_interactive_preview(
        &mut self,
        value: Value,
        preview_limit: usize,
    ) -> Result<crate::session::InteractiveRuntimeValue, RuntimeFault> {
        let Value::Resource(id) = value else {
            return Ok(crate::session::InteractiveRuntimeValue {
                value,
                stream_preview: false,
                truncated: false,
                presentation: crate::session::InteractivePresentation::Value,
            });
        };
        if self.context.resources().get::<StreamResource>(id).is_none() {
            return Err(runtime_error(
                "runtime resources other than Stream<T> cannot be displayed interactively",
                &Span::dummy(),
            )
            .into());
        }

        let mut stream = self
            .context
            .resources_mut()
            .remove::<StreamResource>(id)
            .ok_or_else(|| runtime_error("stream handle is no longer valid", &Span::dummy()))?;
        let limit = preview_limit.max(1);
        let mut values = Vec::with_capacity(limit.min(256));
        for _ in 0..limit {
            match self.pull_stream(&mut stream, &Span::dummy())? {
                Some(value) => values.push(value),
                None => break,
            }
        }
        let truncated = if values.len() == limit {
            self.pull_stream(&mut stream, &Span::dummy())?.is_some()
        } else {
            false
        };
        if truncated {
            stream.cancel();
        }

        let materialized = if values.iter().all(|value| matches!(value, Value::Object(_))) {
            match TableValue::from_records(values.clone()) {
                Ok(table) => Value::Table(Shared::from(table)),
                Err(_) => Value::List(Shared::from(values)),
            }
        } else {
            Value::List(Shared::from(values))
        };
        Ok(crate::session::InteractiveRuntimeValue {
            value: materialized,
            stream_preview: true,
            truncated,
            presentation: crate::session::InteractivePresentation::Value,
        })
    }

    fn pull_stream(
        &mut self,
        stream: &mut StreamResource,
        span: &Span,
    ) -> Result<Option<Value>, RuntimeFault> {
        let mut invoke = |callable: &Value, arguments: Vec<Value>| {
            self.invoke_data_callable(callable, arguments, span)
                .map_err(RuntimeFault::into_error)
        };
        stream.next_with(&mut invoke).map_err(RuntimeFault::from)
    }

    fn materialize_data_sequence(
        &mut self,
        source: &Value,
        span: &Span,
    ) -> Result<(DataSequenceShape, Vec<Value>), RuntimeFault> {
        match source {
            Value::List(values) => Ok((DataSequenceShape::List, (values.clone()).into_inner())),
            Value::Table(table) => Ok((
                DataSequenceShape::Table(table.schema().clone()),
                table.rows().to_vec(),
            )),
            Value::Resource(_) => {
                let mut stream = self.take_stream_resource(source, span)?;
                let element_type = stream.element_type().clone();
                let mut values = Vec::new();
                while let Some(value) = self.pull_stream(&mut stream, span)? {
                    values.push(value);
                }
                Ok((DataSequenceShape::Stream(element_type), values))
            }
            other => Err(evaluation_error(
                &format!(
                    "this operation needs a list, table or stream, but got {}",
                    sequence_kind(other)
                ),
                span,
            )
            .into()),
        }
    }

    fn rebuild_data_sequence(
        &mut self,
        shape: DataSequenceShape,
        values: Vec<Value>,
        _span: &Span,
    ) -> Result<Value, RuntimeFault> {
        match shape {
            DataSequenceShape::List => Ok(Value::List(Shared::from(values))),
            DataSequenceShape::Table(schema) => Ok(Value::Table(Shared::from(
                TableValue::with_schema(values, schema),
            ))),
            DataSequenceShape::Stream(element_type) => Ok(Value::Resource(
                self.context
                    .insert_stream(StreamResource::from_values(element_type, values)),
            )),
        }
    }

    fn data_count_arg(
        &self,
        args: &[Value],
        index: usize,
        span: &Span,
    ) -> Result<usize, RuntimeFault> {
        match args.get(index) {
            Some(Value::Int(value)) if *value >= 0 => Ok(*value as usize),
            Some(Value::Int(_)) => Err(runtime_error("count cannot be negative", span).into()),
            Some(other) => Err(type_error("int", other, span).into()),
            None => Err(runtime_error("missing count argument", span).into()),
        }
    }

    fn data_index(&self, value: &Value, span: &Span) -> Result<usize, RuntimeFault> {
        match value {
            Value::Int(index) if *index >= 0 => Ok(*index as usize),
            Value::Int(_) => Err(runtime_error("index cannot be negative", span).into()),
            other => Err(type_error("int", other, span).into()),
        }
    }

    fn data_fields_arg(
        &self,
        args: &[Value],
        index: usize,
        span: &Span,
    ) -> Result<Vec<String>, RuntimeFault> {
        let Some(value) = args.get(index) else {
            return Err(runtime_error("missing fields argument", span).into());
        };
        let Value::List(values) = value else {
            return Err(type_error("List<str>", value, span).into());
        };
        values
            .iter()
            .map(|value| match value {
                Value::String(field) => Ok(field.clone()),
                other => Err(type_error("str", other, span).into()),
            })
            .collect()
    }

    fn ensure_data_comparable(
        &self,
        value: &Value,
        label: &str,
        span: &Span,
    ) -> Result<(), RuntimeFault> {
        if value.is_data_comparable() {
            Ok(())
        } else {
            Err(runtime_error(&format!("{label} cannot be {}", value.type_name()), span).into())
        }
    }

    fn project_data_record(
        &self,
        value: Value,
        fields: &[String],
        span: &Span,
    ) -> Result<Value, RuntimeFault> {
        let Value::Object(values) = value else {
            return Err(runtime_error("select expects Record rows", span).into());
        };
        let mut selected = indexmap::IndexMap::new();
        for field in fields {
            let value = values
                .get(field)
                .ok_or_else(|| runtime_error(&format!("record has no field '{field}'"), span))?;
            selected.insert(field.clone(), value.clone());
        }
        Ok(Value::Object(Shared::from(selected)))
    }

    fn execute_command_substitution(
        &mut self,
        shell: &CompiledShellExpr,
        frame: &mut Frame,
        module: crate::compiled::ModuleId,
    ) -> Result<String, RuntimeFault> {
        let plan = self.eval_shell_plan(shell, frame, module)?;
        if plan.steps.is_empty() {
            return Err(evaluation_error("empty command substitution", &shell.span).into());
        }
        let options = spar_process::ExecutionOptions {
            capture_stdout: true,
            capture_stderr: false,
            environment: Some(self.context.environment_pairs()),
        };
        let mut success = true;
        let mut exit_code = 0;
        let mut captured = Vec::new();
        let mut executed = false;

        for (join, source_step) in &plan.steps {
            let should_run = match join {
                spar_command::Join::Always => true,
                spar_command::Join::OnSuccess => success,
                spar_command::Join::OnFailure => !success,
            };
            if !should_run {
                continue;
            }

            let mut step = source_step.clone();
            self.apply_shell_cwd_to_step(&mut step, &shell.span)?;
            if matches!(&step, spar_command::Step::Command(command) if command.background)
                || matches!(&step, spar_command::Step::Pipeline(pipeline)
                    if pipeline.commands.iter().any(|command| command.background))
            {
                return Err(evaluation_error(
                    "background commands are not allowed inside command substitution",
                    &shell.span,
                )
                .into());
            }
            if matches!(&step, spar_command::Step::Command(command) if command.program == "cd")
                || matches!(&step, spar_command::Step::Pipeline(pipeline)
                    if pipeline.commands.iter().any(|command| command.program == "cd"))
            {
                return Err(evaluation_error(
                    "'cd' inside command substitution is not supported; use a normal shell block before substitution",
                    &shell.span,
                )
                .into());
            }

            let command_output = match &step {
                spar_command::Step::Command(command) => {
                    spar_process::run_command(command, &options)
                }
                spar_command::Step::Pipeline(pipeline) => {
                    spar_process::run_pipeline(pipeline, &options)
                }
            }
            .map_err(|error| {
                evaluation_error(
                    &format!("command substitution failed to start: {error}"),
                    &shell.span,
                )
            })?;
            success = command_output.status.success;
            exit_code = command_output
                .status
                .code
                .unwrap_or(if success { 0 } else { 1 });
            captured = command_output.stdout.unwrap_or_default();
            executed = true;
        }

        if !executed {
            return Err(evaluation_error("empty command substitution", &shell.span).into());
        }
        if !success {
            return Err(evaluation_error(
                &format!("command substitution exited with status {exit_code}"),
                &shell.span,
            )
            .into());
        }
        let mut text = String::from_utf8(captured).map_err(|_| {
            evaluation_error(
                "command substitution output is not valid UTF-8",
                &shell.span,
            )
        })?;
        while text.ends_with('\n') || text.ends_with('\r') {
            text.pop();
        }
        Ok(text)
    }

    fn ensure_shell_cwd(&mut self, span: &Span) -> Result<std::path::PathBuf, RuntimeFault> {
        if let Some(cwd) = &self.shell_cwd {
            return Ok(cwd.clone());
        }
        let cwd = self.context.cwd().to_path_buf();
        if cwd.as_os_str().is_empty() {
            return Err(runtime_error("runtime working directory is empty", span).into());
        }
        self.shell_cwd = Some(cwd.clone());
        Ok(cwd)
    }

    fn apply_shell_cwd_to_step(
        &mut self,
        step: &mut spar_command::Step,
        span: &Span,
    ) -> Result<(), RuntimeFault> {
        let cwd = self.ensure_shell_cwd(span)?;
        let cwd = spar_command::WorkingDirectory::Path(cwd.to_string_lossy().into_owned());
        match step {
            spar_command::Step::Command(command) => {
                if command.cwd.is_none() {
                    command.cwd = Some(cwd);
                }
            }
            spar_command::Step::Pipeline(pipeline) => {
                for command in &mut pipeline.commands {
                    if command.cwd.is_none() {
                        command.cwd = Some(cwd.clone());
                    }
                }
            }
        }
        Ok(())
    }

    fn execute_cd_builtin(
        &mut self,
        command: &spar_command::CommandPlan,
        span: &Span,
    ) -> Result<crate::evaluator::ShellPlanOutcome, RuntimeFault> {
        if command.background {
            return Err(runtime_error("'cd' cannot run in the background", span).into());
        }
        if command.args.len() > 1 {
            return Err(runtime_error("'cd' accepts zero or one path argument", span).into());
        }

        let current = self.ensure_shell_cwd(span)?;
        let requested = match command.args.first() {
            Some(path) => std::path::PathBuf::from(path),
            None => self
                .context
                .env_get("HOME")
                .map(std::path::PathBuf::from)
                .ok_or_else(|| runtime_error("'cd' requires HOME when no path is given", span))?,
        };
        let candidate = if requested.is_absolute() {
            requested
        } else {
            current.join(requested)
        };
        let metadata = std::fs::metadata(&candidate).map_err(|error| {
            runtime_error(&format!("cd: '{}': {error}", candidate.display()), span)
        })?;
        if !metadata.is_dir() {
            return Err(runtime_error(
                &format!("cd: '{}' is not a directory", candidate.display()),
                span,
            )
            .into());
        }
        let resolved = std::fs::canonicalize(&candidate).map_err(|error| {
            runtime_error(
                &format!("cd: could not resolve '{}': {error}", candidate.display()),
                span,
            )
        })?;
        self.context.set_cwd(resolved.clone());
        self.shell_cwd = Some(resolved);
        Ok(crate::evaluator::ShellPlanOutcome {
            success: true,
            exit_code: 0,
            signal: None,
            pid: 0,
            pipeline: vec![],
        })
    }

    /// Reads a line of stdin into a shell-local variable, at *build* time
    /// (called from `eval_shell_command`, not `execute_native_shell_plan`).
    /// `eval_shell_plan` resolves every step's words for the whole block in
    /// one upfront pass before any step executes, so a later step's `$name`
    /// word is only able to see this variable if the read (and its
    /// `context.env_set`) happens during that same build pass — waiting for
    /// this step's own execution turn would be too late. One consequence:
    /// `read` runs unconditionally as its build is reached, regardless of a
    /// preceding `&&`/`||` join condition (join gating is checked only at
    /// execution time).
    ///
    /// Returns a no-op `CommandPlan` (`true`, no args) standing in for this
    /// step, so `execute_native_shell_plan` still spawns something (cheaply
    /// succeeding) at this position in the plan.
    fn eval_read_builtin(
        &mut self,
        args: &[String],
        background: bool,
        span: &Span,
    ) -> Result<spar_command::CommandPlan, RuntimeFault> {
        if background {
            return Err(runtime_error("'read' cannot run in the background", span).into());
        }
        if args.len() != 2 {
            return Err(runtime_error(
                "'read' requires a type and a variable name: read <int|float|bool|str> <name>",
                span,
            )
            .into());
        }
        let type_token = args[0].as_str();
        let name = &args[1];
        if !matches!(type_token, "int" | "float" | "bool" | "str") {
            return Err(runtime_error(
                &format!("'read': unknown type '{type_token}' (expected int, float, bool, or str)"),
                span,
            )
            .into());
        }

        let bytes = self
            .context
            .read_stdin_line()
            .map_err(|error| runtime_error(&format!("'read': stdin read failed: {error}"), span))?;
        let text = String::from_utf8(bytes).map_err(|_| {
            runtime_error(
                "'read': stdin contains bytes that are not valid UTF-8",
                span,
            )
        })?;
        let value = text.trim_end_matches(['\r', '\n']).to_string();

        match type_token {
            "int" => {
                value.parse::<i64>().map_err(|_| {
                    runtime_error(&format!("'read': '{value}' is not a valid int"), span)
                })?;
            }
            "float" => {
                value.parse::<f64>().map_err(|_| {
                    runtime_error(&format!("'read': '{value}' is not a valid float"), span)
                })?;
            }
            "bool" => {
                if value != "true" && value != "false" {
                    return Err(runtime_error(
                        &format!("'read': '{value}' is not a valid bool (expected true or false)"),
                        span,
                    )
                    .into());
                }
            }
            _ => {}
        }

        self.context.env_set(name.clone(), value);
        Ok(spar_command::CommandPlan {
            program: "true".to_string(),
            args: vec![],
            env: vec![],
            cwd: None,
            stdin: None,
            stdout: None,
            stderr: None,
            redirections: vec![],
            background: false,
        })
    }

    fn execute_native_shell_plan(
        &mut self,
        plan: &spar_command::ShellPlan,
        span: &Span,
    ) -> Result<crate::evaluator::ShellPlanOutcome, RuntimeFault> {
        let mut outcome = crate::evaluator::ShellPlanOutcome {
            success: true,
            exit_code: 0,
            signal: None,
            pid: 0,
            pipeline: vec![],
        };
        let options = spar_process::ExecutionOptions {
            environment: Some(self.context.environment_pairs()),
            ..spar_process::ExecutionOptions::default()
        };
        for (join, source_step) in &plan.steps {
            let should_run = match join {
                spar_command::Join::Always => true,
                spar_command::Join::OnSuccess => outcome.success,
                spar_command::Join::OnFailure => !outcome.success,
            };
            if !should_run {
                continue;
            }

            let mut step = source_step.clone();
            self.apply_shell_cwd_to_step(&mut step, span)?;
            if let spar_command::Step::Pipeline(pipeline) = &step {
                if pipeline
                    .commands
                    .iter()
                    .any(|command| command.program == "cd")
                {
                    return Err(runtime_error(
                        "'cd' cannot be used as a pipeline stage; run it as a standalone command",
                        span,
                    )
                    .into());
                }
            }

            let output = match &step {
                spar_command::Step::Command(command) if command.program == "exit" => {
                    let code = command
                        .args
                        .first()
                        .map(|value| value.parse::<i32>())
                        .transpose()
                        .map_err(|_| runtime_error("exit status must be an integer", span))?
                        .unwrap_or(0);
                    outcome = crate::evaluator::ShellPlanOutcome {
                        success: code == 0,
                        exit_code: code,
                        signal: None,
                        pid: 0,
                        pipeline: vec![],
                    };
                    self.shell_outcome = Some(outcome.clone());
                    self.shell_exit = true;
                    break;
                }
                spar_command::Step::Command(command) if command.program == "cd" => {
                    outcome = self.execute_cd_builtin(command, span)?;
                    self.shell_outcome = Some(outcome.clone());
                    continue;
                }
                spar_command::Step::Command(command) if command.background => {
                    let job = spar_process::spawn_background_with_options(command, &options)
                        .map_err(|error| {
                            runtime_error(
                                &format!("could not start background command: {error}"),
                                span,
                            )
                        })?;
                    let pid = job.pid();
                    let id = self.jobs.len() + 1;
                    self.last_job = Some(Value::Object(Shared::from(indexmap::IndexMap::from([
                        ("id".into(), Value::Int(id as i64)),
                        ("pid".into(), Value::Int(i64::from(pid))),
                        ("processGroup".into(), Value::Int(i64::from(pid))),
                        ("state".into(), Value::String("running".into())),
                    ]))));
                    self.jobs.push(job);
                    outcome = crate::evaluator::ShellPlanOutcome {
                        success: true,
                        exit_code: 0,
                        signal: None,
                        pid,
                        pipeline: vec![],
                    };
                    self.shell_outcome = Some(outcome.clone());
                    continue;
                }
                spar_command::Step::Pipeline(pipeline)
                    if pipeline
                        .commands
                        .last()
                        .is_some_and(|command| command.background) =>
                {
                    let job =
                        spar_process::spawn_pipeline_background_with_options(pipeline, &options)
                            .map_err(|error| {
                                runtime_error(
                                    &format!("could not start background pipeline: {error}"),
                                    span,
                                )
                            })?;
                    let pid = job.pid();
                    let id = self.jobs.len() + 1;
                    self.last_job = Some(Value::Object(Shared::from(indexmap::IndexMap::from([
                        ("id".into(), Value::Int(id as i64)),
                        ("pid".into(), Value::Int(i64::from(pid))),
                        ("processGroup".into(), Value::Int(i64::from(pid))),
                        ("state".into(), Value::String("running".into())),
                    ]))));
                    self.jobs.push(job);
                    outcome = crate::evaluator::ShellPlanOutcome {
                        success: true,
                        exit_code: 0,
                        signal: None,
                        pid,
                        pipeline: vec![],
                    };
                    self.shell_outcome = Some(outcome.clone());
                    continue;
                }
                spar_command::Step::Command(command)
                    if command.program == "disown" && command.args.is_empty() =>
                {
                    if let Some(job) = self.jobs.pop() {
                        job.detach();
                    }
                    outcome = crate::evaluator::ShellPlanOutcome {
                        success: true,
                        exit_code: 0,
                        signal: None,
                        pid: 0,
                        pipeline: vec![],
                    };
                    self.shell_outcome = Some(outcome.clone());
                    continue;
                }
                spar_command::Step::Command(command) => {
                    spar_process::run_command(command, &options).map_err(|error| {
                        if error.kind() == std::io::ErrorKind::NotFound
                            && crate::evaluator::is_shell_only_builtin(&command.program)
                        {
                            std::io::Error::new(
                                std::io::ErrorKind::NotFound,
                                format!(
                                "`{}` is a Spar shell builtin, not a program on PATH -- it only \
                                     runs inside the Spar shell. Install sparsh to run this task.",
                                command.program
                            ),
                            )
                        } else {
                            error
                        }
                    })
                }
                spar_command::Step::Pipeline(pipeline) => {
                    spar_process::run_pipeline(pipeline, &options)
                }
            }
            .map_err(|error| {
                runtime_error(&format!("could not execute native command: {error}"), span)
            })?;
            let status = output
                .pipeline_status
                .unwrap_or(spar_process::PipelineStatus {
                    code: output.status.code.unwrap_or(1),
                    success: output.status.success,
                    processes: vec![],
                });
            let last = status.processes.last();
            outcome = crate::evaluator::ShellPlanOutcome {
                success: status.success,
                exit_code: status.code,
                signal: last.and_then(|process| process.signal),
                pid: last.map_or(0, |process| process.pid),
                pipeline: status.processes,
            };
            self.shell_outcome = Some(outcome.clone());
        }
        Ok(outcome)
    }

    fn execute_mixed_shell(
        &mut self,
        shell: &MixedShellValue,
    ) -> Result<crate::evaluator::ShellPlanOutcome, RuntimeFault> {
        let outermost = self.shell_depth == 0;
        let prior_outcome = self.shell_outcome.take();
        self.shell_outcome = None;
        let prior_cwd = if outermost {
            self.shell_cwd.take()
        } else {
            None
        };
        let prior_exit = if outermost {
            std::mem::replace(&mut self.shell_exit, false)
        } else {
            false
        };

        self.shell_depth += 1;
        let mut frame = shell.captured.clone();
        let execution = (|| {
            let mut outcome = crate::evaluator::ShellPlanOutcome {
                success: true,
                exit_code: 0,
                signal: None,
                pid: 0,
                pipeline: vec![],
            };
            for (join, step) in &shell.plan.steps {
                let should_run = match join {
                    crate::ast::ShellJoin::Always => true,
                    crate::ast::ShellJoin::OnSuccess => outcome.success,
                    crate::ast::ShellJoin::OnFailure => !outcome.success,
                };
                if !should_run {
                    continue;
                }
                outcome = match step {
                    CompiledShellStep::MixedPipeline(pipeline) => {
                        self.execute_mixed_pipeline(pipeline, &mut frame, shell.module)?
                    }
                    CompiledShellStep::Command(_) | CompiledShellStep::Pipeline(_) => {
                        let single = CompiledShellExpr {
                            steps: vec![(crate::ast::ShellJoin::Always, step.clone())],
                            span: shell.span.clone(),
                        };
                        let plan = self.eval_shell_plan(&single, &mut frame, shell.module)?;
                        self.execute_native_shell_plan(&plan, &shell.span)?
                    }
                };
                if self.shell_exit {
                    break;
                }
            }
            Ok(outcome)
        })();
        self.shell_depth -= 1;
        self.shell_outcome = prior_outcome;
        if outermost {
            self.shell_cwd = prior_cwd;
            self.shell_exit = prior_exit;
        }
        execution
    }

    fn execute_mixed_pipeline(
        &mut self,
        pipeline: &CompiledShellMixedPipeline,
        frame: &mut Frame,
        module: crate::compiled::ModuleId,
    ) -> Result<crate::evaluator::ShellPlanOutcome, RuntimeFault> {
        let registry = crate::structured_codec::StructuredFormatRegistry::builtin();
        let input_registry = crate::structured_input::StructuredInputRegistry::builtin();
        let resolved = input_registry
            .resolve(
                pipeline.decoder.namespace,
                &pipeline.decoder.name,
                &pipeline.decoder.span,
            )
            .map_err(RuntimeFault::from)?;

        let mut parse_options = scoc::ParseOptions::new();
        let mut streaming_mode = resolved
            .forced_streaming
            .unwrap_or(crate::structured_input::StreamingMode::Auto);
        for arg in &pipeline.decoder.args {
            let value = self.eval_expression(&arg.value, frame, module)?;
            if arg.name == "streaming" {
                let Value::Bool(enabled) = value else {
                    return Err(runtime_error(
                        "decoder option `streaming` must evaluate to bool",
                        &arg.span,
                    )
                    .into());
                };
                if resolved.forced_streaming
                    == Some(crate::structured_input::StreamingMode::Enabled)
                    && !enabled
                {
                    return Err(runtime_error(
                        &format!(
                            "decoder `{}` is a streaming compatibility alias and cannot set `streaming: false`",
                            pipeline.decoder.name
                        ),
                        &arg.span,
                    )
                    .into());
                }
                streaming_mode = if enabled {
                    crate::structured_input::StreamingMode::Enabled
                } else {
                    crate::structured_input::StreamingMode::Disabled
                };
                continue;
            }
            let option =
                runtime_value_to_scoc_option(value, &arg.span).map_err(RuntimeFault::from)?;
            parse_options.insert(arg.name.clone(), option);
        }

        let raw_output = parse_options.bool("raw").unwrap_or(false);
        let use_scoc_streaming = match resolved.descriptor.kind {
            crate::structured_input::DecoderKind::Scoc => match streaming_mode {
                crate::structured_input::StreamingMode::Auto => {
                    resolved.descriptor.capabilities.streaming
                }
                crate::structured_input::StreamingMode::Enabled => {
                    if !resolved.descriptor.capabilities.streaming {
                        return Err(runtime_error(
                            &format!(
                                "SCOC parser `{}` does not support streaming",
                                resolved.canonical_name
                            ),
                            &pipeline.decoder.span,
                        )
                        .into());
                    }
                    true
                }
                crate::structured_input::StreamingMode::Disabled => false,
            },
            crate::structured_input::DecoderKind::Codec => false,
            crate::structured_input::DecoderKind::Custom => {
                return Err(runtime_error(
                    "custom decoders are reserved but not implemented in this milestone",
                    &pipeline.decoder.span,
                )
                .into());
            }
        };

        let decoder = match resolved.descriptor.kind {
            crate::structured_input::DecoderKind::Codec => MixedDecoderState::Codec(
                registry
                    .parser(&resolved.canonical_name)
                    .map_err(RuntimeFault::from)?,
            ),
            crate::structured_input::DecoderKind::Scoc if use_scoc_streaming => {
                let parser = scoc::stream_parser(&resolved.canonical_name, &parse_options)
                    .map_err(|error| {
                        crate::structured_input::scoc_error(error, &pipeline.decoder.span)
                    })?;
                MixedDecoderState::ScocStreaming {
                    parser,
                    span: pipeline.decoder.span.clone(),
                }
            }
            crate::structured_input::DecoderKind::Scoc => {
                let descriptor = scoc::parser(&resolved.canonical_name).ok_or_else(|| {
                    runtime_error(
                        &format!("unknown SCOC parser `{}`", resolved.canonical_name),
                        &pipeline.decoder.span,
                    )
                })?;
                parse_options
                    .validate_for(descriptor.name, descriptor.options)
                    .map_err(|error| {
                        crate::structured_input::scoc_error(error, &pipeline.decoder.span)
                    })?;
                MixedDecoderState::ScocBuffered {
                    parser_name: resolved.canonical_name.clone(),
                    options: parse_options,
                    output_shape: descriptor.output.shape(raw_output),
                    bytes: Vec::new(),
                    span: pipeline.decoder.span.clone(),
                }
            }
            crate::structured_input::DecoderKind::Custom => unreachable!(
                "custom decoder resolution returned successfully before implementation"
            ),
        };

        // Without `to`, non-terminal output falls back to JSON Lines.
        let encoder_format = pipeline.encoder_format.as_deref().unwrap_or("jsonl");
        let serializer = registry
            .serializer(encoder_format)
            .map_err(RuntimeFault::from)?;

        let mut input_commands = Vec::with_capacity(pipeline.input.len());
        for command in &pipeline.input {
            input_commands.push(self.eval_shell_command(command, frame, module)?);
        }
        let mut input_step = if input_commands.len() == 1 {
            spar_command::Step::Command(input_commands.pop().expect("one input command"))
        } else {
            spar_command::Step::Pipeline(spar_command::PipelinePlan {
                commands: input_commands,
            })
        };
        self.apply_shell_cwd_to_step(&mut input_step, &pipeline.span)?;

        let stream_options = spar_process::StreamingOptions {
            environment: Some(self.context.environment_pairs()),
            ..spar_process::StreamingOptions::default()
        };
        let input_stream = match input_step {
            spar_command::Step::Command(command) => {
                spar_process::stream_command(&command, &stream_options)
            }
            spar_command::Step::Pipeline(commands) => {
                spar_process::stream_pipeline(&commands, &stream_options)
            }
        }
        .map_err(|error| {
            runtime_error(
                &format!("could not start mixed pipeline input: {error}"),
                &pipeline.span,
            )
        })?;

        let element_type =
            mixed_decoder_element_type(&resolved.descriptor, raw_output, use_scoc_streaming);
        let shared = Arc::new(Mutex::new(MixedInputState {
            stream: Some(input_stream),
            decoder: Some(decoder),
            pending: VecDeque::new(),
            stderr: Vec::new(),
            status: None,
            finished: false,
            cancelled: false,
        }));
        let pull_state = Arc::clone(&shared);
        let cancel_state = Arc::clone(&shared);
        let stream = StreamResource::with_cancel(
            element_type,
            move || mixed_input_next(&pull_state),
            move || mixed_input_cancel(&cancel_state),
        );
        let mut current = Value::Resource(self.context.insert_stream(stream));

        for stage in &pipeline.stages {
            frame.write(stage.input_slot, current, &stage.span)?;
            current = self.eval_expression(&stage.expression, frame, module)?;
        }

        let downstream = if pipeline.output.is_empty()
            && pipeline.encoder_redirect.is_none()
            && self.context.capture_mixed()
        {
            // Nothing consumes the bytes and a terminal will render the value:
            // hand the whole structured result back instead of serializing it.
            let mut value = self.collect_mixed_value(current)?;
            // Document-like decoders yield one structured value. Without a
            // `to`, show that value itself rather than a one-element stream
            // materialization. Table and streaming decoders keep collection
            // semantics.
            let document_decoder = match resolved.descriptor.kind {
                crate::structured_input::DecoderKind::Codec => registry
                    .descriptor(&resolved.canonical_name)
                    .is_some_and(|descriptor| {
                        descriptor.mode() == crate::structured_codec::CodecMode::Document
                    }),
                crate::structured_input::DecoderKind::Scoc => {
                    !use_scoc_streaming
                        && resolved.descriptor.output_shape(raw_output)
                            != crate::structured_input::DecoderOutputShape::Table
                }
                crate::structured_input::DecoderKind::Custom => false,
            };
            if pipeline.encoder_format.is_none() && document_decoder {
                value = match value {
                    Value::List(mut items) if items.len() == 1 => items.remove(0),
                    Value::Table(table) if table.len() == 1 => table.rows()[0].clone(),
                    other => other,
                };
            }
            let format = pipeline
                .encoder_format
                .as_deref()
                .and_then(|name| registry.descriptor(name))
                .map(|descriptor| descriptor.name());
            self.context
                .set_mixed_capture(crate::runtime::context::MixedCapture { value, format });
            None
        } else if pipeline.output.is_empty() {
            let mut bytes = Vec::new();
            self.serialize_mixed_value(current, serializer, &mut bytes, &pipeline.encoder_span)?;
            match &pipeline.encoder_redirect {
                Some(redirect) => {
                    let path = self.eval_shell_word(&redirect.target, frame, module)?;
                    let path = self.ensure_shell_cwd(&pipeline.encoder_span)?.join(path);
                    let mut options = std::fs::OpenOptions::new();
                    options.create(true).write(true);
                    match redirect.mode {
                        spar_command::RedirectMode::Append => options.append(true),
                        spar_command::RedirectMode::Truncate => options.truncate(true),
                    };
                    options
                        .open(&path)
                        .and_then(|mut file| Write::write_all(&mut file, &bytes))
                        .map_err(|error| {
                            runtime_error(
                                &format!("could not write {}: {error}", path.display()),
                                &pipeline.encoder_span,
                            )
                        })?;
                }
                None => {
                    self.context.write_stdout(&bytes).map_err(|error| {
                        runtime_error(
                            &format!("could not write mixed pipeline output: {error}"),
                            &pipeline.encoder_span,
                        )
                    })?;
                }
            }
            None
        } else {
            let mut output_commands = Vec::with_capacity(pipeline.output.len());
            for command in &pipeline.output {
                output_commands.push(self.eval_shell_command(command, frame, module)?);
            }
            let mut output_step = if output_commands.len() == 1 {
                spar_command::Step::Command(output_commands.pop().expect("one output command"))
            } else {
                spar_command::Step::Pipeline(spar_command::PipelinePlan {
                    commands: output_commands,
                })
            };
            self.apply_shell_cwd_to_step(&mut output_step, &pipeline.span)?;
            let mut process = match output_step {
                spar_command::Step::Command(command) => {
                    spar_process::stream_command_with_stdin(&command, &stream_options)
                }
                spar_command::Step::Pipeline(commands) => {
                    spar_process::stream_pipeline_with_stdin(&commands, &stream_options)
                }
            }
            .map_err(|error| {
                runtime_error(
                    &format!("could not start mixed pipeline output: {error}"),
                    &pipeline.span,
                )
            })?;
            let mut stdin = process.take_stdin().ok_or_else(|| {
                runtime_error(
                    "mixed pipeline output process did not expose writable stdin",
                    &pipeline.span,
                )
            })?;
            let drain = std::thread::spawn(move || process.collect());
            self.serialize_mixed_value(current, serializer, &mut stdin, &pipeline.encoder_span)?;
            drop(stdin);
            let result = drain
                .join()
                .map_err(|_| {
                    runtime_error(
                        "mixed pipeline output reader thread panicked",
                        &pipeline.span,
                    )
                })?
                .map_err(|error| {
                    runtime_error(
                        &format!("could not collect mixed pipeline output: {error}"),
                        &pipeline.span,
                    )
                })?;
            if !result.stdout.is_empty() {
                self.context.write_stdout(&result.stdout).map_err(|error| {
                    runtime_error(
                        &format!("could not write downstream stdout: {error}"),
                        &pipeline.span,
                    )
                })?;
            }
            if !result.stderr.is_empty() {
                self.context.write_stderr(&result.stderr).map_err(|error| {
                    runtime_error(
                        &format!("could not write downstream stderr: {error}"),
                        &pipeline.span,
                    )
                })?;
            }
            Some(result.status)
        };

        // If a structured stage short-circuited without pulling the producer to
        // EOF, stop the upstream process group now. This is an intentional
        // cancellation, so its SIGPIPE/termination status is not surfaced as a
        // user-facing failure.
        mixed_input_cancel(&shared);
        let (upstream_status, upstream_cancelled, upstream_stderr) = {
            let state = shared.lock().map_err(|_| mixed_input_lock_error())?;
            (state.status.clone(), state.cancelled, state.stderr.clone())
        };
        if !upstream_stderr.is_empty() {
            self.context
                .write_stderr(&upstream_stderr)
                .map_err(|error| {
                    runtime_error(
                        &format!("could not write upstream stderr: {error}"),
                        &pipeline.span,
                    )
                })?;
        }

        let mut processes = Vec::new();
        let upstream_success = match &upstream_status {
            Some(status) => {
                processes.extend(status.processes.clone());
                upstream_cancelled || status.success
            }
            None => true,
        };
        let mut success = upstream_success;
        let mut exit_code = upstream_status
            .as_ref()
            .filter(|status| !upstream_cancelled && !status.success)
            .map_or(0, |status| status.code);
        if let Some(status) = downstream {
            processes.extend(status.processes.clone());
            if success && !status.success {
                success = false;
                exit_code = status.code;
            }
        }
        let last = processes.last();
        Ok(crate::evaluator::ShellPlanOutcome {
            success,
            exit_code,
            signal: last.and_then(|process| process.signal),
            pid: last.map_or(0, |process| process.pid),
            pipeline: processes,
        })
    }

    /// Materializes the final value of a mixed pipeline: a stream is drained
    /// completely (a table when every element is a record), anything else is
    /// already a value.
    fn collect_mixed_value(&mut self, value: Value) -> Result<Value, RuntimeFault> {
        Ok(self
            .materialize_interactive_preview(value, usize::MAX)?
            .value)
    }

    fn serialize_mixed_value<W: Write>(
        &mut self,
        value: Value,
        mut serializer: crate::structured_codec::StructuredSerializer,
        writer: &mut W,
        span: &Span,
    ) -> Result<(), RuntimeFault> {
        // `false` means the reader hung up (`... | head -n 2`). That is how a
        // Unix pipeline normally ends early, so stop quietly instead of
        // failing; the caller still cancels any upstream producer.
        let mut write_chunk = |bytes: Vec<u8>| -> Result<bool, RuntimeFault> {
            if bytes.is_empty() {
                return Ok(true);
            }
            match writer.write_all(&bytes) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(false),
                Err(error) => Err(runtime_error(
                    &format!("could not write structured bytes: {error}"),
                    span,
                )
                .into()),
            }
        };

        let mut open = true;
        match value {
            Value::Resource(id) => {
                let mut stream = self
                    .context
                    .resources_mut()
                    .remove::<StreamResource>(id)
                    .ok_or_else(|| runtime_error("stream handle is no longer valid", span))?;
                while let Some(value) = self.pull_stream(&mut stream, span)? {
                    if !write_chunk(serializer.push(&value).map_err(RuntimeFault::from)?)? {
                        open = false;
                        break;
                    }
                }
            }
            Value::List(values) => {
                for value in values {
                    if !write_chunk(serializer.push(&value).map_err(RuntimeFault::from)?)? {
                        open = false;
                        break;
                    }
                }
            }
            Value::Table(table) => {
                for value in table.rows() {
                    if !write_chunk(serializer.push(value).map_err(RuntimeFault::from)?)? {
                        open = false;
                        break;
                    }
                }
            }
            value => {
                open = write_chunk(serializer.push(&value).map_err(RuntimeFault::from)?)?;
            }
        }
        if open {
            write_chunk(serializer.finish().map_err(RuntimeFault::from)?)?;
        }
        Ok(())
    }

    fn execute_shell_program(
        &mut self,
        program: &ShellProgramValue,
    ) -> Result<crate::evaluator::ShellPlanOutcome, RuntimeFault> {
        let outermost = self.shell_depth == 0;
        let prior_outcome = self.shell_outcome.take();
        self.shell_outcome = None;
        let prior_cwd = if outermost {
            self.shell_cwd.take()
        } else {
            None
        };
        let prior_exit = if outermost {
            std::mem::replace(&mut self.shell_exit, false)
        } else {
            false
        };

        self.shell_depth += 1;
        let mut frame = program.captured.clone();
        let execution = self.execute_statements(&program.body, &mut frame, program.module);
        self.shell_depth -= 1;
        let outcome = self
            .shell_outcome
            .take()
            .unwrap_or(crate::evaluator::ShellPlanOutcome {
                success: true,
                exit_code: 0,
                signal: None,
                pid: 0,
                pipeline: vec![],
            });
        self.shell_outcome = prior_outcome;
        if outermost {
            self.shell_cwd = prior_cwd;
            self.shell_exit = prior_exit;
        }

        match execution? {
            RuntimeFlow::Normal | RuntimeFlow::Return(_) => Ok(outcome),
            RuntimeFlow::Break | RuntimeFlow::Continue => {
                Err(runtime_error("loop control escaped a shell program", &program.span).into())
            }
        }
    }

    fn eval_shell_plan(
        &mut self,
        shell: &CompiledShellExpr,
        frame: &mut Frame,
        module: crate::compiled::ModuleId,
    ) -> Result<spar_command::ShellPlan, RuntimeFault> {
        let mut steps = Vec::with_capacity(shell.steps.len());
        for (join, step) in &shell.steps {
            let join = match join {
                crate::ast::ShellJoin::Always => spar_command::Join::Always,
                crate::ast::ShellJoin::OnSuccess => spar_command::Join::OnSuccess,
                crate::ast::ShellJoin::OnFailure => spar_command::Join::OnFailure,
            };
            let step = match step {
                CompiledShellStep::Command(command) => {
                    spar_command::Step::Command(self.eval_shell_command(command, frame, module)?)
                }
                CompiledShellStep::Pipeline(commands) => {
                    let mut lowered = Vec::with_capacity(commands.len());
                    for command in commands {
                        lowered.push(self.eval_shell_command(command, frame, module)?);
                    }
                    spar_command::Step::Pipeline(spar_command::PipelinePlan { commands: lowered })
                }
                CompiledShellStep::MixedPipeline(_) => {
                    return Err(runtime_error(
                        "mixed structured pipelines cannot be lowered to a byte-only shell plan",
                        &shell.span,
                    )
                    .into())
                }
            };
            steps.push((join, step));
        }
        Ok(spar_command::ShellPlan { steps })
    }

    fn eval_shell_command(
        &mut self,
        command: &CompiledShellCommand,
        frame: &mut Frame,
        module: crate::compiled::ModuleId,
    ) -> Result<spar_command::CommandPlan, RuntimeFault> {
        let mut args = Vec::with_capacity(command.args.len());
        for argument in &command.args {
            // Args is intentionally expanded only when it occupies the entire
            // shell word. Each stored string is already one argv entry and is
            // spliced directly without whitespace/glob re-tokenization.
            if let [CompiledShellWordPart::Expression(expression)] = argument.parts.as_slice() {
                let value = self.eval_expression(expression, frame, module)?;
                match value {
                    Value::Args(values) => {
                        args.extend(values);
                        continue;
                    }
                    value => {
                        args.push(shell_primitive_to_string(value, &argument.span)?);
                        continue;
                    }
                }
            }

            // Legacy list-spread syntax remains accepted during migration.
            let expansion = match argument.parts.as_slice() {
                [CompiledShellWordPart::Literal(prefix), CompiledShellWordPart::Expression(expression)]
                    if prefix == "..." =>
                {
                    Some(expression)
                }
                _ => None,
            };
            if let Some(expression) = expansion {
                let value = self.eval_expression(expression, frame, module)?;
                let Value::List(values) = value else {
                    return Err(type_error("list", &value, &argument.span).into());
                };
                for value in values {
                    args.push(shell_primitive_to_string(value, &argument.span)?);
                }
            } else {
                args.push(self.eval_shell_word(argument, frame, module)?);
            }
        }
        let program = self.eval_shell_word(&command.program, frame, module)?;
        if program == "read" {
            return self.eval_read_builtin(&args, command.background, &command.program.span);
        }
        let mut env = Vec::with_capacity(command.environment.len());
        for (key, value) in &command.environment {
            env.push(spar_command::EnvironmentOverride {
                key: key.clone(),
                value: self.eval_shell_word(value, frame, module)?,
            });
        }
        Ok(spar_command::CommandPlan {
            program,
            args,
            env,
            cwd: None,
            stdin: self.eval_shell_redirect(command.stdin.as_ref(), frame, module)?,
            stdout: self.eval_shell_redirect(command.stdout.as_ref(), frame, module)?,
            stderr: self.eval_shell_redirect(command.stderr.as_ref(), frame, module)?,
            redirections: command
                .redirections
                .iter()
                .map(|redirect| {
                    Ok(spar_command::OrderedRedirection {
                        fd: redirect.fd,
                        target: match &redirect.target {
                            crate::compiled::CompiledShellFdRedirectTarget::File(file) => {
                                spar_command::Redirection::File {
                                    path: self.eval_shell_word(&file.target, frame, module)?,
                                    mode: file.mode.clone(),
                                }
                            }
                            crate::compiled::CompiledShellFdRedirectTarget::Duplicate(fd) => {
                                spar_command::Redirection::DuplicateFd(*fd)
                            }
                        },
                    })
                })
                .collect::<Result<Vec<_>, RuntimeFault>>()?,
            background: command.background,
        })
    }

    fn eval_shell_redirect(
        &mut self,
        redirect: Option<&CompiledShellRedirect>,
        frame: &mut Frame,
        module: crate::compiled::ModuleId,
    ) -> Result<Option<spar_command::Redirection>, RuntimeFault> {
        redirect
            .map(|redirect| {
                Ok(spar_command::Redirection::File {
                    path: self.eval_shell_word(&redirect.target, frame, module)?,
                    mode: redirect.mode.clone(),
                })
            })
            .transpose()
    }

    fn eval_shell_word(
        &mut self,
        word: &CompiledShellWord,
        frame: &mut Frame,
        module: crate::compiled::ModuleId,
    ) -> Result<String, RuntimeFault> {
        let mut output = String::new();
        for part in &word.parts {
            match part {
                CompiledShellWordPart::Literal(value) => output.push_str(value),
                CompiledShellWordPart::Environment(name) => {
                    if name == "!" {
                        let pid = self
                            .last_job
                            .as_ref()
                            .and_then(|job| match job {
                                Value::Object(fields) => fields.get("pid"),
                                _ => None,
                            })
                            .and_then(|pid| match pid {
                                Value::Int(pid) => Some(*pid),
                                _ => None,
                            })
                            .ok_or_else(|| {
                                runtime_error("$! used before a background job", &word.span)
                            })?;
                        output.push_str(&pid.to_string());
                    } else if name == "?" {
                        output.push_str(
                            &self
                                .shell_outcome
                                .as_ref()
                                .map_or(0, |status| status.exit_code)
                                .to_string(),
                        );
                    } else {
                        output.push_str(self.context.env_get(name).unwrap_or_default())
                    }
                }
                CompiledShellWordPart::Expression(expression) => {
                    let value = self.eval_expression(expression, frame, module)?;
                    match value {
                        Value::String(value) => output.push_str(&value),
                        Value::Int(value) => output.push_str(&value.to_string()),
                        Value::Float(value) => output.push_str(&value.to_string()),
                        Value::Bool(value) => output.push_str(&value.to_string()),
                        Value::Args(_) => {
                            return Err(runtime_error(
                                "Args can only be expanded as an entire shell word; use `${values.asArgs()}` by itself",
                                &word.span,
                            )
                            .into())
                        }
                        other => return Err(type_error("primitive", &other, &word.span).into()),
                    }
                }
                CompiledShellWordPart::CommandSubstitution(shell) => {
                    output.push_str(&self.execute_command_substitution(shell, frame, module)?);
                }
            }
        }
        Ok(output)
    }

    fn ensure_module(&mut self, module: crate::compiled::ModuleId) -> Result<(), RuntimeFault> {
        let state = self
            .state
            .as_ref()
            .ok_or_else(|| module_state_error(&Span::dummy()))?;
        let results = state.results.clone();
        // Locked for the whole function: module initialization is rare and
        // one-shot, so serializing it across every worker thread is cheap,
        // and holding the lock the entire time is what stops two threads
        // from racing to evaluate the same not-yet-loaded module's top
        // level twice (which would double any top-level side effects).
        let mut results_guard = results.lock().unwrap();
        if results_guard.contains_key(&module) {
            return Ok(());
        }
        let compiled = self.program.modules.get(module.0 as usize).ok_or_else(|| {
            runtime_error(&format!("unknown module ID {}", module.0), &Span::dummy())
        })?;
        let base_dir = if module == self.program.entry {
            self.program.base_dir()
        } else {
            compiled
                .identity
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
        };
        if module == self.program.entry {
            if let Some(path) = &compiled.checked.program.load_env {
                // `@LoadEnv` values take precedence over the live shell
                // environment — a key the file declares wins; std/env's
                // `get`/`has` only fall back to the shell for keys the file
                // doesn't mention.
                for (key, value) in crate::dotenv::load(&base_dir.join(path))? {
                    self.context.env_set(key, value);
                }
            }
        }
        let (mut result, pending_promises) = crate::Evaluator::evaluate_for_runtime(
            &compiled.checked.program,
            &compiled.checked.symbols,
            &compiled.checked.imports,
            base_dir,
            state.hosts.clone(),
            (*state.natives).clone(),
            state.effect_ledger.clone(),
        )
        .map_err(|mut errors| {
            errors
                .pop()
                .unwrap_or_else(|| runtime_error("module initialization failed", &Span::dummy()))
        })?;
        let mut replacements = HashMap::new();
        for mut pending in pending_promises {
            let target_module = match pending.import_alias.as_deref() {
                Some(alias) => compiled.import_modules.get(alias).copied().ok_or_else(|| {
                    runtime_error(
                        &format!("unknown import alias '{alias}' for pending promise"),
                        &Span::dummy(),
                    )
                })?,
                None => module,
            };
            let function = self
                .program
                .modules
                .get(target_module.0 as usize)
                .and_then(|module| {
                    module.functions.iter().find(|function| {
                        function.key.group == pending.group && function.name == pending.function
                    })
                })
                .ok_or_else(|| {
                    runtime_error(
                        &format!("unknown async function '{}'", pending.function),
                        &Span::dummy(),
                    )
                })?;
            for argument in &mut pending.arguments {
                remap_promises(argument, &replacements);
            }
            let handle = self.scheduler.spawn(
                function.id,
                pending
                    .arguments
                    .into_iter()
                    .map(Value::from_config)
                    .collect(),
                self.context.spawn_child(),
                self.call_depth,
            );
            replacements.insert(pending.handle, handle);
        }
        remap_promises_in_result(&mut result, &replacements);
        results_guard.insert(module, result);
        Ok(())
    }

    fn read_compiled_lvalue(
        &mut self,
        frame: &mut Frame,
        module: crate::compiled::ModuleId,
        target: &crate::compiled::CompiledLValue,
        span: &Span,
    ) -> Result<Value, RuntimeFault> {
        match target {
            crate::compiled::CompiledLValue::Local(slot) => Ok(frame.read(*slot, span)?.clone()),
            crate::compiled::CompiledLValue::Global(name) => self.read_global(module, name, span),
            crate::compiled::CompiledLValue::Field { base, field } => {
                let base = self.read_compiled_lvalue(frame, module, base, span)?;
                let Value::Object(fields) = base else {
                    return Err(runtime_error(
                        "mutable field receiver traversed a non-object value",
                        span,
                    )
                    .into());
                };
                fields.get(field).cloned().ok_or_else(|| {
                    runtime_error(
                        &format!("mutable receiver field '{field}' is unavailable"),
                        span,
                    )
                    .into()
                })
            }
        }
    }

    fn write_compiled_lvalue(
        &mut self,
        frame: &mut Frame,
        module: crate::compiled::ModuleId,
        target: &crate::compiled::CompiledLValue,
        updated: Value,
        span: &Span,
    ) -> Result<(), RuntimeFault> {
        match target {
            crate::compiled::CompiledLValue::Local(slot) => Ok(frame.write(*slot, updated, span)?),
            crate::compiled::CompiledLValue::Global(name) => {
                self.write_global(module, name, updated, span)
            }
            crate::compiled::CompiledLValue::Field { base, field } => {
                let mut base_value = self.read_compiled_lvalue(frame, module, base, span)?;
                let Value::Object(fields) = &mut base_value else {
                    return Err(runtime_error(
                        "mutable field receiver traversed a non-object value",
                        span,
                    )
                    .into());
                };
                let child = fields.get_mut(field).ok_or_else(|| {
                    runtime_error(
                        &format!("mutable receiver field '{field}' is unavailable"),
                        span,
                    )
                })?;
                *child = updated;
                self.write_compiled_lvalue(frame, module, base, base_value, span)
            }
        }
    }

    /// Executes a global assignment. A compound self-referential form like
    /// `hits = hits + 1` (`Global(name) OP operand`, either operand order)
    /// takes a fetch-modify-store fast path that holds the module's globals
    /// lock across the read and the write as a single critical section, so
    /// concurrent tasks incrementing the same global can't interleave and
    /// drop updates. `read_global` then `write_global` as two separate lock
    /// acquisitions (the fallback below, and the only path before this fix)
    /// left a window between them where another task's update could be lost.
    /// Anything else — the operand referencing `name` on both sides, or a
    /// non-arithmetic/short-circuiting operation — falls back to plain
    /// evaluate-then-write, same as before; it isn't atomic, but it's also
    /// not the counter-style pattern this fixes.
    fn eval_store_global(
        &mut self,
        module: crate::compiled::ModuleId,
        name: &str,
        value: &CompiledExpression,
        frame: &mut Frame,
        span: &Span,
    ) -> Result<(), RuntimeFault> {
        if let CompiledExpression::Operation {
            operation,
            operands,
            span: op_span,
        } = value
        {
            if let [left, right] = operands.as_slice() {
                let left_is_self = matches!(left, CompiledExpression::Global(g, _) if g == name);
                let right_is_self = matches!(right, CompiledExpression::Global(g, _) if g == name);
                let is_atomic_safe = !matches!(
                    operation,
                    TypedOperation::BoolAnd | TypedOperation::BoolOr | TypedOperation::Fallback
                );
                if is_atomic_safe && left_is_self != right_is_self {
                    let (self_is_left, other) = if left_is_self {
                        (true, right)
                    } else {
                        (false, left)
                    };
                    let operand_value = self.eval_expression(other, frame, module)?;
                    return self.apply_compound_global_update(
                        module,
                        name,
                        *operation,
                        operand_value,
                        self_is_left,
                        op_span,
                    );
                }
            }
        }
        let value = self.eval_expression(value, frame, module)?;
        self.write_global(module, name, value, span)
    }

    /// Reads the current value of `name`, combines it with `operand_value`
    /// via `operation`, and writes the result back — all under one
    /// acquisition of the module's globals lock.
    fn apply_compound_global_update(
        &mut self,
        module: crate::compiled::ModuleId,
        name: &str,
        operation: TypedOperation,
        operand_value: Value,
        self_is_left: bool,
        span: &Span,
    ) -> Result<(), RuntimeFault> {
        self.ensure_module(module)?;
        let state = self
            .state
            .as_ref()
            .ok_or_else(|| module_state_error(span))?;
        let symbols = &self
            .program
            .modules
            .get(module.0 as usize)
            .ok_or_else(|| runtime_error("compiled module is unavailable", span))?
            .checked
            .symbols;
        let mut guard = state.results.lock().unwrap();
        let result = guard
            .get_mut(&module)
            .ok_or_else(|| module_state_error(span))?;
        let current_config = result
            .globals
            .get(name)
            .cloned()
            .ok_or_else(|| runtime_error(&format!("global '{name}' is unavailable"), span))?;
        let expected = symbols.globals.get(name).and_then(|entry| match entry {
            crate::resolver::GlobalEntry::Var { ty, .. } => Some(ty),
            crate::resolver::GlobalEntry::Dynamic { .. } => None,
        });
        let current = match expected {
            Some(expected) => value_from_config_typed(current_config, expected, symbols),
            None => Value::from_config(current_config),
        };
        let values = if self_is_left {
            [current, operand_value]
        } else {
            [operand_value, current]
        };
        let new_value = eval_operation(operation, &values, span)?;
        let new_config = new_value.try_into_config(span)?;
        result.globals.insert(name.to_string(), new_config);
        Ok(())
    }

    fn read_global(
        &mut self,
        module: crate::compiled::ModuleId,
        name: &str,
        span: &Span,
    ) -> Result<Value, RuntimeFault> {
        if name == "_" {
            return self.context.previous_value().cloned().ok_or_else(|| {
                runtime_error("no previous interactive value is available", span).into()
            });
        }
        if name == "status" && self.shell_depth > 0 {
            let outcome =
                self.shell_outcome
                    .clone()
                    .unwrap_or(crate::evaluator::ShellPlanOutcome {
                        success: true,
                        exit_code: 0,
                        signal: None,
                        pid: 0,
                        pipeline: vec![],
                    });
            // `signal` is declared `Option<int>` on the synthetic ProcessStatus
            // type (see compiler.rs) — always materialize a real Option value
            // here rather than a bare int (Some case) or an absent field
            // (None case), or `status.signal` can't be compared/unwrapped as
            // the Option it's typed as.
            let signal_value = |signal: Option<i32>| {
                Value::Option(signal.map(|signal| Box::new(Value::Int(i64::from(signal)))))
            };
            let process_value = |process: spar_process::ProcessStatus| {
                let fields = indexmap::IndexMap::from([
                    ("code".into(), Value::Int(i64::from(process.code))),
                    ("success".into(), Value::Bool(process.success)),
                    ("signal".into(), signal_value(process.signal)),
                    ("pid".into(), Value::Int(i64::from(process.pid))),
                ]);
                Value::Object(Shared::from(fields))
            };
            let pipeline = outcome.pipeline.into_iter().map(process_value).collect();
            let fields = indexmap::IndexMap::from([
                ("code".into(), Value::Int(i64::from(outcome.exit_code))),
                ("success".into(), Value::Bool(outcome.success)),
                ("signal".into(), signal_value(outcome.signal)),
                ("pid".into(), Value::Int(i64::from(outcome.pid))),
                ("pipeline".into(), Value::List(pipeline)),
            ]);
            return Ok(Value::Object(Shared::from(fields)));
        }
        if name == "lastJob" && self.shell_depth > 0 {
            return self
                .last_job
                .clone()
                .ok_or_else(|| runtime_error("no background job has been started", span).into());
        }
        self.ensure_module(module)?;
        let value = match self.state.as_ref() {
            Some(state) => {
                let guard = state.results.lock().unwrap();
                guard
                    .get(&module)
                    .and_then(|result| result.globals.get(name))
                    .cloned()
            }
            None => None,
        }
        .ok_or_else(|| runtime_error(&format!("global '{name}' is unavailable"), span))?;
        let symbols = &self
            .program
            .modules
            .get(module.0 as usize)
            .ok_or_else(|| runtime_error("compiled module is unavailable", span))?
            .checked
            .symbols;
        let expected = symbols.globals.get(name).and_then(|entry| match entry {
            crate::resolver::GlobalEntry::Var { ty, .. } => Some(ty),
            crate::resolver::GlobalEntry::Dynamic { .. } => None,
        });
        Ok(match expected {
            Some(expected) => value_from_config_typed(value, expected, symbols),
            None => Value::from_config(value),
        })
    }

    fn write_global(
        &mut self,
        module: crate::compiled::ModuleId,
        name: &str,
        value: Value,
        span: &Span,
    ) -> Result<(), RuntimeFault> {
        self.ensure_module(module)?;
        let state = self
            .state
            .as_ref()
            .ok_or_else(|| module_state_error(span))?;
        let mut guard = state.results.lock().unwrap();
        let result = guard
            .get_mut(&module)
            .ok_or_else(|| module_state_error(span))?;
        let value = value.try_into_config(span)?;
        result.globals.insert(name.to_string(), value);
        Ok(())
    }

    fn read_path(
        &mut self,
        module: crate::compiled::ModuleId,
        path: &[String],
        span: &Span,
    ) -> Result<Value, RuntimeFault> {
        self.ensure_module(module)?;
        let state = self
            .state
            .as_ref()
            .ok_or_else(|| module_state_error(span))?;
        let guard = state.results.lock().unwrap();
        let result = guard.get(&module).ok_or_else(|| module_state_error(span))?;
        let symbols = &self
            .program
            .modules
            .get(module.0 as usize)
            .ok_or_else(|| runtime_error("compiled module is unavailable", span))?
            .checked
            .symbols;
        let (value, expected) = match path {
            [name] => {
                if let Some(value) = result.globals.get(name).cloned() {
                    let expected = symbols.globals.get(name).and_then(|entry| match entry {
                        crate::resolver::GlobalEntry::Var { ty, .. } => Some(ty.clone()),
                        crate::resolver::GlobalEntry::Dynamic { .. } => None,
                    });
                    Some((value, expected))
                } else {
                    result
                        .structs
                        .get(std::slice::from_ref(name))
                        .cloned()
                        .map(|fields| {
                            (
                                ConfigValue::Object(fields),
                                Some(SparType::Named(name.clone())),
                            )
                        })
                }
            }
            [owner @ .., field] => {
                let value = result
                    .structs
                    .get(owner)
                    .and_then(|fields| fields.get(field))
                    .cloned();
                value.map(|value| {
                    let owner_type = owner.last().map(|name| SparType::Named(name.clone()));
                    let expected = owner_type.as_ref().and_then(|owner_type| {
                        crate::typechecker::TypeChecker::field_type(owner_type, field, symbols)
                    });
                    (value, expected)
                })
            }
            [] => None,
        }
        .ok_or_else(|| {
            runtime_error(
                &format!("imported path '{}' is unavailable", path.join("::")),
                span,
            )
        })?;
        Ok(match expected.as_ref() {
            Some(expected) => value_from_config_typed(value, expected, symbols),
            None => Value::from_config(value),
        })
    }

    fn execute_shell(
        &self,
        plan: &spar_command::ShellPlan,
        span: &Span,
    ) -> Result<ConfigValue, RuntimeFault> {
        let state = self
            .state
            .as_ref()
            .ok_or_else(|| module_state_error(span))?;
        let run = || {
            let capture = !self.context.inherit_exec_output();
            let options = spar_process::ExecutionOptions {
                capture_stdout: capture,
                capture_stderr: capture,
                environment: Some(self.context.environment_pairs()),
            };
            let mut success = true;
            let mut exit_code = 0;
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let mut structured_status = None;
            for (join, step) in &plan.steps {
                let should_run = match join {
                    spar_command::Join::Always => true,
                    spar_command::Join::OnSuccess => success,
                    spar_command::Join::OnFailure => !success,
                };
                if !should_run {
                    continue;
                }
                let output = match step {
                    spar_command::Step::Command(command) => {
                        spar_process::run_command(command, &options)
                    }
                    spar_command::Step::Pipeline(pipeline) => {
                        spar_process::run_pipeline(pipeline, &options)
                    }
                }
                .map_err(|error| SparError::EvalError {
                    message: format!("could not execute shell plan: {error}"),
                    span: span.clone(),
                })?;
                success = output.status.success;
                exit_code = output.status.code.unwrap_or(if success { 0 } else { 1 });
                stdout = output.stdout.unwrap_or_default();
                stderr = output.stderr.unwrap_or_default();
                structured_status = output.pipeline_status;
            }
            let bytes = |values: Vec<u8>| {
                ConfigValue::List(
                    values
                        .into_iter()
                        .map(|value| ConfigValue::Int(i64::from(value)))
                        .collect(),
                )
            };
            // Same `Option<int>` contract as the compiled-runtime twin of
            // this closure above — always a real Option, never a bare int
            // or an absent field.
            let process_value = |process: spar_process::ProcessStatus| {
                let fields = indexmap::IndexMap::from([
                    ("code".into(), ConfigValue::Int(i64::from(process.code))),
                    ("success".into(), ConfigValue::Bool(process.success)),
                    (
                        "signal".into(),
                        ConfigValue::Option(
                            process
                                .signal
                                .map(|signal| Box::new(ConfigValue::Int(i64::from(signal)))),
                        ),
                    ),
                    ("pid".into(), ConfigValue::Int(i64::from(process.pid))),
                ]);
                ConfigValue::Object(fields)
            };
            let status = structured_status.unwrap_or(spar_process::PipelineStatus {
                code: exit_code,
                success,
                processes: vec![],
            });
            let status_value = ConfigValue::Object(indexmap::IndexMap::from([
                ("code".into(), ConfigValue::Int(i64::from(status.code))),
                ("success".into(), ConfigValue::Bool(status.success)),
                (
                    "processes".into(),
                    ConfigValue::List(status.processes.into_iter().map(process_value).collect()),
                ),
            ]));
            Ok::<ConfigValue, SparError>(ConfigValue::Object(indexmap::IndexMap::from([
                ("success".into(), ConfigValue::Bool(success)),
                ("exitCode".into(), ConfigValue::Int(i64::from(exit_code))),
                ("status".into(), status_value),
                ("stdout".into(), bytes(stdout)),
                ("stderr".into(), bytes(stderr)),
            ])))
        };
        Ok(match &state.effect_ledger {
            Some(ledger) => ledger.get_or_try_run((span.start, span.end), run),
            None => run(),
        }?)
    }
}

fn value_from_config_typed(
    value: ConfigValue,
    expected: &SparType,
    symbols: &crate::resolver::SymbolTable,
) -> Value {
    match (value, expected) {
        (ConfigValue::List(values), SparType::Tuple(elements)) => Value::List(
            values
                .into_iter()
                .zip(elements)
                .map(|(value, element)| value_from_config_typed(value, element, symbols))
                .collect(),
        ),
        (ConfigValue::List(values), SparType::List(element)) => Value::List(
            values
                .into_iter()
                .map(|value| value_from_config_typed(value, element, symbols))
                .collect(),
        ),
        (ConfigValue::Object(values), SparType::Applied { name, arguments })
            if name == "Map" && arguments.len() == 2 =>
        {
            let value_type = &arguments[1];
            Value::Map(
                values
                    .into_iter()
                    .map(|(key, value)| {
                        (
                            Value::String(key),
                            value_from_config_typed(value, value_type, symbols),
                        )
                    })
                    .collect(),
            )
        }
        (ConfigValue::Map(values), SparType::Applied { name, arguments })
            if name == "Map" && arguments.len() == 2 =>
        {
            let key_type = &arguments[0];
            let value_type = &arguments[1];
            Value::Map(
                values
                    .into_iter()
                    .map(|(key, value)| {
                        (
                            value_from_config_typed(key, key_type, symbols),
                            value_from_config_typed(value, value_type, symbols),
                        )
                    })
                    .collect(),
            )
        }
        (ConfigValue::Option(value), SparType::Applied { name, arguments })
            if name == "Option" && arguments.len() == 1 =>
        {
            Value::Option(
                value
                    .map(|value| Box::new(value_from_config_typed(*value, &arguments[0], symbols))),
            )
        }
        (ConfigValue::Result(value), SparType::Applied { name, arguments })
            if name == "Result" && arguments.len() == 2 =>
        {
            Value::Result(match value {
                Ok(value) => Ok(Box::new(value_from_config_typed(
                    *value,
                    &arguments[0],
                    symbols,
                ))),
                Err(value) => Err(Box::new(value_from_config_typed(
                    *value,
                    &arguments[1],
                    symbols,
                ))),
            })
        }
        (ConfigValue::Object(values), ty)
            if crate::typechecker::TypeChecker::fields_for_type(ty, symbols).is_some() =>
        {
            Value::Object(
                values
                    .into_iter()
                    .map(|(field, value)| {
                        let value =
                            crate::typechecker::TypeChecker::field_type(ty, &field, symbols)
                                .map(|field_type| {
                                    value_from_config_typed(value.clone(), &field_type, symbols)
                                })
                                .unwrap_or_else(|| Value::from_config(value));
                        (field, value)
                    })
                    .collect(),
            )
        }
        (value, _) => Value::from_config(value),
    }
}

fn shell_primitive_to_string(value: Value, span: &Span) -> Result<String, RuntimeFault> {
    match value {
        Value::String(value) => Ok(value),
        Value::Int(value) => Ok(value.to_string()),
        Value::Float(value) => Ok(value.to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        other => Err(type_error("primitive", &other, span).into()),
    }
}

fn remap_promises_in_result(
    result: &mut crate::evaluator::EvalResult,
    replacements: &HashMap<crate::PromiseHandle, crate::PromiseHandle>,
) {
    for value in result.globals.values_mut() {
        remap_promises(value, replacements);
    }
    for fields in result.structs.values_mut() {
        for value in fields.values_mut() {
            remap_promises(value, replacements);
        }
    }
}

fn remap_promises(
    value: &mut ConfigValue,
    replacements: &HashMap<crate::PromiseHandle, crate::PromiseHandle>,
) {
    match value {
        ConfigValue::Promise(handle) => {
            if let Some(replacement) = replacements.get(handle) {
                *handle = *replacement;
            }
        }
        ConfigValue::List(values) => {
            for value in values {
                remap_promises(value, replacements);
            }
        }
        ConfigValue::Object(fields) => {
            for value in fields.values_mut() {
                remap_promises(value, replacements);
            }
        }
        ConfigValue::Map(entries) => {
            for (key, value) in entries {
                remap_promises(key, replacements);
                remap_promises(value, replacements);
            }
        }
        ConfigValue::Option(value) => {
            if let Some(value) = value {
                remap_promises(value, replacements);
            }
        }
        ConfigValue::Result(value) => match value {
            Ok(value) | Err(value) => remap_promises(value, replacements),
        },
        ConfigValue::Error { cause, .. } => {
            if let Some(cause) = cause {
                remap_promises(cause, replacements);
            }
        }
        ConfigValue::Str(_)
        | ConfigValue::Int(_)
        | ConfigValue::Float(_)
        | ConfigValue::Bool(_)
        | ConfigValue::Shell(_)
        | ConfigValue::ShellProgram(_) => {}
    }
}

/// Fast path for two already-evaluated operands; `None` means "use the
/// general `eval_operation`".
#[inline(always)]
fn eval_int_binary_value(operation: TypedOperation, left: &Value, right: &Value) -> Option<Value> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => eval_int_binary(operation, *a, *b),
        (Value::Float(a), Value::Float(b)) => Some(match operation {
            TypedOperation::FloatAdd => Value::Float(a + b),
            TypedOperation::FloatSub => Value::Float(a - b),
            TypedOperation::FloatMul => Value::Float(a * b),
            TypedOperation::FloatLt => Value::Bool(a < b),
            TypedOperation::FloatGt => Value::Bool(a > b),
            TypedOperation::FloatLtEq => Value::Bool(a <= b),
            TypedOperation::FloatGtEq => Value::Bool(a >= b),
            _ => return None,
        }),
        _ => None,
    }
}

/// Error for an integer operation whose result does not fit in `i64`.
/// Spar integers are checked: overflow is a runtime error, never a silent wrap
/// (use the `wrapping*`/`checked*` int methods when wrapping is intended).
pub(crate) fn integer_overflow_error(what: &str, span: &Span) -> SparError {
    SparError::EvalError {
        message: format!("integer overflow in {what}"),
        span: span.clone(),
    }
}

/// Hot-path int/int arithmetic and comparison. Mirrors the corresponding arms
/// of `eval_operation`; returns `None` for anything that needs the general path
/// (division, and overflow, which the general path reports as an error).
#[inline(always)]
fn eval_int_binary(operation: TypedOperation, a: i64, b: i64) -> Option<Value> {
    Some(match operation {
        TypedOperation::IntAdd => Value::Int(a.checked_add(b)?),
        TypedOperation::IntSub => Value::Int(a.checked_sub(b)?),
        TypedOperation::IntMul => Value::Int(a.checked_mul(b)?),
        TypedOperation::IntEq => Value::Bool(a == b),
        TypedOperation::IntLt => Value::Bool(a < b),
        TypedOperation::IntGt => Value::Bool(a > b),
        TypedOperation::IntLtEq => Value::Bool(a <= b),
        TypedOperation::IntGtEq => Value::Bool(a >= b),
        _ => return None,
    })
}

fn eval_operation(
    operation: TypedOperation,
    values: &[Value],
    span: &Span,
) -> Result<Value, SparError> {
    macro_rules! binary {
        ($left:pat, $right:pat => $value:expr) => {
            match values {
                [$left, $right] => Ok($value),
                _ => Err(operation_type_error(operation, values, span)),
            }
        };
    }
    match operation {
        TypedOperation::IntAdd => match values {
            [Value::Int(a), Value::Int(b)] => a
                .checked_add(*b)
                .map(Value::Int)
                .ok_or_else(|| integer_overflow_error("addition", span)),
            _ => Err(operation_type_error(operation, values, span)),
        },
        TypedOperation::FloatAdd => {
            binary!(Value::Float(a), Value::Float(b) => Value::Float(a + b))
        }
        TypedOperation::StringConcat => {
            binary!(Value::String(a), Value::String(b) => Value::String(format!("{a}{b}")))
        }
        TypedOperation::ShellConcat => {
            binary!(Value::Shell(a), Value::Shell(b) => Value::Shell(Shared::from((**a).clone().then((**b).clone()))))
        }
        TypedOperation::IntSub => match values {
            [Value::Int(a), Value::Int(b)] => a
                .checked_sub(*b)
                .map(Value::Int)
                .ok_or_else(|| integer_overflow_error("subtraction", span)),
            _ => Err(operation_type_error(operation, values, span)),
        },
        TypedOperation::FloatSub => {
            binary!(Value::Float(a), Value::Float(b) => Value::Float(a - b))
        }
        TypedOperation::IntMul => match values {
            [Value::Int(a), Value::Int(b)] => a
                .checked_mul(*b)
                .map(Value::Int)
                .ok_or_else(|| integer_overflow_error("multiplication", span)),
            _ => Err(operation_type_error(operation, values, span)),
        },
        TypedOperation::FloatMul => {
            binary!(Value::Float(a), Value::Float(b) => Value::Float(a * b))
        }
        TypedOperation::IntDiv => match values {
            [Value::Int(_), Value::Int(0)] => Err(SparError::EvalError {
                message: "division by zero".into(),
                span: span.clone(),
            }),
            [Value::Int(a), Value::Int(b)] => a
                .checked_div(*b)
                .map(Value::Int)
                .ok_or_else(|| integer_overflow_error("division", span)),
            _ => Err(operation_type_error(operation, values, span)),
        },
        TypedOperation::FloatDiv => match values {
            [Value::Float(_), Value::Float(b)] if *b == 0.0 => Err(SparError::EvalError {
                message: "division by zero".into(),
                span: span.clone(),
            }),
            [Value::Float(a), Value::Float(b)] => Ok(Value::Float(a / b)),
            _ => Err(operation_type_error(operation, values, span)),
        },
        TypedOperation::IntRem => match values {
            [Value::Int(_), Value::Int(0)] => Err(SparError::EvalError {
                message: "division by zero".into(),
                span: span.clone(),
            }),
            [Value::Int(a), Value::Int(b)] => a
                .checked_rem(*b)
                .map(Value::Int)
                .ok_or_else(|| integer_overflow_error("remainder", span)),
            _ => Err(operation_type_error(operation, values, span)),
        },
        TypedOperation::FloatRem => match values {
            [Value::Float(_), Value::Float(b)] if *b == 0.0 => Err(SparError::EvalError {
                message: "division by zero".into(),
                span: span.clone(),
            }),
            [Value::Float(a), Value::Float(b)] => Ok(Value::Float(a % b)),
            _ => Err(operation_type_error(operation, values, span)),
        },
        TypedOperation::IntEq => {
            binary!(Value::Int(a), Value::Int(b) => Value::Bool(a == b))
        }
        TypedOperation::FloatEq => {
            binary!(Value::Float(a), Value::Float(b) => Value::Bool(a == b))
        }
        TypedOperation::StringEq => {
            binary!(Value::String(a), Value::String(b) => Value::Bool(a == b))
        }
        TypedOperation::DynamicEq => match values {
            [left, right] => Ok(Value::Bool(left == right)),
            _ => Err(operation_type_error(operation, values, span)),
        },
        TypedOperation::DynamicLt
        | TypedOperation::DynamicGt
        | TypedOperation::DynamicLtEq
        | TypedOperation::DynamicGtEq => match values {
            [left, right] => {
                let ordering = match (left, right) {
                    (Value::Int(a), Value::Int(b)) => a.partial_cmp(b),
                    (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
                    (Value::Int(a), Value::Float(b)) => (*a as f64).partial_cmp(b),
                    (Value::Float(a), Value::Int(b)) => a.partial_cmp(&(*b as f64)),
                    (Value::String(a), Value::String(b)) => a.partial_cmp(b),
                    _ => {
                        return Err(SparError::EvalError {
                            message: format!(
                                "cannot order {} and {}",
                                left.type_name(),
                                right.type_name()
                            ),
                            span: span.clone(),
                        })
                    }
                };
                let Some(ordering) = ordering else {
                    return Ok(Value::Bool(false));
                };
                Ok(Value::Bool(match operation {
                    TypedOperation::DynamicLt => ordering.is_lt(),
                    TypedOperation::DynamicGt => ordering.is_gt(),
                    TypedOperation::DynamicLtEq => ordering.is_le(),
                    _ => ordering.is_ge(),
                }))
            }
            _ => Err(operation_type_error(operation, values, span)),
        },
        TypedOperation::DynamicNotEq => match values {
            [left, right] => Ok(Value::Bool(left != right)),
            _ => Err(operation_type_error(operation, values, span)),
        },
        TypedOperation::BoolEq => {
            binary!(Value::Bool(a), Value::Bool(b) => Value::Bool(a == b))
        }
        TypedOperation::IntNotEq => {
            binary!(Value::Int(a), Value::Int(b) => Value::Bool(a != b))
        }
        TypedOperation::FloatNotEq => {
            binary!(Value::Float(a), Value::Float(b) => Value::Bool(a != b))
        }
        TypedOperation::StringNotEq => {
            binary!(Value::String(a), Value::String(b) => Value::Bool(a != b))
        }
        TypedOperation::BoolNotEq => {
            binary!(Value::Bool(a), Value::Bool(b) => Value::Bool(a != b))
        }
        TypedOperation::IntLt => {
            binary!(Value::Int(a), Value::Int(b) => Value::Bool(a < b))
        }
        TypedOperation::FloatLt => {
            binary!(Value::Float(a), Value::Float(b) => Value::Bool(a < b))
        }
        TypedOperation::IntGt => {
            binary!(Value::Int(a), Value::Int(b) => Value::Bool(a > b))
        }
        TypedOperation::FloatGt => {
            binary!(Value::Float(a), Value::Float(b) => Value::Bool(a > b))
        }
        TypedOperation::IntLtEq => {
            binary!(Value::Int(a), Value::Int(b) => Value::Bool(a <= b))
        }
        TypedOperation::FloatLtEq => {
            binary!(Value::Float(a), Value::Float(b) => Value::Bool(a <= b))
        }
        TypedOperation::IntGtEq => {
            binary!(Value::Int(a), Value::Int(b) => Value::Bool(a >= b))
        }
        TypedOperation::FloatGtEq => {
            binary!(Value::Float(a), Value::Float(b) => Value::Bool(a >= b))
        }
        TypedOperation::BoolAnd => {
            binary!(Value::Bool(a), Value::Bool(b) => Value::Bool(*a && *b))
        }
        TypedOperation::BoolOr => {
            binary!(Value::Bool(a), Value::Bool(b) => Value::Bool(*a || *b))
        }
        TypedOperation::BoolNot => match values {
            [Value::Bool(value)] => Ok(Value::Bool(!value)),
            _ => Err(operation_type_error(operation, values, span)),
        },
        TypedOperation::IntNeg => match values {
            [Value::Int(value)] => value
                .checked_neg()
                .map(Value::Int)
                .ok_or_else(|| integer_overflow_error("negation", span)),
            _ => Err(operation_type_error(operation, values, span)),
        },
        TypedOperation::FloatNeg => match values {
            [Value::Float(value)] => Ok(Value::Float(-value)),
            _ => Err(operation_type_error(operation, values, span)),
        },
        TypedOperation::Fallback => Err(runtime_error(
            "fallback reached eager operation dispatch",
            span,
        )),
    }
}

fn operation_type_error(operation: TypedOperation, values: &[Value], span: &Span) -> SparError {
    let types = values
        .iter()
        .map(Value::type_name)
        .collect::<Vec<_>>()
        .join(", ");
    runtime_error(
        &format!("checked operation {operation:?} received [{types}]"),
        span,
    )
}

fn assign_field_path(
    target: &mut Value,
    fields: &[String],
    value: Value,
    span: &Span,
) -> Result<(), RuntimeFault> {
    let Some((field, rest)) = fields.split_first() else {
        return Err(runtime_error("field assignment requires a field path", span).into());
    };
    let Value::Object(object) = target else {
        return Err(runtime_error("field assignment target is not an object", span).into());
    };
    if rest.is_empty() {
        if !object.contains_key(field) {
            return Err(runtime_error(&format!("object has no field '{field}'"), span).into());
        }
        object.insert(field.clone(), value);
        return Ok(());
    }
    let nested = object
        .get_mut(field)
        .ok_or_else(|| runtime_error(&format!("object has no field '{field}'"), span))?;
    assign_field_path(nested, rest, value, span)
}

fn type_error(expected: &str, value: &Value, span: &Span) -> SparError {
    runtime_error(
        &format!("expected {expected}, received {}", value.type_name()),
        span,
    )
}

fn module_state_error(span: &Span) -> SparError {
    runtime_error("module state is unavailable", span)
}

fn evaluation_error(message: &str, span: &Span) -> SparError {
    SparError::EvalError {
        message: message.to_string(),
        span: span.clone(),
    }
}

/// How a value that is not a sequence is named in "needs a list, table or
/// stream, but got ..." messages.
fn sequence_kind(value: &Value) -> String {
    match value {
        Value::Object(_) => "a record".into(),
        Value::String(_) => "a string".into(),
        Value::Int(_) => "an int".into(),
        Value::Float(_) => "a float".into(),
        Value::Bool(_) => "a bool".into(),
        other => format!("`{}`", other.type_name()),
    }
}

/// `base.field` on an already-evaluated base value.
pub(crate) fn field_of_value(base: Value, field: &str, span: &Span) -> Result<Value, RuntimeFault> {
    match base {
        Value::Object(fields) => Ok(fields
            .get(field)
            .cloned()
            .ok_or_else(|| runtime_error(&format!("object has no field '{field}'"), span))?),
        Value::Bytes(bytes) if field == "values" => Ok(Value::List(
            bytes
                .into_iter()
                .map(|value| Value::Int(i64::from(value)))
                .collect(),
        )),
        Value::Error(error) => {
            let value::ErrorValue {
                message,
                kind,
                code,
                cause,
            } = *error;
            match field {
                "message" => Ok(Value::String(message)),
                "kind" => Ok(Value::String(kind)),
                "code" => Ok(Value::Int(code)),
                "cause" => Ok(cause
                    .map(|value| *value)
                    .ok_or_else(|| runtime_error("error has no cause", span))?),
                _ => Err(runtime_error(&format!("error has no field '{field}'"), span).into()),
            }
        }
        value => Err(type_error("object", &value, span).into()),
    }
}

fn runtime_error(message: &str, span: &Span) -> SparError {
    SparError::EvalError {
        message: format!("internal runtime error: {message}"),
        span: span.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_runtime_drop_frees_its_scheduler() {
        let program = Arc::new(
            crate::Engine::default()
                .compile_source("function main() -> int { return 0; };")
                .unwrap(),
        );
        let context = RuntimeContext::for_base_dir(&program.options.base_dir);
        let runtime = Runtime::new_entry_runtime(Arc::clone(&program), context);
        let weak_scheduler = Arc::downgrade(&runtime.scheduler);
        drop(runtime);
        assert!(
            weak_scheduler.upgrade().is_none(),
            "Scheduler should be freed once the entry Runtime that owns it drops \
             (a reference cycle between the Scheduler and its own `run` closure \
             would keep it alive forever)"
        );
    }

    #[test]
    fn frame_reads_and_writes_valid_slots() {
        let mut frame = Frame::new(1);
        frame
            .write(LocalSlot(0), Value::Int(7), &Span::dummy())
            .unwrap();
        assert_eq!(
            frame.read(LocalSlot(0), &Span::dummy()).unwrap(),
            &Value::Int(7)
        );
    }

    #[test]
    fn frame_rejects_invalid_and_uninitialized_slots() {
        let frame = Frame::new(1);
        let uninitialized = frame.read(LocalSlot(0), &Span::dummy()).unwrap_err();
        let invalid = frame.read(LocalSlot(1), &Span::dummy()).unwrap_err();
        assert!(uninitialized
            .to_string()
            .contains("internal runtime error:"));
        assert!(uninitialized.to_string().contains("uninitialized"));
        assert!(invalid.to_string().contains("internal runtime error:"));
        assert!(invalid.to_string().contains("invalid"));
    }

    #[test]
    fn interactive_preview_materializes_only_the_bounded_stream_prefix() {
        let options = crate::CompileOptions {
            evaluate: false,
            ..crate::CompileOptions::default()
        };
        let previous_type = crate::ast::SparType::Applied {
            name: "Stream".into(),
            arguments: vec![crate::ast::SparType::Int],
        };
        let compilation = crate::Compiler::new(options.clone())
            .with_interactive_previous_type(Some(previous_type))
            .compile("function sparshPreview() -> Stream<int> { return _; };")
            .into_result()
            .expect("interactive preview helper should compile");
        let program = Arc::new(
            crate::CompiledProgram::from_compilation(compilation, options)
                .expect("interactive preview helper should lower"),
        );
        let mut context = RuntimeContext::for_base_dir(program.base_dir());
        let pulls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let pulls_for_stream = std::sync::Arc::clone(&pulls);
        let values = (1_i64..=10).collect::<Vec<_>>();
        let mut iter = values.into_iter();
        let stream = StreamResource::new(crate::ast::SparType::Int, move || {
            pulls_for_stream.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(iter.next().map(Value::Int))
        });
        let id = context.insert_stream(stream);
        context.set_previous_value(Some(Value::Resource(id)));

        let (execution, _) =
            execute_interactive_preview_with_context(&program, "sparshPreview", context, 3, false)
                .expect("stream preview should execute");
        let InteractiveRuntimeExecution::Value(preview) = execution else {
            panic!("expected a structured value preview");
        };

        assert_eq!(
            preview.value,
            Value::List(Shared::from(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(3)
            ]))
        );
        assert!(preview.stream_preview);
        assert!(preview.truncated);
        assert_eq!(pulls.load(std::sync::atomic::Ordering::SeqCst), 4);
    }

    #[test]
    fn internal_unknown_function_and_operation_mismatch_are_diagnostics() {
        let program = Arc::new(
            crate::Engine::default()
                .compile_source("function main() -> int { return 0; };")
                .unwrap(),
        );
        let context = RuntimeContext::for_base_dir(&program.options.base_dir);
        let mut runtime = Runtime::new_entry_runtime(Arc::clone(&program), context);
        runtime.state = None;
        let unknown = runtime
            .call_function(FunctionId(999), Vec::new())
            .unwrap_err();
        assert!(unknown.to_string().contains("internal runtime error:"));
        assert!(unknown.to_string().contains("unknown function ID"));

        let span = Span::new(4, 8, 2, 3);
        let mismatch = eval_operation(
            TypedOperation::IntAdd,
            &[Value::Bool(true), Value::Bool(false)],
            &span,
        )
        .unwrap_err();
        let SparError::EvalError {
            message,
            span: actual_span,
        } = mismatch
        else {
            panic!("expected runtime diagnostic")
        };
        assert!(message.contains("internal runtime error:"));
        assert_eq!(actual_span, span);
    }
}
