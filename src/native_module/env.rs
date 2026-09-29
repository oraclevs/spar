//! Per-call environment behind the opaque `SparEnv*`: generational handle arena, borrow table,
//! pending error. One `CallEnv` is pooled per thread and reused across calls so the steady state
//! allocates nothing.
//!
//! Safety model: a `Borrowed` slot stores a raw pointer to a `Value` that the runtime keeps alive
//! and unmodified for the whole native call (arguments, or elements inside immutable shared
//! containers). `Owned` slots own their value. Handles carry `(generation << 32) | index`; a slot
//! generation changes on every allocation, so handles from a finished call never resolve.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};

use spar_native_sys::*;

use crate::runtime::{RuntimeContext, Value};

const MAGIC: u64 = 0x5350_4152_4E56_3030;

pub(crate) enum SlotVal {
    Free,
    Borrowed(*const Value),
    Owned(Value),
}

pub(crate) struct Slot {
    pub gen: u32,
    pub val: SlotVal,
    /// Borrow state: 0 available, >0 shared borrows, -1 mutable borrow.
    pub borrow: i32,
}

pub(crate) struct BorrowRec {
    pub gen: u32,
    pub live: bool,
    pub mutable: bool,
    /// Slot index the borrow was taken on (`u32::MAX` for buffer resources).
    pub slot: u32,
    /// Resource buffer id when the target is a `NativeBuffer` resource.
    pub resource: Option<crate::runtime::ResourceId>,
}

pub struct CallEnv {
    magic: u64,
    owner: u64,
    pub(crate) ctx: *mut RuntimeContext,
    pub(crate) slots: Vec<Slot>,
    pub(crate) used: usize,
    pub(crate) argv: Vec<SparValue>,
    pub(crate) error: Option<(i32, String)>,
    pub(crate) borrows: Vec<BorrowRec>,
    pub(crate) scratch: Vec<Vec<u64>>,
    pub(crate) in_call: bool,
    pub(crate) epoch: u32,
}

static NEXT_THREAD: AtomicU64 = AtomicU64::new(1);
thread_local! {
    static THREAD_TOKEN: u64 = NEXT_THREAD.fetch_add(1, Ordering::Relaxed);
    static POOL: RefCell<Vec<Box<CallEnv>>> = const { RefCell::new(Vec::new()) };
}

#[inline]
pub(crate) fn thread_token() -> u64 {
    THREAD_TOKEN.with(|t| *t)
}

impl CallEnv {
    fn fresh() -> Box<Self> {
        Box::new(Self {
            magic: MAGIC,
            owner: thread_token(),
            ctx: std::ptr::null_mut(),
            slots: Vec::with_capacity(16),
            used: 0,
            argv: Vec::with_capacity(8),
            error: None,
            borrows: Vec::new(),
            scratch: Vec::new(),
            in_call: false,
            epoch: 0,
        })
    }

    /// Takes a pooled env for this thread (allocating only on first use / deep nesting).
    pub(crate) fn acquire(ctx: *mut RuntimeContext) -> Box<CallEnv> {
        let mut env = POOL.with(|p| p.borrow_mut().pop()).unwrap_or_else(Self::fresh);
        env.ctx = ctx;
        env.in_call = true;
        env.epoch = env.epoch.wrapping_add(1).max(1);
        env
    }

    /// Ends the call scope: drops owned values, invalidates every handle, releases borrows and
    /// returns the env to the pool.
    pub(crate) fn release(mut self: Box<CallEnv>) {
        self.end_scope();
        self.ctx = std::ptr::null_mut();
        self.in_call = false;
        POOL.with(|p| {
            let mut pool = p.borrow_mut();
            if pool.len() < 8 {
                pool.push(self);
            }
        });
    }

    fn end_scope(&mut self) {
        // Release buffer-resource borrows still open so misuse can't leak a lock across calls.
        let ctx = self.ctx;
        for rec in self.borrows.drain(..) {
            if rec.live {
                if let (Some(id), false) = (rec.resource, ctx.is_null()) {
                    // SAFETY: ctx is the live RuntimeContext of this call.
                    let ctx = unsafe { &*ctx };
                    if let Some(buf) = ctx.resources().get::<super::buffer::NativeBuffer>(id) {
                        buf.unborrow(rec.mutable);
                    }
                }
            }
        }
        for slot in &mut self.slots[..self.used] {
            slot.val = SlotVal::Free;
            slot.borrow = 0;
        }
        self.used = 0;
        self.error = None;
        self.scratch.clear();
        self.argv.clear();
    }

    #[inline]
    pub(crate) fn check_thread(&self) -> Result<(), i32> {
        if self.owner == thread_token() && self.in_call {
            Ok(())
        } else {
            Err(SPAR_E_WRONG_THREAD)
        }
    }

    /// Validates an env pointer from C. Null and freed/garbage magic are rejected.
    ///
    /// # Safety
    /// `p` must be null or point to readable memory at least `size_of::<u64>()` big; anything
    /// else is a caller bug the ABI cannot detect.
    #[inline]
    pub(crate) unsafe fn from_ptr<'a>(p: *mut SparEnv) -> Result<&'a mut CallEnv, i32> {
        if p.is_null() {
            return Err(SPAR_E_INVALID_ARGUMENT);
        }
        let env = &mut *(p as *mut CallEnv);
        if env.magic != MAGIC {
            return Err(SPAR_E_INVALID_ARGUMENT);
        }
        env.check_thread()?;
        Ok(env)
    }

    #[inline]
    pub(crate) fn as_ptr(&mut self) -> *mut SparEnv {
        self as *mut CallEnv as *mut SparEnv
    }

    fn alloc(&mut self, val: SlotVal) -> u64 {
        let index = self.used;
        if index == self.slots.len() {
            self.slots.push(Slot { gen: 0, val: SlotVal::Free, borrow: 0 });
        }
        let slot = &mut self.slots[index];
        slot.gen = slot.gen.wrapping_add(1).max(1);
        slot.val = val;
        slot.borrow = 0;
        self.used += 1;
        ((slot.gen as u64) << 32) | index as u64
    }

    /// Wraps a value owned by the caller of the native function (argument or element inside an
    /// immutable container). The pointee must outlive the call and must not be mutated.
    ///
    /// # Safety
    /// See the module docs.
    #[inline]
    pub(crate) unsafe fn borrow_value(&mut self, v: &Value) -> SparValue {
        match v {
            Value::Void => SparValue::void(),
            Value::Bool(b) => SparValue::bool(*b),
            Value::Int(i) => SparValue::int(*i),
            Value::Float(f) => SparValue::float(*f),
            other => {
                let tag = tag_of(other);
                let handle = self.alloc(SlotVal::Borrowed(other as *const Value));
                SparValue { tag, flags: 0, payload: SparPayload { handle } }
            }
        }
    }

    /// Moves a value into the arena.
    pub(crate) fn own_value(&mut self, v: Value) -> SparValue {
        match v {
            Value::Void => SparValue::void(),
            Value::Bool(b) => SparValue::bool(b),
            Value::Int(i) => SparValue::int(i),
            Value::Float(f) => SparValue::float(f),
            other => {
                let tag = tag_of(&other);
                let handle = self.alloc(SlotVal::Owned(other));
                SparValue { tag, flags: 0, payload: SparPayload { handle } }
            }
        }
    }

    #[inline]
    fn slot_index(&self, v: &SparValue) -> Result<usize, i32> {
        if v.tag < 16 {
            return Err(SPAR_E_TYPE);
        }
        // SAFETY: all payload variants are plain 8-byte data; reading the handle view is valid.
        let handle = unsafe { v.payload.handle };
        let index = (handle & 0xffff_ffff) as usize;
        let gen = (handle >> 32) as u32;
        if gen == 0 || index >= self.used || self.slots[index].gen != gen {
            return Err(SPAR_E_INVALID_HANDLE);
        }
        Ok(index)
    }

    /// Resolves a heap handle to its value.
    #[inline]
    pub(crate) fn value_of(&self, v: &SparValue) -> Result<&Value, i32> {
        let index = self.slot_index(v)?;
        match &self.slots[index].val {
            // SAFETY: see the module docs; the pointee outlives the call.
            SlotVal::Borrowed(p) => Ok(unsafe { &**p }),
            SlotVal::Owned(v) => Ok(v),
            SlotVal::Free => Err(SPAR_E_INVALID_HANDLE),
        }
    }

    /// Mutable access, only for owned values created in this call.
    pub(crate) fn owned_mut(&mut self, v: &SparValue) -> Result<&mut Value, i32> {
        let index = self.slot_index(v)?;
        if self.slots[index].borrow != 0 {
            return Err(SPAR_E_BORROW);
        }
        match &mut self.slots[index].val {
            SlotVal::Owned(v) => Ok(v),
            _ => Err(SPAR_E_BORROW),
        }
    }

    pub(crate) fn is_owned(&self, v: &SparValue) -> Result<bool, i32> {
        let index = self.slot_index(v)?;
        Ok(matches!(self.slots[index].val, SlotVal::Owned(_)))
    }

    pub(crate) fn slot_state(&mut self, v: &SparValue) -> Result<(usize, &mut Slot), i32> {
        let index = self.slot_index(v)?;
        Ok((index, &mut self.slots[index]))
    }

    /// Converts a returned `SparValue` back into a `Value`. Owned slots are moved out.
    pub(crate) fn take_value(&mut self, v: &SparValue) -> Result<Value, i32> {
        match v.tag {
            SPAR_TAG_VOID => Ok(Value::Void),
            // SAFETY: tag says which union member is live.
            SPAR_TAG_BOOL => Ok(Value::Bool(unsafe { v.payload.u64_ } != 0)),
            SPAR_TAG_INT => Ok(Value::Int(unsafe { v.payload.i64_ })),
            SPAR_TAG_FLOAT => Ok(Value::Float(unsafe { v.payload.f64_ })),
            _ => {
                let index = self.slot_index(v)?;
                if self.slots[index].borrow != 0 {
                    return Err(SPAR_E_BORROW);
                }
                let slot = &mut self.slots[index];
                match std::mem::replace(&mut slot.val, SlotVal::Free) {
                    SlotVal::Owned(value) => Ok(value),
                    SlotVal::Borrowed(p) => {
                        // SAFETY: see the module docs.
                        let value = unsafe { (*p).clone() };
                        slot.val = SlotVal::Borrowed(p);
                        Ok(value)
                    }
                    SlotVal::Free => Err(SPAR_E_INVALID_HANDLE),
                }
            }
        }
    }
}

pub(crate) fn tag_of(v: &Value) -> u32 {
    match v {
        Value::Void => SPAR_TAG_VOID,
        Value::Bool(_) => SPAR_TAG_BOOL,
        Value::Int(_) => SPAR_TAG_INT,
        Value::Float(_) => SPAR_TAG_FLOAT,
        Value::String(_) => SPAR_TAG_STRING,
        Value::Bytes(_) => SPAR_TAG_BYTES,
        Value::List(_) => SPAR_TAG_LIST,
        Value::Object(_) => SPAR_TAG_RECORD,
        Value::Option(_) => SPAR_TAG_OPTION,
        Value::Resource(_) => SPAR_TAG_RESOURCE,
        Value::Closure(_) | Value::Function(_) => SPAR_TAG_CALLABLE,
        _ => SPAR_TAG_OTHER,
    }
}
