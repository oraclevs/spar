use std::fmt;
use std::process::ExitStatus;

use super::ScalarKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerError {
    DuplicateTaskName {
        name: String,
    },
    CliNameCollision {
        first: String,
        second: String,
        cli_name: String,
    },
    MissingDefaultTask,
    MultipleDefaultTasks {
        names: Vec<String>,
    },
    UnknownTask {
        name: String,
    },
    MissingDependency {
        task: String,
        dependency: String,
    },
    DependencyCycle {
        path: Vec<String>,
    },
    UnsupportedOperatingSystem {
        task: String,
        actual: String,
        allowed: Vec<String>,
    },
    ArgumentCount {
        task: String,
        minimum: usize,
        maximum: Option<usize>,
        actual: usize,
    },
    InvalidArgument {
        task: String,
        parameter: String,
        value: String,
        kind: ScalarKind,
    },
    InvalidShebang {
        task: String,
        message: String,
    },
    Aborted {
        task: String,
    },
    CommandExecution {
        task: String,
        command: String,
        message: String,
    },
    CommandFailed {
        task: String,
        command: String,
        status: ExitStatus,
    },
}

impl fmt::Display for RunnerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateTaskName { name } => write!(formatter, "duplicate task name: {name}"),
            Self::CliNameCollision {
                first,
                second,
                cli_name,
            } => write!(
                formatter,
                "tasks {first} and {second} collide at CLI name {cli_name}"
            ),
            Self::MissingDefaultTask => formatter.write_str("no default task is configured"),
            Self::MultipleDefaultTasks { names } => {
                write!(
                    formatter,
                    "multiple default tasks are configured: {}",
                    names.join(", ")
                )
            }
            Self::UnknownTask { name } => write!(formatter, "unknown task: {name}"),
            Self::MissingDependency { task, dependency } => {
                write!(
                    formatter,
                    "task {task} depends on unknown task {dependency}"
                )
            }
            Self::DependencyCycle { path } => {
                write!(formatter, "task dependency cycle: {}", path.join(" -> "))
            }
            Self::UnsupportedOperatingSystem {
                task,
                actual,
                allowed,
            } => write!(
                formatter,
                "task {task} does not support operating system {actual}; allowed: {}",
                allowed.join(", ")
            ),
            Self::ArgumentCount {
                task,
                minimum,
                maximum,
                actual,
            } => match maximum {
                Some(maximum) if minimum == maximum => write!(
                    formatter,
                    "task {task} expects {minimum} arguments but received {actual}"
                ),
                Some(maximum) => write!(
                    formatter,
                    "task {task} expects {minimum} to {maximum} arguments but received {actual}"
                ),
                None => write!(
                    formatter,
                    "task {task} expects at least {minimum} arguments but received {actual}"
                ),
            },
            Self::InvalidArgument {
                task,
                parameter,
                value,
                kind,
            } => write!(
                formatter,
                "task {task} argument {parameter} must be a {kind:?}, but received {value}"
            ),
            Self::InvalidShebang { task, message } => {
                write!(formatter, "task {task} has an invalid shebang: {message}")
            }
            Self::Aborted { task } => write!(formatter, "task {task} aborted"),
            Self::CommandExecution {
                task,
                command,
                message,
            } => write!(
                formatter,
                "failed to execute task {task} command {command:?}: {message}"
            ),
            Self::CommandFailed {
                task,
                command,
                status,
            } => write!(
                formatter,
                "task {task} command {command:?} failed with status {status}"
            ),
        }
    }
}

impl std::error::Error for RunnerError {}
