//! The atomics on an object too wide for one instruction, and the table of locks under them.
//!
//! Design: `spec/12-abi-and-runtime.md` section 12.8, and tamnd/rucc#1064. The reference for
//! `runtime/builtins/atomic.c`, which is the one that ships, for the reason [`crate`] gives.
//!
//! Four routines, which are the ones libatomic exports without a width in their name. They take
//! the size as their first argument and everything else through a pointer, so one routine serves
//! every width: an access to a seventeen byte structure and an access to a `__int128` are the same
//! call with a different number in it.
//!
//! # The lock, and what it does not give
//!
//! An access is a lock, a copy of a few bytes, and an unlock. There is nothing else available on a
//! machine with no instruction at the width, since the only way to make the access indivisible is
//! for everybody who touches the object to agree.
//!
//! Which is the hazard. Two pieces of a program agree only if they take the same lock, and the
//! lock is in here. A program half of whose accesses come through this and half of which go
//! through the libatomic GCC linked in has two tables and two locks for one object, and is not
//! atomic at all. GCC's libatomic and LLVM's compiler-rt have the same disagreement with each
//! other, so this is the state of the world rather than a hole opened here, and the way out is one
//! library on the link line.
//!
//! # The ordering argument
//!
//! Every one of the four takes one and none of them reads it. The lock is an acquire and the
//! unlock is a release, so two accesses to the same object are ordered against each other whatever
//! either asked for. A caller that asked for less has been given more, which it is allowed to be,
//! and libatomic ignores the argument here for the same reason.

// Only the C entry points below name it, and they are not compiled under `cargo test`.
#[cfg(not(test))]
use core::ffi::c_void;
use core::sync::atomic::{AtomicU8, Ordering};

/// How many locks there are, as the number of bits the hash keeps. Sixty four entries, which is
/// what libatomic has: small enough to be one page of zeroed memory and large enough that two
/// unrelated objects meeting in it is rare.
const LOCK_BITS: u32 = 6;

/// How many that is.
const LOCKS: usize = 1 << LOCK_BITS;

/// One lock, on a cache line of its own so that two threads holding different ones are not handing
/// the same line back and forth for no reason.
#[repr(align(64))]
struct Guard(AtomicU8);

/// The table. Zero is free and one is held.
static TABLE: [Guard; LOCKS] = [const { Guard(AtomicU8::new(0)) }; LOCKS];

/// Which entry an object uses, decided by its address and by nothing else.
///
/// The multiply is the whole of it. An object wide enough to reach these is aligned to eight bytes
/// at least and usually to sixteen, so the low bits of every address here are zero and reading the
/// entry straight off the address would put a whole array of atomic objects onto one entry. A
/// multiply by an odd number carries what is above those bits up into the top of the word, and the
/// top six bits of the product are the entry. The constant is the odd number nearest the golden
/// ratio at sixty four bits, truncated to the low half on a target whose pointers are narrower,
/// which leaves it odd and is all this asks of it.
///
/// One lock for the whole object, at the address the caller passed. libatomic hashes by cache line
/// and takes every lock the object spans, which is what lets it handle an access to part of an
/// atomic object. There is no such access in C: an `_Atomic` object is read and written whole, so
/// every call about one arrives with the same address and lands on the same entry. The simpler
/// rule also cannot deadlock, since it never holds two locks at once.
fn guard_for(object: *const u8) -> &'static Guard {
    let mixed = (object as usize).wrapping_mul(0x9E37_79B9_7F4A_7C15_u64 as usize);
    &TABLE[mixed >> (usize::BITS - LOCK_BITS)]
}

/// Takes the lock, by spinning. Not by asking a scheduler for anything: what runs under it is a
/// copy of a few bytes, the wait is shorter than the call into the kernel that would avoid it, and
/// half the targets in the matrix have no kernel to call.
fn take(guard: &Guard) {
    while guard.0.swap(1, Ordering::Acquire) != 0 {
        core::hint::spin_loop();
    }
}

fn drop_guard(guard: &Guard) {
    guard.0.store(0, Ordering::Release);
}

/// A byte at a time, for the reason the C does it: a routine holding a spin lock should not reach
/// a name a program is allowed to replace with its own.
///
/// # Safety
///
/// Both ranges usable for `count` bytes, and not overlapping.
unsafe fn copy(to: *mut u8, from: *const u8, count: usize) {
    for at in 0..count {
        // SAFETY: the caller promises `count` bytes on both sides.
        unsafe { to.add(at).write(from.add(at).read()) };
    }
}

/// Whether two ranges hold the same bytes.
///
/// The comparison an exchange makes is over the object's representation and not over its value,
/// which is what the standard says and is why a structure with padding compares unequal to a copy
/// of itself that was built a different way. gcc compares representations here too.
///
/// # Safety
///
/// Both ranges readable for `count` bytes.
unsafe fn same(one: *const u8, two: *const u8, count: usize) -> bool {
    for at in 0..count {
        // SAFETY: the caller promises `count` readable bytes on both sides.
        if unsafe { one.add(at).read() != two.add(at).read() } {
            return false;
        }
    }
    true
}

/// `void __atomic_load(size_t size, void *object, void *into, int order)`.
///
/// # Safety
///
/// The C contract: both ranges usable for `size` bytes, and `object` reached only through these
/// four routines for as long as anybody else may be reaching it.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __atomic_load(
    size: usize,
    object: *mut c_void,
    into: *mut c_void,
    order: i32,
) {
    let _ = order;
    // SAFETY: the caller promises the C contract.
    unsafe { load(size, object.cast(), into.cast()) };
}

/// `void __atomic_store(size_t size, void *object, void *value, int order)`.
///
/// # Safety
///
/// As [`__atomic_load`].
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __atomic_store(
    size: usize,
    object: *mut c_void,
    value: *mut c_void,
    order: i32,
) {
    let _ = order;
    // SAFETY: the caller promises the C contract.
    unsafe { store(size, object.cast(), value.cast()) };
}

/// `void __atomic_exchange(size_t size, void *object, void *value, void *into, int order)`.
///
/// # Safety
///
/// As [`__atomic_load`], with three ranges rather than two.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __atomic_exchange(
    size: usize,
    object: *mut c_void,
    value: *mut c_void,
    into: *mut c_void,
    order: i32,
) {
    let _ = order;
    // SAFETY: the caller promises the C contract.
    unsafe { exchange(size, object.cast(), value.cast(), into.cast()) };
}

/// `bool __atomic_compare_exchange(size_t size, void *object, void *expected, void *desired, int
/// success, int failure)`.
///
/// # Safety
///
/// As [`__atomic_load`], with `expected` writable as well as readable.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __atomic_compare_exchange(
    size: usize,
    object: *mut c_void,
    expected: *mut c_void,
    desired: *mut c_void,
    success: i32,
    failure: i32,
) -> bool {
    let _ = (success, failure);
    // SAFETY: the caller promises the C contract.
    unsafe { compare_exchange(size, object.cast(), expected.cast(), desired.cast()) }
}

/// The load, with the lock around it.
///
/// # Safety
///
/// As [`__atomic_load`].
pub unsafe fn load(size: usize, object: *mut u8, into: *mut u8) {
    let guard = guard_for(object);
    take(guard);
    // SAFETY: the caller promises `size` bytes on both sides.
    unsafe { copy(into, object, size) };
    drop_guard(guard);
}

/// The store.
///
/// # Safety
///
/// As [`__atomic_store`].
pub unsafe fn store(size: usize, object: *mut u8, value: *const u8) {
    let guard = guard_for(object);
    take(guard);
    // SAFETY: the caller promises `size` bytes on both sides.
    unsafe { copy(object, value, size) };
    drop_guard(guard);
}

/// The exchange.
///
/// The old value comes out before the new one goes in, which is the order libatomic writes it in
/// and is the order that gives a caller who passed one buffer for both an exchange with itself
/// rather than a buffer full of what it already had.
///
/// # Safety
///
/// As [`__atomic_exchange`].
pub unsafe fn exchange(size: usize, object: *mut u8, value: *const u8, into: *mut u8) {
    let guard = guard_for(object);
    take(guard);
    // SAFETY: the caller promises `size` bytes on all three sides.
    unsafe {
        copy(into, object, size);
        copy(object, value, size);
    }
    drop_guard(guard);
}

/// The compare and exchange.
///
/// The write back on failure is the reason the expected value arrives by pointer. A caller that
/// lost the race gets what was actually there, so the loop it is almost certainly sitting in can
/// go round again without reading the object a second time.
///
/// # Safety
///
/// As [`__atomic_compare_exchange`].
pub unsafe fn compare_exchange(
    size: usize,
    object: *mut u8,
    expected: *mut u8,
    desired: *const u8,
) -> bool {
    let guard = guard_for(object);
    take(guard);
    // SAFETY: the caller promises `size` bytes on all three sides.
    let matched = unsafe {
        let matched = same(object, expected, size);
        if matched {
            copy(object, desired, size);
        } else {
            copy(expected, object, size);
        }
        matched
    };
    drop_guard(guard);
    matched
}

#[cfg(test)]
mod tests {
    use core::cell::UnsafeCell;
    use std::sync::Arc;
    use std::vec;
    use std::vec::Vec;

    use super::*;

    /// The widths worth running everything at: below a word, at one, at the two the machine stops
    /// at, and a few that are not a multiple of anything.
    const SIZES: [usize; 8] = [0, 1, 7, 8, 9, 16, 17, 24];

    fn bytes(seed: u8, size: usize) -> Vec<u8> {
        (0..size).map(|at| seed.wrapping_add(at as u8).wrapping_mul(31)).collect()
    }

    #[test]
    fn a_load_gives_back_what_is_there_and_a_store_puts_it_there() {
        for size in SIZES {
            let mut object = bytes(1, size);
            let mut into = vec![0xAA; size];
            // SAFETY: both buffers are `size` bytes long and neither overlaps the other.
            unsafe { load(size, object.as_mut_ptr(), into.as_mut_ptr()) };
            assert_eq!(into, object, "load at {size}");

            let value = bytes(2, size);
            // SAFETY: both buffers are `size` bytes long and neither overlaps the other.
            unsafe { store(size, object.as_mut_ptr(), value.as_ptr()) };
            assert_eq!(object, value, "store at {size}");
        }
    }

    #[test]
    fn an_exchange_answers_the_old_value_and_leaves_the_new_one() {
        for size in SIZES {
            let mut object = bytes(3, size);
            let was = object.clone();
            let value = bytes(4, size);
            let mut into = vec![0xAA; size];
            // SAFETY: all three buffers are `size` bytes long and none overlaps another.
            unsafe { exchange(size, object.as_mut_ptr(), value.as_ptr(), into.as_mut_ptr()) };
            assert_eq!(into, was, "the old value at {size}");
            assert_eq!(object, value, "the new value at {size}");
        }
    }

    /// One buffer for the value and the answer, which the order of the two copies decides. The
    /// object keeps what it had, because the old value was read out into the buffer the new one
    /// was about to come from.
    #[test]
    fn an_exchange_through_one_buffer_is_an_exchange_with_itself() {
        for size in SIZES {
            let mut object = bytes(5, size);
            let was = object.clone();
            let mut both = bytes(6, size);
            let pointer = both.as_mut_ptr();
            // SAFETY: both buffers are `size` bytes long, and the value and the answer being the
            // same buffer is the case under test rather than an accident.
            unsafe { exchange(size, object.as_mut_ptr(), pointer, pointer) };
            assert_eq!(object, was, "at {size}");
            assert_eq!(both, was, "at {size}");
        }
    }

    #[test]
    fn a_compare_exchange_that_matched_writes_the_desired_value() {
        for size in SIZES {
            let mut object = bytes(7, size);
            let mut expected = object.clone();
            let desired = bytes(8, size);
            // SAFETY: all three buffers are `size` bytes long and none overlaps another.
            let matched = unsafe {
                compare_exchange(size, object.as_mut_ptr(), expected.as_mut_ptr(), desired.as_ptr())
            };
            assert!(matched, "at {size}");
            assert_eq!(object, desired, "at {size}");
        }
    }

    /// The failing case, which is the one with something to say. Nothing is written to the object
    /// and what was there is written over the expected value.
    #[test]
    fn a_compare_exchange_that_did_not_match_hands_back_what_was_there() {
        for size in SIZES.into_iter().filter(|size| *size != 0) {
            let mut object = bytes(9, size);
            let was = object.clone();
            let mut expected = bytes(10, size);
            let desired = bytes(11, size);
            // SAFETY: all three buffers are `size` bytes long and none overlaps another.
            let matched = unsafe {
                compare_exchange(size, object.as_mut_ptr(), expected.as_mut_ptr(), desired.as_ptr())
            };
            assert!(!matched, "at {size}");
            assert_eq!(object, was, "at {size}");
            assert_eq!(expected, was, "at {size}");
        }
    }

    /// A size of zero is two objects with nothing in them, which are equal, so the exchange
    /// succeeds. Its own test because it is the one width where the answer is not obvious and
    /// because a loop that runs no times is where an off by one shows up.
    #[test]
    fn nothing_compares_equal_to_nothing() {
        let mut object = [0u8; 1];
        let mut expected = [1u8; 1];
        let desired = [2u8; 1];
        // SAFETY: all three buffers are a byte long, which is more than the zero the call reads.
        let matched = unsafe {
            compare_exchange(0, object.as_mut_ptr(), expected.as_mut_ptr(), desired.as_ptr())
        };
        assert!(matched);
        assert_eq!(object, [0], "nothing was copied either");
        assert_eq!(expected, [1]);
    }

    /// Every entry in the table is reachable, which is what the multiply is there for. Addresses
    /// sixteen bytes apart, which is how an array of atomic objects is laid out and is the case a
    /// hash that read the address directly would put onto one entry.
    ///
    /// It takes a few hundred of them to reach all sixty four. A step of sixteen bytes moves the
    /// entry seven or eight places back, so the walk goes round the table many times over before
    /// the entries it keeps stepping past have all been landed on: two hundred and fifty six
    /// addresses leave seven of them out, and the last one comes in at two hundred and eighty
    /// seven. The count below is eight times the table, which is that with room to spare.
    #[test]
    fn an_array_of_objects_spreads_across_the_whole_table() {
        let mut seen = [false; LOCKS];
        for at in 0..LOCKS * 8 {
            let address = (0x1000 + at * 16) as *const u8;
            let entry = (guard_for(address) as *const Guard as usize - TABLE.as_ptr() as usize)
                / size_of::<Guard>();
            seen[entry] = true;
        }
        assert!(seen.iter().all(|hit| *hit), "{seen:?}");
    }

    /// The lock, held against itself. Four threads each add one to a counter that is three words
    /// wide through the compare and exchange, and the answer is the number of additions, which it
    /// would not be if two of them ever got inside at once.
    ///
    /// The count is small on purpose. What this can catch is a lock that does not lock at all, and
    /// that shows up in the first few thousand rounds or not at all; running it longer would spend
    /// the time in the spin rather than in the test.
    #[test]
    fn four_threads_counting_through_the_exchange_do_not_lose_an_addition() {
        const WIDTH: usize = 24;
        const THREADS: usize = 4;
        const ROUNDS: usize = 2000;

        let mut start = [0u8; WIDTH];
        start[16..].copy_from_slice(&7usize.to_ne_bytes());
        let object = Arc::new(Counter(UnsafeCell::new(start)));
        let mut running = Vec::new();
        for _ in 0..THREADS {
            let held = Arc::clone(&object);
            running.push(std::thread::spawn(move || {
                let raw = held.0.get().cast::<u8>();
                for _ in 0..ROUNDS {
                    let mut expected = [0u8; WIDTH];
                    // SAFETY: the counter and the buffer are both `WIDTH` bytes long, and the
                    // other threads reach the same counter only through these same routines.
                    unsafe { load(WIDTH, raw, expected.as_mut_ptr()) };
                    loop {
                        let mut desired = expected;
                        let count =
                            usize::from_ne_bytes(desired[..8].try_into().unwrap()).wrapping_add(1);
                        desired[..8].copy_from_slice(&count.to_ne_bytes());
                        // SAFETY: the counter and both buffers are `WIDTH` bytes long, and the
                        // other threads reach the same counter only through these same routines.
                        let done = unsafe {
                            compare_exchange(WIDTH, raw, expected.as_mut_ptr(), desired.as_ptr())
                        };
                        if done {
                            break;
                        }
                    }
                }
            }));
        }
        for thread in running {
            thread.join().unwrap();
        }
        // SAFETY: every thread has been joined, so this is the only reader left.
        let ended = unsafe { *object.0.get() };
        let word = |at: usize| usize::from_ne_bytes(ended[at..at + 8].try_into().unwrap());
        assert_eq!(word(0), THREADS * ROUNDS);
        assert_eq!(word(8), 0, "the middle word was carried along");
        assert_eq!(word(16), 7, "and so was the last one");
    }

    /// Twenty four bytes, which is the width the test above counts in. Three words rather than
    /// one, so that a copy which stopped at the counter and a copy which ran past the object are
    /// both visible: the first word holds the count, the second is zero the whole way through and
    /// the third holds a number nothing writes.
    ///
    /// A cell rather than three atomics, because what the threads do to it is exactly what a
    /// program does to an object too wide for an instruction. Every byte of it is read and written
    /// by an ordinary copy, and what makes that safe is the lock and nothing else.
    #[repr(align(8))]
    struct Counter(UnsafeCell<[u8; 24]>);

    // SAFETY: every access to the bytes goes through the routines above, which hold the lock.
    unsafe impl Sync for Counter {}
}
