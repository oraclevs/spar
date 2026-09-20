use std::collections::HashMap;

pub(crate) mod context;
pub(crate) mod native;
pub(crate) mod resource;
pub(crate) mod value;

pub use context::{RuntimeContext, RuntimeInput, RuntimeOutput};
pub use native::{NativeExecutionKind, NativeFunction, NativeFunctionId, NativeIntrinsic, NativeRegistry, NativeSignature};
pub use resource::{ResourceId, ResourceTable};
pub use value::Value;

use crate::async_runtime::{RuntimeFault, TaskInvocation, TaskStatus, TaskTable};
use crate::compiled::{
    CompiledExpression, CompiledObjectItem, CompiledProgram, CompiledShellCommand,
    CompiledShellExpr, CompiledShellRedirect, CompiledShellStep, CompiledShellWord,
    CompiledShellWordPart, CompiledStatement, CompiledStringPart, FunctionId, LocalSlot,
    TypedOperation,
};
use crate::error::{Span, SparError};
use crate::evaluator::ConfigValue;

#[derive(Clone)]
pub(crate) struct Frame {
    slots: Vec<Option<Value>>,
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

    pub fn write(
        &mut self,
        slot: LocalSlot,
        value: Value,
        span: &Span,
    ) -> Result<(), SparError> {
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

    fn call_function(
        &mut self,
        id: FunctionId,
        arguments: Vec<Value>,
    ) -> Result<Value, RuntimeFault> {
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
        let result: Result<Value, RuntimeFault> = (|| {
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
            match self.execute_statements(&body, &mut frame, module)? {
                RuntimeFlow::Return(value) => Ok(value),
                RuntimeFlow::Normal => Ok(Value::Void),
                RuntimeFlow::Break | RuntimeFlow::Continue => Err(runtime_error(
                    "loop control escaped a compiled function",
                    &function_span,
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
                CompiledStatement::Expression(expression, statement_span) => {
                    let value = self.eval_expression(expression, frame, module)?;
                    if self.shell_depth > 0 {
                        let outcome = match value {
                            Value::Shell(plan) => {
                                Some(self.execute_native_shell_plan(&plan, statement_span)?)
                            }
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
                    Value::Bool(false) => {
                        self.execute_statements(else_body, frame, module)?
                    }
                    value => return Err(type_error("bool", &value, span).into()),
                },
                CompiledStatement::For {
                    index_slot,
                    value_slot,
                    iterable,
                    body,
                    span,
                } => {
                    let Value::List(items) = self.eval_expression(iterable, frame, module)?
                    else {
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
            CompiledExpression::ExecShell(shell) => self.execute_shell(shell).map(Value::from_config),
            CompiledExpression::Await { promise, span } => {
                let value = self.eval_expression(promise, frame, module)?;
                let Value::Promise(handle) = value else {
                    return Err(type_error("Promise", &value, span).into());
                };
                self.drive_promise(handle, span)
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
                let mut object = HashMap::new();
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
                    (Value::Bytes(items), Value::Int(index)) if index >= 0 => Ok(Value::Int(
                        i64::from(*items
                            .get(index as usize)
                            .ok_or_else(|| runtime_error("byte index is out of bounds", span))?),
                    )),
                    (Value::List(items), Value::Int(index)) if index >= 0 => Ok(items
                        .get(index as usize)
                        .cloned()
                        .ok_or_else(|| runtime_error("list index is out of bounds", span))?),
                    (Value::Object(mut fields), Value::Int(index)) if index >= 0 => {
                        let Some(Value::List(items)) = fields.remove("values") else {
                            return Err(runtime_error("checked index received a non-list object", span).into());
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
                    Value::Object(fields) => {
                        Ok(fields.get(field).cloned().ok_or_else(|| {
                            runtime_error(&format!("object has no field '{field}'"), span)
                        })?)
                    }
                    Value::Bytes(bytes) if field == "values" => Ok(Value::List(
                        bytes.into_iter().map(|value| Value::Int(i64::from(value))).collect(),
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
            CompiledExpression::Shell(shell) => Ok(Value::Shell(
                self.eval_shell_plan(shell, frame, module)?,
            )),
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
                                &format!("race expected Promise<T> values, received {}", other.type_name()),
                                span,
                            )),
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    Some(other) => {
                        return Err(runtime_error(
                            &format!("race expected a list of promises, received {}", other.type_name()),
                            span,
                        )
                        .into())
                    }
                    None => return Err(runtime_error("race requires a promises argument", span).into()),
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
                                return Err(runtime_error("race encountered a cancelled promise", span).into())
                            }
                            TaskStatus::Unknown => {
                                return Err(RuntimeFault::Fatal(runtime_error("race received an unknown promise", span)))
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
                    None => return Err(runtime_error("timeout requires a promise argument", span).into()),
                };
                let millis = match args.get(1) {
                    Some(Value::Int(value)) if *value >= 0 => *value as u64,
                    Some(Value::Int(_)) => {
                        return Err(runtime_error("timeout duration cannot be negative", span).into())
                    }
                    Some(other) => {
                        return Err(runtime_error(
                            &format!("timeout expected an int duration, received {}", other.type_name()),
                            span,
                        )
                        .into())
                    }
                    None => return Err(runtime_error("timeout requires a millis argument", span).into()),
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
                            return Err(runtime_error("promise await cycle detected", span).into())
                        }
                        TaskStatus::Cancelled => {
                            return Err(runtime_error("promise was cancelled", span).into())
                        }
                        TaskStatus::Unknown => {
                            return Err(RuntimeFault::Fatal(runtime_error("unknown promise handle", span)))
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
        }
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
                spar_command::Step::Command(command) => spar_process::run_command(command, &options),
                spar_command::Step::Pipeline(pipeline) => spar_process::run_pipeline(pipeline, &options),
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
            runtime_error(
                &format!("cd: '{}': {error}", candidate.display()),
                span,
            )
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
                if pipeline.commands.iter().any(|command| command.program == "cd") {
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
                    let job = spar_process::spawn_background_with_options(command, &options).map_err(|error| {
                        runtime_error(
                            &format!("could not start background command: {error}"),
                            span,
                        )
                    })?;
                    let pid = job.pid();
                    let id = self.jobs.len() + 1;
                    self.last_job = Some(Value::Object(HashMap::from([
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
                        spar_process::spawn_pipeline_background_with_options(pipeline, &options).map_err(|error| {
                            runtime_error(
                                &format!("could not start background pipeline: {error}"),
                                span,
                            )
                        })?;
                    let pid = job.pid();
                    let id = self.jobs.len() + 1;
                    self.last_job = Some(Value::Object(HashMap::from([
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

    fn execute_shell_program(
        &mut self,
        program: &ShellProgramValue,
    ) -> Result<crate::evaluator::ShellPlanOutcome, RuntimeFault> {
        let outermost = self.shell_depth == 0;
        let prior_outcome = self.shell_outcome.take();
        self.shell_outcome = None;
        let prior_cwd = if outermost { self.shell_cwd.take() } else { None };
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
        Ok(spar_command::CommandPlan {
            program: self.eval_shell_word(&command.program, frame, module)?,
            args,
            env: command
                .environment
                .iter()
                .map(|(key, value)| spar_command::EnvironmentOverride {
                    key: key.clone(),
                    value: value.clone(),
                })
                .collect(),
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
                pending.arguments.into_iter().map(Value::from_config).collect(),
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
                let mut fields = HashMap::from([
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
            let mut fields = HashMap::from([
                (
                    "code".into(),
                    Value::Int(i64::from(outcome.exit_code)),
                ),
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

    fn execute_shell(&self, shell: &crate::ast::ShellExpr) -> Result<ConfigValue, RuntimeFault> {
        let state = self
            .state
            .as_ref()
            .ok_or_else(|| module_state_error(&shell.span))?;
        let run = || {
            let plan = crate::evaluator::lower_shell_expr(shell);
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
                    span: shell.span.clone(),
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
                let mut fields = HashMap::from([
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
            let status_value = ConfigValue::Section(HashMap::from([
                ("code".into(), ConfigValue::Int(i64::from(status.code))),
                ("success".into(), ConfigValue::Bool(status.success)),
                (
                    "processes".into(),
                    ConfigValue::List(status.processes.into_iter().map(process_value).collect()),
                ),
            ]));
            Ok::<ConfigValue, SparError>(ConfigValue::Section(HashMap::from([
                ("success".into(), ConfigValue::Bool(success)),
                ("exitCode".into(), ConfigValue::Int(i64::from(exit_code))),
                ("status".into(), status_value),
                ("stdout".into(), bytes(stdout)),
                ("stderr".into(), bytes(stderr)),
            ])))
        };
        Ok(match &state.effect_ledger {
            Some(ledger) => ledger.get_or_try_run((shell.span.start, shell.span.end), run),
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
            [Value::Float(_), Value::Float(b)] if *b == 0.0 => {
                Err(SparError::EvalError {
                    message: "division by zero".into(),
                    span: span.clone(),
                })
            }
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

fn operation_type_error(
    operation: TypedOperation,
    values: &[Value],
    span: &Span,
) -> SparError {
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
