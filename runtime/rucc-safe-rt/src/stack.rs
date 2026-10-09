//! The init plane over a thread's own stack, for the locals the compiler can vouch for.
//!
//! Design: `spec/safe-memory/09-type-init-and-races.md` section 9.2, and row Y6 of document 03 for
//! automatic storage.
//!
//! The heap's init plane lives beside the heap, in the region [`crate::alloc`] reserves for it, and
//! a byte becomes unwritten there when an instance begins. A stack has no such moment that anything
//! outside the frame could see: a frame is made by moving a register. So the moment comes from the
//! compiler instead, which calls [`begin`] for a local at the top of the function and [`end`] for it
//! on every way out, and this file keeps the bits those two write in a shadow of the thread's stack
//! made the first time a thread begins anything.
//!
//! # Which locals
//!
//! Only the ones whose address never leaves the function they are declared in, which is
//! `rucc_safety::local`'s decision and the whole reason this is not a source of false positives. The
//! bit means unwritten, and the only thing that clears it is a store the compiler recorded. A local
//! handed to `read`, `stat` or `strtol` would be written by code that records nothing and then read
//! back as if nothing had written it, which is MSan's problem and the one section 9.2 refuses to
//! have. A local nobody else can name is written by this function's stores or not at all, so what
//! the plane says about it is the truth.
//!
//! The same decision leaves out a local whose bytes the frame layout may hand to another local
//! once its block is left, which is one with a `lifetime_end`. Beginning that one at the top of the
//! function would mark bytes unwritten under a neighbour that is live at the time.
//!
//! # Frames that are never ended
//!
//! A frame `longjmp` leaves never runs its ends, so its locals keep saying unwritten after the frame
//! is gone, and a later frame in the same place would read those bits back about storage that is
//! now its own. Two things clear them. The thread remembers the lowest address it began anything at
//! since it last looked, and both [`begin`] and [`landed`], which the compiler calls after every
//! `setjmp`, clear the plane from there up to their own frame. Everything below the frame of the
//! function that is running is a frame that has already returned, so nothing live loses a bit.
//!
//! # Where the plane is not
//!
//! A stack this thread did not start on, which is a coroutine's or a signal handler's alternate one,
//! is outside the range the plane covers, and a local begun there is not begun at all. Every
//! question about it gets the answer every address outside the watched regions gets, which is that
//! nothing is wrong. A target without `pthread_getattr_np` is the same everywhere.

use core::cell::Cell;
use core::ffi::c_void;

use crate::init::{self, Init};
use crate::tls::Slot;

/// The thread's stack and its plane, or null before it began anything, or [`REFUSED`].
static STACK: Slot = Slot::new(crate::tls::STACK);

/// What the slot holds when the plane could not be made, so that a thread that failed once does not
/// ask the system again on every local it begins.
const REFUSED: *mut c_void = core::ptr::dangling_mut::<c_void>();

/// How far below the top of its stack a thread's plane reaches at most.
///
/// A gibibyte, so that an unlimited stack size does not ask for an eighth of the address space. A
/// frame deeper than that is a frame whose locals are not begun.
const DEEPEST: usize = 1 << 30;

/// How many bytes in front of the shadow the description of it takes.
const HEADER: usize = 64;

const _: () = assert!(size_of::<Stack>() <= HEADER);

/// One thread's stack and the plane over it, kept at the front of the plane's own mapping.
pub(crate) struct Stack {
    /// The lowest address of the stack the plane covers.
    lo: usize,
    /// One past the highest.
    hi: usize,
    /// How long the mapping this sits at the front of is, for giving it back.
    len: usize,
    /// No byte below this has a bit set, as far as this thread knows.
    low: Cell<usize>,
    /// The plane.
    init: Init,
}

impl Stack {
    /// Whether `addr` is on the part of the stack the plane covers.
    const fn holds(&self, addr: usize) -> bool {
        self.lo <= addr && addr < self.hi
    }

    /// `size` cut down to what is left of the stack from `addr`, which it holds.
    const fn clipped(&self, addr: usize, size: usize) -> usize {
        let room = self.hi - addr;
        if size < room { size } else { room }
    }

    /// Clears every bit between the lowest one that may be set and `here`, which is in the frame of
    /// the function that is running, and says nothing below `here` is set any more.
    fn forget_below(&self, here: usize) {
        let from = self.low.get().max(self.lo);
        if from < here && here <= self.hi {
            // SAFETY: `[from, here)` is inside the stack the plane was made for.
            unsafe { self.init.set(from, here - from) };
            self.low.set(here);
        }
    }
}

/// This thread's stack and plane, when it has begun anything.
fn current() -> Option<&'static Stack> {
    let at = STACK.get();
    if at.is_null() || at == REFUSED {
        return None;
    }
    // SAFETY: a pointer this thread stored, to the front of a mapping that stays until the thread
    // exits, and nothing else writes it.
    Some(unsafe { &*at.cast::<Stack>() })
}

/// The plane for `addr`, when it is on this thread's stack and the thread has begun anything.
fn covering(addr: usize) -> Option<&'static Stack> {
    current().filter(|stack| stack.holds(addr))
}

/// Whether `addr` is on the stack of the thread asking, as far as this thread's plane knows.
///
/// For the report, which says so rather than saying the address is outside the heap and leaving
/// the reader to work out where it is.
pub(crate) fn mine(addr: usize) -> bool {
    covering(addr).is_some()
}

/// An address in the frame of the function that called this, or as near as it matters.
#[inline(always)]
fn here() -> usize {
    let mark = 0_u8;
    core::hint::black_box(&raw const mark) as usize
}

/// Whether every byte of `[addr, addr + size)` on this thread's stack has been written since its
/// local began, which is true of every byte that is not on it.
///
/// # Safety
///
/// `addr` is whatever the program computed and is never read through.
pub(crate) unsafe fn allows(addr: usize, size: usize) -> bool {
    let Some(stack) = covering(addr) else { return true };
    // SAFETY: the range starts on the stack and is clipped to it.
    unsafe { stack.init.allows(addr, stack.clipped(addr, size)) }
}

/// [`allows`] for a long range, a word of the plane at a time.
///
/// # Safety
///
/// As [`allows`].
pub(crate) unsafe fn sweeps(addr: usize, size: usize) -> bool {
    let Some(stack) = covering(addr) else { return true };
    // SAFETY: as in `allows`.
    unsafe { stack.init.sweep(addr, stack.clipped(addr, size)) }
}

/// The judgement a store makes, for one on this thread's stack.
///
/// # Safety
///
/// As [`allows`].
pub(crate) unsafe fn wrote(addr: usize, size: usize) {
    let Some(stack) = covering(addr) else { return };
    // SAFETY: as in `allows`.
    unsafe { stack.init.set(addr, stack.clipped(addr, size)) }
}

/// A local of `size` bytes at `base` begins, and nothing has written it yet.
///
/// Makes the plane the first time a thread asks. Before marking anything it clears whatever a frame
/// left by `longjmp` left below this one, as the module comment says.
///
/// # Safety
///
/// `base` is a local of the calling function's frame, and `size` is its size.
#[inline(never)]
pub unsafe fn begin(base: *const c_void, size: usize) {
    let base = base as usize;
    let Some(stack) = made() else { return };
    if !stack.holds(base) {
        return;
    }
    stack.forget_below(here());
    // SAFETY: the range starts on the stack and is clipped to it.
    unsafe { stack.init.forget(base, stack.clipped(base, size)) };
    stack.low.set(stack.low.get().min(base));
}

/// The local of `size` bytes at `base` is going away with its frame, so its bytes say nothing.
///
/// # Safety
///
/// As [`begin`].
pub unsafe fn end(base: *const c_void, size: usize) {
    // SAFETY: the same range the begin named.
    unsafe { wrote(base as usize, size) };
}

/// Control came back to a `setjmp`, so every frame below this one has returned.
///
/// After every `setjmp` and not only after one that came back from a `longjmp`, because the first
/// return is the cheap one: nothing below has been begun since the last time anything looked.
#[inline(never)]
pub fn landed() {
    if let Some(stack) = current() {
        stack.forget_below(here());
    }
}

/// This thread's stack and plane, made now if the thread has none yet.
fn made() -> Option<&'static Stack> {
    let at = STACK.get();
    if at == REFUSED {
        return None;
    }
    if at.is_null() {
        let made = build().unwrap_or(REFUSED);
        // SAFETY: the mapping lives until the thread exits, and [`gone`] takes it out of the slot
        // before it gives it back.
        unsafe { STACK.set(made) };
    }
    current()
}

/// Maps a plane for the thread's stack and writes its description at the front of it.
#[cfg(target_os = "linux")]
fn build() -> Option<*mut c_void> {
    let (lo, hi) = bounds()?;
    let lo = lo.max(hi.saturating_sub(DEEPEST)).next_multiple_of(init::SPAN);
    if hi <= lo {
        return None;
    }
    let len = HEADER + init::shadow(hi - lo);
    let at = crate::alloc::map(len)?;
    let origin = (at + HEADER).wrapping_sub(lo / init::SPAN);
    // SAFETY: the shadow is the part of the mapping behind the header, and its first byte answers
    // for `lo` and its last for `hi - 1` by the arithmetic above.
    let init = unsafe { Init::new(origin) };
    let stack = Stack { lo, hi, len, low: Cell::new(usize::MAX), init };
    // SAFETY: the mapping is fresh, writable and longer than the header, and page aligned.
    unsafe { (at as *mut Stack).write(stack) };
    watch(at as *mut c_void);
    Some(at as *mut c_void)
}

/// Everywhere else there is no portable way to ask where the stack is.
#[cfg(not(target_os = "linux"))]
fn build() -> Option<*mut c_void> {
    None
}

/// Where this thread's stack is, as the C library sees it.
#[cfg(target_os = "linux")]
pub(crate) fn bounds() -> Option<(usize, usize)> {
    /// Room for a `pthread_attr_t`, which is fifty six or sixty four bytes on the targets there are.
    #[repr(C, align(16))]
    struct Attr([u8; 128]);

    unsafe extern "C" {
        fn pthread_self() -> *mut c_void;
        fn pthread_getattr_np(thread: *mut c_void, attr: *mut Attr) -> i32;
        fn pthread_attr_getstack(attr: *const Attr, at: *mut *mut c_void, size: *mut usize) -> i32;
        fn pthread_attr_destroy(attr: *mut Attr) -> i32;
    }
    let mut attr = Attr([0; 128]);
    // SAFETY: the attribute is ours and big enough, and is destroyed below once it was made.
    if unsafe { pthread_getattr_np(pthread_self(), &raw mut attr) } != 0 {
        return None;
    }
    let mut at: *mut c_void = core::ptr::null_mut();
    let mut size = 0_usize;
    // SAFETY: the attribute was made above, and both outputs are ours.
    let failed = unsafe { pthread_attr_getstack(&raw const attr, &raw mut at, &raw mut size) };
    // SAFETY: made above and not used again.
    unsafe { pthread_attr_destroy(&raw mut attr) };
    let lo = at as usize;
    (failed == 0 && size != 0).then(|| (lo, lo.saturating_add(size)))
}

/// Gives the plane back when its thread exits, so that a program that starts a thread per request
/// does not run out of mappings.
#[cfg(target_os = "linux")]
fn watch(at: *mut c_void) {
    use core::sync::atomic::{AtomicU32, Ordering};

    /// Not made yet.
    const COLD: u32 = 0;
    /// Being made by another thread right now.
    const MAKING: u32 = 1;
    /// Made, and `KEY` holds it.
    const MADE: u32 = 2;
    /// Could not be made, and asking again would not help.
    const FAILED: u32 = 3;
    /// Which of the four the key is in.
    static STATE: AtomicU32 = AtomicU32::new(COLD);
    /// The key, once `STATE` says it was made.
    static KEY: AtomicU32 = AtomicU32::new(0);

    unsafe extern "C" {
        fn pthread_key_create(key: *mut u32, dtor: unsafe extern "C" fn(*mut c_void)) -> i32;
        fn pthread_setspecific(key: u32, value: *const c_void) -> i32;
        fn munmap(at: *mut c_void, len: usize) -> i32;
    }

    /// The destructor: out of the slot first, so that nothing reads the plane after it is gone.
    unsafe extern "C" fn gone(at: *mut c_void) {
        if STACK.get() == at {
            // SAFETY: null is always a valid thing to store.
            unsafe { STACK.set(core::ptr::null_mut()) };
        }
        // SAFETY: the mapping `build` made, whose header says how long it is.
        let len = unsafe { (*at.cast::<Stack>()).len };
        // SAFETY: nothing names the mapping any more.
        unsafe { munmap(at, len) };
    }

    loop {
        match STATE.load(Ordering::Acquire) {
            MADE => break,
            FAILED => return,
            MAKING => core::hint::spin_loop(),
            _ => {
                if STATE
                    .compare_exchange(COLD, MAKING, Ordering::Acquire, Ordering::Relaxed)
                    .is_err()
                {
                    continue;
                }
                let mut key = 0_u32;
                // SAFETY: the key is ours to fill in and the destructor has the signature asked for.
                let failed = unsafe { pthread_key_create(&raw mut key, gone) };
                KEY.store(key, Ordering::Relaxed);
                STATE.store(if failed == 0 { MADE } else { FAILED }, Ordering::Release);
            }
        }
    }
    // SAFETY: the key was made above and the value is this thread's own mapping.
    unsafe { pthread_setspecific(KEY.load(Ordering::Relaxed), at) };
}

/// The names generated code is compiled against.
pub mod exports {
    /// As [`super::begin`].
    ///
    /// # Safety
    ///
    /// As [`super::begin`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_local_begin(base: *const core::ffi::c_void, size: usize) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::begin(base, size) };
    }

    /// As [`super::end`].
    ///
    /// # Safety
    ///
    /// As [`super::end`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_local_end(base: *const core::ffi::c_void, size: usize) {
        // SAFETY: as above.
        unsafe { super::end(base, size) };
    }

    /// As [`super::landed`].
    #[unsafe(no_mangle)]
    pub extern "C" fn __rucc_local_landed() {
        super::landed();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn a_local_reads_as_unwritten_until_it_is_written_and_says_nothing_once_it_ends() {
        let local = [0_u8; 32];
        let at = core::hint::black_box(local.as_ptr()) as usize;
        // SAFETY: a local of this test, which outlives every use below.
        unsafe {
            begin(at as *const c_void, 32);
            assert!(!allows(at, 4));
            wrote(at, 4);
            assert!(allows(at, 4));
            assert!(!allows(at + 4, 4));
            assert!(!sweeps(at, 32));
            wrote(at, 32);
            assert!(sweeps(at, 32));
            begin(at as *const c_void, 32);
            assert!(!allows(at + 8, 8));
            end(at as *const c_void, 32);
            assert!(allows(at, 32));
        }
    }

    /// A frame that begins a local and returns without ending it, which is what `longjmp` leaves.
    #[inline(never)]
    fn abandoned(depth: usize) -> usize {
        if depth > 0 {
            return core::hint::black_box(abandoned(depth - 1));
        }
        let local = [0_u8; 64];
        let at = core::hint::black_box(local.as_ptr()) as usize;
        // SAFETY: a local of this frame.
        unsafe { begin(at as *const c_void, 64) };
        at
    }

    #[test]
    fn a_frame_left_without_its_end_is_forgotten_where_control_lands() {
        let at = abandoned(8);
        // SAFETY: only the plane is read, never the address.
        unsafe { assert!(!allows(at, 8), "the abandoned local still says unwritten") };
        landed();
        // SAFETY: as above.
        unsafe { assert!(allows(at, 64)) };
    }

    #[test]
    fn a_frame_left_without_its_end_is_forgotten_by_the_next_local_begun_above_it() {
        let at = abandoned(8);
        let local = [0_u8; 16];
        let mine = core::hint::black_box(local.as_ptr()) as usize;
        // SAFETY: as above, and `mine` is a local of this test.
        unsafe {
            begin(mine as *const c_void, 16);
            assert!(allows(at, 64));
            assert!(!allows(mine, 16));
            end(mine as *const c_void, 16);
        }
    }

    #[test]
    fn an_address_off_this_threads_stack_is_never_refused() {
        let heap = std::boxed::Box::new([0_u8; 16]);
        let at = heap.as_ptr() as usize;
        // SAFETY: never read through.
        unsafe {
            begin(at as *const c_void, 16);
            assert!(allows(at, 16));
        }
    }
}
