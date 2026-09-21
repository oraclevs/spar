use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::sync::{Arc, Mutex};

pub(crate) mod context;
pub(crate) mod native;
pub(crate) mod resource;
pub(crate) mod schema;
pub(crate) mod stream;
pub(crate) mod table;
pub(crate) mod value;

pub use context::{RuntimeContext, RuntimeInput, RuntimeOutput};
pub use native::{
    NativeExecutionKind, NativeFunction, NativeFunctionId, NativeIntrinsic, NativeMethod,
    NativeMethodId, NativeMethodSignature, NativeRegistry, NativeSignature,
};
pub use resource::ResourceId;
pub use schema::{Schema, SchemaField, SchemaInferenceError, SchemaType};
pub use stream::{StreamResource, StreamState};
pub use table::TableValue;
pub use value::Value;

use crate::async_runtime::{RuntimeFault, TaskInvocation, TaskStatus, TaskTable};
use crate::compiled::{
    CompiledExpression, CompiledMethodTarget, CompiledObjectItem, CompiledProgram,
    CompiledShellCommand, CompiledShellExpr, CompiledShellMixedPipeline, CompiledShellRedirect,
    CompiledShellStep, CompiledShellWord, CompiledShellWordPart, CompiledStatement,
    CompiledStringPart, FunctionId, LocalSlot, TypedOperation,
};
use crate::error::{Span, SparError};
use crate::evaluator::ConfigValue;

#[derive(Clone)]
pub(crate) struct Frame {
    slots: Vec<Option<Value>>,
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
}

fn internal_slot_error(slot: LocalSlot, detail: &str, span: &Span) -> SparError {
    SparError::EvalError {
        message: format!("internal runtime error: local slot {} {detail}", slot.0),
        span: span.clone(),
    }
}

const MAX_CALL_DEPTH: usize = 20;

#[cfg(test)]
pub(crate) fn execute_self_contained_entry(
    program: &CompiledProgram,
) -> Result<Value, Vec<SparError>> {
    let entry = program.entry_main.ok_or_else(|| {
        vec![SparError::ResolveError {
            message: "no 'main' function found".into(),
            hint: None,
            span: Span::dummy(),
        }]
    })?;
    Runtime {
        program,
        call_depth: 0,
        state: None,
        tasks: TaskTable::default(),
        shell_depth: 0,
        shell_outcome: None,
        jobs: Vec::new(),
        last_job: None,
        shell_exit: false,
        shell_cwd: None,
        context: RuntimeContext::for_base_dir(&program.options.base_dir),
    }
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
    program: &'a CompiledProgram,
    call_depth: usize,
    state: Option<ModuleState>,
    tasks: TaskTable,
    shell_depth: usize,
    shell_outcome: Option<crate::evaluator::ShellPlanOutcome>,
    jobs: Vec<spar_process::Job>,
    last_job: Option<Value>,
    shell_exit: bool,
    shell_cwd: Option<std::path::PathBuf>,
    context: RuntimeContext,
}

impl Drop for Runtime<'_> {
    fn drop(&mut self) {
        self.tasks.cancel_pending();
        self.context.shutdown();
    }
}

struct ModuleState {
    results: HashMap<crate::compiled::ModuleId, crate::evaluator::EvalResult>,
    hosts: crate::HostRegistry,
    natives: NativeRegistry,
    effect_ledger: Option<crate::session::EffectLedger>,
}

impl ModuleState {
    fn new(program: &CompiledProgram) -> Self {
        Self {
            results: HashMap::new(),
            hosts: program.options.hosts.clone(),
            natives: program.options.natives.clone(),
            effect_ledger: program.options.effect_ledger.clone(),
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
pub(crate) fn execute_program(program: &CompiledProgram) -> Result<Value, Vec<SparError>> {
    execute_program_with_context(
        program,
        RuntimeContext::for_base_dir(&program.options.base_dir),
    )
}

pub(crate) fn execute_program_with_context(
    program: &CompiledProgram,
    context: RuntimeContext,
) -> Result<Value, Vec<SparError>> {
    let entry = program.entry_main.ok_or_else(|| {
        vec![SparError::ResolveError {
            message: "no 'main' function found — Execute mode requires a zero-argument 'main' returning 'int', 'void', or 'shell'".into(),
            hint: None,
            span: Span::dummy(),
        }]
    })?;
    let mut runtime = Runtime {
        program,
        call_depth: 0,
        state: Some(ModuleState::new(program)),
        tasks: TaskTable::default(),
        shell_depth: 0,
        shell_outcome: None,
        jobs: Vec::new(),
        last_job: None,
        shell_exit: false,
        shell_cwd: None,
        context,
    };
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

pub(crate) enum InteractiveRuntimeExecution {
    Value(crate::session::InteractiveRuntimeValue),
    Process(crate::evaluator::ShellPlanOutcome),
}

pub(crate) fn execute_interactive_preview_with_context(
    program: &CompiledProgram,
    function_name: &str,
    context: RuntimeContext,
    preview_limit: usize,
    await_result: bool,
) -> Result<(InteractiveRuntimeExecution, crate::evaluator::EvalResult), Vec<SparError>> {
    let mut runtime = Runtime {
        program,
        call_depth: 0,
        state: Some(ModuleState::new(program)),
        tasks: TaskTable::default(),
        shell_depth: 0,
        shell_outcome: None,
        jobs: Vec::new(),
        last_job: None,
        shell_exit: false,
        shell_cwd: None,
        context,
    };
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
        .and_then(|state| state.results.remove(&program.entry))
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

impl Runtime<'_> {
    fn run_entry(&mut self, entry: FunctionId) -> Result<Value, RuntimeFault> {
        let result = if self.function_is_async(entry)? {
            let handle = self.tasks.spawn(entry, Vec::new());
            self.drive_promise(handle, &Span::dummy())
        } else {
            self.call_function(entry, Vec::new())
        };
        self.tasks.cancel_pending();
        result
    }

    fn function_is_async(&self, id: FunctionId) -> Result<bool, RuntimeFault> {
        self.program
            .modules
            .iter()
            .flat_map(|module| module.functions.iter())
            .find(|function| function.id == id)
            .map(|function| function.is_async)
            .ok_or_else(|| {
                RuntimeFault::Fatal(runtime_error(
                    &format!("unknown function ID {}", id.0),
                    &Span::dummy(),
                ))
            })
    }

    fn entry_function_span(&self, id: FunctionId) -> Option<Span> {
        self.program
            .modules
            .iter()
            .flat_map(|module| module.functions.iter())
            .find(|function| function.id == id)
            .map(|function| function.span.clone())
    }

    fn run_task(
        &mut self,
        handle: crate::PromiseHandle,
        invocation: TaskInvocation,
    ) -> Result<(), RuntimeFault> {
        let result = self.call_function(invocation.function, invocation.arguments);
        let fatal = match &result {
            Err(RuntimeFault::Fatal(error)) => Some(RuntimeFault::Fatal(error.clone())),
            _ => None,
        };
        self.tasks.complete(handle, result);
        match fatal {
            Some(fatal) => Err(fatal),
            None => Ok(()),
        }
    }

    fn tick_one(&mut self) -> Result<(), RuntimeFault> {
        if let Some((handle, invocation)) = self.tasks.next_pending() {
            self.run_task(handle, invocation)?;
        }
        Ok(())
    }

    fn drive_promise(
        &mut self,
        handle: crate::PromiseHandle,
        span: &Span,
    ) -> Result<Value, RuntimeFault> {
        loop {
            match self.tasks.status(handle) {
                TaskStatus::Pending => {
                    let Some((next_handle, invocation)) = self.tasks.next_pending() else {
                        return Err(RuntimeFault::Fatal(runtime_error(
                            "promise scheduler made no progress",
                            span,
                        )));
                    };
                    self.run_task(next_handle, invocation)?;
                }
                TaskStatus::Ready(result) => return result,
                TaskStatus::Running => {
                    return Err(runtime_error("promise await cycle detected", span).into());
                }
                TaskStatus::Cancelled => {
                    return Err(runtime_error("promise was cancelled", span).into());
                }
                TaskStatus::Unknown => {
                    return Err(RuntimeFault::Fatal(runtime_error(
                        "unknown promise handle",
                        span,
                    )));
                }
            }
        }
    }

    fn call_closure(
        &mut self,
        closure: ClosureValue,
        arguments: Vec<Value>,
        call_span: &Span,
    ) -> Result<Value, RuntimeFault> {
        if self.call_depth >= MAX_CALL_DEPTH {
            return Err(runtime_error("maximum function call depth exceeded", call_span).into());
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
        self.call_function_with_frame(id, arguments)
            .map(|(value, _, _)| value)
    }

    fn call_function_with_frame(
        &mut self,
        id: FunctionId,
        arguments: Vec<Value>,
    ) -> Result<(Value, Frame, Vec<LocalSlot>), RuntimeFault> {
        if self.call_depth >= MAX_CALL_DEPTH {
            return Err(
                runtime_error("maximum function call depth exceeded", &Span::dummy()).into(),
            );
        }
        let function = self
            .program
            .modules
            .iter()
            .flat_map(|module| module.functions.iter())
            .find(|function| function.id == id)
            .ok_or_else(|| {
                runtime_error(&format!("unknown function ID {}", id.0), &Span::dummy())
            })?;
        let parameter_slots = function.parameter_slots.clone();
        let module = function.key.module;
        let default_values = function.default_values.clone();
        let slot_count = function.slot_count;
        let body = function.body.clone();
        let function_span = function.span.clone();
        if arguments.len() > parameter_slots.len() {
            return Err(runtime_error("too many direct-call arguments", &function_span).into());
        }
        self.call_depth += 1;
        let result: Result<(Value, Frame, Vec<LocalSlot>), RuntimeFault> = (|| {
            let mut frame = Frame::new(slot_count);
            let supplied_count = arguments.len();
            for (slot, value) in parameter_slots.iter().copied().zip(arguments) {
                frame.write(slot, value, &function_span)?;
            }
            for (index, slot) in parameter_slots
                .iter()
                .copied()
                .enumerate()
                .skip(supplied_count)
            {
                let default = default_values
                    .get(index)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| {
                        runtime_error("missing required direct-call argument", &function_span)
                    })?;
                let value = self.eval_expression(default, &mut frame, module)?;
                frame.write(slot, value, &function_span)?;
            }
            let value = match self.execute_statements(&body, &mut frame, module)? {
                RuntimeFlow::Return(value) => value,
                RuntimeFlow::Normal => Value::Void,
                RuntimeFlow::Break | RuntimeFlow::Continue => {
                    return Err(runtime_error(
                        "loop control escaped a compiled function",
                        &function_span,
                    )
                    .into())
                }
            };
            Ok((value, frame, parameter_slots))
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
                CompiledStatement::StoreLocal { slot, value, span } => {
                    let value = self.eval_expression(value, frame, module)?;
                    frame.write(*slot, value, span)?;
                    RuntimeFlow::Normal
                }
                CompiledStatement::StoreGlobal { name, value, span } => {
                    let value = self.eval_expression(value, frame, module)?;
                    self.write_global(module, name, value, span)?;
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
                        let caught = Value::Error {
                            message: error.to_string(),
                            kind: "runtime".into(),
                            code: 1,
                            cause: None,
                        };
                        if let Some(catch_slot) = catch_slot {
                            frame.write(*catch_slot, caught, span)?;
                        }
                        self.execute_statements(handler, frame, module)?
                    }
                    Err(fatal @ RuntimeFault::Fatal(_)) => return Err(fatal),
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
            self.tick_one()?;
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
            CompiledExpression::Constant(value, _) => Ok(Value::from_config(value.clone())),
            CompiledExpression::Local(slot, span) => Ok(frame.read(*slot, span)?.clone()),
            CompiledExpression::Global(name, span) => self.read_global(module, name, span),
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
                for (slot, expression) in captures {
                    let value = self.eval_expression(expression, frame, module)?;
                    captured.write(*slot, value, span)?;
                }
                Ok(Value::Closure(ClosureValue {
                    captured,
                    parameter_slots: parameter_slots.clone(),
                    body: body.clone(),
                    module: *closure_module,
                    span: span.clone(),
                }))
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
                    Value::Closure(closure) => self.call_closure(closure, values, span),
                    Value::Function(function) => {
                        if self.function_is_async(function)? {
                            Ok(Value::Promise(self.tasks.spawn(function, values)))
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
                    return self.execute_native_intrinsic(intrinsic, &values, span);
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
                Err(RuntimeFault::Fatal(runtime_error(&message, span)))
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
                    fields.insert(field.clone(), override_value);
                }
                Ok(value)
            }
            CompiledExpression::MethodCall {
                target,
                receiver,
                receiver_slot,
                receiver_global,
                arguments,
                mutates_receiver,
                span,
            } => {
                let mut values = Vec::new();
                if let Some(receiver) = receiver {
                    values.push(self.eval_expression(receiver, frame, module)?);
                }
                for argument in arguments {
                    values.push(self.eval_expression(argument, frame, module)?);
                }
                let (result, method_frame, parameter_slots) = match target {
                    CompiledMethodTarget::Function(function) => {
                        self.call_function_with_frame(*function, values)?
                    }
                    CompiledMethodTarget::Native(method) => {
                        let natives = self
                            .state
                            .as_ref()
                            .ok_or_else(|| module_state_error(span))?
                            .natives
                            .clone();
                        let result = if let Some(intrinsic) = natives.method_intrinsic(*method) {
                            self.execute_native_intrinsic(intrinsic, &values, span)?
                        } else {
                            natives.call_method(*method, &mut self.context, &values, span)?
                        };
                        (result, Frame::new(0), Vec::new())
                    }
                };
                if *mutates_receiver {
                    let self_slot = parameter_slots.first().copied().ok_or_else(|| {
                        runtime_error("mutable method is missing self parameter", span)
                    })?;
                    let updated = method_frame.read(self_slot, span)?.clone();
                    if let Some(slot) = receiver_slot {
                        frame.write(*slot, updated, span)?;
                    } else if let Some(name) = receiver_global {
                        self.write_global(module, name, updated, span)?;
                    } else {
                        return Err(runtime_error(
                            "mutable method receiver has no writeback target",
                            span,
                        )
                        .into());
                    }
                }
                Ok(result)
            }
            CompiledExpression::DirectCall {
                function,
                arguments,
                span: _,
            } => {
                let values = arguments
                    .iter()
                    .map(|argument| self.eval_expression(argument, frame, module))
                    .collect::<Result<Vec<_>, _>>()?;
                if self.function_is_async(*function)? {
                    Ok(Value::Promise(self.tasks.spawn(*function, values)))
                } else {
                    self.call_function(*function, values)
                }
            }
            CompiledExpression::List(items, _) => Ok(Value::List(
                items
                    .iter()
                    .map(|item| self.eval_expression(item, frame, module))
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            CompiledExpression::Object(items, span) => {
                let mut object = indexmap::IndexMap::new();
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
                                    "checked object spread received a non-object",
                                    span,
                                )
                                .into());
                            };
                            object.extend(fields);
                        }
                    }
                }
                Ok(Value::Object(object))
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
                let values = operands
                    .iter()
                    .map(|operand| self.eval_expression(operand, frame, module))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(eval_operation(*operation, &values, span)?)
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
                match base {
                    Value::Object(fields) => Ok(fields.get(field).cloned().ok_or_else(|| {
                        runtime_error(&format!("object has no field '{field}'"), span)
                    })?),
                    Value::Bytes(bytes) if field == "values" => Ok(Value::List(
                        bytes
                            .into_iter()
                            .map(|value| Value::Int(i64::from(value)))
                            .collect(),
                    )),
                    Value::Error {
                        message,
                        kind,
                        code,
                        cause,
                    } => match field.as_str() {
                        "message" => Ok(Value::String(message)),
                        "kind" => Ok(Value::String(kind)),
                        "code" => Ok(Value::Int(code)),
                        "cause" => Ok(cause
                            .map(|value| *value)
                            .ok_or_else(|| runtime_error("error has no cause", span))?),
                        _ => Err(
                            runtime_error(&format!("error has no field '{field}'"), span).into(),
                        ),
                    },
                    value => Err(type_error("object", &value, span).into()),
                }
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
                Ok(Value::List(output))
            }
            CompiledExpression::Shell(shell) => {
                Ok(Value::Shell(self.eval_shell_plan(shell, frame, module)?))
            }
            CompiledExpression::MixedShell(shell) => Ok(Value::MixedShell(MixedShellValue {
                plan: shell.clone(),
                captured: frame.clone(),
                module,
                span: shell.span.clone(),
            })),
            CompiledExpression::CommandSubstitution(shell) => Ok(Value::String(
                self.execute_command_substitution(shell, frame, module)?,
            )),
            CompiledExpression::ShellProgram { body, span } => {
                Ok(Value::ShellProgram(ShellProgramValue {
                    body: body.clone(),
                    captured: frame.clone(),
                    module,
                    span: span.clone(),
                }))
            }
        }
    }

    fn execute_native_intrinsic(
        &mut self,
        intrinsic: NativeIntrinsic,
        args: &[Value],
        span: &Span,
    ) -> Result<Value, RuntimeFault> {
        match intrinsic {
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
                        match self.tasks.status(*handle) {
                            TaskStatus::Ready(result) => return result,
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
                                return Err(RuntimeFault::Fatal(runtime_error(
                                    "race received an unknown promise",
                                    span,
                                )))
                            }
                        }
                    }
                    if !pending {
                        return Err(RuntimeFault::Fatal(runtime_error(
                            "race scheduler made no progress",
                            span,
                        )));
                    }
                    self.tick_one()?;
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
                    .tasks
                    .created_at(handle)
                    .unwrap_or_else(std::time::Instant::now);
                let limit = std::time::Duration::from_millis(millis);
                loop {
                    match self.tasks.status(handle) {
                        TaskStatus::Ready(result) => {
                            if started.elapsed() > limit {
                                return Err(runtime_error(
                                    &format!("promise timed out after {millis} ms"),
                                    span,
                                )
                                .into());
                            }
                            return result;
                        }
                        TaskStatus::Pending => {}
                        TaskStatus::Running => {
                            // The promise is suspended further down this
                            // stack, so it cannot finish while we wait.
                            if started.elapsed() >= limit {
                                return Err(runtime_error(
                                    &format!("promise timed out after {millis} ms"),
                                    span,
                                )
                                .into());
                            }
                            return Err(runtime_error("promise await cycle detected", span).into());
                        }
                        TaskStatus::Cancelled => {
                            return Err(runtime_error("promise was cancelled", span).into())
                        }
                        TaskStatus::Unknown => {
                            return Err(RuntimeFault::Fatal(runtime_error(
                                "unknown promise handle",
                                span,
                            )))
                        }
                    }
                    if started.elapsed() >= limit {
                        return Err(runtime_error(
                            &format!("promise timed out after {millis} ms"),
                            span,
                        )
                        .into());
                    }
                    let Some((next_handle, invocation)) = self.tasks.next_pending() else {
                        return Err(RuntimeFault::Fatal(runtime_error(
                            "promise scheduler made no progress",
                            span,
                        )));
                    };
                    self.run_task(next_handle, invocation)?;
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
            | NativeIntrinsic::DataInspect) => self.execute_data_intrinsic(intrinsic, args, span),
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
                        .map(Value::List),
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
                            return Ok(Value::List(rows));
                        }
                        let table = TableValue::from_records(rows).map_err(|error| {
                            runtime_error(
                                &format!("map over Table must produce Record rows: {error}"),
                                span,
                            )
                        })?;
                        Ok(Value::Table(table))
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
                        Ok(Value::List(output))
                    }
                    Value::Table(table) => {
                        let mut rows = Vec::new();
                        for value in table.rows().iter().cloned() {
                            if self.invoke_predicate(&predicate, value.clone(), span)? {
                                rows.push(value);
                            }
                        }
                        Ok(Value::Table(TableValue::with_schema(
                            rows,
                            table.schema().clone(),
                        )))
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
                    Value::Table(table) => Ok(Value::Table(table.take(count))),
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
                    Value::Table(table) => Ok(Value::Table(table.skip(count))),
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
                Ok(Value::List(values))
            }
            NativeIntrinsic::DataCollectTable => match source {
                Value::Table(table) => Ok(Value::Table(table.clone())),
                _ => {
                    let (_, rows) = self.materialize_data_sequence(source, span)?;
                    let table = TableValue::from_records(rows).map_err(|error| {
                        runtime_error(&format!("collectTable requires Record rows: {error}"), span)
                    })?;
                    Ok(Value::Table(table))
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
                    output.push((key, Value::Table(table)));
                }
                Ok(Value::Map(output))
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
                    Ok(Value::List(output))
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
                        entries
                            .iter()
                            .find(|(existing, _)| existing == key)
                            .map(|(_, value)| value.clone())
                            .ok_or_else(|| {
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
                        .map(Value::List),
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
                        Ok(Value::Table(table))
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
                Value::Table(table) => Ok(Value::Schema(table.schema().clone())),
                Value::List(values) => {
                    Schema::infer_records(values)
                        .map(Value::Schema)
                        .map_err(|error| {
                            runtime_error(&format!("schema requires Record rows: {error}"), span)
                                .into()
                        })
                }
                Value::Resource(_) => {
                    let (_, rows) = self.materialize_data_sequence(source, span)?;
                    Schema::infer_records(&rows)
                        .map(Value::Schema)
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
            NativeIntrinsic::DataInspect => Ok(source.clone()),
            NativeIntrinsic::PromiseRace | NativeIntrinsic::PromiseTimeout => unreachable!(),
        }
    }

    fn invoke_data_callable(
        &mut self,
        callable: &Value,
        arguments: Vec<Value>,
        span: &Span,
    ) -> Result<Value, RuntimeFault> {
        match callable.clone() {
            Value::Closure(closure) => self.call_closure(closure, arguments, span),
            Value::Function(function) => {
                if self.function_is_async(function)? {
                    return Err(runtime_error(
                        "structured data callbacks must be synchronous",
                        span,
                    )
                    .into());
                }
                self.call_function(function, arguments)
            }
            other => Err(type_error("fn", &other, span).into()),
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
                Ok(table) => Value::Table(table),
                Err(_) => Value::List(values),
            }
        } else {
            Value::List(values)
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
            Value::List(values) => Ok((DataSequenceShape::List, values.clone())),
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
            DataSequenceShape::List => Ok(Value::List(values)),
            DataSequenceShape::Table(schema) => {
                Ok(Value::Table(TableValue::with_schema(values, schema)))
            }
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
        Ok(Value::Object(selected))
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
                    self.shell_exit = true;
                    break;
                }
                spar_command::Step::Command(command) if command.program == "cd" => {
                    outcome = self.execute_cd_builtin(command, span)?;
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
                    self.last_job = Some(Value::Object(indexmap::IndexMap::from([
                        ("id".into(), Value::Int(id as i64)),
                        ("pid".into(), Value::Int(i64::from(pid))),
                        ("processGroup".into(), Value::Int(i64::from(pid))),
                        ("state".into(), Value::String("running".into())),
                    ])));
                    self.jobs.push(job);
                    outcome = crate::evaluator::ShellPlanOutcome {
                        success: true,
                        exit_code: 0,
                        signal: None,
                        pid,
                        pipeline: vec![],
                    };
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
                    self.last_job = Some(Value::Object(indexmap::IndexMap::from([
                        ("id".into(), Value::Int(id as i64)),
                        ("pid".into(), Value::Int(i64::from(pid))),
                        ("processGroup".into(), Value::Int(i64::from(pid))),
                        ("state".into(), Value::String("running".into())),
                    ])));
                    self.jobs.push(job);
                    outcome = crate::evaluator::ShellPlanOutcome {
                        success: true,
                        exit_code: 0,
                        signal: None,
                        pid,
                        pipeline: vec![],
                    };
                    continue;
                }
                spar_command::Step::Command(command) => {
                    spar_process::run_command(command, &options)
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
        let mut env = Vec::with_capacity(command.environment.len());
        for (key, value) in &command.environment {
            env.push(spar_command::EnvironmentOverride {
                key: key.clone(),
                value: self.eval_shell_word(value, frame, module)?,
            });
        }
        Ok(spar_command::CommandPlan {
            program: self.eval_shell_word(&command.program, frame, module)?,
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
        if state.results.contains_key(&module) {
            return Ok(());
        }
        let compiled = self.program.modules.get(module.0 as usize).ok_or_else(|| {
            runtime_error(&format!("unknown module ID {}", module.0), &Span::dummy())
        })?;
        let base_dir = compiled
            .identity
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let (mut result, pending_promises) = crate::Evaluator::evaluate_for_runtime(
            &compiled.checked.program,
            &compiled.checked.symbols,
            &compiled.checked.imports,
            base_dir,
            state.hosts.clone(),
            state.natives.clone(),
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
            let handle = self.tasks.spawn(
                function.id,
                pending
                    .arguments
                    .into_iter()
                    .map(Value::from_config)
                    .collect(),
            );
            replacements.insert(pending.handle, handle);
        }
        remap_promises_in_result(&mut result, &replacements);
        self.state
            .as_mut()
            .ok_or_else(|| module_state_error(&Span::dummy()))?
            .results
            .insert(module, result);
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
            let process_value = |process: spar_process::ProcessStatus| {
                let mut fields = indexmap::IndexMap::from([
                    ("code".into(), Value::Int(i64::from(process.code))),
                    ("success".into(), Value::Bool(process.success)),
                    ("pid".into(), Value::Int(i64::from(process.pid))),
                ]);
                if let Some(signal) = process.signal {
                    fields.insert("signal".into(), Value::Int(i64::from(signal)));
                }
                Value::Object(fields)
            };
            let pipeline = outcome.pipeline.into_iter().map(process_value).collect();
            let mut fields = indexmap::IndexMap::from([
                ("code".into(), Value::Int(i64::from(outcome.exit_code))),
                ("success".into(), Value::Bool(outcome.success)),
                ("pid".into(), Value::Int(i64::from(outcome.pid))),
                ("pipeline".into(), Value::List(pipeline)),
            ]);
            if let Some(signal) = outcome.signal {
                fields.insert("signal".into(), Value::Int(i64::from(signal)));
            }
            return Ok(Value::Object(fields));
        }
        if name == "lastJob" && self.shell_depth > 0 {
            return self
                .last_job
                .clone()
                .ok_or_else(|| runtime_error("no background job has been started", span).into());
        }
        self.ensure_module(module)?;
        Ok(Value::from_config(
            self.state
                .as_ref()
                .and_then(|state| state.results.get(&module))
                .and_then(|result| result.globals.get(name))
                .cloned()
                .ok_or_else(|| runtime_error(&format!("global '{name}' is unavailable"), span))?,
        ))
    }

    fn write_global(
        &mut self,
        module: crate::compiled::ModuleId,
        name: &str,
        value: Value,
        span: &Span,
    ) -> Result<(), RuntimeFault> {
        self.ensure_module(module)?;
        let result = self
            .state
            .as_mut()
            .and_then(|state| state.results.get_mut(&module))
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
        let result = self
            .state
            .as_ref()
            .and_then(|state| state.results.get(&module))
            .ok_or_else(|| module_state_error(span))?;
        let value = match path {
            [name] => result.globals.get(name).cloned().or_else(|| {
                result
                    .sections
                    .get(std::slice::from_ref(name))
                    .cloned()
                    .map(ConfigValue::Section)
            }),
            [section @ .., field] => result
                .sections
                .get(section)
                .and_then(|fields| fields.get(field))
                .cloned(),
            [] => None,
        }
        .ok_or_else(|| {
            runtime_error(
                &format!("imported path '{}' is unavailable", path.join("::")),
                span,
            )
        })?;
        Ok(Value::from_config(value))
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
            let options = spar_process::ExecutionOptions {
                capture_stdout: true,
                capture_stderr: true,
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
            let process_value = |process: spar_process::ProcessStatus| {
                let mut fields = indexmap::IndexMap::from([
                    ("code".into(), ConfigValue::Int(i64::from(process.code))),
                    ("success".into(), ConfigValue::Bool(process.success)),
                    ("pid".into(), ConfigValue::Int(i64::from(process.pid))),
                ]);
                if let Some(signal) = process.signal {
                    fields.insert("signal".into(), ConfigValue::Int(i64::from(signal)));
                }
                ConfigValue::Section(fields)
            };
            let status = structured_status.unwrap_or(spar_process::PipelineStatus {
                code: exit_code,
                success,
                processes: vec![],
            });
            let status_value = ConfigValue::Section(indexmap::IndexMap::from([
                ("code".into(), ConfigValue::Int(i64::from(status.code))),
                ("success".into(), ConfigValue::Bool(status.success)),
                (
                    "processes".into(),
                    ConfigValue::List(status.processes.into_iter().map(process_value).collect()),
                ),
            ]));
            Ok::<ConfigValue, SparError>(ConfigValue::Section(indexmap::IndexMap::from([
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
    for fields in result.sections.values_mut() {
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
        ConfigValue::Section(fields) => {
            for value in fields.values_mut() {
                remap_promises(value, replacements);
            }
        }
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
        TypedOperation::IntAdd => {
            binary!(Value::Int(a), Value::Int(b) => Value::Int(a + b))
        }
        TypedOperation::FloatAdd => {
            binary!(Value::Float(a), Value::Float(b) => Value::Float(a + b))
        }
        TypedOperation::StringConcat => {
            binary!(Value::String(a), Value::String(b) => Value::String(format!("{a}{b}")))
        }
        TypedOperation::ShellConcat => {
            binary!(Value::Shell(a), Value::Shell(b) => Value::Shell(a.clone().then(b.clone())))
        }
        TypedOperation::IntSub => {
            binary!(Value::Int(a), Value::Int(b) => Value::Int(a - b))
        }
        TypedOperation::FloatSub => {
            binary!(Value::Float(a), Value::Float(b) => Value::Float(a - b))
        }
        TypedOperation::IntMul => {
            binary!(Value::Int(a), Value::Int(b) => Value::Int(a * b))
        }
        TypedOperation::FloatMul => {
            binary!(Value::Float(a), Value::Float(b) => Value::Float(a * b))
        }
        TypedOperation::IntDiv => match values {
            [Value::Int(_), Value::Int(0)] => Err(SparError::EvalError {
                message: "division by zero".into(),
                span: span.clone(),
            }),
            [Value::Int(a), Value::Int(b)] => Ok(Value::Int(a / b)),
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
            [Value::Int(value)] => Ok(Value::Int(-value)),
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
        let program = crate::CompiledProgram::from_compilation(compilation, options)
            .expect("interactive preview helper should lower");
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
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
        );
        assert!(preview.stream_preview);
        assert!(preview.truncated);
        assert_eq!(pulls.load(std::sync::atomic::Ordering::SeqCst), 4);
    }

    #[test]
    fn internal_unknown_function_and_operation_mismatch_are_diagnostics() {
        let program = crate::Engine::default()
            .compile_source("function main() -> int { return 0; };")
            .unwrap();
        let unknown = Runtime {
            program: &program,
            call_depth: 0,
            state: None,
            tasks: TaskTable::default(),
            shell_depth: 0,
            shell_outcome: None,
            jobs: Vec::new(),
            last_job: None,
            shell_exit: false,
            shell_cwd: None,
            context: RuntimeContext::for_base_dir(&program.options.base_dir),
        }
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
