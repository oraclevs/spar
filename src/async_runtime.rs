use std::collections::{HashMap, VecDeque};

use crate::compiled::FunctionId;
use crate::{ConfigValue, PromiseHandle, SparError};

#[derive(Clone, Debug)]
pub(crate) struct TaskInvocation {
    pub(crate) function: FunctionId,
    pub(crate) arguments: Vec<ConfigValue>,
}

enum TaskState {
    Pending(TaskInvocation),
    Running,
    Ready(Result<ConfigValue, SparError>),
    Cancelled,
}

#[derive(Default)]
pub(crate) struct TaskTable {
    next_id: u64,
    queue: VecDeque<PromiseHandle>,
    states: HashMap<PromiseHandle, TaskState>,
}

impl TaskTable {
    pub(crate) fn spawn(
        &mut self,
        function: FunctionId,
        arguments: Vec<ConfigValue>,
    ) -> PromiseHandle {
        let handle = PromiseHandle::new(self.next_id);
        self.next_id += 1;
        self.states.insert(
            handle,
            TaskState::Pending(TaskInvocation {
                function,
                arguments,
            }),
        );
        self.queue.push_back(handle);
        handle
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

    pub(crate) fn complete(
        &mut self,
        handle: PromiseHandle,
        result: Result<ConfigValue, SparError>,
    ) {
        self.states.insert(handle, TaskState::Ready(result));
    }

    pub(crate) fn cancel_pending(&mut self) {
        for state in self.states.values_mut() {
            if matches!(state, TaskState::Pending(_)) {
                *state = TaskState::Cancelled;
            }
        }
        self.queue.clear();
    }
}

#[derive(Debug)]
pub(crate) enum TaskStatus {
    Unknown,
    Running,
    Ready(Result<ConfigValue, SparError>),
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_task_is_started_only_once() {
        let mut tasks = TaskTable::default();
        let handle = tasks.spawn(FunctionId(7), vec![ConfigValue::Int(3)]);
        let invocation = tasks.start(handle).unwrap();
        assert_eq!(invocation.function, FunctionId(7));
        tasks.complete(handle, Ok(ConfigValue::Int(4)));

        assert!(matches!(
            tasks.start(handle),
            Err(TaskStatus::Ready(Ok(ConfigValue::Int(4))))
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
    fn starting_a_running_task_reports_a_cycle_candidate() {
        let mut tasks = TaskTable::default();
        let handle = tasks.spawn(FunctionId(1), Vec::new());
        assert!(tasks.start(handle).is_ok());
        assert!(matches!(tasks.start(handle), Err(TaskStatus::Running)));
    }
}
