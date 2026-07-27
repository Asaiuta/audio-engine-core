use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static MEASUREMENT_ENABLED: AtomicBool = AtomicBool::new(false);
static SCOPE_ACTIVE: AtomicBool = AtomicBool::new(false);
static ALLOCATION_CALLS: AtomicUsize = AtomicUsize::new(0);
static DEALLOCATION_CALLS: AtomicUsize = AtomicUsize::new(0);
static REALLOCATION_CALLS: AtomicUsize = AtomicUsize::new(0);
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() && MEASUREMENT_ENABLED.load(Ordering::Relaxed) {
            record_allocation(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() && MEASUREMENT_ENABLED.load(Ordering::Relaxed) {
            record_allocation(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        if !pointer.is_null() && MEASUREMENT_ENABLED.load(Ordering::Relaxed) {
            record_deallocation(layout.size());
        }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_pointer = unsafe { System.realloc(pointer, layout, new_size) };
        if !new_pointer.is_null() && MEASUREMENT_ENABLED.load(Ordering::Relaxed) {
            REALLOCATION_CALLS.fetch_add(1, Ordering::Relaxed);
            adjust_live_bytes(layout.size(), new_size);
        }
        new_pointer
    }
}

fn record_allocation(bytes: usize) {
    ALLOCATION_CALLS.fetch_add(1, Ordering::Relaxed);
    let live = LIVE_BYTES
        .fetch_add(bytes, Ordering::Relaxed)
        .saturating_add(bytes);
    PEAK_LIVE_BYTES.fetch_max(live, Ordering::Relaxed);
}

fn record_deallocation(bytes: usize) {
    DEALLOCATION_CALLS.fetch_add(1, Ordering::Relaxed);
    let _ = LIVE_BYTES.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_sub(bytes))
    });
}

fn adjust_live_bytes(old_size: usize, new_size: usize) {
    if new_size >= old_size {
        let delta = new_size - old_size;
        let live = LIVE_BYTES
            .fetch_add(delta, Ordering::Relaxed)
            .saturating_add(delta);
        PEAK_LIVE_BYTES.fetch_max(live, Ordering::Relaxed);
    } else {
        let delta = old_size - new_size;
        let _ = LIVE_BYTES.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.saturating_sub(delta))
        });
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AllocationSnapshot {
    pub allocations: usize,
    pub deallocations: usize,
    pub reallocations: usize,
    pub live_bytes: usize,
    pub peak_live_bytes: usize,
}

pub struct AllocationScope;

impl AllocationScope {
    /// Begin an exclusive allocation measurement window.
    ///
    /// The counters are process-global, so overlapping scopes would silently
    /// corrupt each other's evidence. Only one scope may be active at a time;
    /// a second concurrent or nested `start` panics instead of measuring wrong.
    pub fn start() -> Self {
        if SCOPE_ACTIVE
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            panic!(
                "AllocationScope::start called while another scope is active; \
                 the global allocation counters support exactly one measurement window at a time"
            );
        }
        ALLOCATION_CALLS.store(0, Ordering::Relaxed);
        DEALLOCATION_CALLS.store(0, Ordering::Relaxed);
        REALLOCATION_CALLS.store(0, Ordering::Relaxed);
        LIVE_BYTES.store(0, Ordering::Relaxed);
        PEAK_LIVE_BYTES.store(0, Ordering::Relaxed);
        MEASUREMENT_ENABLED.store(true, Ordering::Relaxed);
        Self
    }

    pub fn finish(self) -> AllocationSnapshot {
        MEASUREMENT_ENABLED.store(false, Ordering::Relaxed);
        // `self` drops at the end of this function, releasing SCOPE_ACTIVE.
        AllocationSnapshot {
            allocations: ALLOCATION_CALLS.load(Ordering::Relaxed),
            deallocations: DEALLOCATION_CALLS.load(Ordering::Relaxed),
            reallocations: REALLOCATION_CALLS.load(Ordering::Relaxed),
            live_bytes: LIVE_BYTES.load(Ordering::Relaxed),
            peak_live_bytes: PEAK_LIVE_BYTES.load(Ordering::Relaxed),
        }
    }
}

impl Drop for AllocationScope {
    fn drop(&mut self) {
        MEASUREMENT_ENABLED.store(false, Ordering::Relaxed);
        SCOPE_ACTIVE.store(false, Ordering::Release);
    }
}
