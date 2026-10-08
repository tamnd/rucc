//! The allocator the binary runs on.
//!
//! A compile makes and drops a great many small blocks: a list of the instructions of one block,
//! the readers of one value, a table of one pass. With the C library's allocator on duktape.c at
//! `-O2` that was about a quarter of the compile, most of it in the paths a free takes once the
//! small cache in front of them is full. tamnd/rucc#3052.
//!
//! [`Pool`] keeps a free list per size for each thread and nothing else. A block of up to
//! [`LARGEST`] bytes is rounded up to one of [`CLASSES`] sizes and taken off the list for that
//! size, or cut from the end of a region the thread got from the system when the list is empty.
//! A free puts the block on the list of the thread that frees it. Rust says how big a block is
//! when it gives it back, so nothing is kept beside the block to say so, and the class a free
//! goes to is the class the allocation came from. Anything bigger, or aligned to more than
//! sixteen, goes to the system allocator as before.
//!
//! The regions are never given back. A block freed on another thread than the one that made it
//! is only memory moving from one list to another, which is why that is safe, and the memory a
//! compile holds at its peak is all a list can ever hold.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ptr;

/// The biggest block the lists hold. Anything bigger is rare enough that the system allocator is
/// fast at it, and keeping it here would hold whole tables on a list after they are dropped.
const LARGEST: usize = 32 << 10;

/// How many sizes there are: sixteen steps of sixteen bytes up to 256, then four for each
/// doubling up to [`LARGEST`].
const CLASSES: usize = 16 + 4 * 7;

/// How much is asked of the system at a time when a thread runs out, a mebibyte on a page.
const REGION: Layout = match Layout::from_size_align(1 << 20, 4096) {
    Ok(layout) => layout,
    Err(_) => panic!("a mebibyte on a page is a layout"),
};

/// The alignment of every block, which each class size is a multiple of.
const ALIGN: usize = 16;

/// The allocator. See the module documentation.
#[derive(Debug)]
pub(crate) struct Pool;

/// What one thread holds: the head of a free list for each class, and what is left of the region
/// it cuts new blocks from.
struct Local {
    free: [Cell<*mut u8>; CLASSES],
    next: Cell<*mut u8>,
    end: Cell<*mut u8>,
}

thread_local! {
    // Nothing in it has a destructor, so it is never torn down, and an allocation made while
    // another thread local is being dropped still finds it.
    static LOCAL: Local = const {
        Local {
            free: [const { Cell::new(ptr::null_mut()) }; CLASSES],
            next: Cell::new(ptr::null_mut()),
            end: Cell::new(ptr::null_mut()),
        }
    };
}

/// The class a block of this layout belongs to, or none when the system allocator takes it.
#[inline]
fn class(layout: Layout) -> Option<usize> {
    let size = layout.size();
    if size > LARGEST || layout.align() > ALIGN || size == 0 {
        return None;
    }
    let last = size - 1;
    if last < 256 {
        return Some(last >> 4);
    }
    // The top bit of the last byte's offset says which doubling, the two bits under it which
    // quarter of it.
    let top = usize::BITS - 1 - last.leading_zeros();
    let quarter = (last >> (top - 2)) & 3;
    Some(16 + (top as usize - 8) * 4 + quarter)
}

/// How many bytes a block of the class is.
#[inline]
fn size(class: usize) -> usize {
    if class < 16 {
        return (class + 1) << 4;
    }
    let top = (class - 16) / 4 + 8;
    let quarter = (class - 16) % 4;
    (4 + quarter + 1) << (top - 2)
}

impl Local {
    /// A block of the class off its list, or cut from the region when the list is empty.
    #[inline]
    fn take(&self, class: usize) -> *mut u8 {
        let head = self.free[class].get();
        if !head.is_null() {
            // SAFETY: a block on a list was put there by `give`, which wrote the next block's
            // address in its first word. Every block is at least sixteen bytes and aligned to
            // sixteen, and nothing else holds it while it is on the list.
            self.free[class].set(unsafe { head.cast::<*mut u8>().read() });
            return head;
        }
        self.cut(size(class))
    }

    /// A block cut from the end of the region, with a new region asked for when this one is too
    /// short. What is left of the old one is not used again.
    #[inline(never)]
    fn cut(&self, bytes: usize) -> *mut u8 {
        let next = self.next.get();
        if (self.end.get() as usize) - (next as usize) >= bytes {
            // SAFETY: there are at least `bytes` bytes from `next` to the end of the region, so
            // the pointer moved by `bytes` is still in it or one past its end.
            self.next.set(unsafe { next.add(bytes) });
            return next;
        }
        // SAFETY: the layout is not zero sized.
        let region = unsafe { System.alloc(REGION) };
        if region.is_null() {
            return region;
        }
        // SAFETY: the region is a mebibyte and `bytes` is at most `LARGEST`, which is less.
        unsafe {
            self.next.set(region.add(bytes));
            self.end.set(region.add(REGION.size()));
        }
        region
    }

    /// Puts a block back on the list of its class.
    #[inline]
    fn give(&self, block: *mut u8, class: usize) {
        // SAFETY: the block came from `take` for this class, so it is at least sixteen bytes and
        // aligned to sixteen, and the caller has given it up.
        unsafe { block.cast::<*mut u8>().write(self.free[class].get()) };
        self.free[class].set(block);
    }
}

// SAFETY: a block from `alloc` is at least as big as the layout asks and aligned to sixteen, or
// is the system allocator's, and the layout says which of the two a block given back is, the same
// way each time, because `class` reads nothing but the layout.
unsafe impl GlobalAlloc for Pool {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        match class(layout) {
            Some(class) => LOCAL.with(|local| local.take(class)),
            // SAFETY: the caller's layout is not zero sized, which is all the system asks.
            None => unsafe { System.alloc(layout) },
        }
    }

    #[inline]
    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        match class(layout) {
            Some(class) => LOCAL.with(|local| local.give(block, class)),
            // SAFETY: a block of this layout came from the system allocator with it.
            None => unsafe { System.dealloc(block, layout) },
        }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        match class(layout) {
            Some(class) => {
                let block = LOCAL.with(|local| local.take(class));
                if !block.is_null() {
                    // SAFETY: the block is at least `layout.size()` bytes.
                    unsafe { block.write_bytes(0, layout.size()) };
                }
                block
            }
            // SAFETY: as for `alloc`.
            None => unsafe { System.alloc_zeroed(layout) },
        }
    }

    #[inline]
    unsafe fn realloc(&self, block: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller promises the new size rounded up to the alignment does not overflow.
        let new = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        match (class(layout), class(new)) {
            // A block of the class is already as big as the new size needs.
            (Some(old), Some(class)) if old == class => block,
            // SAFETY: both layouts are the system allocator's, and the caller's promises are the
            // ones it asks for.
            (None, None) => unsafe { System.realloc(block, layout, new_size) },
            _ => {
                // SAFETY: the new layout is not zero sized, and the old block is at least
                // `layout.size()` bytes and the new one at least `new_size`, so copying the
                // smaller of the two stays inside both. The old block is the caller's to give up.
                unsafe {
                    let moved = self.alloc(new);
                    if !moved.is_null() {
                        ptr::copy_nonoverlapping(block, moved, layout.size().min(new_size));
                        self.dealloc(block, layout);
                    }
                    moved
                }
            }
        }
    }
}

/// Asks the C library's allocator to keep the memory a compile gives back instead of returning it
/// to the kernel, for the blocks [`Pool`] passes on to it.
///
/// Out of the box glibc maps each block of 128 KiB or more on its own and unmaps it on free, and
/// it trims the top of the heap as soon as 128 KiB of it is free. A compile grows a token list or
/// a table of a function past that again and again, so each growth was a fresh mapping whose
/// pages all faulted in from zero, and the regions of the pool were one mapping each as well.
/// With the heap allowed to keep 32 MiB of slack and to hold blocks up to 32 MiB, duktape.c at
/// `-O0` takes about a quarter fewer page faults and a third less system time. The 32 MiB is the
/// most glibc lets a heap block be on a 64 bit target. tamnd/rucc#3052.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(crate) fn keep_freed_memory() {
    use std::ffi::c_int;
    unsafe extern "C" {
        fn mallopt(param: c_int, value: c_int) -> c_int;
    }
    const M_TOP_PAD: c_int = -2;
    const M_MMAP_THRESHOLD: c_int = -3;
    const SLACK: c_int = 32 << 20;
    // SAFETY: `mallopt` only sets the allocator's parameters, which the C library allows at any
    // time, and nothing has been allocated by another thread yet because there is none.
    unsafe {
        mallopt(M_MMAP_THRESHOLD, SLACK);
        mallopt(M_TOP_PAD, SLACK);
    }
}

/// Nothing to tune on a C library this does not know the parameters of.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub(crate) fn keep_freed_memory() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_size_fits_its_class_and_the_classes_climb() {
        for bytes in 1..=LARGEST {
            let class = class(Layout::from_size_align(bytes, 1).unwrap()).unwrap();
            assert!(size(class) >= bytes, "{bytes} in a class of {}", size(class));
            assert_eq!(size(class) % ALIGN, 0);
            if class > 0 {
                assert!(size(class - 1) < bytes, "{bytes} would have fit a smaller class");
            }
        }
        assert_eq!(size(CLASSES - 1), LARGEST);
        assert_eq!(class(Layout::from_size_align(LARGEST + 1, 1).unwrap()), None);
        assert_eq!(class(Layout::from_size_align(64, 32).unwrap()), None);
    }

    #[test]
    fn a_freed_block_is_handed_out_again_and_keeps_what_was_written() {
        let layout = Layout::from_size_align(40, 8).unwrap();
        // SAFETY: the layout is not zero sized, each block is written inside its size, and each
        // is given back once with the layout it was made with.
        unsafe {
            let first = Pool.alloc(layout);
            first.write_bytes(7, 40);
            Pool.dealloc(first, layout);
            let again = Pool.alloc(layout);
            assert_eq!(again, first);
            let zeroed = Pool.alloc_zeroed(layout);
            assert!((0..40).all(|at| *zeroed.add(at) == 0));
            let grown = Pool.realloc(again, layout, 300);
            let grown_layout = Layout::from_size_align(300, 8).unwrap();
            grown.add(299).write(1);
            let large = Pool.realloc(grown, grown_layout, LARGEST * 2);
            assert_eq!(*large.add(299), 1);
            Pool.dealloc(large, Layout::from_size_align(LARGEST * 2, 8).unwrap());
            Pool.dealloc(zeroed, layout);
        }
    }

    #[test]
    fn a_block_freed_on_another_thread_is_still_good() {
        let layout = Layout::from_size_align(24, 8).unwrap();
        // SAFETY: the layout is not zero sized.
        let block = unsafe { Pool.alloc(layout) } as usize;
        std::thread::spawn(move || {
            let block = block as *mut u8;
            // SAFETY: the block is 24 bytes and nothing else holds it, and it is given back once.
            unsafe {
                block.write_bytes(3, 24);
                Pool.dealloc(block, layout);
                assert_eq!(Pool.alloc(layout), block);
                Pool.dealloc(block, layout);
            }
        })
        .join()
        .unwrap();
    }
}
