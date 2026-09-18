use std::path::{Path, PathBuf};

use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeRegistry, Value};

use super::support::{error, int_arg, object, string_arg, string_list_arg};

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
                    context
                        .args()
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
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
            "run",
            vec![
                ("program", SparType::Str),
                ("args", SparType::List(Box::new(SparType::Str))),
            ],
            SparType::Named("ProcessResult".into()),
            true,
            |context, args| {
                let command = command_plan(context, args)?;
                let options = execution_options(context, true, true);
                let output = spar_process::run_command(&command, &options)
                    .map_err(|error_value| error(format!("could not execute process: {error_value}")))?;
                Ok(process_result_value(output))
            },
        ))
        .expect("nativeProcess::run registration must be unique");

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
                let job = spar_process::spawn_background_with_options(&command, &options)
                    .map_err(|error_value| error(format!("could not spawn process: {error_value}")))?;
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
                let id = resource_arg(args, 0, "process")?;
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
                let id = resource_arg(args, 0, "process")?;
                let mut job = context
                    .resources_mut()
                    .remove::<spar_process::Job>(id)
                    .ok_or_else(|| error("process handle is no longer valid"))?;
                let status = job
                    .wait()
                    .map_err(|error_value| error(format!("could not wait for process: {error_value}")))?;
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
                let id = resource_arg(args, 0, "process")?;
                let job = context
                    .resources_mut()
                    .get_mut::<spar_process::Job>(id)
                    .ok_or_else(|| error("process handle is no longer valid"))?;
                job.kill()
                    .map_err(|error_value| error(format!("could not kill process: {error_value}")))?;
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
}

fn command_plan(
    context: &crate::runtime::RuntimeContext,
    args: &[Value],
) -> Result<spar_command::CommandPlan, crate::SparError> {
    Ok(spar_command::CommandPlan {
        program: string_arg(args, 0, "program")?.to_string(),
        args: string_list_arg(args, 1, "args")?,
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
) -> Result<crate::runtime::ResourceId, crate::SparError> {
    match args.get(index) {
        Some(Value::Resource(id)) => Ok(*id),
        Some(value) => Err(error(format!(
            "native argument '{name}' expected Process, received {}",
            value.type_name()
        ))),
        None => Err(error(format!("missing native argument '{name}'"))),
    }
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
    let status_value = pipeline_status_value(&status);
    object([
        ("success", Value::Bool(status.success)),
        ("exitCode", Value::Int(i64::from(status.code))),
        ("status", status_value),
        ("stdout", Value::Bytes(output.stdout.unwrap_or_default())),
        ("stderr", Value::Bytes(output.stderr.unwrap_or_default())),
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
    let mut fields = std::collections::HashMap::from([
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
