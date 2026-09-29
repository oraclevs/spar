//! Native-owned typed buffers exposed to Spar as resources. This is the zero-copy typed-array path:
//! the storage is contiguous, aligned, and a borrow hands its address to native code without
//! per-element conversion.

use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, Ordering};

use spar_native_sys::*;

pub(crate) fn dtype_size(dtype: u32) -> Option<usize> {
    Some(match dtype {
        SPAR_DTYPE_U8 | SPAR_DTYPE_I8 | SPAR_DTYPE_BOOL => 1,
        SPAR_DTYPE_U16 | SPAR_DTYPE_I16 => 2,
        SPAR_DTYPE_U32 | SPAR_DTYPE_I32 | SPAR_DTYPE_F32 => 4,
        SPAR_DTYPE_U64 | SPAR_DTYPE_I64 | SPAR_DTYPE_F64 => 8,
        _ => return None,
    })
}

enum Storage {
    /// 8-byte aligned runtime allocation (Vec<u64> guarantees alignment for every dtype).
    Owned(Vec<u64>),
    External {
        ptr: *mut u8,
        finalize: Option<SparFinalizer>,
        userdata: *mut c_void,
    },
}

pub struct NativeBuffer {
    pub(crate) dtype: u32,
    pub(crate) len: usize,
    storage: Storage,
    /// 0 available, >0 shared borrows, -1 mutable borrow.
    state: AtomicI32,
}

// SAFETY: the buffer is plain memory. External pointers are owned by this object and released by
// the extension-provided finalizer, which the ABI requires to be thread-safe.
unsafe impl Send for NativeBuffer {}
unsafe impl Sync for NativeBuffer {}

impl NativeBuffer {
    pub(crate) fn zeroed(dtype: u32, len: usize) -> Option<Self> {
        let bytes = len.checked_mul(dtype_size(dtype)?)?;
        let words = bytes.div_ceil(8);
        Some(Self {
            dtype,
            len,
            storage: Storage::Owned(vec![0u64; words]),
            state: AtomicI32::new(0),
        })
    }

    pub(crate) fn external(
        dtype: u32,
        len: usize,
        ptr: *mut u8,
        finalize: Option<SparFinalizer>,
        userdata: *mut c_void,
    ) -> Self {
        Self {
            dtype,
            len,
            storage: Storage::External {
                ptr,
                finalize,
                userdata,
            },
            state: AtomicI32::new(0),
        }
    }

    #[inline]
    pub(crate) fn data(&self) -> *mut u8 {
        match &self.storage {
            Storage::Owned(v) => v.as_ptr() as *mut u8,
            Storage::External { ptr, .. } => *ptr,
        }
    }

    /// Borrow state machine: AVAILABLE -> SHARED(n) | MUT. Returns false on conflict.
    pub(crate) fn try_borrow(&self, mutable: bool) -> bool {
        if mutable {
            self.state
                .compare_exchange(0, -1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        } else {
            let mut cur = self.state.load(Ordering::Acquire);
            loop {
                if cur < 0 {
                    return false;
                }
                match self.state.compare_exchange_weak(
                    cur,
                    cur + 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return true,
                    Err(actual) => cur = actual,
                }
            }
        }
    }

    pub(crate) fn unborrow(&self, mutable: bool) {
        if mutable {
            self.state.store(0, Ordering::Release);
        } else {
            self.state.fetch_sub(1, Ordering::AcqRel);
        }
    }

    pub(crate) fn borrow_state(&self) -> i32 {
        self.state.load(Ordering::Acquire)
    }
}

impl Drop for NativeBuffer {
    fn drop(&mut self) {
        if let Storage::External {
            ptr,
            finalize: Some(f),
            userdata,
        } = &self.storage
        {
            // SAFETY: the extension supplied this finalizer for this pointer; libraries are never
            // unloaded so the code is still mapped.
            unsafe { f(*ptr as *mut c_void, *userdata) };
        }
    }
}

/// Native resource with an extension-supplied finalizer (`resource_new`).
pub struct NativeResource {
    pub(crate) type_tag: u64,
    pub(crate) ptr: *mut c_void,
    finalize: Option<SparFinalizer>,
    userdata: *mut c_void,
}

// SAFETY: ownership of `ptr` moved to the runtime; the ABI requires thread-safe finalizers.
unsafe impl Send for NativeResource {}

impl NativeResource {
    pub(crate) fn new(
        type_tag: u64,
        ptr: *mut c_void,
        finalize: Option<SparFinalizer>,
        userdata: *mut c_void,
    ) -> Self {
        Self {
            type_tag,
            ptr,
            finalize,
            userdata,
        }
    }
}

impl Drop for NativeResource {
    fn drop(&mut self) {
        if let Some(f) = self.finalize.take() {
            // SAFETY: see NativeBuffer::drop.
            unsafe { f(self.ptr, self.userdata) };
        }
    }
}
