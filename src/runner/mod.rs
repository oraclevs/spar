mod error;
mod task;

pub use error::RunnerError;
pub use task::{
    CommandTemplate, ScalarKind, Task, TaskCommand, TaskInvocation, TaskParameter, TaskSet,
    TemplatePart,
};
