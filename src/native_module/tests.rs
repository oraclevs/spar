//! Boundary hardening tests: every API entry is driven with invalid handles, stale generations,
//! bad lengths and borrow misuse. None may crash; all must return a status.
use std::ffi::c_void;

use spar_native_sys::*;

use super::env::CallEnv;
use super::loader::api_table;
use crate::runtime::{RuntimeContext, Value};

static REF_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn ctx() -> RuntimeContext {
    RuntimeContext::new(std::env::temp_dir())
}

/// Runs `f` with a live env whose args are `args`.
fn with_env<R>(args: &[Value], f: impl FnOnce(*mut SparEnv, &[SparValue]) -> R) -> R {
    let mut context = ctx();
    let raw = Box::into_raw(CallEnv::acquire(&mut context as *mut _));
    let (envp, argv) = unsafe {
        let env = &mut *raw;
        for a in args {
            let v = env.borrow_value(a);
            env.argv.push(v);
        }
        (env.as_ptr(), env.argv.clone())
    };
    let r = f(envp, &argv);
    unsafe { Box::from_raw(raw) }.release();
    r
}

#[test]
fn null_and_garbage_env_are_rejected() {
    let api = api_table();
    let mut out = 0i64;
    unsafe {
        assert_eq!(
            (api.int_get.unwrap())(std::ptr::null_mut(), SparValue::int(1), &mut out),
            SPAR_E_INVALID_ARGUMENT
        );
        let mut fake = [0u64; 64];
        assert_eq!(
            (api.int_get.unwrap())(
                fake.as_mut_ptr() as *mut SparEnv,
                SparValue::int(1),
                &mut out
            ),
            SPAR_E_INVALID_ARGUMENT
        );
    }
}

#[test]
fn scalar_type_errors() {
    let api = api_table();
    with_env(&[], |env, _| unsafe {
        let mut i = 0i64;
        let mut f = 0f64;
        let mut b = 0u8;
        assert_eq!(
            (api.int_get.unwrap())(env, SparValue::float(1.0), &mut i),
            SPAR_E_TYPE
        );
        assert_eq!(
            (api.float_get.unwrap())(env, SparValue::int(2), &mut f),
            SPAR_OK
        );
        assert_eq!(f, 2.0);
        assert_eq!(
            (api.float_get.unwrap())(env, SparValue::bool(true), &mut f),
            SPAR_E_TYPE
        );
        assert_eq!(
            (api.bool_get.unwrap())(env, SparValue::int(1), &mut b),
            SPAR_E_TYPE
        );
        assert_eq!(
            (api.int_get.unwrap())(env, SparValue::int(5), std::ptr::null_mut()),
            SPAR_E_INVALID_ARGUMENT
        );
    });
}

#[test]
fn stale_and_forged_handles_are_detected() {
    let api = api_table();
    // A handle from a finished call must not resolve in the next call, even if the slot is reused.
    let stale = with_env(&[Value::String("hello".into())], |_, argv| argv[0]);
    with_env(&[Value::String("other".into())], |env, argv| unsafe {
        let mut view = SparStrView {
            ptr: std::ptr::null(),
            len: 0,
        };
        assert_eq!(
            (api.string_view.unwrap())(env, stale, &mut view),
            SPAR_E_INVALID_HANDLE
        );
        assert_eq!((api.string_view.unwrap())(env, argv[0], &mut view), SPAR_OK);
        assert_eq!(
            std::slice::from_raw_parts(view.ptr, view.len as usize),
            b"other"
        );
        // forged handles: zero generation, huge index, wrong tag
        let forged = |handle: u64| SparValue {
            tag: SPAR_TAG_STRING,
            flags: 0,
            payload: SparPayload { handle },
        };
        for h in [0u64, 1, u64::MAX, (1u64 << 32) | 9999, (7u64 << 32)] {
            let s = (api.string_view.unwrap())(env, forged(h), &mut view);
            assert!(s == SPAR_E_INVALID_HANDLE, "handle {h:#x} -> {s}");
        }
        // an int is not a heap value
        assert_eq!(
            (api.string_view.unwrap())(env, SparValue::int(3), &mut view),
            SPAR_E_TYPE
        );
    });
}

#[test]
fn utf8_and_length_validation() {
    let api = api_table();
    with_env(&[], |env, _| unsafe {
        let mut out = SparValue::void();
        let bad = [0xffu8, 0xfe];
        assert_eq!(
            (api.string_new.unwrap())(env, bad.as_ptr(), 2, &mut out),
            SPAR_E_UTF8
        );
        assert_eq!(
            (api.string_new.unwrap())(env, std::ptr::null(), 5, &mut out),
            SPAR_E_INVALID_ARGUMENT
        );
        assert_eq!(
            (api.string_new.unwrap())(env, b"x".as_ptr(), u64::MAX, &mut out),
            SPAR_E_INVALID_ARGUMENT
        );
        assert_eq!(
            (api.string_new.unwrap())(env, std::ptr::null(), 0, &mut out),
            SPAR_OK
        );
        assert_eq!(
            (api.bytes_new.unwrap())(env, std::ptr::null(), 1 << 62, &mut out),
            SPAR_E_INVALID_ARGUMENT
        );
        let mut view = SparBufferView::zeroed();
        assert_eq!(
            (api.buffer_new.unwrap())(env, SPAR_DTYPE_F64, u64::MAX, &mut view, &mut out),
            SPAR_E_RANGE
        );
        assert_eq!(
            (api.buffer_new.unwrap())(env, 999, 4, &mut view, &mut out),
            SPAR_E_INVALID_ARGUMENT
        );
        assert_eq!((api.list_new.unwrap())(env, u64::MAX, &mut out), SPAR_OK); // capacity is clamped
    });
}

#[test]
fn borrow_state_machine() {
    let api = api_table();
    with_env(&[], |env, _| unsafe {
        let mut buf = SparValue::void();
        let mut wview = SparBufferView::zeroed();
        assert_eq!(
            (api.buffer_new.unwrap())(env, SPAR_DTYPE_F64, 8, &mut wview, &mut buf),
            SPAR_OK
        );
        // buffer_new hands out a mutable borrow: any other borrow conflicts
        let mut v2 = SparBufferView::zeroed();
        assert_eq!(
            (api.buffer_borrow.unwrap())(env, buf, SPAR_DTYPE_F64, SPAR_BUFFER_READ, &mut v2),
            SPAR_E_BORROW
        );
        assert_eq!((api.buffer_release.unwrap())(env, wview.borrow), SPAR_OK);
        assert_eq!(
            (api.buffer_release.unwrap())(env, wview.borrow),
            SPAR_E_BORROW
        ); // double release
           // two shared borrows coexist, a mutable one does not
        let (mut s1, mut s2, mut m) = (
            SparBufferView::zeroed(),
            SparBufferView::zeroed(),
            SparBufferView::zeroed(),
        );
        assert_eq!(
            (api.buffer_borrow.unwrap())(env, buf, SPAR_DTYPE_F64, SPAR_BUFFER_READ, &mut s1),
            SPAR_OK
        );
        assert_eq!(
            (api.buffer_borrow.unwrap())(env, buf, SPAR_DTYPE_F64, SPAR_BUFFER_READ, &mut s2),
            SPAR_OK
        );
        assert_eq!(s1.data, s2.data);
        assert_eq!(
            (api.buffer_borrow.unwrap())(env, buf, SPAR_DTYPE_F64, SPAR_BUFFER_WRITE, &mut m),
            SPAR_E_BORROW
        );
        // wrong dtype
        assert_eq!(
            (api.buffer_borrow.unwrap())(env, buf, SPAR_DTYPE_I32, SPAR_BUFFER_READ, &mut m),
            SPAR_E_TYPE
        );
        assert_eq!((api.buffer_release.unwrap())(env, s1.borrow), SPAR_OK);
        assert_eq!((api.buffer_release.unwrap())(env, s2.borrow), SPAR_OK);
        assert_eq!(
            (api.buffer_borrow.unwrap())(env, buf, SPAR_DTYPE_F64, SPAR_BUFFER_WRITE, &mut m),
            SPAR_OK
        );
        // forged tokens
        for tok in [0u64, 1, u64::MAX, (5u64 << 32) | 3] {
            assert_eq!(
                (api.buffer_release.unwrap())(env, tok),
                SPAR_E_INVALID_HANDLE,
                "token {tok:#x}"
            );
        }
        // leaving the call with `m` still open must not leak the lock: checked below
    });
    // A borrow left open at call end is released by the scope.
    let mut context = ctx();
    let id;
    {
        let raw = Box::into_raw(CallEnv::acquire(&mut context as *mut _));
        unsafe {
            let env = (*raw).as_ptr();
            let (mut buf, mut view) = (SparValue::void(), SparBufferView::zeroed());
            assert_eq!(
                (api.buffer_new.unwrap())(env, SPAR_DTYPE_U8, 4, &mut view, &mut buf),
                SPAR_OK
            );
            id = match (*raw).value_of(&buf).unwrap() {
                Value::Resource(id) => *id,
                _ => unreachable!(),
            };
            Box::from_raw(raw).release();
        }
    }
    let b = context
        .resources()
        .get::<super::buffer::NativeBuffer>(id)
        .unwrap();
    assert_eq!(
        b.borrow_state(),
        0,
        "open mutable borrow leaked across the call boundary"
    );
}

#[test]
fn list_and_bytes_borrow_rules() {
    let api = api_table();
    let args = [
        Value::List(vec![Value::Int(1), Value::Int(2)].into()),
        Value::List(vec![Value::Float(1.5), Value::String("x".into())].into()),
        Value::Bytes(vec![1, 2, 3]),
    ];
    with_env(&args, |env, argv| unsafe {
        let mut v = SparBufferView::zeroed();
        // list copy path
        assert_eq!(
            (api.buffer_borrow.unwrap())(env, argv[0], SPAR_DTYPE_I64, SPAR_BUFFER_READ, &mut v),
            SPAR_OK
        );
        assert!(v.flags & SPAR_BUFFER_COPIED != 0);
        assert_eq!(std::slice::from_raw_parts(v.data as *const i64, 2), &[1, 2]);
        // NO_COPY refuses to copy
        assert_eq!(
            (api.buffer_borrow.unwrap())(
                env,
                argv[0],
                SPAR_DTYPE_I64,
                SPAR_BUFFER_READ | SPAR_BUFFER_NO_COPY,
                &mut v
            ),
            SPAR_E_UNSUPPORTED
        );
        // writable copies are never offered
        assert_eq!(
            (api.buffer_borrow.unwrap())(env, argv[0], SPAR_DTYPE_I64, SPAR_BUFFER_WRITE, &mut v),
            SPAR_E_UNSUPPORTED
        );
        // range and type violations
        assert_eq!(
            (api.buffer_borrow.unwrap())(env, argv[0], SPAR_DTYPE_U8, SPAR_BUFFER_READ, &mut v),
            SPAR_OK
        );
        assert_eq!(
            (api.buffer_borrow.unwrap())(env, argv[1], SPAR_DTYPE_F64, SPAR_BUFFER_READ, &mut v),
            SPAR_E_TYPE
        );
        // bytes: zero-copy read, never writable (argument is immutable)
        assert_eq!(
            (api.buffer_borrow.unwrap())(env, argv[2], SPAR_DTYPE_U8, SPAR_BUFFER_READ, &mut v),
            SPAR_OK
        );
        assert_eq!(v.flags & SPAR_BUFFER_COPIED, 0);
        assert_eq!(
            (api.buffer_borrow.unwrap())(env, argv[2], SPAR_DTYPE_U8, SPAR_BUFFER_WRITE, &mut v),
            SPAR_E_BORROW
        );
        assert_eq!(
            (api.buffer_borrow.unwrap())(env, argv[2], SPAR_DTYPE_F64, SPAR_BUFFER_READ, &mut v),
            SPAR_E_TYPE
        );
    });
}

#[test]
fn owned_containers_only_are_mutable() {
    let api = api_table();
    let args = [Value::List(vec![Value::Int(1)].into())];
    with_env(&args, |env, argv| unsafe {
        // pushing into an argument list is rejected (arguments are immutable)
        assert_eq!(
            (api.list_push.unwrap())(env, argv[0], SparValue::int(9)),
            SPAR_E_BORROW
        );
        let mut l = SparValue::void();
        assert_eq!((api.list_new.unwrap())(env, 2, &mut l), SPAR_OK);
        assert_eq!((api.list_push.unwrap())(env, l, SparValue::int(9)), SPAR_OK);
        let mut n = 0u64;
        assert_eq!((api.list_len.unwrap())(env, l, &mut n), SPAR_OK);
        assert_eq!(n, 1);
        let mut item = SparValue::void();
        assert_eq!((api.list_get.unwrap())(env, l, 1, &mut item), SPAR_E_RANGE);
        assert_eq!(
            (api.list_get.unwrap())(env, l, u64::MAX, &mut item),
            SPAR_E_RANGE
        );
        assert_eq!((api.list_get.unwrap())(env, argv[0], 0, &mut item), SPAR_OK);
        // record misuse
        let mut sym = 0u32;
        assert_eq!(
            (api.symbol_intern.unwrap())(env, b"f".as_ptr(), 1, &mut sym),
            SPAR_OK
        );
        assert_eq!(
            (api.symbol_intern.unwrap())(env, [0xffu8].as_ptr(), 1, &mut sym),
            SPAR_E_UTF8
        );
        let mut rec = SparValue::void();
        assert_eq!((api.record_new.unwrap())(env, 1, &mut rec), SPAR_OK);
        assert_eq!(
            (api.record_get.unwrap())(env, rec, 424242, &mut item),
            SPAR_E_INVALID_ARGUMENT
        );
        assert_eq!(
            (api.record_set.unwrap())(env, rec, 424242, SparValue::int(1)),
            SPAR_E_INVALID_ARGUMENT
        );
        assert_eq!(
            (api.record_get.unwrap())(env, l, sym, &mut item),
            SPAR_E_TYPE
        );
    });
}

#[test]
fn persistent_refs_survive_calls_and_detect_reuse() {
    let _g = REF_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let api = api_table();
    let before = super::live_persistent_refs();
    let r = with_env(&[Value::String("keep".into())], |env, argv| unsafe {
        let mut r = 0u64;
        assert_eq!((api.ref_new.unwrap())(env, argv[0], &mut r), SPAR_OK);
        r
    });
    with_env(&[], |env, _| unsafe {
        let mut v = SparValue::void();
        assert_eq!((api.ref_get.unwrap())(env, r, &mut v), SPAR_OK);
        let mut view = SparStrView {
            ptr: std::ptr::null(),
            len: 0,
        };
        assert_eq!((api.string_view.unwrap())(env, v, &mut view), SPAR_OK);
        assert_eq!(
            std::slice::from_raw_parts(view.ptr, view.len as usize),
            b"keep"
        );
    });
    assert_eq!(unsafe { (api.ref_drop.unwrap())(r) }, SPAR_OK);
    assert_eq!(unsafe { (api.ref_drop.unwrap())(r) }, SPAR_E_INVALID_HANDLE); // double drop
    with_env(&[], |env, _| unsafe {
        let mut v = SparValue::void();
        assert_eq!(
            (api.ref_get.unwrap())(env, r, &mut v),
            SPAR_E_INVALID_HANDLE
        );
        assert_eq!(
            (api.ref_get.unwrap())(env, 0, &mut v),
            SPAR_E_INVALID_HANDLE
        );
    });
    assert_eq!(super::live_persistent_refs(), before, "persistent ref leak");
}

#[test]
fn wrong_thread_use_is_rejected() {
    let api = api_table();
    let addr = with_env(&[], |env, _| env as usize);
    // The env pointer is dangling after release; a different thread must be rejected *before* it is
    // dereferenced beyond the magic check, so use a live env sent to another thread.
    let mut context = ctx();
    let raw = Box::into_raw(CallEnv::acquire(&mut context as *mut _));
    let envp = unsafe { (*raw).as_ptr() } as usize;
    let status = std::thread::spawn(move || {
        let mut out = 0i64;
        unsafe { (api.int_get.unwrap())(envp as *mut SparEnv, SparValue::int(1), &mut out) }
    })
    .join()
    .unwrap();
    assert_eq!(status, SPAR_E_WRONG_THREAD);
    unsafe { Box::from_raw(raw) }.release();
    let _ = addr;
}

struct Count(std::sync::Arc<std::sync::atomic::AtomicUsize>);
unsafe extern "C" fn count_finalizer(_p: *mut c_void, ud: *mut c_void) {
    let c = Box::from_raw(ud as *mut Count);
    c.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn finalizers_run_exactly_once_on_table_drop_and_close() {
    let api = api_table();
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    {
        let mut context = ctx();
        let raw = Box::into_raw(CallEnv::acquire(&mut context as *mut _));
        unsafe {
            let env = (*raw).as_ptr();
            let mut a = SparValue::void();
            let mut b = SparValue::void();
            let mut x = 5u64;
            for out in [&mut a, &mut b] {
                let ud = Box::into_raw(Box::new(Count(counter.clone()))) as *mut c_void;
                assert_eq!(
                    (api.resource_new.unwrap())(
                        env,
                        77,
                        &mut x as *mut u64 as *mut c_void,
                        Some(count_finalizer),
                        ud,
                        out
                    ),
                    SPAR_OK
                );
            }
            // wrong tag is refused, right tag closes exactly once
            assert_eq!((api.resource_close.unwrap())(env, a, 78), SPAR_E_TYPE);
            assert_eq!((api.resource_close.unwrap())(env, a, 77), SPAR_OK);
            assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert_eq!(
                (api.resource_close.unwrap())(env, a, 77),
                SPAR_E_INVALID_STATE
            );
            let mut p: *mut c_void = std::ptr::null_mut();
            assert_eq!((api.resource_get.unwrap())(env, b, 77, &mut p), SPAR_OK);
            assert_eq!((api.resource_get.unwrap())(env, b, 1, &mut p), SPAR_E_TYPE);
            Box::from_raw(raw).release();
        }
        // `b` still alive in the table here
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
    assert_eq!(
        counter.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "context drop must finalize remaining resources"
    );
}

#[test]
fn external_buffer_alignment_and_finalizer() {
    let api = api_table();
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    {
        let mut context = ctx();
        let raw = Box::into_raw(CallEnv::acquire(&mut context as *mut _));
        unsafe {
            let env = (*raw).as_ptr();
            let mut out = SparValue::void();
            let bytes = [0u8; 16];
            // misaligned for f64
            let ud = Box::into_raw(Box::new(Count(counter.clone()))) as *mut c_void;
            let misaligned = bytes.as_ptr().add(1) as *mut c_void;
            assert_eq!(
                (api.buffer_from_external.unwrap())(
                    env,
                    misaligned,
                    SPAR_DTYPE_F64,
                    1,
                    Some(count_finalizer),
                    ud,
                    &mut out
                ),
                SPAR_E_INVALID_ARGUMENT
            );
            drop(Box::from_raw(ud as *mut Count));
            assert_eq!(
                (api.buffer_from_external.unwrap())(
                    env,
                    std::ptr::null_mut(),
                    SPAR_DTYPE_U8,
                    4,
                    None,
                    std::ptr::null_mut(),
                    &mut out
                ),
                SPAR_E_INVALID_ARGUMENT
            );
            Box::from_raw(raw).release();
        }
    }
    assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn pseudo_random_api_fuzz_never_crashes() {
    let _g = REF_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut refs: Vec<u64> = Vec::new();
    // Deterministic xorshift-driven call sequences over the whole table with hostile arguments.
    let api = api_table();
    let mut state = 0x9e3779b97f4a7c15u64;
    let mut rnd = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let args = [
        Value::String("fuzz".into()),
        Value::Bytes(vec![7; 33]),
        Value::List(vec![Value::Float(1.0), Value::Float(2.0)].into()),
        Value::Int(5),
    ];
    with_env(&args, |env, argv| unsafe {
        let mut pool: Vec<SparValue> = argv.to_vec();
        for _ in 0..20_000 {
            let pick = |r: u64, pool: &Vec<SparValue>| -> SparValue {
                match r % 7 {
                    0 => SparValue::int(r as i64),
                    1 => SparValue {
                        tag: (r >> 8) as u32 % 40,
                        flags: 0,
                        payload: SparPayload { handle: r },
                    },
                    _ => pool[(r >> 3) as usize % pool.len()],
                }
            };
            let a = pick(rnd(), &pool);
            let b = pick(rnd(), &pool);
            let n = rnd();
            let mut out = SparValue::void();
            let mut i = 0i64;
            let mut u = 0u64;
            let mut view = SparBufferView::zeroed();
            let mut sv = SparStrView {
                ptr: std::ptr::null(),
                len: 0,
            };
            let s = match rnd() % 14 {
                0 => (api.string_view.unwrap())(env, a, &mut sv),
                1 => (api.buffer_borrow.unwrap())(
                    env,
                    a,
                    (n % 14) as u32,
                    (n >> 8) as u32 & 0x1f,
                    &mut view,
                ),
                2 => (api.buffer_release.unwrap())(env, n),
                3 => (api.list_get.unwrap())(env, a, n % 5, &mut out),
                4 => (api.list_push.unwrap())(env, a, b),
                5 => (api.list_len.unwrap())(env, a, &mut u),
                6 => (api.record_get.unwrap())(env, a, (n % 5) as u32, &mut out),
                7 => (api.record_set.unwrap())(env, a, (n % 5) as u32, b),
                8 => (api.int_get.unwrap())(env, a, &mut i),
                9 => (api.option_some.unwrap())(env, a, &mut out),
                10 => (api.buffer_new.unwrap())(env, (n % 14) as u32, n % 64, &mut view, &mut out),
                11 => (api.list_new.unwrap())(env, n % 8, &mut out),
                12 => (api.record_new.unwrap())(env, n % 8, &mut out),
                _ => {
                    let s = (api.ref_new.unwrap())(env, a, &mut u);
                    if s == SPAR_OK {
                        refs.push(u);
                    }
                    s
                }
            };
            if s == SPAR_OK && out.tag >= 16 && pool.len() < 256 {
                pool.push(out);
            }
            if s == SPAR_OK && (out.tag == SPAR_TAG_VOID || out.tag >= 16) {
                // keep going
            }
        }
    });
    for r in refs {
        assert_eq!(unsafe { (api.ref_drop.unwrap())(r) }, SPAR_OK);
    }
}

#[test]
fn async_state_machine_and_shutdown_cancellation() {
    use super::async_op::*;
    use crate::runtime::scheduler::Scheduler;
    let api = api_table();
    let sched = Scheduler::new(1, std::sync::Arc::new(|_| Ok(Value::Void)));
    let (op, handle) = begin(sched.clone());
    unsafe {
        let mut cancelled = 9u8;
        assert_eq!(
            (api.async_is_cancelled.unwrap())(op, &mut cancelled),
            SPAR_OK
        );
        assert_eq!(cancelled, 0);
        // invalid json is rejected without settling
        assert_eq!(
            (api.async_complete.unwrap())(op, b"{oops".as_ptr(), 5),
            SPAR_E_INVALID_ARGUMENT
        );
        assert_eq!((api.async_complete.unwrap())(op, b"7".as_ptr(), 1), SPAR_OK);
        assert_eq!(
            (api.async_complete.unwrap())(op, b"8".as_ptr(), 1),
            SPAR_E_INVALID_STATE
        );
        assert_eq!(
            (api.async_fail.unwrap())(op, b"x".as_ptr(), 1),
            SPAR_E_INVALID_STATE
        );
        assert_eq!((api.async_release.unwrap())(op), SPAR_OK);
        assert_eq!((api.async_release.unwrap())(op), SPAR_E_INVALID_HANDLE);
        assert_eq!(
            (api.async_complete.unwrap())(op, b"7".as_ptr(), 1),
            SPAR_E_INVALID_HANDLE
        );
        assert_eq!(
            (api.async_complete.unwrap())(std::ptr::null_mut(), b"7".as_ptr(), 1),
            SPAR_E_INVALID_ARGUMENT
        );
    }
    assert!(matches!(
        sched.status_snapshot(handle),
        crate::async_runtime::TaskStatus::Ready(Ok(Value::Int(7)))
    ));

    // shutdown cancels a running operation; a late completion is refused and never resumes anything
    let (op2, h2) = begin(sched.clone());
    sched.shutdown();
    unsafe {
        let mut cancelled = 0u8;
        assert_eq!(
            (api.async_is_cancelled.unwrap())(op2, &mut cancelled),
            SPAR_OK
        );
        assert_eq!(cancelled, 1);
        assert_eq!(
            (api.async_complete.unwrap())(op2, b"1".as_ptr(), 1),
            SPAR_E_CANCELLED
        );
        assert_eq!((api.async_release.unwrap())(op2), SPAR_OK);
    }
    assert!(sched.is_cancelled(h2));
}

/// `cargo test --release --lib native_module::tests::bench_env -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_env() {
    use std::hint::black_box;
    use std::time::Instant;
    let mut context = ctx();
    let n = 10_000_000u64;
    let t = Instant::now();
    for _ in 0..n {
        let env = CallEnv::acquire(&mut context as *mut _);
        black_box(&env);
        env.release();
    }
    println!(
        "acquire+release: {:.2} ns",
        t.elapsed().as_secs_f64() * 1e9 / n as f64
    );
    let api = api_table();
    let raw = Box::into_raw(CallEnv::acquire(&mut context as *mut _));
    let envp = unsafe { (*raw).as_ptr() };
    let t = Instant::now();
    let mut out = 0i64;
    for i in 0..n {
        unsafe { (api.int_get.unwrap())(envp, SparValue::int(black_box(i as i64)), &mut out) };
    }
    println!(
        "int_get: {:.2} ns",
        t.elapsed().as_secs_f64() * 1e9 / n as f64
    );
    unsafe { Box::from_raw(raw) }.release();
    {
        unsafe extern "C" fn noop(
            _e: *mut SparEnv,
            _u: *mut c_void,
            _a: *const SparValue,
            _n: u64,
            o: *mut SparValue,
        ) -> spar_status_t {
            *o = SparValue::void();
            SPAR_OK
        }
        let t = Instant::now();
        for _ in 0..n {
            let raw = Box::into_raw(CallEnv::acquire(&mut context as *mut _));
            let (status, out) = unsafe {
                let env = &mut *raw;
                let mut out = SparValue::void();
                let envp = env.as_ptr();
                let argv = env.argv.as_ptr();
                let f: unsafe extern "C" fn(
                    *mut SparEnv,
                    *mut c_void,
                    *const SparValue,
                    u64,
                    *mut SparValue,
                ) -> spar_status_t = black_box(noop);
                (f(envp, std::ptr::null_mut(), argv, 0, &mut out), out)
            };
            black_box((status, out.tag));
            unsafe { Box::from_raw(raw) }.release();
        }
        println!(
            "manual acquire+call+release: {:.2} ns",
            t.elapsed().as_secs_f64() * 1e9 / n as f64
        );
        let t = Instant::now();
        for _ in 0..n {
            let raw = Box::into_raw(CallEnv::acquire(&mut context as *mut _));
            let v = unsafe { (*raw).take_value(&SparValue::void()) };
            black_box(v.is_ok());
            unsafe { Box::from_raw(raw) }.release();
        }
        println!(
            "acquire+take_value+release: {:.2} ns",
            t.elapsed().as_secs_f64() * 1e9 / n as f64
        );
    }
    let info = super::loader::bench_support::info();
    for (name, f) in [
        (
            "v_a",
            super::loader::bench_support::v_a as fn(&_, &mut _) -> _,
        ),
        ("v_b", super::loader::bench_support::v_b),
    ] {
        let t = Instant::now();
        for _ in 0..n {
            black_box(f(&info, &mut context).unwrap());
        }
        println!(
            "{name}: {:.2} ns",
            t.elapsed().as_secs_f64() * 1e9 / n as f64
        );
    }
    let t = Instant::now();
    for _ in 0..n {
        black_box(super::loader::bench_support::step1(&info, &mut context).unwrap());
    }
    println!(
        "step1 (no check_ret): {:.2} ns",
        t.elapsed().as_secs_f64() * 1e9 / n as f64
    );
    let t = Instant::now();
    for _ in 0..n {
        black_box(super::loader::bench_support::step2(&info, &mut context).unwrap());
    }
    println!(
        "step2 (+check_ret): {:.2} ns",
        t.elapsed().as_secs_f64() * 1e9 / n as f64
    );
    let t = Instant::now();
    for _ in 0..n {
        black_box(super::loader::bench_support::call(&info, &mut context).unwrap());
    }
    println!(
        "invoke (noop): {:.2} ns",
        t.elapsed().as_secs_f64() * 1e9 / n as f64
    );
    let t = Instant::now();
    for _ in 0..n {
        black_box(super::loader::bench_support::call_full(&info, &mut context).unwrap());
    }
    println!(
        "invoke_full (noop): {:.2} ns",
        t.elapsed().as_secs_f64() * 1e9 / n as f64
    );
    let t = Instant::now();
    for _ in 0..n {
        black_box(super::thread_token_for_bench());
    }
    println!(
        "thread_token: {:.2} ns",
        t.elapsed().as_secs_f64() * 1e9 / n as f64
    );
}
