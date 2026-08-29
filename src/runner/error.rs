use std::fmt;

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
        }
    }
}

impl std::error::Error for RunnerError {}
