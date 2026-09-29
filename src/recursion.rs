// Native calls grow the Rust stack before entering another Spar frame.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const MAX_CALL_DEPTH: usize = 1000;

// WebAssembly cannot switch stacks with stacker. Keep its existing guard.
#[cfg(target_arch = "wasm32")]
pub(crate) const MAX_CALL_DEPTH: usize = 20;

#[inline]
pub(crate) fn with_stack<R>(call: impl FnOnce() -> R) -> R {
    #[cfg(not(target_arch = "wasm32"))]
    {
        stacker::maybe_grow(1024 * 1024, 8 * 1024 * 1024, call)
    }
    #[cfg(target_arch = "wasm32")]
    {
        call()
    }
}
