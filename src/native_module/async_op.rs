//! Native async operations. A `SPAR_FN_ASYNC` function calls `async_begin`, returns immediately,
//! and completes the operation later from any thread with `async_complete` / `async_fail`.
//!
//! Lifecycle (`AsyncState`): CREATED -> RUNNING -> COMPLETED | FAILED | CANCELLED -> CONSUMED.
//! `RUNNING` is entered when the runtime takes the promise (native function returned OK). Every
//! transition is validated; nothing here touches interpreter state, only the scheduler's promise
//! table, and a cancelled promise (runtime shutdown) is never completed.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use spar_native_sys::*;

use crate::async_runtime::RuntimeFault;
use crate::error::{Span, SparError};
use crate::runtime::scheduler::Scheduler;
use crate::runtime::Value;
use crate::PromiseHandle;

pub(crate) const CREATED: u8 = 0;
pub(crate) const RUNNING: u8 = 1;
pub(crate) const COMPLETED: u8 = 2;
pub(crate) const FAILED: u8 = 3;
pub(crate) const CANCELLED: u8 = 4;
pub(crate) const CONSUMED: u8 = 5;

pub(crate) struct AsyncOp {
    sched: Arc<Scheduler>,
    pub(crate) handle: PromiseHandle,
    state: AtomicU8,
}

static LIVE: Mutex<Option<HashMap<usize, Arc<AsyncOp>>>> = Mutex::new(None);

/// Number of async operations begun but not yet released (leak tests).
pub fn live_async_ops() -> usize {
    LIVE.lock().unwrap().as_ref().map_or(0, |m| m.len())
}

pub(crate) fn begin(sched: Arc<Scheduler>) -> (*mut SparAsync, PromiseHandle) {
    let handle = sched.create_external();
    let op = Arc::new(AsyncOp {
        sched,
        handle,
        state: AtomicU8::new(CREATED),
    });
    let ptr = Arc::as_ptr(&op) as usize;
    LIVE.lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(ptr, op);
    (ptr as *mut SparAsync, handle)
}

fn lookup(a: *mut SparAsync) -> Result<Arc<AsyncOp>, i32> {
    if a.is_null() {
        return Err(SPAR_E_INVALID_ARGUMENT);
    }
    LIVE.lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&(a as usize)).cloned())
        .ok_or(SPAR_E_INVALID_HANDLE)
}

/// Marks the operation as taken by the runtime (called when the starting function returns OK).
pub(crate) fn mark_running(a: *mut SparAsync) {
    if let Ok(op) = lookup(a) {
        let _ = op
            .state
            .compare_exchange(CREATED, RUNNING, Ordering::AcqRel, Ordering::Acquire);
    }
}

fn settle(a: *mut SparAsync, target: u8, result: Result<Value, RuntimeFault>) -> i32 {
    let op = match lookup(a) {
        Ok(op) => op,
        Err(code) => return code,
    };
    // Only CREATED/RUNNING may settle; anything else is a protocol violation.
    loop {
        let cur = op.state.load(Ordering::Acquire);
        if cur != CREATED && cur != RUNNING {
            return SPAR_E_INVALID_STATE;
        }
        if op
            .state
            .compare_exchange(cur, target, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            break;
        }
    }
    if op.sched.complete_external(op.handle, result) {
        SPAR_OK
    } else {
        op.state.store(CANCELLED, Ordering::Release);
        SPAR_E_CANCELLED
    }
}

pub(super) unsafe extern "C" fn async_complete(
    a: *mut SparAsync,
    json: *const u8,
    len: u64,
) -> spar_status_t {
    std::panic::catch_unwind(|| {
        if len > 0 && (json.is_null() || len > isize::MAX as u64) {
            return SPAR_E_INVALID_ARGUMENT;
        }
        let bytes: &[u8] = if len == 0 {
            b"null"
        } else {
            unsafe { std::slice::from_raw_parts(json, len as usize) }
        };
        let value = match serde_json::from_slice::<serde_json::Value>(bytes)
            .map_err(|e| e.to_string())
            .and_then(|v| crate::stdlib::support::serde_to_value(v).map_err(|e| e.to_string()))
        {
            Ok(v) => v,
            Err(_) => return SPAR_E_INVALID_ARGUMENT,
        };
        settle(a, COMPLETED, Ok(value))
    })
    .unwrap_or(SPAR_E_PANIC)
}

pub(super) unsafe extern "C" fn async_fail(
    a: *mut SparAsync,
    msg: *const u8,
    len: u64,
) -> spar_status_t {
    std::panic::catch_unwind(|| {
        if len > 0 && (msg.is_null() || len > isize::MAX as u64) {
            return SPAR_E_INVALID_ARGUMENT;
        }
        let text = if len == 0 {
            String::new()
        } else {
            String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(msg, len as usize) })
                .into_owned()
        };
        let fault = RuntimeFault::Raised(Box::new(SparError::EvalError {
            message: text,
            span: Span::dummy(),
        }));
        settle(a, FAILED, Err(fault))
    })
    .unwrap_or(SPAR_E_PANIC)
}

pub(super) unsafe extern "C" fn async_is_cancelled(
    a: *mut SparAsync,
    out: *mut u8,
) -> spar_status_t {
    std::panic::catch_unwind(|| match lookup(a) {
        Ok(op) => {
            if out.is_null() {
                return SPAR_E_INVALID_ARGUMENT;
            }
            let cancelled =
                op.state.load(Ordering::Acquire) == CANCELLED || op.sched.is_cancelled(op.handle);
            unsafe { out.write(cancelled as u8) };
            SPAR_OK
        }
        Err(code) => code,
    })
    .unwrap_or(SPAR_E_PANIC)
}

/// Ends the operation's life. Releasing an unsettled operation fails the promise so nothing waits
/// forever on an operation its owner abandoned.
pub(super) unsafe extern "C" fn async_release(a: *mut SparAsync) -> spar_status_t {
    std::panic::catch_unwind(|| {
        let op = match LIVE
            .lock()
            .unwrap()
            .as_mut()
            .and_then(|m| m.remove(&(a as usize)))
        {
            Some(op) => op,
            None => {
                return if a.is_null() {
                    SPAR_E_INVALID_ARGUMENT
                } else {
                    SPAR_E_INVALID_HANDLE
                }
            }
        };
        let cur = op.state.swap(CONSUMED, Ordering::AcqRel);
        if cur == CREATED || cur == RUNNING {
            let fault = RuntimeFault::Raised(Box::new(SparError::EvalError {
                message: "native async operation was released without completing".into(),
                span: Span::dummy(),
            }));
            op.sched.complete_external(op.handle, Err(fault));
        }
        SPAR_OK
    })
    .unwrap_or(SPAR_E_PANIC)
}

pub(super) unsafe extern "C" fn async_begin_api(
    env: *mut SparEnv,
    out: *mut *mut SparAsync,
) -> spar_status_t {
    use super::env::CallEnv;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let env = match unsafe { CallEnv::from_ptr(env) } {
            Ok(e) => e,
            Err(code) => return code,
        };
        let Some(host) = env.host else {
            return SPAR_E_UNSUPPORTED;
        };
        if out.is_null() {
            return SPAR_E_INVALID_ARGUMENT;
        }
        if env.async_op.is_some() {
            return SPAR_E_INVALID_STATE; // one operation per call
        }
        // SAFETY: host is valid for the duration of this native call.
        let sched = unsafe { (*host).scheduler() };
        let (ptr, handle) = begin(sched);
        env.async_op = Some((ptr, handle));
        unsafe { out.write(ptr) };
        SPAR_OK
    }))
    .unwrap_or(SPAR_E_PANIC)
}
