use std::collections::{HashMap, VecDeque};

use crate::compiled::FunctionId;
use crate::{PromiseHandle, SparError, Value};

#[derive(Clone, Debug)]
pub(crate) enum RuntimeFault {
    Raised(SparError),
    Fatal(SparError),
    /// A spawned task's own execution called `exit(code:)`. This is not a
    /// user-facing error — it's a side channel from a worker-side `Runtime`
    /// (whose `RuntimeContext` is an isolated `spawn_child()` copy, so
    /// setting `requested_exit` on it is invisible to the awaiting side)
    /// back to whichever `Runtime` `await`s this task's promise, so it can
    /// apply the same exit request to its own context and keep the
    /// existing per-statement unwind-on-`requested_exit` mechanism working
    /// across an `await` boundary the same way it always did within one
    /// thread. See `Runtime::apply_exit_and_unwrap`.
    Exit(i32),
}

impl RuntimeFault {
    pub(crate) fn into_error(self) -> SparError {
        match self {
            Self::Raised(error) | Self::Fatal(error) => error,
            // Should never actually surface: every place that consumes a
            // task's result (`drive_promise`, `race`, `timeout`) translates
            // `Exit` before it can reach a caller that renders it as a
            // diagnostic. This is a defensive fallback, not a normal path.
            Self::Exit(code) => SparError::EvalError {
                message: format!(
                    "internal runtime error: exit request (code {code}) was not applied \
                     before its promise result was rendered as an error"
                ),
                span: crate::error::Span::dummy(),
            },
        }
    }
}

impl std::fmt::Display for RuntimeFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Raised(error) | Self::Fatal(error) => error.fmt(formatter),
            Self::Exit(code) => write!(formatter, "exit request (code {code})"),
        }
    }
}

impl From<SparError> for RuntimeFault {
    fn from(error: SparError) -> Self {
        Self::Raised(error)
    }
}

#[derive(Debug)]
pub(crate) struct TaskInvocation {
    pub(crate) function: FunctionId,
    pub(crate) arguments: Vec<Value>,
    pub(crate) context: crate::runtime::context::RuntimeContext,
    /// The spawning `Runtime`'s own `call_depth` at spawn time, plus one.
    /// Each worker-side `Runtime` built to run this task starts its
    /// `call_depth` from here instead of 0 — otherwise `MAX_CALL_DEPTH`
    /// never trips for recursion that goes through `async fn`/`await`
    /// (every nested async call would reset the counter), turning a
    /// language-level "recursion too deep" diagnostic into an unbounded
    /// resource consumer instead.
    pub(crate) call_depth: usize,
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
    pub(crate) fn spawn(
        &mut self,
        function: FunctionId,
        arguments: Vec<Value>,
        context: crate::runtime::context::RuntimeContext,
        call_depth: usize,
    ) -> PromiseHandle {
        let handle = PromiseHandle::new(self.next_id);
        self.next_id += 1;
        self.states.insert(
            handle,
            TaskState::Pending(TaskInvocation {
                function,
                arguments,
                context,
                call_depth,
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
            TaskState::Pending(_) => {
                // `RuntimeContext` (inside `TaskInvocation`) intentionally
                // isn't `Clone` — it owns per-task resource handles — so the
                // invocation is moved out of `Pending` rather than cloned.
                let TaskState::Pending(invocation) = std::mem::replace(state, TaskState::Running)
                else {
                    unreachable!("state was just matched as Pending")
                };
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

    /// Whether any task is still `Pending` or `Running` — used to wait for
    /// every spawned task (including ones nothing ever `await`s) to reach a
    /// final state before an entry point declares success.
    pub(crate) fn any_unsettled(&self) -> bool {
        self.states
            .values()
            .any(|state| matches!(state, TaskState::Pending(_) | TaskState::Running))
    }

    /// The first `Fatal` fault among completed tasks, if any — a panic
    /// inside a spawned-but-never-`await`ed task must still abort the
    /// program, the same way it did under the old single-threaded
    /// cooperative scheduler (where every statement ticked the queue).
    pub(crate) fn first_fatal(&self) -> Option<RuntimeFault> {
        self.states.values().find_map(|state| match state {
            TaskState::Ready(Err(fault @ RuntimeFault::Fatal(_))) => Some(fault.clone()),
            _ => None,
        })
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

    fn test_context() -> crate::runtime::context::RuntimeContext {
        crate::runtime::context::RuntimeContext::new(std::path::PathBuf::from("/tmp"))
    }

    #[test]
    fn spawned_task_captures_a_context_snapshot() {
        let mut tasks = TaskTable::default();
        let ctx = crate::runtime::context::RuntimeContext::new(std::path::PathBuf::from("/tmp"));
        let handle = tasks.spawn(FunctionId(1), Vec::new(), ctx.spawn_child(), 0);
        let invocation = tasks.start(handle).unwrap();
        assert_eq!(invocation.context.cwd(), std::path::Path::new("/tmp"));
    }

    #[test]
    fn completed_task_is_started_only_once() {
        let mut tasks = TaskTable::default();
        let handle = tasks.spawn(FunctionId(7), vec![Value::Int(3)], test_context(), 0);
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
        let handle = tasks.spawn(FunctionId(1), Vec::new(), test_context(), 0);
        tasks.cancel_pending();
        assert!(matches!(tasks.start(handle), Err(TaskStatus::Cancelled)));
    }

    #[test]
    fn running_tasks_are_cancelled_at_shutdown() {
        let mut tasks = TaskTable::default();
        let handle = tasks.spawn(FunctionId(1), Vec::new(), test_context(), 0);
        tasks.start(handle).unwrap();
        tasks.cancel_pending();
        assert!(matches!(tasks.status(handle), TaskStatus::Cancelled));
    }

    #[test]
    fn starting_a_running_task_reports_a_cycle_candidate() {
        let mut tasks = TaskTable::default();
        let handle = tasks.spawn(FunctionId(1), Vec::new(), test_context(), 0);
        assert!(tasks.start(handle).is_ok());
        assert!(matches!(tasks.start(handle), Err(TaskStatus::Running)));
    }

    #[test]
    fn queued_task_reports_pending_until_the_scheduler_starts_it() {
        let mut tasks = TaskTable::default();
        let handle = tasks.spawn(FunctionId(1), Vec::new(), test_context(), 0);
        assert!(matches!(tasks.status(handle), TaskStatus::Pending));
        assert!(tasks.next_pending().is_some());
        assert!(matches!(tasks.status(handle), TaskStatus::Running));
    }
}
