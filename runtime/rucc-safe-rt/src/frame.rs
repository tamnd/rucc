//! The call frame capabilities travel in, beside the arguments rather than inside them.
//!
//! Design: `spec/safe-memory/05-representation.md` section 5.3.
//!
//! The rule the whole design hangs on is that an instrumented function's calling convention is
//! unchanged. Pointer arguments go in the same registers, in the same order, at the same size, and
//! `sizeof(void *)` is still eight, so an instrumented `struct stat` is the `struct stat` the
//! kernel writes. This is where Fil-C stops and it is the reason it stops there: a design that
//! needs the whole world instrumented cannot reach a kernel, because a kernel links firmware blobs
//! and hand written assembly.
//!
//! So the capability of an argument does not travel in the argument. It travels here, in a small
//! frame in thread local storage that the caller writes and the callee reads, indexed by argument
//! position.
//!
//! # What makes it safe to read
//!
//! The frame is only meaningful if the function reading it is the one the caller wrote it for.
//! Three things together are what say so.
//!
//! The magic word, which is what tells an instrumented function that it was called by another one
//! rather than by code that knows nothing about any of this.
//!
//! Taking the frame consumes it. A callee that reads the frame unlinks it in the same breath, so a
//! function it calls afterwards finds nothing there and treats its own arguments as recovered.
//! Without this a frame written once would be believed by every function down the chain.
//!
//! And a call whose callee might not be instrumented is preceded by [`clear`] rather than by
//! [`publish`], which is the compiler's half. Publishing to a callee that never takes would leave
//! the frame in place for whatever that callee calls back into, and a callback entered from
//! uninstrumented code with somebody else's capabilities in hand is worse than one entered with
//! none. Document 10 section 10.8 is where that case is written down, and its answer is that the
//! callback recovers, which is what finding nothing here gets it.
//!
//! # Why the storage is a `pthread` key
//!
//! The spec costs this at one thread local access per call, and that is what generated code will
//! eventually do: the frame is a thread local symbol and reaching it is an add to the thread
//! pointer. What is here is a call to a function that does the access, because the attribute that
//! gives a `#![no_std]` Rust crate a thread local of its own is not stable, and because until the
//! compiler emits the access there is nothing to be faster than. The shape of the frame is the part
//! that has to be right now, since it is an ABI and the two halves have to agree about it.

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::layout::Cap;

/// How many arguments a frame carries capabilities for.
///
/// Eight, which is section 5.3's number and is more pointer arguments than nearly any function
/// takes. A call with more than eight pointers hands capabilities for the first eight and the rest
/// are recovered at the callee, which is a weakening the summary counts rather than a refusal.
pub const ARGS: usize = 8;

/// What an instrumented caller writes so the callee knows there is a frame.
///
/// Not a checksum and not a secret. It is one word that uninstrumented storage is very unlikely to
/// hold by accident, and the two things that actually make the frame trustworthy are that taking it
/// consumes it and that a call to an unknown callee clears it first.
pub const MAGIC: u32 = 0x7275_6363;

/// The frame itself, which is section 5.3's structure word for word.
///
/// `#[repr(C)]` because the writer and the reader are different functions in different translation
/// units, and eventually one of them is generated code and the other is this crate.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Frame {
    /// [`MAGIC`] when an instrumented caller wrote this, and anything else otherwise.
    pub magic: u32,
    /// How many of `args` the caller filled in.
    pub argc: u16,
    /// Spare, for the things section 5.3 has not needed yet.
    pub flags: u16,
    /// The capabilities of the first [`ARGS`] pointer arguments, in argument order.
    pub args: [Cap; ARGS],
    /// The capability of the returned pointer, which the callee writes and the caller reads.
    pub ret: Cap,
    /// The frame this one was published over, restored when this one is taken.
    pub outer: *mut Frame,
}

impl Frame {
    /// A frame that says nothing, which is what a caller starts from and fills in.
    pub const EMPTY: Self = Self {
        magic: 0,
        argc: 0,
        flags: 0,
        args: [Cap::BOTTOM; ARGS],
        ret: Cap::BOTTOM,
        outer: core::ptr::null_mut(),
    };

    /// The capability for the argument at `at`, or the bottom one.
    ///
    /// Bottom for a position the caller did not describe as well as for one it described as
    /// nothing, and the callee treats the two the same way: recover from the planes, and count the
    /// recovery. There is no third answer a callee could act on.
    #[must_use]
    pub fn arg(&self, at: usize) -> Cap {
        if at >= self.argc as usize || at >= ARGS {
            return Cap::BOTTOM;
        }
        self.args[at]
    }
}

/// What the platform calls a `pthread_key_t`.
///
/// The one place this module is not the same everywhere. Apple's is an `unsigned long` and
/// everybody else's is an `unsigned int`, and getting it wrong means `pthread_key_create` writing
/// eight bytes into four.
#[cfg(target_vendor = "apple")]
type Key = core::ffi::c_ulong;
/// See the Apple arm above.
#[cfg(not(target_vendor = "apple"))]
type Key = core::ffi::c_uint;

/// Where this thread's innermost published frame is.
///
/// `pthread_getspecific` rather than a thread local variable, for the reason the module comment
/// gives. The value is a `*mut Frame` that points into the publishing function's own stack frame,
/// which is why taking one unlinks it: the storage stops being a frame the moment that function
/// returns.
#[must_use]
pub fn current() -> *mut Frame {
    let Some(key) = key() else { return core::ptr::null_mut() };
    // SAFETY: the key was made by `pthread_key_create` and never deleted, and the only values ever
    // stored under it are the frame pointers below.
    unsafe { pthread_getspecific(key) }.cast()
}

/// Makes `frame` the one the next instrumented callee will read.
///
/// The caller fills the arguments in, publishes, and makes the call. Whatever was published before
/// is remembered in `outer` and comes back when this frame is taken or when the caller restores it.
///
/// # Safety
///
/// `frame` stays where it is and stays valid until the call it was published for has returned, and
/// nothing else publishes it in the meantime. In practice it is a local of the calling function,
/// which is exactly as long lived as that.
pub unsafe fn publish(frame: *mut Frame) {
    let Some(key) = key() else { return };
    if frame.is_null() {
        return;
    }
    // SAFETY: the caller says the frame is theirs and is valid for the call it is about to make.
    unsafe {
        (*frame).magic = MAGIC;
        (*frame).outer = current();
    }
    // SAFETY: as in `current`, and the pointer stored is the caller's own frame.
    unsafe { pthread_setspecific(key, frame.cast()) };
}

/// The frame this call was given, if it was given one, and unlinks it either way.
///
/// A copy rather than a pointer, because what the callee wants is the capabilities and the frame
/// stops being a frame as soon as the caller returns. `None` is a call from uninstrumented code, a
/// call the compiler could not prove was instrumented, or a second call to this function within one
/// callee, and all three mean the same thing: recover the arguments from the planes and count it.
#[must_use]
pub fn take() -> Option<Frame> {
    let frame = current();
    if frame.is_null() {
        return None;
    }
    // SAFETY: a published frame is valid for the call it was published for, which is this one.
    if unsafe { (*frame).magic } != MAGIC {
        return None;
    }
    // SAFETY: as above.
    let taken = unsafe { frame.read() };
    // Consumed. Anything this callee goes on to call finds what its own caller published, which for
    // an uninstrumented callee is nothing.
    // SAFETY: as above, and clearing the word is what stops the frame being believed twice.
    unsafe { (*frame).magic = 0 };
    restore(taken.outer);
    Some(taken)
}

/// Puts the frame back to `outer`, which a caller does after the call it published for.
///
/// # Safety
///
/// `outer` is a frame that is still live, or null. Passing the frame of a function that has already
/// returned would leave the next callee reading a stack slot that has been reused.
pub unsafe fn restore_to(outer: *mut Frame) {
    restore(outer);
}

/// Says there is no frame, which is what a call to a callee that might not be instrumented does.
///
/// One store, and only at a call site whose callee the compiler could not prove was instrumented.
/// Without it a frame published for a callee that never takes stays there, and a callback that
/// callee makes into instrumented code would read capabilities belonging to a different call.
pub fn clear() {
    restore(core::ptr::null_mut());
}

/// The store behind [`clear`] and [`restore_to`].
fn restore(frame: *mut Frame) {
    let Some(key) = key() else { return };
    // SAFETY: as in `current`.
    unsafe { pthread_setspecific(key, frame.cast()) };
}

/// The key the frame pointer is stored under, made once for the program.
///
/// `None` when the key could not be made, which is a process that has run out of them. Everything
/// above degrades to no frame, so the program keeps running with every argument recovered, which is
/// the weaker answer rather than the wrong one.
fn key() -> Option<Key> {
    /// Not made yet.
    const COLD: u32 = 0;
    /// Being made by another thread right now.
    const MAKING: u32 = 1;
    /// Made, and `KEY` holds it.
    const MADE: u32 = 2;
    /// Could not be made, and asking again would not help.
    const FAILED: u32 = 3;

    static STATE: AtomicU32 = AtomicU32::new(COLD);
    static KEY: AtomicUsize = AtomicUsize::new(0);

    loop {
        match STATE.load(Ordering::Acquire) {
            MADE => return Some(KEY.load(Ordering::Relaxed) as Key),
            FAILED => return None,
            MAKING => core::hint::spin_loop(),
            _ => {
                if STATE
                    .compare_exchange(COLD, MAKING, Ordering::Acquire, Ordering::Relaxed)
                    .is_err()
                {
                    continue;
                }
                let mut made: Key = 0;
                // SAFETY: the pointer is to a local this call fills in, and no destructor is
                // wanted: the frame points into a stack that is going away with the thread.
                let failed = unsafe { pthread_key_create(&raw mut made, core::ptr::null_mut()) };
                if failed == 0 {
                    KEY.store(made as usize, Ordering::Relaxed);
                    STATE.store(MADE, Ordering::Release);
                } else {
                    STATE.store(FAILED, Ordering::Release);
                }
            }
        }
    }
}

unsafe extern "C" {
    fn pthread_key_create(key: *mut Key, dtor: *mut c_void) -> i32;
    fn pthread_getspecific(key: Key) -> *mut c_void;
    fn pthread_setspecific(key: Key, value: *const c_void) -> i32;
}

/// The names generated code is compiled against.
///
/// Separate from the functions above for the reason every other exports module in this crate is:
/// these are an ABI and those are Rust.
pub mod exports {
    use super::Frame;

    /// # Safety
    ///
    /// As [`super::publish`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_frame_publish(frame: *mut Frame) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::publish(frame) };
    }

    /// The frame this call was given, or null, unlinking it either way.
    ///
    /// A pointer rather than the frame itself, because a callee reads one or two capabilities out
    /// of it and copying thirty two bytes per argument to hand back nine of them would cost more
    /// than the frame saves. The pointer is the caller's stack and is good until this callee
    /// returns.
    #[unsafe(no_mangle)]
    pub extern "C" fn __rucc_frame_take() -> *mut Frame {
        let frame = super::current();
        if frame.is_null() {
            return frame;
        }
        // SAFETY: a published frame is valid for the call it was published for, which is this one.
        let magic = unsafe { (*frame).magic };
        if magic != super::MAGIC {
            return core::ptr::null_mut();
        }
        // SAFETY: as above.
        unsafe {
            (*frame).magic = 0;
            super::restore((*frame).outer);
        }
        frame
    }

    /// Says there is no frame. See [`super::clear`].
    #[unsafe(no_mangle)]
    pub extern "C" fn __rucc_frame_clear() {
        super::clear();
    }

    /// # Safety
    ///
    /// As [`super::restore_to`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_frame_restore(outer: *mut Frame) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::restore_to(outer) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Class, Meta, perm};

    /// A capability naming an instance nothing else in the test uses.
    fn cap(lo: u64, ver: u64) -> Cap {
        Cap::new(lo, 64, ver, Meta::new(Class::Allocated, perm::READ | perm::WRITE, ver))
    }

    /// A frame carrying `caps`, published and taken the way a call would.
    fn published(caps: &[Cap]) -> Option<Frame> {
        let mut frame = Frame::EMPTY;
        frame.argc = caps.len() as u16;
        frame.args[..caps.len()].copy_from_slice(caps);
        // SAFETY: the frame is this function's local and the take below happens before it returns.
        unsafe { publish(&raw mut frame) };
        take()
    }

    #[test]
    fn what_the_caller_writes_is_what_the_callee_reads() {
        let caps = [cap(4096, 2), cap(8192, 4)];
        let taken = published(&caps).expect("the frame was published by an instrumented caller");
        assert_eq!(taken.argc, 2);
        assert_eq!(taken.arg(0), caps[0]);
        assert_eq!(taken.arg(1), caps[1]);
    }

    #[test]
    fn an_argument_the_caller_did_not_describe_is_bottom() {
        // Which is the same answer as an argument it described as nothing, on purpose. The callee
        // has one thing to do about either, and that is to recover the pointer from the planes.
        let taken = published(&[cap(4096, 2)]).expect("published above");
        assert_eq!(taken.arg(1), Cap::BOTTOM);
        assert_eq!(taken.arg(ARGS), Cap::BOTTOM);
        assert_eq!(taken.arg(usize::MAX), Cap::BOTTOM);
    }

    #[test]
    fn a_frame_is_believed_once() {
        // The rule that keeps a call chain honest. A callee that takes its frame and then calls
        // something else must not have that something else read the same capabilities, or one
        // published frame would describe every call below it.
        let _ = published(&[cap(4096, 2)]).expect("published above");
        assert!(take().is_none());
        assert!(current().is_null());
    }

    #[test]
    fn a_callee_with_no_frame_gets_nothing() {
        // Entry from uninstrumented code, which is document 10 section 10.8's callback and is the
        // case the whole recovery path exists for.
        clear();
        assert!(take().is_none());

        // And a frame nobody signed, which is a stack slot that happens to be where a frame would
        // have been.
        let mut stale = Frame::EMPTY;
        stale.argc = 2;
        stale.args[0] = cap(4096, 2);
        // SAFETY: the local outlives the read below.
        unsafe { pthread_setspecific(key().expect("a key"), (&raw mut stale).cast()) };
        assert!(take().is_none());
        clear();
    }

    #[test]
    fn a_published_frame_gives_the_one_it_covered_back() {
        // Frames nest the way calls do, so taking the inner one has to leave the outer one where
        // the function that published it will find it again.
        let mut outer = Frame::EMPTY;
        outer.argc = 1;
        outer.args[0] = cap(4096, 2);
        let mut inner = Frame::EMPTY;
        inner.argc = 1;
        inner.args[0] = cap(8192, 4);

        // SAFETY: both frames are locals of this function and both calls happen before it returns.
        unsafe {
            publish(&raw mut outer);
            publish(&raw mut inner);
        }
        assert_eq!(take().expect("the inner frame").arg(0), cap(8192, 4));
        assert_eq!(take().expect("the outer frame").arg(0), cap(4096, 2));
        assert!(take().is_none());
    }

    #[test]
    fn one_threads_frame_is_not_anothers() {
        // The whole reason the side channel is thread local. Two threads calling at the same time
        // would otherwise hand each other capabilities, which is a monitor reporting on the wrong
        // memory rather than a monitor being slow.
        let mut mine = Frame::EMPTY;
        mine.argc = 1;
        mine.args[0] = cap(4096, 2);
        // SAFETY: the local outlives the thread joined below.
        unsafe { publish(&raw mut mine) };

        let theirs = std::thread::spawn(|| {
            let seen = current().is_null();
            let taken = published(&[cap(8192, 4)]).expect("published on this thread");
            (seen, taken.arg(0))
        })
        .join()
        .expect("the thread ran");

        // Nothing of ours was visible over there.
        assert!(theirs.0);
        assert_eq!(theirs.1, cap(8192, 4));
        // And nothing of theirs disturbed ours, which is still where it was.
        assert_eq!(take().expect("still ours").arg(0), cap(4096, 2));
    }

    #[test]
    fn the_frame_is_the_shape_the_other_half_will_be_compiled_against() {
        // An ABI, so the offsets are the contract. Generated code will address these fields by
        // constant, and a change here without a change there is two halves that disagree about
        // where the capabilities are.
        assert_eq!(size_of::<Cap>(), 32);
        assert_eq!(core::mem::offset_of!(Frame, magic), 0);
        assert_eq!(core::mem::offset_of!(Frame, argc), 4);
        assert_eq!(core::mem::offset_of!(Frame, flags), 6);
        assert_eq!(core::mem::offset_of!(Frame, args), 8);
        assert_eq!(core::mem::offset_of!(Frame, ret), 8 + 32 * ARGS);
        assert_eq!(core::mem::offset_of!(Frame, outer), 8 + 32 * (ARGS + 1));
        assert_eq!(size_of::<Frame>(), 8 + 32 * (ARGS + 1) + 8);
    }
}
