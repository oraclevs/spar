use std::collections::{HashMap, VecDeque};

use crate::compiled::FunctionId;
use crate::{PromiseHandle, SparError, Value};

#[derive(Clone, Debug)]
pub(crate) enum RuntimeFault {
    Raised(SparError),
    Fatal(SparError),
}

impl RuntimeFault {
    pub(crate) fn into_error(self) -> SparError {
        match self {
            Self::Raised(error) | Self::Fatal(error) => error,
        }
    }
}

impl std::fmt::Display for RuntimeFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Raised(error) | Self::Fatal(error) => error.fmt(formatter),
        }
    }
}

impl From<SparError> for RuntimeFault {
    fn from(error: SparError) -> Self {
        Self::Raised(error)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TaskInvocation {
    pub(crate) function: FunctionId,
    pub(crate) arguments: Vec<Value>,
}

enum TaskState {
    Pending(TaskInvocation),
    Running,
    Ready(Result<Value, RuntimeFault>),
    Cancelled,
}

#[derive(Default)]
pub(crate) struct TaskTable {
    next_id: u64,
    queue: VecDeque<PromiseHandle>,
    states: HashMap<PromiseHandle, TaskState>,
    created: HashMap<PromiseHandle, std::time::Instant>,
}

impl TaskTable {
    pub(crate) fn spawn(&mut self, function: FunctionId, arguments: Vec<Value>) -> PromiseHandle {
        let handle = PromiseHandle::new(self.next_id);
        self.next_id += 1;
        self.states.insert(
            handle,
            TaskState::Pending(TaskInvocation {
                function,
                arguments,
            }),
        );
        self.created.insert(handle, std::time::Instant::now());
        self.queue.push_back(handle);
        handle
    }

    /// When the promise was created; deadlines are measured from here so a
    /// promise that was slow to run still counts against its timeout.
    pub(crate) fn created_at(&self, handle: PromiseHandle) -> Option<std::time::Instant> {
        self.created.get(&handle).copied()
    }

    pub(crate) fn start(&mut self, handle: PromiseHandle) -> Result<TaskInvocation, TaskStatus> {
        let Some(state) = self.states.get_mut(&handle) else {
            return Err(TaskStatus::Unknown);
        };
        match state {
            TaskState::Pending(invocation) => {
                let invocation = invocation.clone();
                *state = TaskState::Running;
                Ok(invocation)
            }
            TaskState::Running => Err(TaskStatus::Running),
            TaskState::Ready(result) => Err(TaskStatus::Ready(result.clone())),
            TaskState::Cancelled => Err(TaskStatus::Cancelled),
        }
    }

    pub(crate) fn next_pending(&mut self) -> Option<(PromiseHandle, TaskInvocation)> {
        while let Some(handle) = self.queue.pop_front() {
            if let Ok(invocation) = self.start(handle) {
                return Some((handle, invocation));
            }
        }
        None
    }

    pub(crate) fn status(&self, handle: PromiseHandle) -> TaskStatus {
        match self.states.get(&handle) {
            Some(TaskState::Pending(_)) => TaskStatus::Pending,
            Some(TaskState::Running) => TaskStatus::Running,
            Some(TaskState::Ready(result)) => TaskStatus::Ready(result.clone()),
            Some(TaskState::Cancelled) => TaskStatus::Cancelled,
            None => TaskStatus::Unknown,
        }
    }

    pub(crate) fn complete(&mut self, handle: PromiseHandle, result: Result<Value, RuntimeFault>) {
        self.states.insert(handle, TaskState::Ready(result));
    }

    pub(crate) fn cancel_pending(&mut self) {
        for state in self.states.values_mut() {
            if matches!(state, TaskState::Pending(_) | TaskState::Running) {
                *state = TaskState::Cancelled;
            }
        }
        self.queue.clear();
    }
}

#[derive(Debug)]
pub(crate) enum TaskStatus {
    Unknown,
    Pending,
    Running,
    Ready(Result<Value, RuntimeFault>),
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_task_is_started_only_once() {
        let mut tasks = TaskTable::default();
        let handle = tasks.spawn(FunctionId(7), vec![Value::Int(3)]);
        let invocation = tasks.start(handle).unwrap();
        assert_eq!(invocation.function, FunctionId(7));
        tasks.complete(handle, Ok(Value::Int(4)));

        assert!(matches!(
            tasks.start(handle),
            Err(TaskStatus::Ready(Ok(Value::Int(4))))
        ));
    }

    #[test]
    fn pending_tasks_are_cancelled_at_shutdown() {
        let mut tasks = TaskTable::default();
        let handle = tasks.spawn(FunctionId(1), Vec::new());
        tasks.cancel_pending();
        assert!(matches!(tasks.start(handle), Err(TaskStatus::Cancelled)));
    }

    #[test]
    fn running_tasks_are_cancelled_at_shutdown() {
        let mut tasks = TaskTable::default();
        let handle = tasks.spawn(FunctionId(1), Vec::new());
        tasks.start(handle).unwrap();
        tasks.cancel_pending();
        assert!(matches!(tasks.status(handle), TaskStatus::Cancelled));
    }

    #[test]
    fn starting_a_running_task_reports_a_cycle_candidate() {
        let mut tasks = TaskTable::default();
        let handle = tasks.spawn(FunctionId(1), Vec::new());
        assert!(tasks.start(handle).is_ok());
        assert!(matches!(tasks.start(handle), Err(TaskStatus::Running)));
    }

    #[test]
    fn queued_task_reports_pending_until_the_scheduler_starts_it() {
        let mut tasks = TaskTable::default();
        let handle = tasks.spawn(FunctionId(1), Vec::new());
        assert!(matches!(tasks.status(handle), TaskStatus::Pending));
        assert!(tasks.next_pending().is_some());
        assert!(matches!(tasks.status(handle), TaskStatus::Running));
    }
}
