//! The heap: a bump allocator over a stretch of RAM the boot layout leaves
//! alone. Nothing the boot allocates outlives it, so it never frees, except
//! that the most recent allocation can grow or shrink in place, which is how
//! a growing `Vec` behaves.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;

/// The firmware's global allocator: a bump allocator over one stretch of
/// RAM.
pub struct Bump {
    state: UnsafeCell<State>,
}

struct State {
    /// The stretch's start.
    start: usize,
    /// Where the next block may start.
    next: usize,
    /// The stretch's end.
    end: usize,
    /// The most recent block, which can grow, shrink or be freed in place;
    /// 0 when there is none.
    last: usize,
    /// The highest end any block has had.
    high: usize,
}

// SAFETY: the firmware runs on one CPU, with interrupts masked, so no two
// threads ever reach the state.
unsafe impl Sync for Bump {}

impl Bump {
    /// An allocator with no memory: every allocation fails until
    /// [`init`](Self::init) gives it a stretch.
    pub const fn new() -> Self {
        Bump {
            state: UnsafeCell::new(State {
                start: 0,
                next: 0,
                end: 0,
                last: 0,
                high: 0,
            }),
        }
    }

    /// Gives the allocator `[start, end)`.
    ///
    /// # Safety
    ///
    /// Must be called at most once, before the first allocation: a second
    /// call resets the bump pointer and would hand out live blocks again.
    pub unsafe fn init(&self, start: usize, end: usize) {
        // SAFETY: one CPU and no reentrancy, so no other reference to the
        // state exists while this one does.
        let s = unsafe { &mut *self.state.get() };
        s.start = start;
        s.next = start;
        s.end = end;
        s.last = 0;
        s.high = start;
    }

    /// The stretch allocations have used: its start and its high-water mark.
    pub fn used(&self) -> (usize, usize) {
        // SAFETY: one CPU and no reentrancy, so nothing changes the state
        // while this reference lives.
        let s = unsafe { &*self.state.get() };
        (s.start, s.high)
    }
}

// SAFETY: `alloc` returns null or a block of `layout.size()` bytes aligned to
// `layout.align()` that no live block overlaps: `next` moves up past each
// block handed out, and back down only when the most recent block is freed,
// or to grow that block in place. The image runs on one CPU with no
// reentrancy, so nothing touches the state concurrently.
unsafe impl GlobalAlloc for Bump {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: single CPU, no reentrancy.
        let s = unsafe { &mut *self.state.get() };
        let Some(start) = s.next.checked_next_multiple_of(layout.align()) else {
            return core::ptr::null_mut();
        };
        match start.checked_add(layout.size()) {
            Some(end) if end <= s.end => {
                s.next = end;
                s.last = start;
                s.high = s.high.max(end);
                start as *mut u8
            }
            _ => core::ptr::null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        // SAFETY: single CPU, no reentrancy.
        let s = unsafe { &mut *self.state.get() };
        if ptr as usize == s.last {
            s.next = s.last;
            s.last = 0;
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: single CPU, no reentrancy.
        let s = unsafe { &mut *self.state.get() };
        if ptr as usize == s.last {
            return match s.last.checked_add(new_size) {
                Some(end) if end <= s.end => {
                    s.next = end;
                    s.high = s.high.max(end);
                    ptr
                }
                _ => core::ptr::null_mut(),
            };
        }
        // SAFETY: the layout is the caller's, with the new size.
        let new =
            unsafe { self.alloc(Layout::from_size_align_unchecked(new_size, layout.align())) };
        if !new.is_null() {
            // SAFETY: both blocks are live and at least this long.
            unsafe { core::ptr::copy_nonoverlapping(ptr, new, layout.size().min(new_size)) };
        }
        new
    }
}
