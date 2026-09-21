use std::path::{Path, PathBuf};

use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeMethod, NativeRegistry, Value};

use super::support::{error, int_arg, object, string_arg, string_list_arg};

fn option(inner: SparType) -> SparType {
    SparType::Applied {
        name: "Option".into(),
        arguments: vec![inner],
    }
}

struct ProcessStreamHandle {
    stream: spar_process::ProcessStream,
    consumed_output: bool,
}

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry
        .register(NativeFunction::sync(
            "nativeProcess",
            "args",
            vec![],
            SparType::List(Box::new(SparType::Str)),
            true,
            |context, _args| {
                Ok(Value::List(
                    context.args().iter().cloned().map(Value::String).collect(),
                ))
            },
        ))
        .expect("nativeProcess::args registration must be unique");

    registry
        .register(NativeFunction::sync(
            "nativeProcess",
            "which",
            vec![("program", SparType::Str)],
            SparType::Str,
            true,
            |context, args| {
                Ok(Value::String(
                    which(context, string_arg(args, 0, "program")?)
                        .map(|path| path.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                ))
            },
        ))
        .expect("nativeProcess::which registration must be unique");

    registry
        .register(NativeFunction::sync(
            "nativeProcess",
            "command",
            vec![
                ("program", SparType::Str),
                ("args", SparType::List(Box::new(SparType::Str))),
            ],
            SparType::Named("Command".into()),
            true,
            |_context, args| {
                Ok(command_value(
                    string_arg(args, 0, "program")?,
                    string_list_arg(args, 1, "args")?,
                ))
            },
        ))
        .expect("nativeProcess::command registration must be unique");

    registry
        .register(NativeFunction::sync(
            "nativeProcess",
            "run",
            vec![
                ("program", SparType::Str),
                ("args", SparType::List(Box::new(SparType::Str))),
            ],
            SparType::Named("ProcessResult".into()),
            true,
            |context, args| {
                let command = command_plan(context, args)?;
                run_command_value(context, &command)
            },
        ))
        .expect("nativeProcess::run registration must be unique");

    registry
        .register(NativeFunction::sync(
            "nativeProcess",
            "stream",
            vec![
                ("program", SparType::Str),
                ("args", SparType::List(Box::new(SparType::Str))),
            ],
            SparType::Named("ProcessStream".into()),
            true,
            |context, args| {
                let command = command_plan(context, args)?;
                start_stream_value(context, &command)
            },
        ))
        .expect("nativeProcess::stream registration must be unique");

    registry
        .register(NativeFunction::sync(
            "nativeProcess",
            "spawn",
            vec![
                ("program", SparType::Str),
                ("args", SparType::List(Box::new(SparType::Str))),
            ],
            SparType::Named("Process".into()),
            true,
            |context, args| {
                let command = command_plan(context, args)?;
                let options = execution_options(context, false, false);
                let job = spar_process::spawn_background_with_options(&command, &options).map_err(
                    |error_value| error(format!("could not spawn process: {error_value}")),
                )?;
                let id = context.resources_mut().insert(job);
                Ok(Value::Resource(id))
            },
        ))
        .expect("nativeProcess::spawn registration must be unique");

    registry
        .register(NativeFunction::sync(
            "nativeProcess",
            "pid",
            vec![("process", SparType::Named("Process".into()))],
            SparType::Int,
            true,
            |context, args| {
                let id = resource_arg(args, 0, "process", "Process")?;
                let job = context
                    .resources()
                    .get::<spar_process::Job>(id)
                    .ok_or_else(|| error("process handle is no longer valid"))?;
                Ok(Value::Int(i64::from(job.pid())))
            },
        ))
        .expect("nativeProcess::pid registration must be unique");

    registry
        .register(NativeFunction::sync(
            "nativeProcess",
            "wait",
            vec![("process", SparType::Named("Process".into()))],
            SparType::Named("ProcessStatus".into()),
            true,
            |context, args| {
                let id = resource_arg(args, 0, "process", "Process")?;
                let mut job = context
                    .resources_mut()
                    .remove::<spar_process::Job>(id)
                    .ok_or_else(|| error("process handle is no longer valid"))?;
                let status = job.wait().map_err(|error_value| {
                    error(format!("could not wait for process: {error_value}"))
                })?;
                Ok(process_status_value(status))
            },
        ))
        .expect("nativeProcess::wait registration must be unique");

    registry
        .register(NativeFunction::sync(
            "nativeProcess",
            "kill",
            vec![("process", SparType::Named("Process".into()))],
            SparType::Void,
            true,
            |context, args| {
                let id = resource_arg(args, 0, "process", "Process")?;
                let job = context
                    .resources_mut()
                    .get_mut::<spar_process::Job>(id)
                    .ok_or_else(|| error("process handle is no longer valid"))?;
                job.kill().map_err(|error_value| {
                    error(format!("could not kill process: {error_value}"))
                })?;
                Ok(Value::Void)
            },
        ))
        .expect("nativeProcess::kill registration must be unique");

    registry
        .register(NativeFunction::sync(
            "nativeProcess",
            "exit",
            vec![("code", SparType::Int)],
            SparType::Void,
            true,
            |context, args| {
                let code = int_arg(args, 0, "code")?;
                let code = i32::try_from(code)
                    .map_err(|_| error("process exit code must fit in a signed 32-bit integer"))?;
                context.request_exit(code);
                Ok(Value::Void)
            },
        ))
        .expect("nativeProcess::exit registration must be unique");

    register_command_methods(registry);
    register_process_stream_methods(registry);
}

fn register_command_methods(registry: &mut NativeRegistry) {
    let command = SparType::Named("Command".into());
    registry
        .register_method(NativeMethod::sync(
            "Command",
            "run",
            command.clone(),
            vec![],
            SparType::Named("ProcessResult".into()),
            false,
            |context, args| {
                let plan = command_plan_from_value(context, args.first(), "Command.run")?;
                run_command_value(context, &plan)
            },
        ))
        .expect("Command.run registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "Command",
            "stream",
            command,
            vec![],
            SparType::Named("ProcessStream".into()),
            false,
            |context, args| {
                let plan = command_plan_from_value(context, args.first(), "Command.stream")?;
                start_stream_value(context, &plan)
            },
        ))
        .expect("Command.stream registration must be unique");
}

fn register_process_stream_methods(registry: &mut NativeRegistry) {
    let stream = SparType::Named("ProcessStream".into());
    registry
        .register_method(NativeMethod::sync(
            "ProcessStream",
            "pid",
            stream.clone(),
            vec![],
            SparType::Int,
            false,
            |context, args| {
                let id = resource_arg(args, 0, "stream", "ProcessStream")?;
                let stream = context
                    .resources()
                    .get::<ProcessStreamHandle>(id)
                    .ok_or_else(|| error("process stream handle is no longer valid"))?;
                Ok(Value::Int(i64::from(stream.stream.pid())))
            },
        ))
        .expect("ProcessStream.pid registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "ProcessStream",
            "next",
            stream.clone(),
            vec![],
            option(SparType::Named("ProcessChunk".into())),
            false,
            |context, args| {
                let id = resource_arg(args, 0, "stream", "ProcessStream")?;
                let stream = context
                    .resources_mut()
                    .get_mut::<ProcessStreamHandle>(id)
                    .ok_or_else(|| error("process stream handle is no longer valid"))?;
                let chunk = stream.stream.next_chunk().map_err(|error_value| {
                    error(format!("could not read process stream: {error_value}"))
                })?;
                if chunk.is_some() {
                    stream.consumed_output = true;
                }
                Ok(Value::Option(
                    chunk.map(|chunk| Box::new(process_chunk_value(chunk))),
                ))
            },
        ))
        .expect("ProcessStream.next registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "ProcessStream",
            "cancel",
            stream.clone(),
            vec![],
            SparType::Void,
            false,
            |context, args| {
                let id = resource_arg(args, 0, "stream", "ProcessStream")?;
                let stream = context
                    .resources_mut()
                    .get_mut::<ProcessStreamHandle>(id)
                    .ok_or_else(|| error("process stream handle is no longer valid"))?;
                stream.stream.cancel().map_err(|error_value| {
                    error(format!("could not cancel process stream: {error_value}"))
                })?;
                Ok(Value::Void)
            },
        ))
        .expect("ProcessStream.cancel registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "ProcessStream",
            "wait",
            stream.clone(),
            vec![],
            SparType::Named("PipelineStatus".into()),
            false,
            |context, args| {
                let id = resource_arg(args, 0, "stream", "ProcessStream")?;
                let mut handle = context
                    .resources_mut()
                    .remove::<ProcessStreamHandle>(id)
                    .ok_or_else(|| error("process stream handle is no longer valid"))?;
                while handle
                    .stream
                    .next_chunk()
                    .map_err(|error_value| {
                        error(format!("could not drain process stream: {error_value}"))
                    })?
                    .is_some()
                {}
                let status = handle.stream.wait().map_err(|error_value| {
                    error(format!("could not wait for process stream: {error_value}"))
                })?;
                Ok(pipeline_status_value(&status))
            },
        ))
        .expect("ProcessStream.wait registration must be unique");

    registry
        .register_method(NativeMethod::sync(
            "ProcessStream",
            "collect",
            stream,
            vec![],
            SparType::Named("ProcessResult".into()),
            false,
            |context, args| {
                let id = resource_arg(args, 0, "stream", "ProcessStream")?;
                let handle = context
                    .resources_mut()
                    .remove::<ProcessStreamHandle>(id)
                    .ok_or_else(|| error("process stream handle is no longer valid"))?;
                if handle.consumed_output {
                    return Err(error(
                        "cannot collect a complete ProcessResult after consuming stream chunks; use wait() after next()",
                    ));
                }
                let result = handle.stream.collect().map_err(|error_value| {
                    error(format!("could not collect process stream: {error_value}"))
                })?;
                Ok(stream_result_value(result))
            },
        ))
        .expect("ProcessStream.collect registration must be unique");
}

fn command_value(program: &str, args: Vec<String>) -> Value {
    object([
        ("program", Value::String(program.to_string())),
        (
            "args",
            Value::List(args.into_iter().map(Value::String).collect()),
        ),
    ])
}

fn command_plan(
    context: &crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<spar_command::CommandPlan, crate::SparError> {
    command_plan_from_parts(
        context,
        string_arg(args, 0, "program")?,
        string_list_arg(args, 1, "args")?,
    )
}

fn command_plan_from_value(
    context: &crate::runtime::RuntimeContext,
    value: Option<&Value>,
    operation: &str,
) -> Result<spar_command::CommandPlan, crate::SparError> {
    let Some(Value::Object(fields)) = value else {
        return Err(error(format!("{operation} expected a Command value")));
    };
    let program = match fields.get("program") {
        Some(Value::String(value)) => value.as_str(),
        _ => {
            return Err(error(format!(
                "{operation} received an invalid Command.program"
            )))
        }
    };
    let args = match fields.get("args") {
        Some(Value::List(values)) => values
            .iter()
            .map(|value| match value {
                Value::String(value) => Ok(value.clone()),
                _ => Err(error(format!(
                    "{operation} received an invalid Command.args"
                ))),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            return Err(error(format!(
                "{operation} received an invalid Command.args"
            )))
        }
    };
    command_plan_from_parts(context, program, args)
}

fn command_plan_from_parts(
    context: &crate::runtime::RuntimeContext,
    program: &str,
    args: Vec<String>,
) -> Result<spar_command::CommandPlan, crate::SparError> {
    Ok(spar_command::CommandPlan {
        program: program.to_string(),
        args,
        env: vec![],
        cwd: Some(spar_command::WorkingDirectory::Path(
            context.cwd().to_string_lossy().into_owned(),
        )),
        stdin: None,
        stdout: None,
        stderr: None,
        redirections: vec![],
        background: false,
    })
}

fn run_command_value(
    context: &crate::runtime::RuntimeContext,
    command: &spar_command::CommandPlan,
) -> Result<Value, crate::SparError> {
    let options = execution_options(context, true, true);
    let output = spar_process::run_command(command, &options)
        .map_err(|error_value| error(format!("could not execute process: {error_value}")))?;
    Ok(process_result_value(output))
}

fn start_stream_value(
    context: &mut crate::runtime::RuntimeContext,
    command: &spar_command::CommandPlan,
) -> Result<Value, crate::SparError> {
    let options = spar_process::StreamingOptions {
        environment: Some(context.environment_pairs()),
        ..spar_process::StreamingOptions::default()
    };
    let stream = spar_process::stream_command(command, &options)
        .map_err(|error_value| error(format!("could not start process stream: {error_value}")))?;
    let id = context.resources_mut().insert(ProcessStreamHandle {
        stream,
        consumed_output: false,
    });
    Ok(Value::Resource(id))
}

fn execution_options(
    context: &crate::runtime::RuntimeContext,
    capture_stdout: bool,
    capture_stderr: bool,
) -> spar_process::ExecutionOptions {
    spar_process::ExecutionOptions {
        capture_stdout,
        capture_stderr,
        environment: Some(context.environment_pairs()),
    }
}

fn resource_arg(
    args: &[Value],
    index: usize,
    name: &str,
    expected: &str,
) -> Result<crate::runtime::ResourceId, crate::SparError> {
    match args.get(index) {
        Some(Value::Resource(id)) => Ok(*id),
        Some(value) => Err(error(format!(
            "native argument '{name}' expected {expected}, received {}",
            value.type_name()
        ))),
        None => Err(error(format!("missing native argument '{name}'"))),
    }
}

fn process_chunk_value(chunk: spar_process::ProcessOutputChunk) -> Value {
    let (source, bytes) = match chunk {
        spar_process::ProcessOutputChunk::Stdout(bytes) => ("stdout", bytes),
        spar_process::ProcessOutputChunk::Stderr(bytes) => ("stderr", bytes),
    };
    object([
        ("source", Value::String(source.into())),
        ("bytes", Value::Bytes(bytes)),
    ])
}

fn process_result_value(output: spar_process::CommandOutput) -> Value {
    let status = output
        .pipeline_status
        .unwrap_or_else(|| spar_process::PipelineStatus {
            code: output
                .status
                .code
                .unwrap_or(if output.status.success { 0 } else { 1 }),
            success: output.status.success,
            processes: Vec::new(),
        });
    process_result_fields(
        status,
        output.stdout.unwrap_or_default(),
        output.stderr.unwrap_or_default(),
    )
}

fn stream_result_value(output: spar_process::ProcessResult) -> Value {
    process_result_fields(output.status, output.stdout, output.stderr)
}

fn process_result_fields(
    status: spar_process::PipelineStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
) -> Value {
    let status_value = pipeline_status_value(&status);
    object([
        ("success", Value::Bool(status.success)),
        ("exitCode", Value::Int(i64::from(status.code))),
        ("status", status_value),
        ("stdout", Value::Bytes(stdout)),
        ("stderr", Value::Bytes(stderr)),
    ])
}

fn pipeline_status_value(status: &spar_process::PipelineStatus) -> Value {
    object([
        ("code", Value::Int(i64::from(status.code))),
        ("success", Value::Bool(status.success)),
        (
            "processes",
            Value::List(
                status
                    .processes
                    .iter()
                    .cloned()
                    .map(process_status_value)
                    .collect(),
            ),
        ),
    ])
}

fn process_status_value(status: spar_process::ProcessStatus) -> Value {
    let mut fields = indexmap::IndexMap::from([
        ("code".to_string(), Value::Int(i64::from(status.code))),
        ("success".to_string(), Value::Bool(status.success)),
        ("pid".to_string(), Value::Int(i64::from(status.pid))),
        ("pipeline".to_string(), Value::List(Vec::new())),
    ]);
    if let Some(signal) = status.signal {
        fields.insert("signal".into(), Value::Int(i64::from(signal)));
    }
    Value::Object(fields)
}

fn which(context: &crate::runtime::RuntimeContext, program: &str) -> Option<PathBuf> {
    let candidate = Path::new(program);
    if candidate.components().count() > 1 {
        let resolved = context.resolve_path(candidate);
        return resolved.is_file().then_some(resolved);
    }
    let path = context.env_get("PATH")?;
    std::env::split_paths(path)
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}
