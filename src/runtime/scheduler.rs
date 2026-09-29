use std::sync::{Arc, Condvar, Mutex};

use crate::async_runtime::{RuntimeFault, TaskInvocation, TaskStatus, TaskTable};
use crate::compiled::FunctionId;
use crate::error::Span;
use crate::runtime::context::RuntimeContext;
use crate::{PromiseHandle, Value};

type TaskRunner = Arc<dyn Fn(TaskInvocation) -> Result<Value, RuntimeFault> + Send + Sync>;

/// Runs one task's invocation, converting a Rust-level panic into a
/// `RuntimeFault::Fatal` instead of letting it unwind past the caller. A
/// worker thread that panics mid-task otherwise leaves that task's handle
/// stuck at `Running` forever — nothing ever calls `complete()` for it — so
/// `await_handle`/`settle_and_take_fatal` on any other thread waiting on it
/// hang indefinitely, and if the panic happened while a `Mutex` guard was
/// held, that mutex poisons and every later `.lock().unwrap()` anywhere in
/// the scheduler panics too. Before this pool existed, a panic on the
/// single interpreter thread just crashed the process with a message —
/// this restores an equivalent (a clean error), not a worse outcome.
fn run_catching_panics(
    run: &TaskRunner,
    invocation: TaskInvocation,
) -> Result<Value, RuntimeFault> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(invocation))) {
        Ok(result) => result,
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic payload".to_string());
            Err(RuntimeFault::Fatal(Box::new(super::runtime_error(
                &format!("task panicked: {message}"),
                &Span::dummy(),
            ))))
        }
    }
}

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
        crate::runtime_config::async_workers()
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
            let result = run_catching_panics(&self.run, invocation);
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
        call_depth: usize,
    ) -> PromiseHandle {
        self.ensure_workers_started();
        let handle = {
            let mut guard = self.inner.lock().unwrap();
            guard.spawn(function, arguments, context, call_depth)
        };
        self.changed.notify_all();
        handle
    }

    /// Creates a promise that some other thread will complete (native async operations).
    pub(crate) fn create_external(&self) -> PromiseHandle {
        self.inner.lock().unwrap().create_running()
    }

    /// Completes an external promise. Returns false if it was cancelled (runtime shutdown) or
    /// already settled, in which case nothing is touched.
    pub(crate) fn complete_external(&self, handle: PromiseHandle, result: Result<Value, RuntimeFault>) -> bool {
        let done = self.inner.lock().unwrap().complete_if_running(handle, result);
        if done {
            self.changed.notify_all();
        }
        done
    }

    pub(crate) fn is_cancelled(&self, handle: PromiseHandle) -> bool {
        matches!(self.inner.lock().unwrap().status(handle), TaskStatus::Cancelled)
    }

    /// Blocks until `handle`'s task is done. If it hasn't started yet, this
    /// claims and runs it *inline*, on the calling thread, instead of
    /// waiting for an idle pool worker to notice it. That's not an
    /// optimization — it's what stops nested `await` from deadlocking the
    /// pool: a worker that's itself waiting here for a task it spawned
    /// (`get()` awaiting `request()`, `async main` awaiting `all()`, an
    /// `async fn` awaiting its own recursive call, …) used to hold its
    /// thread doing nothing, so at real fan-out — even well under the
    /// pool's own size limit, since each logical await can chain through
    /// several `async fn` layers — every worker could end up blocked
    /// waiting on a `Pending` task that no free worker exists to pick up.
    /// With inline execution, an awaited task is always either already
    /// running somewhere, or about to run right here: a pool deadlock on
    /// `await` becomes structurally impossible.
    pub(crate) fn await_handle(
        self: &Arc<Self>,
        handle: PromiseHandle,
        span: &Span,
    ) -> Result<Value, RuntimeFault> {
        self.ensure_workers_started();
        loop {
            let mut guard = self.inner.lock().unwrap();
            match guard.status(handle) {
                TaskStatus::Ready(result) => return result,
                TaskStatus::Cancelled => {
                    return Err(super::runtime_error("promise was cancelled", span).into())
                }
                TaskStatus::Unknown => {
                    return Err(RuntimeFault::Fatal(Box::new(super::runtime_error(
                        "unknown promise handle",
                        span,
                    ))))
                }
                // Already running elsewhere (another worker, or this same
                // thread via a genuine self-referential await) — nothing
                // to claim, just wait for it to finish.
                TaskStatus::Running => {
                    let _guard = self.changed.wait(guard).unwrap();
                }
                TaskStatus::Pending => match guard.start(handle) {
                    Ok(invocation) => {
                        drop(guard);
                        let result = run_catching_panics(&self.run, invocation);
                        let mut complete_guard = self.inner.lock().unwrap();
                        complete_guard.complete(handle, result);
                        drop(complete_guard);
                        self.changed.notify_all();
                    }
                    Err(_) => {
                        // Raced with a worker claiming it between our
                        // `status` check and this `start` call — fine,
                        // just wait for whichever of us "won".
                        let _guard = self.changed.wait(guard).unwrap();
                    }
                },
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

    /// Blocks until every spawned task (including ones nothing ever
    /// `await`s) has reached a final state, then returns the first `Fatal`
    /// fault among them, if any. An entry point calls this right before
    /// returning success — the old single-threaded scheduler ticked its
    /// queue after every statement, so a panic in a detached task always
    /// got a chance to run and abort the program before `main` returned;
    /// under the pool, tasks run on their own regardless of whether
    /// anything awaits them, so this is the equivalent checkpoint. Every
    /// still-`Pending` task is guaranteed to eventually be picked up by a
    /// pool worker (nothing here claims one inline, unlike `await_handle`),
    /// so this waits for real, finite work to finish, not forever — the
    /// only way it doesn't return is a task that itself never terminates,
    /// which is a property of that task, not of this wait.
    pub(crate) fn settle_and_take_fatal(&self) -> Option<RuntimeFault> {
        let mut guard = self.inner.lock().unwrap();
        while guard.any_unsettled() {
            guard = self.changed.wait(guard).unwrap();
        }
        guard.first_fatal()
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
            dyn Fn(
                    crate::async_runtime::TaskInvocation,
                ) -> Result<Value, crate::async_runtime::RuntimeFault>
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
        let h1 = scheduler.spawn(
            crate::compiled::FunctionId(1),
            Vec::new(),
            ctx.spawn_child(),
            0,
        );
        let h2 = scheduler.spawn(
            crate::compiled::FunctionId(1),
            Vec::new(),
            ctx.spawn_child(),
            0,
        );
        let span = crate::error::Span::dummy();
        scheduler.await_handle(h1, &span).unwrap();
        scheduler.await_handle(h2, &span).unwrap();
        assert_eq!(max_concurrent.load(Ordering::SeqCst), 2);
        scheduler.shutdown();
    }

    #[test]
    fn a_panicking_task_becomes_a_fatal_error_not_a_hang() {
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let calls_in_closure = calls.clone();
        let run: TaskRunner = Arc::new(move |_invocation| {
            if calls_in_closure.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("deliberate panic for run_catching_panics coverage");
            }
            Ok(Value::Int(1))
        });
        let scheduler = Scheduler::new(2, run);
        let ctx = RuntimeContext::new(std::path::PathBuf::from("/tmp"));
        let span = crate::error::Span::dummy();

        let panicking = scheduler.spawn(
            crate::compiled::FunctionId(1),
            Vec::new(),
            ctx.spawn_child(),
            0,
        );
        let result = scheduler.await_handle(panicking, &span);
        assert!(
            matches!(result, Err(RuntimeFault::Fatal(_))),
            "a task that panics should surface as a Fatal error, got {result:?}"
        );

        // A second, unrelated task must still run correctly afterward — a
        // panic that poisoned a Mutex somewhere would make every later
        // `.lock().unwrap()` in the scheduler panic too.
        let ok = scheduler.spawn(
            crate::compiled::FunctionId(1),
            Vec::new(),
            ctx.spawn_child(),
            0,
        );
        let result = scheduler.await_handle(ok, &span);
        assert_eq!(
            result.unwrap(),
            Value::Int(1),
            "the scheduler must keep working normally after a panic"
        );

        scheduler.shutdown();
    }
}
