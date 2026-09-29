//! Direct signatures: pure-scalar native functions exported as plain C functions
//! (`int64_t (*)(int64_t, int64_t)`), skipping `SparValue` marshalling and the handle arena.
//!
//! The signature string is `<args>><ret>` with `i` = int64_t, `f` = double, `b` = uint8_t (bool),
//! `v` = void (return only), at most 4 arguments. The generic path stays available for anything
//! else; this exists because scalar calls dominate call-heavy loops (measured in
//! `docs/native-api/performance.md`).

use std::ffi::c_void;

use crate::error::{Span, SparError};
use crate::runtime::Value;

pub(crate) type DirectFn = Box<dyn Fn(&[Value]) -> Result<Value, SparError> + Send + Sync>;

trait DArg: Copy + 'static {
    fn get(v: &Value) -> Option<Self>;
}
impl DArg for i64 {
    #[inline(always)]
    fn get(v: &Value) -> Option<i64> {
        if let Value::Int(i) = v { Some(*i) } else { None }
    }
}
impl DArg for f64 {
    #[inline(always)]
    fn get(v: &Value) -> Option<f64> {
        match v {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }
    }
}
impl DArg for u8 {
    #[inline(always)]
    fn get(v: &Value) -> Option<u8> {
        if let Value::Bool(b) = v { Some(*b as u8) } else { None }
    }
}

trait DRet: Copy + 'static {
    fn put(self) -> Value;
}
impl DRet for i64 {
    #[inline(always)]
    fn put(self) -> Value {
        Value::Int(self)
    }
}
impl DRet for f64 {
    #[inline(always)]
    fn put(self) -> Value {
        Value::Float(self)
    }
}
impl DRet for u8 {
    #[inline(always)]
    fn put(self) -> Value {
        Value::Bool(self != 0)
    }
}
impl DRet for () {
    #[inline(always)]
    fn put(self) -> Value {
        Value::Void
    }
}

trait DArgs {
    /// # Safety
    /// `f` must be a C function with exactly this signature.
    unsafe fn call<R: DRet>(f: *const c_void, args: &[Value]) -> Option<R>;
}
impl DArgs for () {
    #[inline(always)]
    unsafe fn call<R: DRet>(f: *const c_void, _args: &[Value]) -> Option<R> {
        let f: unsafe extern "C" fn() -> R = std::mem::transmute(f);
        Some(f())
    }
}
impl<A: DArg> DArgs for (A,) {
    #[inline(always)]
    unsafe fn call<R: DRet>(f: *const c_void, args: &[Value]) -> Option<R> {
        let f: unsafe extern "C" fn(A) -> R = std::mem::transmute(f);
        Some(f(A::get(args.first()?)?))
    }
}
impl<A: DArg, B: DArg> DArgs for (A, B) {
    #[inline(always)]
    unsafe fn call<R: DRet>(f: *const c_void, args: &[Value]) -> Option<R> {
        let f: unsafe extern "C" fn(A, B) -> R = std::mem::transmute(f);
        Some(f(A::get(args.first()?)?, B::get(args.get(1)?)?))
    }
}
impl<A: DArg, B: DArg, C: DArg> DArgs for (A, B, C) {
    #[inline(always)]
    unsafe fn call<R: DRet>(f: *const c_void, args: &[Value]) -> Option<R> {
        let f: unsafe extern "C" fn(A, B, C) -> R = std::mem::transmute(f);
        Some(f(A::get(args.first()?)?, B::get(args.get(1)?)?, C::get(args.get(2)?)?))
    }
}
impl<A: DArg, B: DArg, C: DArg, D: DArg> DArgs for (A, B, C, D) {
    #[inline(always)]
    unsafe fn call<R: DRet>(f: *const c_void, args: &[Value]) -> Option<R> {
        let f: unsafe extern "C" fn(A, B, C, D) -> R = std::mem::transmute(f);
        Some(f(A::get(args.first()?)?, B::get(args.get(1)?)?, C::get(args.get(2)?)?, D::get(args.get(3)?)?))
    }
}

/// Raw function pointer that may be shared across threads (module code is immutable and
/// re-entrant by contract).
#[derive(Clone, Copy)]
struct FnPtr(*const c_void);
impl FnPtr {
    #[inline(always)]
    fn get(self) -> *const c_void {
        self.0
    }
}
unsafe impl Send for FnPtr {}
unsafe impl Sync for FnPtr {}

fn make<A: DArgs + 'static, R: DRet>(f: *const c_void, name: String) -> DirectFn {
    let f = FnPtr(f);
    Box::new(move |args| {
        // SAFETY: signature was validated against the declared parameter types at registration.
        match unsafe { A::call::<R>(f.get(), args) } {
            Some(r) => Ok(r.put()),
            None => Err(SparError::EvalError {
                message: format!("native function '{name}' received arguments that do not match its direct signature"),
                span: Span::dummy(),
            }),
        }
    })
}

macro_rules! by_ret {
    ($f:expr, $name:expr, $ret:expr, $A:ty) => {
        match $ret {
            b'i' => Some(make::<$A, i64>($f, $name)),
            b'f' => Some(make::<$A, f64>($f, $name)),
            b'b' => Some(make::<$A, u8>($f, $name)),
            b'v' => Some(make::<$A, ()>($f, $name)),
            _ => None,
        }
    };
}
macro_rules! by_kind {
    ($k:expr, $cb:ident ! ( $($pre:tt)* )) => {
        match $k {
            b'i' => $cb!($($pre)* i64),
            b'f' => $cb!($($pre)* f64),
            b'b' => $cb!($($pre)* u8),
            _ => None,
        }
    };
}

/// Builds the adapter for a validated direct signature. `None` = unsupported signature.
pub(crate) fn build(f: *const c_void, sig: &str, name: String) -> Option<DirectFn> {
    let (args, ret) = sig.split_once('>')?;
    if ret.len() != 1 || args.len() > 4 || !args.bytes().all(|c| matches!(c, b'i' | b'f' | b'b')) {
        return None;
    }
    let ret = ret.as_bytes()[0];
    let a = args.as_bytes();
    let (f, n) = (f, name);
    match a.len() {
        0 => by_ret!(f, n, ret, ()),
        1 => {
            macro_rules! l1 { ($t0:ty) => { by_ret!(f, n.clone(), ret, ($t0,)) }; }
            by_kind!(a[0], l1!())
        }
        2 => {
            macro_rules! l2b { ($t0:ty, $t1:ty) => { by_ret!(f, n.clone(), ret, ($t0, $t1)) }; }
            macro_rules! l2a { ($t0:ty) => { by_kind!(a[1], l2b!($t0,)) }; }
            by_kind!(a[0], l2a!())
        }
        3 => {
            macro_rules! l3c { ($t0:ty, $t1:ty, $t2:ty) => { by_ret!(f, n.clone(), ret, ($t0, $t1, $t2)) }; }
            macro_rules! l3b { ($t0:ty, $t1:ty) => { by_kind!(a[2], l3c!($t0, $t1,)) }; }
            macro_rules! l3a { ($t0:ty) => { by_kind!(a[1], l3b!($t0,)) }; }
            by_kind!(a[0], l3a!())
        }
        4 => {
            macro_rules! l4d { ($t0:ty, $t1:ty, $t2:ty, $t3:ty) => { by_ret!(f, n.clone(), ret, ($t0, $t1, $t2, $t3)) }; }
            macro_rules! l4c { ($t0:ty, $t1:ty, $t2:ty) => { by_kind!(a[3], l4d!($t0, $t1, $t2,)) }; }
            macro_rules! l4b { ($t0:ty, $t1:ty) => { by_kind!(a[2], l4c!($t0, $t1,)) }; }
            macro_rules! l4a { ($t0:ty) => { by_kind!(a[1], l4b!($t0,)) }; }
            by_kind!(a[0], l4a!())
        }
        _ => None,
    }
}

/// Which declared Spar type each signature letter must correspond to.
pub(crate) fn letter_matches(letter: u8, ty: &crate::ast::SparType) -> bool {
    use crate::ast::SparType::*;
    matches!((letter, ty), (b'i', Int) | (b'f', Float) | (b'b', Bool) | (b'v', Void))
}
