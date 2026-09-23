//! One pointer per thread, for the two side channels that need one.
//!
//! Design: `spec/safe-memory/05-representation.md` section 5.3, which is where the first of them is
//! specified and which costs it at one thread local access.
//!
//! The attribute that gives a `#![no_std]` Rust crate a thread local of its own is not stable, so
//! on x86-64 Linux the storage is declared in assembly instead: one block of [`WORDS`] words in
//! `.tbss`, which every thread gets a zeroed copy of, reached through the initial exec model as a
//! load of its offset from the GOT and an add of the thread pointer. That is three instructions
//! where a `pthread_getspecific` or a `pthread_setspecific` was a call into the C library, and on
//! `a-binary-tree-walk`, where every call publishes a frame and takes it back, the two calls were a
//! sixth of the program. Initial exec rather than local exec so that the archive still works linked
//! into a shared object, where the block comes out of the static TLS the loader sets aside.
//!
//! Every other target keeps a `pthread` key and a call to reach it. The shape of the thing stored
//! is the part that has to agree with generated code, and how the runtime finds it is this crate's
//! business and can differ by target without anybody else noticing.
//!
//! There are two of these, [`crate::frame`]'s and [`crate::restrict`]'s, and they are separate keys
//! rather than two fields of one structure because they have different lifetimes. A frame is
//! published for one call and consumed by it, and a restrict scope lives for as long as the block
//! that declared the pointers. Putting them together would mean a call publishing a frame had to
//! carry the enclosing scope along with it.

use core::ffi::c_void;
#[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
use core::sync::atomic::Ordering;
use core::sync::atomic::{AtomicU32, AtomicUsize};

/// What the platform calls a `pthread_key_t`.
///
/// The one place this module is not the same everywhere. Apple's is an `unsigned long` and
/// everybody else's is an `unsigned int`, and getting it wrong means `pthread_key_create` writing
/// eight bytes into four.
#[cfg_attr(all(target_arch = "x86_64", target_os = "linux"), allow(dead_code))]
#[cfg(target_vendor = "apple")]
type Key = core::ffi::c_ulong;
/// See the Apple arm above.
#[cfg_attr(all(target_arch = "x86_64", target_os = "linux"), allow(dead_code))]
#[cfg(not(target_vendor = "apple"))]
type Key = core::ffi::c_uint;

/// How many words each thread's block holds, which is how many slots there can be.
pub const WORDS: usize = 4;

/// The word [`crate::frame`]'s slot keeps.
pub const FRAMES: usize = 0;

/// The word [`crate::epoch`]'s slot keeps.
pub const CLOCK: usize = 1;

/// The word [`crate::restrict`]'s slot keeps.
pub const SCOPES: usize = 2;

/// A pointer each thread has its own copy of.
///
/// Declared as a `static` by whoever wants one, naming a word of the block nobody else names. Where
/// there is a `pthread` key behind it instead, the key is created once for the process, by
/// whichever thread asks first, and never deleted.
#[derive(Debug)]
pub struct Slot {
    /// Which word of the thread's block this is.
    #[cfg_attr(not(all(target_arch = "x86_64", target_os = "linux")), allow(dead_code))]
    word: usize,
    /// Where [`Slot::key`] is in making the key, as one of the four constants in that function.
    #[cfg_attr(all(target_arch = "x86_64", target_os = "linux"), allow(dead_code))]
    state: AtomicU32,
    /// The key itself, meaningful once `state` says it was made.
    #[cfg_attr(all(target_arch = "x86_64", target_os = "linux"), allow(dead_code))]
    key: AtomicUsize,
}

impl Slot {
    /// The slot kept in `word`, which is what a `static` starts as.
    ///
    /// # Panics
    ///
    /// At compile time, when `word` is past the end of the block.
    #[must_use]
    pub const fn new(word: usize) -> Self {
        assert!(word < WORDS, "a slot's word is inside the block");
        Self { word, state: AtomicU32::new(0), key: AtomicUsize::new(0) }
    }

    /// What this thread last stored here, or null.
    ///
    /// Null for a thread that has stored nothing as well as for one that stored null, and every
    /// caller treats the two the same way: there is nothing here, so fall back to whatever the
    /// weaker answer is.
    #[must_use]
    pub fn get(&self) -> *mut c_void {
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        {
            // SAFETY: the word is inside this thread's block, which lives as long as the thread,
            // and only this thread reads or writes it.
            unsafe { block().add(self.word).read() }
        }
        #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
        {
            let Some(key) = self.key() else { return core::ptr::null_mut() };
            // SAFETY: the key was made by `pthread_key_create` and is never deleted, and the only
            // values ever stored under it are the ones `set` was handed.
            unsafe { pthread_getspecific(key) }
        }
    }

    /// Stores `value` for this thread only.
    ///
    /// # Safety
    ///
    /// Whatever `value` points at stays valid for as long as something might read it back. In
    /// practice it is a local of the storing function and the read happens before that function
    /// returns, which is what makes the storage free.
    pub unsafe fn set(&self, value: *mut c_void) {
        #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
        {
            // SAFETY: as in `get`, and the caller says the pointer outlives the reads of it.
            unsafe { block().add(self.word).write(value) }
        }
        #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
        {
            let Some(key) = self.key() else { return };
            // SAFETY: as in `get`, and the caller says the pointer outlives the reads of it.
            unsafe { pthread_setspecific(key, value) };
        }
    }

    /// The key this slot lives under, made once for the program.
    #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
    ///
    /// `None` when the key could not be made, which is a process that has run out of them.
    /// Everything above degrades to an empty slot, so the program keeps running with whatever the
    /// weaker answer is, which is the wrong amount of information rather than the wrong
    /// information.
    fn key(&self) -> Option<Key> {
        /// Not made yet.
        const COLD: u32 = 0;
        /// Being made by another thread right now.
        const MAKING: u32 = 1;
        /// Made, and `key` holds it.
        const MADE: u32 = 2;
        /// Could not be made, and asking again would not help.
        const FAILED: u32 = 3;

        loop {
            match self.state.load(Ordering::Acquire) {
                MADE => return Some(self.key.load(Ordering::Relaxed) as Key),
                FAILED => return None,
                MAKING => core::hint::spin_loop(),
                _ => {
                    if self
                        .state
                        .compare_exchange(COLD, MAKING, Ordering::Acquire, Ordering::Relaxed)
                        .is_err()
                    {
                        continue;
                    }
                    let mut made: Key = 0;
                    // SAFETY: the pointer is to a local this call fills in, and no destructor is
                    // wanted: what is stored points into a stack that is going away with the
                    // thread.
                    let failed =
                        unsafe { pthread_key_create(&raw mut made, core::ptr::null_mut()) };
                    if failed == 0 {
                        self.key.store(made as usize, Ordering::Relaxed);
                        self.state.store(MADE, Ordering::Release);
                    } else {
                        self.state.store(FAILED, Ordering::Release);
                    }
                }
            }
        }
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
core::arch::global_asm!(
    ".pushsection .tbss.__rucc_tls_block,\"awT\",@nobits",
    ".globl __rucc_tls_block",
    ".hidden __rucc_tls_block",
    ".type __rucc_tls_block,@object",
    ".p2align 3",
    "__rucc_tls_block:",
    ".zero {bytes}",
    ".size __rucc_tls_block, {bytes}",
    ".popsection",
    bytes = const WORDS * 8,
);

/// The first word of this thread's block.
///
/// Hidden rather than local so that the reference below resolves whichever object file of the
/// archive each half ends up in, and hidden rather than exported so that nothing outside the
/// runtime can name it.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn block() -> *mut *mut c_void {
    let at: *mut *mut c_void;
    // SAFETY: the GOT entry holds the block's offset from the thread pointer, which the loader
    // wrote before any code ran, and `%fs:0` holds the thread pointer itself on x86-64 Linux. Both
    // are fixed for the life of the thread, so the asm reads nothing that changes under it.
    unsafe {
        core::arch::asm!(
            "movq __rucc_tls_block@GOTTPOFF(%rip), {at}",
            "addq %fs:0, {at}",
            at = out(reg) at,
            options(att_syntax, pure, readonly, nostack),
        );
    }
    at
}

#[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
unsafe extern "C" {
    fn pthread_key_create(key: *mut Key, dtor: *mut c_void) -> i32;
    fn pthread_getspecific(key: Key) -> *mut c_void;
    fn pthread_setspecific(key: Key, value: *const c_void) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slot nothing else in the crate uses, so what it holds is this test's business alone.
    static MINE: Slot = Slot::new(WORDS - 1);

    #[test]
    fn what_a_thread_stores_is_what_it_reads_back() {
        let mut value = 7_u32;
        // SAFETY: the local outlives the read below.
        unsafe { MINE.set((&raw mut value).cast()) };
        assert_eq!(MINE.get(), (&raw mut value).cast());
        // SAFETY: null is always a valid thing to store.
        unsafe { MINE.set(core::ptr::null_mut()) };
        assert!(MINE.get().is_null());
    }

    #[test]
    fn one_threads_slot_is_not_anothers() {
        // The whole reason the side channels are thread local. Two threads calling at the same
        // time would otherwise read each other's bookkeeping, which is a monitor reporting on the
        // wrong memory rather than a monitor being slow.
        let mut mine = 7_u32;
        // SAFETY: the local outlives the thread joined below.
        unsafe { MINE.set((&raw mut mine).cast()) };

        let theirs = std::thread::spawn(|| {
            let empty = MINE.get().is_null();
            let mut theirs = 9_u32;
            // SAFETY: the local outlives the read on the line after it.
            unsafe { MINE.set((&raw mut theirs).cast()) };
            (empty, MINE.get().is_null())
        })
        .join()
        .expect("the thread ran");

        assert!(theirs.0, "nothing of ours was visible over there");
        assert!(!theirs.1, "and what they stored was visible to them");
        assert_eq!(MINE.get(), (&raw mut mine).cast(), "and ours is still ours");
        // SAFETY: null is always a valid thing to store.
        unsafe { MINE.set(core::ptr::null_mut()) };
    }
}
