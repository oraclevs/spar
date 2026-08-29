use std::fmt;

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
    ArgumentCount {
        task: String,
        expected: usize,
        actual: usize,
    },
    InvalidArgument {
        task: String,
        parameter: String,
        value: String,
        kind: ScalarKind,
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
            Self::ArgumentCount {
                task,
                expected,
                actual,
            } => write!(
                formatter,
                "task {task} expects {expected} arguments but received {actual}"
            ),
            Self::InvalidArgument {
                task,
                parameter,
                value,
                kind,
            } => write!(
                formatter,
                "task {task} argument {parameter} must be a {kind:?}, but received {value}"
            ),
        }
    }
}

impl std::error::Error for RunnerError {}
