use std::sync::{Arc, Condvar, Mutex};

use crate::async_runtime::{RuntimeFault, TaskInvocation, TaskStatus, TaskTable};
use crate::compiled::FunctionId;
use crate::error::Span;
use crate::runtime::context::RuntimeContext;
use crate::{PromiseHandle, Value};

type TaskRunner = Arc<dyn Fn(TaskInvocation) -> Result<Value, RuntimeFault> + Send + Sync>;

/// Bounded pool of OS threads that runs spawned `async fn` tasks
/// concurrently. Threads start lazily on the first `spawn()` call, so a
/// purely synchronous program never pays for them. `spawn()` dispatches to
/// an idle worker immediately (instead of queuing lazily until something
/// awaits it) — that's the actual fix for the old cooperative scheduler's
/// lack of real concurrency: `await_handle` just blocks until the handle's
/// task is done, it no longer doubles as the executor.
pub(crate) struct Scheduler {
    inner: Mutex<TaskTable>,
    changed: Condvar,
    pool_size: usize,
    workers_started: Mutex<bool>,
    run: TaskRunner,
    stop: std::sync::atomic::AtomicBool,
    handles: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl Scheduler {
    pub(crate) fn new(pool_size: usize, run: TaskRunner) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(TaskTable::default()),
            changed: Condvar::new(),
            pool_size: pool_size.max(1),
            workers_started: Mutex::new(false),
            run,
            stop: std::sync::atomic::AtomicBool::new(false),
            handles: Mutex::new(Vec::new()),
        })
    }

    /// I/O-bound blocking work (HTTP, process waits), not CPU-bound — sized
    /// generously above `available_parallelism()` rather than pinned to it.
    /// Overridable via `SPAR_ASYNC_WORKERS` for tuning/testing.
    pub(crate) fn default_pool_size() -> usize {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        std::env::var("SPAR_ASYNC_WORKERS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or_else(|| (cpus * 4).min(64))
    }

    fn ensure_workers_started(self: &Arc<Self>) {
        let mut started = self.workers_started.lock().unwrap();
        if *started {
            return;
        }
        *started = true;
        let mut handles = self.handles.lock().unwrap();
        for _ in 0..self.pool_size {
            let scheduler = Arc::clone(self);
            handles.push(std::thread::spawn(move || scheduler.worker_loop()));
        }
    }

    fn worker_loop(self: Arc<Self>) {
        loop {
            if self.stop.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let next = {
                let mut guard = self.inner.lock().unwrap();
                loop {
                    if self.stop.load(std::sync::atomic::Ordering::SeqCst) {
                        return;
                    }
                    if let Some(pair) = guard.next_pending() {
                        break Some(pair);
                    }
                    let (g, timeout) = self
                        .changed
                        .wait_timeout(guard, std::time::Duration::from_millis(200))
                        .unwrap();
                    guard = g;
                    if timeout.timed_out() {
                        // Recheck stop flag periodically even with no signal.
                        continue;
                    }
                }
            };
            let Some((handle, invocation)) = next else {
                continue;
            };
            let result = (self.run)(invocation);
            let mut guard = self.inner.lock().unwrap();
            guard.complete(handle, result);
            drop(guard);
            self.changed.notify_all();
        }
    }

    pub(crate) fn spawn(
        self: &Arc<Self>,
        function: FunctionId,
        arguments: Vec<Value>,
        context: RuntimeContext,
    ) -> PromiseHandle {
        self.ensure_workers_started();
        let handle = {
            let mut guard = self.inner.lock().unwrap();
            guard.spawn(function, arguments, context)
        };
        self.changed.notify_all();
        handle
    }

    pub(crate) fn await_handle(
        self: &Arc<Self>,
        handle: PromiseHandle,
        span: &Span,
    ) -> Result<Value, RuntimeFault> {
        self.ensure_workers_started();
        let mut guard = self.inner.lock().unwrap();
        loop {
            match guard.status(handle) {
                TaskStatus::Ready(result) => return result,
                TaskStatus::Cancelled => {
                    return Err(super::runtime_error("promise was cancelled", span).into())
                }
                TaskStatus::Unknown => {
                    return Err(RuntimeFault::Fatal(super::runtime_error(
                        "unknown promise handle",
                        span,
                    )))
                }
                TaskStatus::Pending | TaskStatus::Running => {
                    guard = self.changed.wait(guard).unwrap();
                }
            }
        }
    }

    pub(crate) fn status_snapshot(&self, handle: PromiseHandle) -> TaskStatus {
        self.inner.lock().unwrap().status(handle)
    }

    pub(crate) fn created_at(&self, handle: PromiseHandle) -> Option<std::time::Instant> {
        self.inner.lock().unwrap().created_at(handle)
    }

    pub(crate) fn wait_for_any_change(&self, timeout: std::time::Duration) {
        let guard = self.inner.lock().unwrap();
        let _ = self.changed.wait_timeout(guard, timeout);
    }

    pub(crate) fn shutdown(&self) {
        {
            let mut guard = self.inner.lock().unwrap();
            guard.cancel_pending();
        }
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.changed.notify_all();
        let mut handles = self.handles.lock().unwrap();
        for handle in handles.drain(..) {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::context::RuntimeContext;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn worker_pool_runs_two_spawned_tasks_concurrently() {
        let concurrent = std::sync::Arc::new(AtomicUsize::new(0));
        let max_concurrent = std::sync::Arc::new(AtomicUsize::new(0));
        let c1 = concurrent.clone();
        let m1 = max_concurrent.clone();
        let run: std::sync::Arc<
            dyn Fn(crate::async_runtime::TaskInvocation) -> Result<Value, crate::async_runtime::RuntimeFault>
                + Send
                + Sync,
        > = std::sync::Arc::new(move |_invocation| {
            let now = c1.fetch_add(1, Ordering::SeqCst) + 1;
            m1.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(200));
            c1.fetch_sub(1, Ordering::SeqCst);
            Ok(Value::Int(1))
        });
        let scheduler = Scheduler::new(4, run);
        let ctx = RuntimeContext::new(std::path::PathBuf::from("/tmp"));
        let h1 = scheduler.spawn(crate::compiled::FunctionId(1), Vec::new(), ctx.spawn_child());
        let h2 = scheduler.spawn(crate::compiled::FunctionId(1), Vec::new(), ctx.spawn_child());
        let span = crate::error::Span::dummy();
        scheduler.await_handle(h1, &span).unwrap();
        scheduler.await_handle(h2, &span).unwrap();
        assert_eq!(max_concurrent.load(Ordering::SeqCst), 2);
        scheduler.shutdown();
    }
}
