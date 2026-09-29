//! Implementation of the `SparApiV0` function table handed to native modules.
//! Every entry validates the env (null, magic, thread, in-call), handles (generation) and lengths,
//! and contains panics: nothing unwinds into the extension.

use std::collections::HashMap;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, RwLock};

use spar_native_sys::*;

use super::buffer::{dtype_size, NativeBuffer, NativeResource};
use super::env::{BorrowRec, CallEnv, SlotVal};
use crate::runtime::{Record, ResourceId, Shared, Value};

type Res = Result<(), i32>;

#[inline]
fn ffi(f: impl FnOnce() -> Res) -> spar_status_t {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => SPAR_OK,
        Ok(Err(code)) => code,
        Err(_) => SPAR_E_PANIC,
    }
}

#[inline]
fn put<T>(out: *mut T, v: T) -> Res {
    if out.is_null() {
        return Err(SPAR_E_INVALID_ARGUMENT);
    }
    // SAFETY: non-null; caller promised a valid, writable out-pointer.
    unsafe { out.write(v) };
    Ok(())
}

#[inline]
unsafe fn bytes<'a>(ptr: *const u8, len: u64) -> Result<&'a [u8], i32> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() || len > isize::MAX as u64 {
        return Err(SPAR_E_INVALID_ARGUMENT);
    }
    Ok(std::slice::from_raw_parts(ptr, len as usize))
}

// ---- symbol interner (field names) ----

struct Interner {
    names: Vec<Arc<str>>,
    map: HashMap<Arc<str>, u32>,
}
static INTERNER: RwLock<Option<Interner>> = RwLock::new(None);

fn intern(name: &str) -> u32 {
    if let Some(i) = INTERNER.read().unwrap().as_ref().and_then(|t| t.map.get(name).copied()) {
        return i;
    }
    let mut guard = INTERNER.write().unwrap();
    let table = guard.get_or_insert_with(|| Interner { names: Vec::new(), map: HashMap::new() });
    if let Some(i) = table.map.get(name) {
        return *i;
    }
    let name: Arc<str> = Arc::from(name);
    let id = table.names.len() as u32;
    table.names.push(name.clone());
    table.map.insert(name, id);
    id
}

fn symbol_name(id: u32) -> Option<Arc<str>> {
    INTERNER.read().unwrap().as_ref().and_then(|t| t.names.get(id as usize).cloned())
}

// ---- persistent references ----

struct RefTable {
    slots: Vec<(u32, Option<Value>)>,
    free: Vec<u32>,
}
static REFS: Mutex<RefTable> = Mutex::new(RefTable { slots: Vec::new(), free: Vec::new() });

/// Number of live persistent references (leak tests).
pub fn live_persistent_refs() -> usize {
    REFS.lock().unwrap().slots.iter().filter(|(_, v)| v.is_some()).count()
}

// ---- helpers ----

fn clone_value(env: &CallEnv, v: &SparValue) -> Result<Value, i32> {
    Ok(match v.tag {
        SPAR_TAG_VOID => Value::Void,
        // SAFETY: tag selects the live union member.
        SPAR_TAG_BOOL => Value::Bool(unsafe { v.payload.u64_ } != 0),
        SPAR_TAG_INT => Value::Int(unsafe { v.payload.i64_ }),
        SPAR_TAG_FLOAT => Value::Float(unsafe { v.payload.f64_ }),
        _ => env.value_of(v)?.clone(),
    })
}

/// Element of an immutable container: borrowed pointer when the parent is call-owned data the
/// extension cannot mutate, a clone when the parent may still grow (owned).
unsafe fn element(env: &mut CallEnv, parent: &SparValue, ptr: *const Value) -> Result<SparValue, i32> {
    if env.is_owned(parent)? {
        let v = (*ptr).clone();
        Ok(env.own_value(v))
    } else {
        Ok(env.borrow_value(&*ptr))
    }
}

// ---- errors ----

pub(super) unsafe extern "C" fn error_set(env: *mut SparEnv, kind: i32, msg: *const u8, len: u64) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let text = String::from_utf8_lossy(bytes(msg, len)?).into_owned();
        env.error = Some((kind, text));
        Ok(())
    })
}

// ---- scalars ----

pub(super) unsafe extern "C" fn int_get(env: *mut SparEnv, v: SparValue, out: *mut i64) -> spar_status_t {
    ffi(|| {
        CallEnv::from_ptr(env)?;
        if v.tag != SPAR_TAG_INT {
            return Err(SPAR_E_TYPE);
        }
        put(out, v.payload.i64_)
    })
}

pub(super) unsafe extern "C" fn float_get(env: *mut SparEnv, v: SparValue, out: *mut f64) -> spar_status_t {
    ffi(|| {
        CallEnv::from_ptr(env)?;
        match v.tag {
            SPAR_TAG_FLOAT => put(out, v.payload.f64_),
            SPAR_TAG_INT => put(out, v.payload.i64_ as f64),
            _ => Err(SPAR_E_TYPE),
        }
    })
}

pub(super) unsafe extern "C" fn bool_get(env: *mut SparEnv, v: SparValue, out: *mut u8) -> spar_status_t {
    ffi(|| {
        CallEnv::from_ptr(env)?;
        if v.tag != SPAR_TAG_BOOL {
            return Err(SPAR_E_TYPE);
        }
        put(out, (v.payload.u64_ != 0) as u8)
    })
}

// ---- strings / bytes ----

pub(super) unsafe extern "C" fn string_new(env: *mut SparEnv, ptr: *const u8, len: u64, out: *mut SparValue) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let s = std::str::from_utf8(bytes(ptr, len)?).map_err(|_| SPAR_E_UTF8)?;
        let v = env.own_value(Value::String(s.to_owned()));
        put(out, v)
    })
}

pub(super) unsafe extern "C" fn string_view(env: *mut SparEnv, v: SparValue, out: *mut SparStrView) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        match env.value_of(&v)? {
            Value::String(s) => put(out, SparStrView { ptr: s.as_ptr(), len: s.len() as u64 }),
            _ => Err(SPAR_E_TYPE),
        }
    })
}

pub(super) unsafe extern "C" fn bytes_new(env: *mut SparEnv, ptr: *const u8, len: u64, out: *mut SparValue) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let v = env.own_value(Value::Bytes(bytes(ptr, len)?.to_vec()));
        put(out, v)
    })
}

// ---- buffers ----

fn token(env: &CallEnv, index: usize) -> u64 {
    ((env.epoch as u64) << 32) | (index as u64 + 1)
}

fn fill_view(view: &mut SparBufferView, data: *mut u8, len: usize, dtype: u32, flags: u32, borrow: u64) {
    *view = SparBufferView {
        struct_size: std::mem::size_of::<SparBufferView>() as u32,
        flags,
        data: data as *mut c_void,
        len_elements: len as u64,
        len_bytes: (len * dtype_size(dtype).unwrap_or(0)) as u64,
        dtype,
        ndim: 1,
        stride_bytes: dtype_size(dtype).unwrap_or(0) as i64,
        reserved: 0,
        borrow,
    };
}

/// Packs list elements into scratch words. Returns (words, len).
fn pack_list(items: &[Value], dtype: u32) -> Result<Vec<u64>, i32> {
    let size = dtype_size(dtype).ok_or(SPAR_E_INVALID_ARGUMENT)?;
    let mut words = vec![0u64; (items.len() * size).div_ceil(8)];
    let base = words.as_mut_ptr() as *mut u8;
    for (i, item) in items.iter().enumerate() {
        // SAFETY: i * size + size <= words.len() * 8 by construction; alignment is 8.
        unsafe {
            let p = base.add(i * size);
            match (dtype, item) {
                (SPAR_DTYPE_F64, Value::Float(f)) => (p as *mut f64).write(*f),
                (SPAR_DTYPE_F64, Value::Int(n)) => (p as *mut f64).write(*n as f64),
                (SPAR_DTYPE_F32, Value::Float(f)) => (p as *mut f32).write(*f as f32),
                (SPAR_DTYPE_F32, Value::Int(n)) => (p as *mut f32).write(*n as f32),
                (SPAR_DTYPE_BOOL, Value::Bool(b)) => p.write(*b as u8),
                (SPAR_DTYPE_I64, Value::Int(n)) => (p as *mut i64).write(*n),
                (SPAR_DTYPE_U64, Value::Int(n)) if *n >= 0 => (p as *mut u64).write(*n as u64),
                (SPAR_DTYPE_I32, Value::Int(n)) => (p as *mut i32).write(i32::try_from(*n).map_err(|_| SPAR_E_RANGE)?),
                (SPAR_DTYPE_U32, Value::Int(n)) => (p as *mut u32).write(u32::try_from(*n).map_err(|_| SPAR_E_RANGE)?),
                (SPAR_DTYPE_I16, Value::Int(n)) => (p as *mut i16).write(i16::try_from(*n).map_err(|_| SPAR_E_RANGE)?),
                (SPAR_DTYPE_U16, Value::Int(n)) => (p as *mut u16).write(u16::try_from(*n).map_err(|_| SPAR_E_RANGE)?),
                (SPAR_DTYPE_I8, Value::Int(n)) => (p as *mut i8).write(i8::try_from(*n).map_err(|_| SPAR_E_RANGE)?),
                (SPAR_DTYPE_U8, Value::Int(n)) => p.write(u8::try_from(*n).map_err(|_| SPAR_E_RANGE)?),
                (SPAR_DTYPE_U64, Value::Int(_)) => return Err(SPAR_E_RANGE),
                _ => return Err(SPAR_E_TYPE),
            }
        }
    }
    Ok(words)
}

pub(super) unsafe extern "C" fn buffer_borrow(
    env: *mut SparEnv,
    v: SparValue,
    dtype: u32,
    flags: u32,
    out: *mut SparBufferView,
) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        if out.is_null() {
            return Err(SPAR_E_INVALID_ARGUMENT);
        }
        let want_write = flags & SPAR_BUFFER_WRITE != 0;
        let no_copy = flags & SPAR_BUFFER_NO_COPY != 0;
        let out_flags = flags & !(SPAR_BUFFER_COPIED | SPAR_BUFFER_NO_COPY);
        match v.tag {
            SPAR_TAG_BYTES => {
                if dtype != SPAR_DTYPE_U8 && dtype != SPAR_DTYPE_I8 {
                    return Err(SPAR_E_TYPE);
                }
                let (index, slot) = env.slot_state(&v)?;
                let owned = matches!(slot.val, SlotVal::Owned(_));
                if want_write && !owned {
                    return Err(SPAR_E_BORROW);
                }
                let ok = if want_write { slot.borrow == 0 } else { slot.borrow >= 0 };
                if !ok {
                    return Err(SPAR_E_BORROW);
                }
                slot.borrow = if want_write { -1 } else { slot.borrow + 1 };
                let (ptr, len) = match &mut slot.val {
                    SlotVal::Owned(Value::Bytes(b)) => (b.as_mut_ptr(), b.len()),
                    SlotVal::Borrowed(p) => match &**p {
                        Value::Bytes(b) => (b.as_ptr() as *mut u8, b.len()),
                        _ => return Err(SPAR_E_TYPE),
                    },
                    _ => return Err(SPAR_E_TYPE),
                };
                env.borrows.push(BorrowRec { gen: env.epoch, live: true, mutable: want_write, slot: index as u32, resource: None });
                let tok = token(env, env.borrows.len() - 1);
                fill_view(&mut *out, ptr, len, dtype, out_flags, tok);
                Ok(())
            }
            SPAR_TAG_RESOURCE => {
                let id = match env.value_of(&v)? {
                    Value::Resource(id) => *id,
                    _ => return Err(SPAR_E_TYPE),
                };
                let ctx = &*env.ctx;
                let buf = ctx.resources().get::<NativeBuffer>(id).ok_or(SPAR_E_TYPE)?;
                if dtype != 0 && dtype != buf.dtype {
                    return Err(SPAR_E_TYPE);
                }
                if !buf.try_borrow(want_write) {
                    return Err(SPAR_E_BORROW);
                }
                let (data, len, dt) = (buf.data(), buf.len, buf.dtype);
                env.borrows.push(BorrowRec { gen: env.epoch, live: true, mutable: want_write, slot: u32::MAX, resource: Some(id) });
                let tok = token(env, env.borrows.len() - 1);
                fill_view(&mut *out, data, len, dt, out_flags, tok);
                Ok(())
            }
            SPAR_TAG_LIST => {
                if want_write {
                    return Err(SPAR_E_UNSUPPORTED);
                }
                if no_copy {
                    return Err(SPAR_E_UNSUPPORTED);
                }
                let words = match env.value_of(&v)? {
                    Value::List(items) => pack_list(items, dtype)?,
                    _ => return Err(SPAR_E_TYPE),
                };
                let len = match env.value_of(&v)? {
                    Value::List(items) => items.len(),
                    _ => 0,
                };
                env.scratch.push(words);
                let ptr = env.scratch.last().unwrap().as_ptr() as *mut u8;
                env.borrows.push(BorrowRec { gen: env.epoch, live: true, mutable: false, slot: u32::MAX, resource: None });
                let tok = token(env, env.borrows.len() - 1);
                fill_view(&mut *out, ptr, len, dtype, out_flags | SPAR_BUFFER_COPIED, tok);
                Ok(())
            }
            SPAR_TAG_VOID | SPAR_TAG_BOOL | SPAR_TAG_INT | SPAR_TAG_FLOAT => Err(SPAR_E_TYPE),
            _ => Err(SPAR_E_TYPE),
        }
    })
}

pub(super) unsafe extern "C" fn buffer_release(env: *mut SparEnv, tok: SparBorrow) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let epoch = (tok >> 32) as u32;
        let idx = (tok & 0xffff_ffff) as usize;
        if tok == 0 || epoch != env.epoch || idx == 0 || idx > env.borrows.len() {
            return Err(SPAR_E_INVALID_HANDLE);
        }
        let idx = idx - 1;
        let (live, mutable, slot, resource) = {
            let rec = &env.borrows[idx];
            (rec.live, rec.mutable, rec.slot, rec.resource)
        };
        if !live {
            return Err(SPAR_E_BORROW);
        }
        env.borrows[idx].live = false;
        if slot != u32::MAX {
            let s = &mut env.slots[slot as usize];
            if mutable {
                s.borrow = 0;
            } else if s.borrow > 0 {
                s.borrow -= 1;
            }
        }
        if let Some(id) = resource {
            if let Some(buf) = (&*env.ctx).resources().get::<NativeBuffer>(id) {
                buf.unborrow(mutable);
            }
        }
        Ok(())
    })
}

pub(super) unsafe extern "C" fn buffer_new(
    env: *mut SparEnv,
    dtype: u32,
    len: u64,
    view: *mut SparBufferView,
    out: *mut SparValue,
) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        if len > isize::MAX as u64 / 16 {
            return Err(SPAR_E_RANGE);
        }
        let buf = NativeBuffer::zeroed(dtype, len as usize).ok_or(SPAR_E_INVALID_ARGUMENT)?;
        let (data, n) = (buf.data(), buf.len);
        let mut borrowed = None;
        if !view.is_null() {
            buf.try_borrow(true);
            borrowed = Some(());
        }
        let id = (&mut *env.ctx).resources_mut().insert(buf);
        let value = env.own_value(Value::Resource(id));
        if borrowed.is_some() {
            env.borrows.push(BorrowRec { gen: env.epoch, live: true, mutable: true, slot: u32::MAX, resource: Some(id) });
            let tok = token(env, env.borrows.len() - 1);
            fill_view(&mut *view, data, n, dtype, SPAR_BUFFER_WRITE | SPAR_BUFFER_READ, tok);
        }
        put(out, value)
    })
}

pub(super) unsafe extern "C" fn buffer_from_external(
    env: *mut SparEnv,
    data: *mut c_void,
    dtype: u32,
    len: u64,
    finalize: Option<SparFinalizer>,
    userdata: *mut c_void,
    out: *mut SparValue,
) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let size = dtype_size(dtype).ok_or(SPAR_E_INVALID_ARGUMENT)?;
        if data.is_null() && len != 0 {
            return Err(SPAR_E_INVALID_ARGUMENT);
        }
        if (data as usize) % size != 0 {
            return Err(SPAR_E_INVALID_ARGUMENT);
        }
        // Once constructed the finalizer owns `data`; even on later failure it runs exactly once.
        let buf = NativeBuffer::external(dtype, len as usize, data as *mut u8, finalize, userdata);
        let id = (&mut *env.ctx).resources_mut().insert(buf);
        let v = env.own_value(Value::Resource(id));
        put(out, v)
    })
}

pub(super) unsafe extern "C" fn buffer_to_list(env: *mut SparEnv, v: SparValue, out: *mut SparValue) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let id = match env.value_of(&v)? {
            Value::Resource(id) => *id,
            _ => return Err(SPAR_E_TYPE),
        };
        let buf = (&*env.ctx).resources().get::<NativeBuffer>(id).ok_or(SPAR_E_TYPE)?;
        if buf.borrow_state() < 0 {
            return Err(SPAR_E_BORROW);
        }
        let p = buf.data();
        let mut items = Vec::with_capacity(buf.len);
        for i in 0..buf.len {
            let item = match buf.dtype {
                SPAR_DTYPE_F64 => Value::Float((p as *const f64).add(i).read()),
                SPAR_DTYPE_F32 => Value::Float((p as *const f32).add(i).read() as f64),
                SPAR_DTYPE_I64 => Value::Int((p as *const i64).add(i).read()),
                SPAR_DTYPE_U64 => Value::Int(i64::try_from((p as *const u64).add(i).read()).map_err(|_| SPAR_E_RANGE)?),
                SPAR_DTYPE_I32 => Value::Int((p as *const i32).add(i).read() as i64),
                SPAR_DTYPE_U32 => Value::Int((p as *const u32).add(i).read() as i64),
                SPAR_DTYPE_I16 => Value::Int((p as *const i16).add(i).read() as i64),
                SPAR_DTYPE_U16 => Value::Int((p as *const u16).add(i).read() as i64),
                SPAR_DTYPE_I8 => Value::Int((p as *const i8).add(i).read() as i64),
                SPAR_DTYPE_U8 => Value::Int(p.add(i).read() as i64),
                SPAR_DTYPE_BOOL => Value::Bool(p.add(i).read() != 0),
                _ => return Err(SPAR_E_TYPE),
            };
            items.push(item);
        }
        let v = env.own_value(Value::List(Shared::from(items)));
        put(out, v)
    })
}

// ---- lists ----

pub(super) unsafe extern "C" fn list_len(env: *mut SparEnv, v: SparValue, out: *mut u64) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        match env.value_of(&v)? {
            Value::List(l) => put(out, l.len() as u64),
            _ => Err(SPAR_E_TYPE),
        }
    })
}

pub(super) unsafe extern "C" fn list_get(env: *mut SparEnv, v: SparValue, index: u64, out: *mut SparValue) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let ptr: *const Value = match env.value_of(&v)? {
            Value::List(l) => l.get(usize::try_from(index).map_err(|_| SPAR_E_RANGE)?).ok_or(SPAR_E_RANGE)? as *const Value,
            _ => return Err(SPAR_E_TYPE),
        };
        let item = element(env, &v, ptr)?;
        put(out, item)
    })
}

pub(super) unsafe extern "C" fn list_new(env: *mut SparEnv, capacity: u64, out: *mut SparValue) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let cap = usize::try_from(capacity.min(1 << 24)).unwrap_or(0);
        let v = env.own_value(Value::List(Shared::from(Vec::with_capacity(cap))));
        put(out, v)
    })
}

pub(super) unsafe extern "C" fn list_push(env: *mut SparEnv, list: SparValue, item: SparValue) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let item = clone_value(env, &item)?;
        match env.owned_mut(&list)? {
            Value::List(l) => {
                l.push(item);
                Ok(())
            }
            _ => Err(SPAR_E_TYPE),
        }
    })
}

// ---- records ----

pub(super) unsafe extern "C" fn symbol_intern(env: *mut SparEnv, name: *const u8, len: u64, out: *mut SparSymbol) -> spar_status_t {
    ffi(|| {
        // Interning is process-global and thread-safe: a NULL env is allowed so modules can
        // intern field names during init.
        if !env.is_null() {
            CallEnv::from_ptr(env)?;
        }
        let s = std::str::from_utf8(bytes(name, len)?).map_err(|_| SPAR_E_UTF8)?;
        put(out, intern(s))
    })
}

pub(super) unsafe extern "C" fn record_new(env: *mut SparEnv, capacity: u64, out: *mut SparValue) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let cap = usize::try_from(capacity.min(1 << 16)).unwrap_or(0);
        let v = env.own_value(Value::Object(Shared::from(Record::with_capacity(cap))));
        put(out, v)
    })
}

pub(super) unsafe extern "C" fn record_set(env: *mut SparEnv, rec: SparValue, field: SparSymbol, item: SparValue) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let name = symbol_name(field).ok_or(SPAR_E_INVALID_ARGUMENT)?;
        let item = clone_value(env, &item)?;
        match env.owned_mut(&rec)? {
            Value::Object(r) => {
                r.insert(name, item);
                Ok(())
            }
            _ => Err(SPAR_E_TYPE),
        }
    })
}

pub(super) unsafe extern "C" fn record_get(env: *mut SparEnv, rec: SparValue, field: SparSymbol, out: *mut SparValue) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let name = symbol_name(field).ok_or(SPAR_E_INVALID_ARGUMENT)?;
        let ptr: *const Value = match env.value_of(&rec)? {
            Value::Object(r) => r.get(&name).ok_or(SPAR_E_RANGE)? as *const Value,
            _ => return Err(SPAR_E_TYPE),
        };
        let item = element(env, &rec, ptr)?;
        put(out, item)
    })
}

pub(super) unsafe extern "C" fn record_len(env: *mut SparEnv, rec: SparValue, out: *mut u64) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        match env.value_of(&rec)? {
            Value::Object(r) => put(out, r.len() as u64),
            _ => Err(SPAR_E_TYPE),
        }
    })
}

// ---- options ----

pub(super) unsafe extern "C" fn option_some(env: *mut SparEnv, item: SparValue, out: *mut SparValue) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let item = clone_value(env, &item)?;
        let v = env.own_value(Value::Option(Some(Box::new(item))));
        put(out, v)
    })
}

pub(super) unsafe extern "C" fn option_none(env: *mut SparEnv, out: *mut SparValue) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let v = env.own_value(Value::Option(None));
        put(out, v)
    })
}

pub(super) unsafe extern "C" fn option_get(env: *mut SparEnv, opt: SparValue, is_some: *mut u8, out: *mut SparValue) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let ptr: Option<*const Value> = match env.value_of(&opt)? {
            Value::Option(o) => o.as_deref().map(|v| v as *const Value),
            _ => return Err(SPAR_E_TYPE),
        };
        match ptr {
            Some(p) => {
                put(is_some, 1u8)?;
                let item = element(env, &opt, p)?;
                put(out, item)
            }
            None => put(is_some, 0u8),
        }
    })
}

// ---- persistent references ----

pub(super) unsafe extern "C" fn ref_new(env: *mut SparEnv, v: SparValue, out: *mut SparRef) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let value = clone_value(env, &v)?;
        let mut t = REFS.lock().map_err(|_| SPAR_E_ERROR)?;
        let index = match t.free.pop() {
            Some(i) => i,
            None => {
                t.slots.push((0, None));
                (t.slots.len() - 1) as u32
            }
        };
        let slot = &mut t.slots[index as usize];
        slot.0 = slot.0.wrapping_add(1).max(1);
        slot.1 = Some(value);
        let r = ((slot.0 as u64) << 32) | index as u64;
        drop(t);
        put(out, r)
    })
}

pub(super) unsafe extern "C" fn ref_get(env: *mut SparEnv, r: SparRef, out: *mut SparValue) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let (index, gen) = ((r & 0xffff_ffff) as usize, (r >> 32) as u32);
        let value = {
            let t = REFS.lock().map_err(|_| SPAR_E_ERROR)?;
            match t.slots.get(index) {
                Some((g, Some(v))) if *g == gen && gen != 0 => v.clone(),
                _ => return Err(SPAR_E_INVALID_HANDLE),
            }
        };
        let v = env.own_value(value);
        put(out, v)
    })
}

pub(super) unsafe extern "C" fn ref_drop(r: SparRef) -> spar_status_t {
    ffi(|| {
        let (index, gen) = ((r & 0xffff_ffff) as usize, (r >> 32) as u32);
        let dropped = {
            let mut t = REFS.lock().map_err(|_| SPAR_E_ERROR)?;
            match t.slots.get_mut(index) {
                Some((g, v @ Some(_))) if *g == gen && gen != 0 => {
                    let old = v.take();
                    t.free.push(index as u32);
                    old
                }
                _ => return Err(SPAR_E_INVALID_HANDLE),
            }
        };
        drop(dropped); // drop outside the lock
        Ok(())
    })
}

// ---- resources ----

pub(super) unsafe extern "C" fn resource_new(
    env: *mut SparEnv,
    type_tag: u64,
    ptr: *mut c_void,
    finalize: Option<SparFinalizer>,
    userdata: *mut c_void,
    out: *mut SparValue,
) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let id = (&mut *env.ctx).resources_mut().insert(NativeResource::new(type_tag, ptr, finalize, userdata));
        let v = env.own_value(Value::Resource(id));
        put(out, v)
    })
}

fn resource_id(env: &CallEnv, v: &SparValue) -> Result<ResourceId, i32> {
    match env.value_of(v)? {
        Value::Resource(id) => Ok(*id),
        _ => Err(SPAR_E_TYPE),
    }
}

pub(super) unsafe extern "C" fn resource_get(env: *mut SparEnv, v: SparValue, type_tag: u64, out: *mut *mut c_void) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let id = resource_id(env, &v)?;
        let res = (&*env.ctx).resources().get::<NativeResource>(id).ok_or(SPAR_E_INVALID_STATE)?;
        if res.type_tag != type_tag {
            return Err(SPAR_E_TYPE);
        }
        put(out, res.ptr)
    })
}

pub(super) unsafe extern "C" fn resource_close(env: *mut SparEnv, v: SparValue, type_tag: u64) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let id = resource_id(env, &v)?;
        let ctx = &mut *env.ctx;
        match ctx.resources().get::<NativeResource>(id) {
            Some(r) if r.type_tag == type_tag => {}
            Some(_) => return Err(SPAR_E_TYPE),
            None => return Err(SPAR_E_INVALID_STATE),
        }
        // Dropping runs the finalizer.
        drop(ctx.resources_mut().remove::<NativeResource>(id));
        Ok(())
    })
}

// ---- not yet implemented ----

pub(super) unsafe extern "C" fn call(
    env: *mut SparEnv,
    callable: SparValue,
    argv: *const SparValue,
    argc: u64,
    out: *mut SparValue,
) -> spar_status_t {
    ffi(|| {
        let env = CallEnv::from_ptr(env)?;
        let host = env.host.ok_or(SPAR_E_UNSUPPORTED)?;
        if callable.tag != SPAR_TAG_CALLABLE {
            return Err(SPAR_E_TYPE);
        }
        if argc > 64 || (argc > 0 && argv.is_null()) {
            return Err(SPAR_E_INVALID_ARGUMENT);
        }
        let target = env.value_of(&callable)?.clone();
        let mut args = Vec::with_capacity(argc as usize);
        for i in 0..argc as usize {
            args.push(clone_value(env, &*argv.add(i))?);
        }
        let span = env.call_span.clone();
        // SAFETY: `host` points at the interpreter that is executing this native call; it is not
        // otherwise used until the native function returns (see `call_external`).
        match (*host).call_callable(&target, args, &span) {
            Ok(value) => {
                let v = env.own_value(value);
                put(out, v)
            }
            Err(fault) => {
                let message = fault.clone().into_error().to_string();
                env.error = Some((SPAR_E_ERROR, message));
                env.pending_fault = Some(fault);
                Err(SPAR_E_ERROR)
            }
        }
    })
}

