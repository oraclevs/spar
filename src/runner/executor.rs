use std::io::{BufRead, Write};
use std::path::PathBuf;

#[cfg(test)]
use super::no_expr_eval;
use super::{no_native_eval, ExecutionPlan, ExprEval, NativeEval, RunnerError, TaskCommand};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionOptions {
    pub dry_run: bool,
    pub base_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionReport {
    pub commands: Vec<String>,
}

pub fn execute(
    plan: &ExecutionPlan,
    options: &ExecutionOptions,
    exprs: &ExprEval,
) -> Result<ExecutionReport, RunnerError> {
    execute_with_native(plan, options, exprs, &no_native_eval)
}

/// Like [`execute`], for plans that may contain native (`run { }`) blocks.
pub fn execute_with_native(
    plan: &ExecutionPlan,
    options: &ExecutionOptions,
    exprs: &ExprEval,
    native: &NativeEval,
) -> Result<ExecutionReport, RunnerError> {
    execute_with_io(
        plan,
        options,
        exprs,
        native,
        &mut std::io::stdin().lock(),
        &mut std::io::stderr().lock(),
    )
}

#[cfg(test)]
fn execute_with_echo(
    plan: &ExecutionPlan,
    options: &ExecutionOptions,
    echo: &mut dyn Write,
) -> Result<ExecutionReport, RunnerError> {
    execute_with_io(
        plan,
        options,
        &no_expr_eval,
        &no_native_eval,
        &mut std::io::empty(),
        echo,
    )
}

fn execute_with_io(
    plan: &ExecutionPlan,
    options: &ExecutionOptions,
    exprs: &ExprEval,
    native: &NativeEval,
    input: &mut dyn BufRead,
    echo: &mut dyn Write,
) -> Result<ExecutionReport, RunnerError> {
    if !options.dry_run {
        for bound_task in &plan.tasks {
            if let Some(message) = &bound_task.task.confirm {
                let _ = write!(echo, "{message} [y/N] ");
                let _ = echo.flush();
                let mut response = String::new();
                let accepted = input.read_line(&mut response).is_ok_and(|read| read > 0)
                    && matches!(response.trim().to_ascii_lowercase().as_str(), "y" | "yes");
                if !accepted {
                    return Err(RunnerError::Aborted {
                        task: bound_task.task.name.clone(),
                    });
                }
            }
        }
    }

    let mut commands = Vec::new();

    for bound_task in &plan.tasks {
        for task_command in &bound_task.task.commands {
            if let TaskCommand::Native(native_command) = task_command {
                let source = native_command.source.clone();
                commands.push(source.clone());
                if !bound_task.task.quiet || options.dry_run {
                    let _ = writeln!(echo, "{source}");
                }
                if options.dry_run {
                    continue;
                }
                let cwd = bound_task.task.cwd.as_ref().map(|cwd| {
                    if cwd.is_relative() {
                        options.base_dir.join(cwd)
                    } else {
                        cwd.clone()
                    }
                });
                let code = native(
                    native_command.id,
                    &bound_task.parameter_values,
                    &bound_task.task.environment,
                    cwd.as_deref(),
                )
                .map_err(|message| RunnerError::CommandExecution {
                    task: bound_task.task.name.clone(),
                    command: source.clone(),
                    message,
                })?;
                if code != 0 {
                    return Err(RunnerError::NativeBlockFailed {
                        task: bound_task.task.name.clone(),
                        source_line: bound_task.task.source_line,
                        code,
                    });
                }
                continue;
            }
            let script = task_command
                .render(&bound_task.parameter_values, exprs)
                .map_err(|message| RunnerError::TaskExprFailed {
                    task: bound_task.task.name.clone(),
                    message,
                })?;
            commands.push(script.clone());

            if !bound_task.task.quiet || options.dry_run {
                let _ = writeln!(echo, "{script}");
            }

            if options.dry_run {
                continue;
            }

            let mut script_file = None;
            let mut child = match task_command {
                TaskCommand::Native(_) => unreachable!("native commands are handled above"),
                TaskCommand::Bash(_) => super::shell::command(&script),
                TaskCommand::BashScript(_) => {
                    let mut file = tempfile::NamedTempFile::new().map_err(|error| {
                        RunnerError::CommandExecution {
                            task: bound_task.task.name.clone(),
                            command: script.clone(),
                            message: error.to_string(),
                        }
                    })?;
                    file.write_all(script.as_bytes())
                        .and_then(|_| file.flush())
                        .map_err(|error| RunnerError::CommandExecution {
                            task: bound_task.task.name.clone(),
                            command: script.clone(),
                            message: error.to_string(),
                        })?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(
                            file.path(),
                            std::fs::Permissions::from_mode(0o700),
                        )
                        .map_err(|error| {
                            RunnerError::CommandExecution {
                                task: bound_task.task.name.clone(),
                                command: script.clone(),
                                message: error.to_string(),
                            }
                        })?;
                    }
                    let path = file.into_temp_path();
                    let child =
                        super::shell::script_command(&path, &script).map_err(|message| {
                            RunnerError::InvalidShebang {
                                task: bound_task.task.name.clone(),
                                message,
                            }
                        })?;
                    script_file = Some(path);
                    child
                }
            };
            super::environment::apply(&mut child, &bound_task.task.environment);
            if let Some(cwd) = &bound_task.task.cwd {
                child.current_dir(if cwd.is_relative() {
                    options.base_dir.join(cwd)
                } else {
                    cwd.clone()
                });
            }
            let status = child
                .status()
                .map_err(|error| RunnerError::CommandExecution {
                    task: bound_task.task.name.clone(),
                    command: script.clone(),
                    message: error.to_string(),
                })?;
            drop(script_file);
            if !status.success() {
                if bound_task.task.quiet {
                    return Err(RunnerError::QuietCommandFailed {
                        task: bound_task.task.name.clone(),
                        source_line: bound_task.task.source_line,
                        status,
                    });
                }
                return Err(RunnerError::CommandFailed {
                    task: bound_task.task.name.clone(),
                    command: script,
                    status,
                });
            }
        }
    }

    Ok(ExecutionReport { commands })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::tempdir;

    use crate::runner::{
        execute, no_expr_eval, no_native_eval, BoundTask, CommandTemplate, ExecutionOptions,
        ExecutionPlan, Task, TaskCommand, TemplatePart,
    };

    fn plan(command: String) -> ExecutionPlan {
        ExecutionPlan {
            tasks: vec![BoundTask {
                task: Task {
                    name: "Build".to_owned(),
                    source_line: None,
                    description: None,
                    default: false,
                    quiet: false,
                    private: false,
                    group: None,
                    confirm: None,
                    dependencies: Vec::new(),
                    parameters: Vec::new(),
                    environment: BTreeMap::new(),
                    cwd: None,
                    commands: vec![TaskCommand::Bash(CommandTemplate {
                        parts: vec![TemplatePart::Literal(command)],
                    })],
                },
                parameter_values: BTreeMap::new(),
            }],
        }
    }

    fn create_marker_command(path: &std::path::Path) -> String {
        format!("printf marker > '{}'", path.display())
    }

    fn append_command(value: &str, path: &std::path::Path) -> String {
        format!("printf {value} >> '{}'", path.display())
    }

    const FAILURE_COMMAND: &str = "exit 7";

    fn write_environment_command(path: &std::path::Path) -> String {
        format!(
            "printf '%s|%s' \"$SPAR_RUNNER_INHERITED\" \"$SPAR_RUNNER_OVERRIDE\" > '{}'",
            path.display()
        )
    }

    fn write_working_directory_command(path: &std::path::Path) -> String {
        format!("pwd > '{}'", path.display())
    }

    fn noisy_marker_command(path: &std::path::Path) -> String {
        format!("printf child-output; printf marker > '{}'", path.display())
    }

    #[test]
    fn executes_a_direct_ir_shell_command() {
        let directory = tempdir().unwrap();
        let marker = directory.path().join("marker");

        let report = execute(
            &plan(create_marker_command(&marker)),
            &ExecutionOptions {
                dry_run: false,
                base_dir: directory.path().to_owned(),
            },
            &no_expr_eval,
        )
        .unwrap();

        assert!(marker.exists());
        assert_eq!(report.commands, [create_marker_command(&marker)]);
    }

    #[test]
    fn executes_commands_in_declaration_order() {
        let directory = tempdir().unwrap();
        let log = directory.path().join("order");
        let first = append_command("first", &log);
        let second = append_command("second", &log);
        let mut plan = plan(first.clone());
        plan.tasks[0]
            .task
            .commands
            .push(TaskCommand::Bash(CommandTemplate {
                parts: vec![TemplatePart::Literal(second.clone())],
            }));

        let report = execute(
            &plan,
            &ExecutionOptions {
                dry_run: false,
                base_dir: directory.path().to_owned(),
            },
            &no_expr_eval,
        )
        .unwrap();

        assert_eq!(std::fs::read_to_string(log).unwrap(), "firstsecond");
        assert_eq!(report.commands, [first, second]);
    }

    #[test]
    fn reports_a_command_failure_with_its_context_and_status() {
        let directory = tempdir().unwrap();

        let error = execute(
            &plan(FAILURE_COMMAND.to_owned()),
            &ExecutionOptions {
                dry_run: false,
                base_dir: directory.path().to_owned(),
            },
            &no_expr_eval,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            crate::runner::RunnerError::CommandFailed {
                task,
                command,
                status,
            } if task == "Build" && command == FAILURE_COMMAND && status.code() == Some(7)
        ));
    }

    #[test]
    fn suppresses_dependents_after_a_command_failure() {
        let directory = tempdir().unwrap();
        let marker = directory.path().join("dependent");
        let mut plan = plan(FAILURE_COMMAND.to_owned());
        let mut dependent = plan.tasks[0].clone();
        dependent.task.name = "Deploy".to_owned();
        dependent.task.commands[0].template_mut().parts =
            vec![TemplatePart::Literal(create_marker_command(&marker))];
        plan.tasks.push(dependent);

        let result = execute(
            &plan,
            &ExecutionOptions {
                dry_run: false,
                base_dir: directory.path().to_owned(),
            },
            &no_expr_eval,
        );

        assert!(result.is_err());
        assert!(!marker.exists());
    }

    #[test]
    fn inherits_parent_environment_and_applies_task_overrides() {
        let directory = tempdir().unwrap();
        let output = directory.path().join("environment");
        std::env::set_var("SPAR_RUNNER_INHERITED", "from-parent");
        std::env::set_var("SPAR_RUNNER_OVERRIDE", "from-parent");
        let mut plan = plan(write_environment_command(&output));
        plan.tasks[0]
            .task
            .environment
            .insert("SPAR_RUNNER_OVERRIDE".to_owned(), "from-task".to_owned());

        execute(
            &plan,
            &ExecutionOptions {
                dry_run: false,
                base_dir: directory.path().to_owned(),
            },
            &no_expr_eval,
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(output).unwrap(),
            "from-parent|from-task"
        );
        std::env::remove_var("SPAR_RUNNER_INHERITED");
        std::env::remove_var("SPAR_RUNNER_OVERRIDE");
    }

    #[test]
    fn resolves_relative_task_cwd_against_base_dir() {
        let directory = tempdir().unwrap();
        let nested = directory.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let output = directory.path().join("cwd");
        let mut plan = plan(write_working_directory_command(&output));
        plan.tasks[0].task.cwd = Some("nested".into());

        execute(
            &plan,
            &ExecutionOptions {
                dry_run: false,
                base_dir: directory.path().to_owned(),
            },
            &no_expr_eval,
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(output).unwrap().trim(),
            nested.display().to_string()
        );
    }

    #[test]
    fn dry_run_spawns_nothing_and_reports_commands_in_plan_order() {
        let directory = tempdir().unwrap();
        let dependency_marker = directory.path().join("dependency");
        let requested_marker = directory.path().join("requested");
        let dependency_command = create_marker_command(&dependency_marker);
        let requested_command = create_marker_command(&requested_marker);
        let mut plan = plan(dependency_command.clone());
        let mut requested = plan.tasks[0].clone();
        requested.task.name = "Deploy".to_owned();
        requested.task.commands[0].template_mut().parts =
            vec![TemplatePart::Literal(requested_command.clone())];
        plan.tasks.push(requested);

        let report = execute(
            &plan,
            &ExecutionOptions {
                dry_run: true,
                base_dir: directory.path().to_owned(),
            },
            &no_expr_eval,
        )
        .unwrap();

        assert!(!dependency_marker.exists());
        assert!(!requested_marker.exists());
        assert_eq!(report.commands, [dependency_command, requested_command]);
    }

    #[test]
    fn quiet_hides_command_echo_without_suppressing_child_output() {
        let directory = tempdir().unwrap();
        let normal_marker = directory.path().join("normal");
        let quiet_marker = directory.path().join("quiet");
        let normal_command = noisy_marker_command(&normal_marker);
        let quiet_command = noisy_marker_command(&quiet_marker);
        let mut plan = plan(normal_command.clone());
        let mut quiet = plan.tasks[0].clone();
        quiet.task.name = "Quiet".to_owned();
        quiet.task.quiet = true;
        quiet.task.commands[0].template_mut().parts =
            vec![TemplatePart::Literal(quiet_command.clone())];
        plan.tasks.push(quiet);
        let mut echo = Vec::new();

        let report = super::execute_with_echo(
            &plan,
            &ExecutionOptions {
                dry_run: false,
                base_dir: directory.path().to_owned(),
            },
            &mut echo,
        )
        .unwrap();

        assert!(normal_marker.exists());
        assert!(quiet_marker.exists());
        assert_eq!(
            String::from_utf8(echo).unwrap(),
            format!("{normal_command}\n")
        );
        assert_eq!(report.commands, [normal_command, quiet_command]);
    }

    #[cfg(unix)]
    #[test]
    fn script_uses_its_shebang() {
        let directory = tempdir().unwrap();
        let marker = directory.path().join("script-marker");
        let script = format!(
            "#!/bin/bash\nprintf -v value script\nprintf '%s' \"$value\" > '{}'\n",
            marker.display()
        );
        let mut plan = plan("unused".to_owned());
        plan.tasks[0].task.commands = vec![TaskCommand::BashScript(CommandTemplate {
            parts: vec![TemplatePart::Literal(script.clone())],
        })];

        let report = execute(
            &plan,
            &ExecutionOptions {
                dry_run: false,
                base_dir: directory.path().to_owned(),
            },
            &no_expr_eval,
        )
        .unwrap();

        assert_eq!(std::fs::read_to_string(marker).unwrap(), "script");
        assert_eq!(report.commands, [script]);
    }

    #[test]
    fn declining_confirmation_aborts_before_any_task_runs() {
        let directory = tempdir().unwrap();
        let dependency_marker = directory.path().join("dependency");
        let requested_marker = directory.path().join("requested");
        let mut execution_plan = plan(create_marker_command(&dependency_marker));
        execution_plan.tasks[0].task.name = "Build".to_owned();
        let mut requested = plan(create_marker_command(&requested_marker))
            .tasks
            .remove(0);
        requested.task.name = "Deploy".to_owned();
        requested.task.confirm = Some("Really deploy?".to_owned());
        execution_plan.tasks.push(requested);
        let mut input = std::io::Cursor::new(b"n\n".to_vec());
        let mut output = Vec::new();

        let error = super::execute_with_io(
            &execution_plan,
            &ExecutionOptions {
                dry_run: false,
                base_dir: directory.path().to_owned(),
            },
            &no_expr_eval,
            &no_native_eval,
            &mut input,
            &mut output,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            crate::runner::RunnerError::Aborted { task } if task == "Deploy"
        ));
        assert!(!dependency_marker.exists());
        assert!(!requested_marker.exists());
        assert_eq!(String::from_utf8(output).unwrap(), "Really deploy? [y/N] ");
    }

    #[test]
    fn accepting_confirmation_runs_the_task() {
        let directory = tempdir().unwrap();
        let marker = directory.path().join("confirmed");
        let mut execution_plan = plan(create_marker_command(&marker));
        execution_plan.tasks[0].task.confirm = Some("Continue?".to_owned());
        let mut input = std::io::Cursor::new(b"yes\n".to_vec());
        let mut output = Vec::new();

        super::execute_with_io(
            &execution_plan,
            &ExecutionOptions {
                dry_run: false,
                base_dir: directory.path().to_owned(),
            },
            &no_expr_eval,
            &no_native_eval,
            &mut input,
            &mut output,
        )
        .unwrap();

        assert!(marker.exists());
        assert!(String::from_utf8(output)
            .unwrap()
            .starts_with("Continue? [y/N] "));
    }

    #[test]
    fn dry_run_does_not_prompt_for_confirmation() {
        let directory = tempdir().unwrap();
        let marker = directory.path().join("dry-confirmed");
        let mut execution_plan = plan(create_marker_command(&marker));
        execution_plan.tasks[0].task.confirm = Some("Continue?".to_owned());
        let mut input = std::io::Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        super::execute_with_io(
            &execution_plan,
            &ExecutionOptions {
                dry_run: true,
                base_dir: directory.path().to_owned(),
            },
            &no_expr_eval,
            &no_native_eval,
            &mut input,
            &mut output,
        )
        .unwrap();

        assert!(!marker.exists());
        assert!(!String::from_utf8(output).unwrap().contains("[y/N]"));
    }
}
