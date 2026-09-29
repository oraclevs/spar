//! Developer-only allocation counters (`--features runtime-stats`).
//!
//! Wraps the system allocator with relaxed atomic counters so `spar exec` can
//! report allocations and bytes for a run. Compiled out of normal builds.

#[cfg(feature = "runtime-stats")]
use std::alloc::{GlobalAlloc, Layout, System};
#[cfg(feature = "runtime-stats")]
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[cfg(feature = "runtime-stats")]
pub struct CountingAlloc;

#[cfg(feature = "runtime-stats")]
static ALLOCS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "runtime-stats")]
static BYTES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "runtime-stats")]
static REALLOCS: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "runtime-stats")]
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(layout.size() as u64, Relaxed);
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(layout.size() as u64, Relaxed);
        System.alloc_zeroed(layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        REALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(new_size as u64, Relaxed);
        System.realloc(ptr, layout, new_size)
    }
}

#[cfg(feature = "runtime-stats")]
pub fn report() {
    eprintln!(
        "runtime-stats: allocs={} reallocs={} bytes={}",
        ALLOCS.load(Relaxed),
        REALLOCS.load(Relaxed),
        BYTES.load(Relaxed)
    );
}

/// Sampling profiler (`--features profile`, enabled by `SPAR_PROFILE=1`).
#[cfg(feature = "profile")]
pub fn start_profiler() -> Option<pprof::ProfilerGuard<'static>> {
    std::env::var_os("SPAR_PROFILE")?;
    pprof::ProfilerGuardBuilder::default()
        .frequency(2000)
        .blocklist(&["libc", "libgcc", "pthread", "vdso"])
        .build()
        .ok()
}

#[cfg(feature = "profile")]
pub fn report_profile(guard: Option<pprof::ProfilerGuard<'static>>) {
    use std::collections::HashMap;
    let Some(guard) = guard else { return };
    let Ok(report) = guard.report().build() else {
        return;
    };
    let mut own: HashMap<String, isize> = HashMap::new();
    let mut inclusive: HashMap<String, isize> = HashMap::new();
    let mut total = 0isize;
    for (frames, count) in &report.data {
        total += count;
        let names: Vec<String> = frames
            .frames
            .iter()
            .map(|f| f.iter().map(|s| s.name()).next().unwrap_or_default())
            .collect();
        if let Some(top) = names.first() {
            *own.entry(top.clone()).or_default() += count;
        }
        let mut seen = std::collections::HashSet::new();
        for n in names {
            if seen.insert(n.clone()) {
                *inclusive.entry(n).or_default() += count;
            }
        }
    }
    for (title, map) in [("self", own), ("inclusive", inclusive)] {
        let mut rows: Vec<_> = map.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        eprintln!("== profile {title} ({total} samples)");
        for (name, n) in rows.into_iter().take(25) {
            let short: String = name.chars().take(110).collect();
            eprintln!("{:5.1}% {}", n as f64 * 100.0 / total as f64, short);
        }
    }
}
