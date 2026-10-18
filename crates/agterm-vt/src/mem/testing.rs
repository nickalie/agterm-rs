//! Test allocators: allocation failures injected on purpose, and allocation counts, for this crate's unit tests.
//!
//! Ghostty: `std.testing.FailingAllocator` (Zig's `lib/std/testing/FailingAllocator.zig`), which its tests wrap
//! around the allocator they pass to the code under test.
//! rustty: `crates/rustty-vt/src/alloc.rs` (`testing`) at `RUSTTY_REV` (MIT).
//! Changes: rustty's failing and counting allocators sit behind its runtime allocator handle on a C ABI; this crate
//! allocates through `std`, so they are a global allocator over [`System`] compiled into the unit-test binary only.
//! Its state is per thread and armed only inside a scope ([`with_failing`], [`with_counting`]), so other tests run
//! normally: every allocation the code under test makes on the thread counts, as with Zig's testing allocator.
//! Fresh allocations and reallocations are both one step of the failure index (Zig's list growth falls back to a
//! fresh allocation when a remap is refused, so either can fail a growth). The page allocator, which maps pages
//! without the heap, has its own failure hook ([`with_failing_pages`]).
//!
//! An infallible allocation that fails inside an armed scope aborts the test binary: a path that cannot handle the
//! failure. Formatting, assertions and other allocating checks go after the scope, which returns the operation's
//! result and the statistics once it has disarmed.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::Cell;
use std::alloc::System;

/// The unit-test binary's global allocator: [`System`], plus the per-thread failure and counting state.
struct TestAllocator;

#[global_allocator]
static GLOBAL: TestAllocator = TestAllocator;

/// One thread's state. Const-initialized cells with no destructor, so the allocator can read it at any time without
/// allocating or recursing.
struct ThreadState {
    failing: Cell<bool>,
    fail_index: Cell<usize>,
    alloc_index: Cell<usize>,
    allocations: Cell<usize>,
    deallocations: Cell<usize>,
    allocated_bytes: Cell<usize>,
    freed_bytes: Cell<usize>,
    induced: Cell<bool>,
    counting: Cell<bool>,
    alloc_calls: Cell<usize>,
    free_calls: Cell<usize>,
    live_bytes: Cell<isize>,
    peak_bytes: Cell<isize>,
    page_successes: Cell<Option<usize>>,
}

impl ThreadState {
    const fn new() -> Self {
        Self {
            failing: Cell::new(false),
            fail_index: Cell::new(usize::MAX),
            alloc_index: Cell::new(0),
            allocations: Cell::new(0),
            deallocations: Cell::new(0),
            allocated_bytes: Cell::new(0),
            freed_bytes: Cell::new(0),
            induced: Cell::new(false),
            counting: Cell::new(false),
            alloc_calls: Cell::new(0),
            free_calls: Cell::new(0),
            live_bytes: Cell::new(0),
            peak_bytes: Cell::new(0),
            page_successes: Cell::new(None),
        }
    }

    /// Whether the next allocation request must fail; counts it otherwise.
    fn refuse(&self) -> bool {
        if !self.failing.get() {
            return false;
        }
        if self.alloc_index.get() == self.fail_index.get() {
            self.induced.set(true);
            return true;
        }
        false
    }

    /// Records a successful request that grew the thread's heap by `grown` bytes and shrank it by `shrunk`.
    fn record(&self, fresh: bool, grown: usize, shrunk: usize) {
        if self.failing.get() {
            self.alloc_index.set(self.alloc_index.get() + 1);
            if fresh {
                self.allocations.set(self.allocations.get() + 1);
            }
            self.allocated_bytes.set(self.allocated_bytes.get() + grown);
            self.freed_bytes.set(self.freed_bytes.get() + shrunk);
        }
        if self.counting.get() {
            self.alloc_calls.set(self.alloc_calls.get() + 1);
            self.add_live(grown, shrunk);
        }
    }

    fn record_free(&self, size: usize) {
        if self.failing.get() {
            self.deallocations.set(self.deallocations.get() + 1);
            self.freed_bytes.set(self.freed_bytes.get() + size);
        }
        if self.counting.get() {
            self.free_calls.set(self.free_calls.get() + 1);
            self.add_live(0, size);
        }
    }

    fn add_live(&self, grown: usize, shrunk: usize) {
        let live = self.live_bytes.get() + signed(grown) - signed(shrunk);
        self.live_bytes.set(live);
        self.peak_bytes.set(self.peak_bytes.get().max(live));
    }
}

/// A byte count as a signed delta; a single allocation never exceeds `isize::MAX` bytes.
fn signed(bytes: usize) -> isize {
    isize::try_from(bytes).unwrap_or(isize::MAX)
}

thread_local! {
    static STATE: ThreadState = const { ThreadState::new() };
}

/// Runs `f` on this thread's state, or does nothing when the thread is being torn down.
fn with_state<R>(f: impl FnOnce(&ThreadState) -> R) -> Option<R> {
    STATE.try_with(f).ok()
}

// SAFETY: every method forwards to `System` with the caller's arguments unchanged, so `System`'s guarantees hold; the
// bookkeeping only reads and writes this thread's cells (no allocation, no recursion) and a refusal returns null,
// which the `GlobalAlloc` contract allows for `alloc`, `alloc_zeroed` and `realloc`.
unsafe impl GlobalAlloc for TestAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if with_state(ThreadState::refuse).unwrap_or(false) {
            return core::ptr::null_mut();
        }
        // SAFETY: forwarded under the caller's contract (a non-zero-sized layout).
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            with_state(|s| s.record(true, layout.size(), 0));
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if with_state(ThreadState::refuse).unwrap_or(false) {
            return core::ptr::null_mut();
        }
        // SAFETY: forwarded under the caller's contract.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            with_state(|s| s.record(true, layout.size(), 0));
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded under the caller's contract (`ptr` was allocated by this allocator with `layout`).
        unsafe { System.dealloc(ptr, layout) };
        with_state(|s| s.record_free(layout.size()));
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if with_state(ThreadState::refuse).unwrap_or(false) {
            return core::ptr::null_mut();
        }
        // SAFETY: forwarded under the caller's contract.
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            let old = layout.size();
            with_state(|s| s.record(false, new_size.saturating_sub(old), old.saturating_sub(new_size)));
        }
        moved
    }
}

/// What the code under a [`with_failing`] scope allocated (`FailingAllocator`'s counters).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Failing {
    /// Successful fresh allocations.
    pub(crate) allocations: usize,
    /// Frees.
    pub(crate) deallocations: usize,
    /// Bytes handed out (fresh, and grown by reallocation).
    pub(crate) allocated_bytes: usize,
    /// Bytes given back (freed, and shrunk by reallocation).
    pub(crate) freed_bytes: usize,
    /// Whether a request was refused on purpose.
    pub(crate) has_induced_failure: bool,
}

/// What the code under a [`with_counting`] scope allocated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counting {
    /// Successful allocation requests, fresh and reallocations.
    pub(crate) alloc_calls: usize,
    /// Frees.
    pub(crate) free_calls: usize,
    /// Bytes allocated in the scope and not freed in it (negative when it freed older memory).
    pub(crate) live_bytes: isize,
    /// The largest `live_bytes` seen in the scope.
    pub(crate) peak_bytes: isize,
}

/// Restores a thread's previous failure state when a scope ends, unwinding included.
struct FailingGuard {
    saved: (bool, usize, usize, Failing),
}

impl Drop for FailingGuard {
    fn drop(&mut self) {
        let (failing, fail_index, alloc_index, stats) = self.saved;
        with_state(|s| {
            s.failing.set(failing);
            s.fail_index.set(fail_index);
            s.alloc_index.set(alloc_index);
            s.allocations.set(stats.allocations);
            s.deallocations.set(stats.deallocations);
            s.allocated_bytes.set(stats.allocated_bytes);
            s.freed_bytes.set(stats.freed_bytes);
            s.induced.set(stats.has_induced_failure);
        });
    }
}

fn failing_stats(s: &ThreadState) -> Failing {
    Failing {
        allocations: s.allocations.get(),
        deallocations: s.deallocations.get(),
        allocated_bytes: s.allocated_bytes.get(),
        freed_bytes: s.freed_bytes.get(),
        has_induced_failure: s.induced.get(),
    }
}

/// Runs `f` with this thread's heap failing from its allocation request number `fail_index` on (0-based: `0` fails
/// the first and every later one, as Zig's `FailingAllocator` stops counting at its index), then disarms and returns
/// `f`'s result with what it allocated.
pub(crate) fn with_failing<R>(fail_index: usize, f: impl FnOnce() -> R) -> (R, Failing) {
    let guard = with_state(|s| {
        let saved = (
            s.failing.get(),
            s.fail_index.get(),
            s.alloc_index.get(),
            failing_stats(s),
        );
        s.failing.set(true);
        s.fail_index.set(fail_index);
        s.alloc_index.set(0);
        s.allocations.set(0);
        s.deallocations.set(0);
        s.allocated_bytes.set(0);
        s.freed_bytes.set(0);
        s.induced.set(false);
        FailingGuard { saved }
    });
    let result = f();
    let stats = with_state(failing_stats).unwrap_or_default();
    drop(guard);
    (result, stats)
}

/// Restores a thread's previous counting state when a scope ends, unwinding included.
struct CountingGuard {
    saved: (bool, Counting),
}

impl Drop for CountingGuard {
    fn drop(&mut self) {
        let (counting, counts) = self.saved;
        with_state(|s| {
            s.counting.set(counting);
            s.alloc_calls.set(counts.alloc_calls);
            s.free_calls.set(counts.free_calls);
            s.live_bytes.set(counts.live_bytes);
            s.peak_bytes.set(counts.peak_bytes);
        });
    }
}

fn counts(s: &ThreadState) -> Counting {
    Counting {
        alloc_calls: s.alloc_calls.get(),
        free_calls: s.free_calls.get(),
        live_bytes: s.live_bytes.get(),
        peak_bytes: s.peak_bytes.get(),
    }
}

/// Runs `f` counting this thread's heap requests: a whole lifecycle fits in one scope (build, an armed
/// [`with_failing`] operation inside, teardown), so `live_bytes == 0` afterwards shows nothing leaked.
pub(crate) fn with_counting<R>(f: impl FnOnce() -> R) -> (R, Counting) {
    let guard = with_state(|s| {
        let saved = (s.counting.get(), counts(s));
        s.counting.set(true);
        s.alloc_calls.set(0);
        s.free_calls.set(0);
        s.live_bytes.set(0);
        s.peak_bytes.set(0);
        CountingGuard { saved }
    });
    let result = f();
    let counted = with_state(counts).unwrap_or_default();
    drop(guard);
    (result, counted)
}

/// Restores a thread's failing and counting flags when a [`suspended`] scope ends, unwinding included.
struct SuspendGuard {
    saved: (bool, bool),
}

impl Drop for SuspendGuard {
    fn drop(&mut self) {
        let (failing, counting) = self.saved;
        with_state(|s| {
            s.failing.set(failing);
            s.counting.set(counting);
        });
    }
}

/// Runs `f` with this thread's failure injection and counting suspended: for the test harness's own allocations
/// inside an armed scope, such as a log capture recording an event the code under test emits, which are not the code
/// under test's.
pub(crate) fn suspended<R>(f: impl FnOnce() -> R) -> R {
    let guard = with_state(|s| SuspendGuard {
        saved: (s.failing.replace(false), s.counting.replace(false)),
    });
    let result = f();
    drop(guard);
    result
}

/// Restores a thread's previous page failure state when a scope ends.
struct PagesGuard {
    saved: Option<usize>,
}

impl Drop for PagesGuard {
    fn drop(&mut self) {
        let saved = self.saved;
        with_state(|s| s.page_successes.set(saved));
    }
}

/// Runs `f` with this thread's page allocator mapping `successes` times and refusing every request after that.
pub(crate) fn with_failing_pages<R>(successes: usize, f: impl FnOnce() -> R) -> R {
    let guard = with_state(|s| PagesGuard {
        saved: s.page_successes.replace(Some(successes)),
    });
    let result = f();
    drop(guard);
    result
}

/// Whether the page allocator must refuse this request ([`with_failing_pages`]); counts it otherwise.
pub(crate) fn refuse_page() -> bool {
    with_state(|s| match s.page_successes.get() {
        Some(0) => true,
        Some(n) => {
            s.page_successes.set(Some(n - 1));
            false
        }
        None => false,
    })
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{Failing, refuse_page, with_counting, with_failing, with_failing_pages, with_state};

    #[test]
    fn failing_fails_exactly_at_the_index() {
        // the results go into an array: a growing vector would allocate inside the scope
        let (results, stats) = with_failing(2, || {
            let mut out = [false; 4];
            let mut kept = [const { Vec::<u8>::new() }; 4];
            for (ok, v) in out.iter_mut().zip(&mut kept) {
                *ok = v.try_reserve_exact(16).is_ok();
            }
            (out, kept)
        });
        let (results, kept) = results;
        drop(kept);
        assert_eq!(
            results,
            [true, true, false, false],
            "every request from the index on fails"
        );
        assert_eq!(stats.allocations, 2);
        assert_eq!(stats.allocated_bytes, 32);
        assert!(stats.has_induced_failure);
    }

    #[test]
    fn failing_counts_frees_and_growth() {
        let ((), stats) = with_failing(usize::MAX, || {
            let mut v: Vec<u8> = Vec::with_capacity(8);
            v.try_reserve_exact(24).unwrap();
            drop(v);
        });
        assert_eq!(stats.allocations, 1);
        assert_eq!(stats.deallocations, 1);
        assert_eq!(stats.allocated_bytes, 24);
        assert_eq!(stats.freed_bytes, 24);
        assert!(!stats.has_induced_failure);
    }

    #[test]
    fn nested_scopes_restore_the_outer_state() {
        let ((), outer) = with_failing(usize::MAX, || {
            let _a = Box::new(1u8);
            let (inner_result, inner) = with_failing(0, || Vec::<u8>::new().try_reserve(1).is_err());
            assert!(inner_result);
            assert!(inner.has_induced_failure);
            let _b = Box::new(2u8);
        });
        assert_eq!(outer.allocations, 2);
        assert!(!outer.has_induced_failure);
        assert_eq!(outer.deallocations, 2);
    }

    #[test]
    fn a_panicking_scope_disarms() {
        // the index is out of reach: the panic machinery allocates, and a refused allocation there would abort
        let caught = std::panic::catch_unwind(|| with_failing(usize::MAX, || panic!("inside")));
        assert!(caught.is_err());
        assert_eq!(with_state(|s| s.failing.get()), Some(false));
    }

    #[test]
    fn counting_spans_a_lifecycle_and_balances() {
        let ((), counts) = with_counting(|| {
            let v = vec![0u64; 16];
            let (grown, failing) = with_failing(0, || {
                let mut w = Vec::<u8>::new();
                w.try_reserve(4).is_err()
            });
            assert!(grown);
            assert_eq!(
                failing,
                Failing {
                    has_induced_failure: true,
                    ..Failing::default()
                }
            );
            drop(v);
        });
        assert_eq!(counts.live_bytes, 0);
        assert_eq!(counts.peak_bytes, 128);
        assert_eq!(counts.alloc_calls, 1);
        assert_eq!(counts.free_calls, 1);
    }

    #[test]
    fn page_failures_follow_their_own_count() {
        assert!(!refuse_page(), "unarmed by default");
        let refused = with_failing_pages(2, || [refuse_page(), refuse_page(), refuse_page(), refuse_page()]);
        assert_eq!(refused, [false, false, true, true]);
        assert!(!refuse_page(), "disarmed after the scope");
    }
}
