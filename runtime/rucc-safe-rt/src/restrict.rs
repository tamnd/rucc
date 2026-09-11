//! The `restrict` contract, which is a promise about a block rather than about an access.
//!
//! Design: `spec/safe-memory/09-type-init-and-races.md` section 9.6, and
//! `spec/safe-memory/04-safety-model.md` section 4.4, which numbers it J8.
//!
//! C 6.7.3.1: if an object reachable through a `restrict` qualified pointer `P` declared in a block
//! is modified anywhere in that block, then every access to that object in that block is through
//! `P`. There is no way to decide that at one access, which is why section 4.6 keeps it out of J1
//! and gives it a judgement of its own. It is decided by comparing an access against the accesses
//! the same block has already made.
//!
//! # What is kept, and why it is this small
//!
//! One [`Scope`] per activation of a block that declares `restrict` pointers, in that block's own
//! stack, linked into a per thread list the way [`crate::frame`] links call frames. It holds the
//! range of addresses each of the block's pointers has reached and whether any of those accesses
//! wrote. A block with four `restrict` pointers is four entries, which is the number section 9.6
//! asks for and is more than `memcpy`, `strcpy` and the numeric kernels want.
//!
//! Nothing else is kept. There is no map from storage instance to pointer, because the addresses
//! answer the question directly and answer it for the stack and for globals as well, where there is
//! no instance to look up and no plane to read. A check here touches the scope and nothing else.
//!
//! # What a violation is
//!
//! Two accesses in one scope, through two different pointers of it, whose byte ranges overlap, at
//! least one of which wrote. That is the contract read literally, with one deliberate
//! approximation: the range kept per pointer is the union of everything reached through it, so two
//! pointers that stride through one array without ever landing on the same byte are reported.
//!
//! That is the right answer rather than a tolerated imprecision, because of what this check is for.
//! A violated `restrict` is not a fault in the abstract, it is a miscompilation, and what the
//! optimizer acts on is that the ranges are disjoint: `spec/optimizer/08-alias-analysis.md` layer 5
//! answers a query about two accesses, and the vectorizer that reorders a loop is reasoning about
//! the whole range the loop touches. A program the union rule reports is a program the optimizer is
//! entitled to break.
//!
//! # What it does not catch
//!
//! An access whose base is zero, which is an access the front end could not trace back to a
//! declaration. `crates/rucc-lower/src/restrict.rs` says which those are. Zero means the names did
//! not say where the pointer came from, not that it came from nowhere, so an unrecognized access is
//! left alone rather than reported, and the check is incomplete in the direction that never accuses
//! a correct program.
//!
//! A block whose scope was never entered, which is a function some other compiler built. The walk
//! finds no scope for the clique and the access is not checked.
//!
//! And a scope left by a `longjmp`, which skips the call that unlinks it. The list then holds a
//! pointer into a stack frame that has returned. The magic word catches the common case, where the
//! storage has since been written over by something else, and beyond that this is the same hazard
//! [`crate::frame`] has and has the same answer: the next `enter` on that thread links over it and
//! the stale entry is only ever reached by a clique that no longer matches.

use core::ffi::c_void;

use crate::fail::Descriptor;
use crate::tls::Slot;

/// How many `restrict` pointers of one block are tracked.
///
/// Four, which is section 9.6's number. A block that declares more gets the first four and the rest
/// are not checked, which loses checks rather than inventing them. The two functions `restrict` was
/// put in the language for take two.
pub const BASES: usize = 4;

/// What an instrumented block writes so a check knows the scope is a scope.
///
/// The same idea as [`crate::frame::MAGIC`] and not the same word, so that a stale frame is never
/// mistaken for a scope or the other way round. It is not a checksum and not a secret.
pub const MAGIC: u32 = 0x7273_6370;

/// What one of the block's `restrict` pointers has reached so far.
///
/// `#[repr(C)]` because generated code allocates the scope and this crate fills it in, so the
/// offsets are a contract between the two halves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Seen {
    /// The lowest address reached through this pointer.
    pub lo: u64,
    /// One past the highest, so an empty range is `lo == hi` and the first access sets both.
    pub hi: u64,
    /// [`Seen::TOUCHED`] once anything has been reached through it, and [`Seen::WROTE`] once
    /// something reached through it has written.
    pub state: u32,
    /// Spare, for the things section 9.6 has not needed yet.
    pub spare: u32,
}

impl Seen {
    /// Something has been reached through this pointer, so `lo` and `hi` mean something.
    pub const TOUCHED: u32 = 1;
    /// At least one of those accesses wrote, which is the half of the contract that bites.
    pub const WROTE: u32 = 2;

    /// A pointer nothing has been reached through.
    pub const EMPTY: Self = Self { lo: 0, hi: 0, state: 0, spare: 0 };

    /// Whether this pointer has reached any of `[lo, hi)`.
    #[must_use]
    pub const fn overlaps(&self, lo: u64, hi: u64) -> bool {
        self.state & Self::TOUCHED != 0 && self.lo < hi && lo < self.hi
    }
}

/// One activation of a block that declares `restrict` pointers.
///
/// `#[repr(C)]` for the reason [`Seen`] is: this is a stack slot generated code reserves and this
/// crate writes into.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Scope {
    /// [`MAGIC`] while this is linked, and zero once it has been left.
    pub magic: u32,
    /// Which `restrict` scope of the module this is, matching the clique on the accesses in it.
    pub clique: u16,
    /// How many `restrict` pointers the block declares, saturating at [`BASES`].
    pub bases: u16,
    /// What each of them has reached, indexed by the base number less one.
    pub seen: [Seen; BASES],
    /// The scope this one was entered inside, restored when this one is left.
    pub outer: *mut Scope,
}

impl Scope {
    /// A scope that has seen nothing, which is what a block starts from.
    pub const EMPTY: Self = Self {
        magic: 0,
        clique: 0,
        bases: 0,
        seen: [Seen::EMPTY; BASES],
        outer: core::ptr::null_mut(),
    };
}

/// The clique and the base of an access, in one word.
///
/// Two numbers rather than one everywhere else, and one word here because this rides in an argument
/// register at every checked access and two of them would cost a register the address and the size
/// want. The halves are `rucc_ir::Restrict`'s two fields, high then low.
#[must_use]
pub const fn tag(clique: u16, base: u16) -> u32 {
    ((clique as u32) << 16) | base as u32
}

/// The clique half of a [`tag`].
#[must_use]
pub const fn clique_of(tag: u32) -> u16 {
    (tag >> 16) as u16
}

/// The base half of a [`tag`], or the count of bases when the tag is the one [`enter`] was given.
#[must_use]
pub const fn base_of(tag: u32) -> u16 {
    (tag & 0xffff) as u16
}

/// Where this thread's innermost live scope is.
static SCOPES: Slot = Slot::new();

/// This thread's innermost live scope, or null.
#[must_use]
pub fn current() -> *mut Scope {
    SCOPES.get().cast()
}

/// Makes `scope` the innermost one, for a block that has just started.
///
/// `tag` is [`tag`] of the block's clique and of how many `restrict` pointers it declares.
///
/// # Safety
///
/// `scope` stays where it is and stays valid until [`leave`] is called on it, which in practice
/// means it is a local of the function whose block this is.
pub unsafe fn enter(scope: *mut Scope, tag: u32) {
    if scope.is_null() {
        return;
    }
    let bases = base_of(tag).min(BASES as u16);
    // SAFETY: the caller says the scope is theirs and lives until they leave it.
    unsafe {
        (*scope).magic = MAGIC;
        (*scope).clique = clique_of(tag);
        (*scope).bases = bases;
        (*scope).seen = [Seen::EMPTY; BASES];
        (*scope).outer = current();
    }
    // SAFETY: as above, and the read of it happens before the caller leaves it.
    unsafe { SCOPES.set(scope.cast()) };
}

/// Ends a scope, which is a block reaching its end, and puts the enclosing one back.
///
/// The magic word is cleared, so a check that reaches this storage afterwards through a stale link
/// stops there rather than believing what it says.
///
/// # Safety
///
/// `scope` is the one [`enter`] was called with and no scope entered inside it is still linked,
/// which a compiler that emits the two calls in one block satisfies by construction.
pub unsafe fn leave(scope: *mut Scope) {
    if scope.is_null() {
        return;
    }
    // SAFETY: the caller says this is their scope and that it is still theirs.
    let outer = unsafe {
        (*scope).magic = 0;
        (*scope).outer
    };
    // SAFETY: the enclosing scope belongs to a block that has not ended, since this one was inside
    // it.
    unsafe { SCOPES.set(outer.cast()) };
}

/// Judgement J8: this access does not reach a byte another `restrict` pointer of its block reached.
///
/// `tag` is [`tag`] of the access's clique and base, which the front end worked out from the names
/// the access was written with. A clique or a base of zero is an access nothing was worked out for
/// and is passed.
///
/// The access is recorded whether or not it is refused, because under the postures that carry on it
/// goes ahead as written and the next one has to be judged against a scope that says what really
/// happened.
///
/// # Panics
///
/// When the access is refused and [`crate::posture`] says to stop, which is the default.
///
/// # Safety
///
/// `descriptor` is the address of a descriptor the same build wrote into `.rucc_safety_desc`, or
/// null. It is only read when the check refuses. `addr` is never read through.
pub unsafe fn access(
    addr: *const c_void,
    size: usize,
    tag: u32,
    write: bool,
    descriptor: *const Descriptor,
) {
    let base = base_of(tag);
    // An access of no bytes reaches nothing, so there is nothing for another pointer to have
    // reached too.
    if clique_of(tag) == 0 || base == 0 || base as usize > BASES || size == 0 {
        return;
    }
    // SAFETY: the linked scopes are the storage of blocks that have not ended, which is what
    // `enter` and `leave` maintain and what the magic word checks.
    let Some(scope) = (unsafe { scope_of(clique_of(tag)) }) else { return };
    let lo = addr as u64;
    let hi = lo.saturating_add(size as u64);
    let at = base as usize - 1;

    // SAFETY: `scope_of` returned a scope that is linked, so it is the storage of a block that has
    // not ended, and `at` is inside `seen` because the base was checked above.
    unsafe {
        for other in 0..BASES {
            if other == at {
                continue;
            }
            let seen = (*scope).seen[other];
            if seen.overlaps(lo, hi) && (write || seen.state & Seen::WROTE != 0) {
                // SAFETY: the descriptor is this function's caller's to get right and is passed on
                // unchanged. The address is the one the access was about.
                crate::fail::report(descriptor, Some(addr as usize));
                break;
            }
        }
        let mine = &mut (*scope).seen[at];
        if mine.state & Seen::TOUCHED == 0 {
            mine.lo = lo;
            mine.hi = hi;
        } else {
            mine.lo = mine.lo.min(lo);
            mine.hi = mine.hi.max(hi);
        }
        mine.state |= Seen::TOUCHED | if write { Seen::WROTE } else { 0 };
    }
}

/// The innermost live scope for `clique`, if this thread is in one.
///
/// The innermost rather than any, which is what a recursive function needs: every activation of it
/// has the same clique and the block an access is in is the one that started last.
///
/// # Safety
///
/// The linked scopes are the storage of blocks that have not ended, which is what [`enter`] and
/// [`leave`] are for. A scope a `longjmp` skipped the leave of breaks that, and the magic word is
/// what stops the walk when it does.
unsafe fn scope_of(clique: u16) -> Option<*mut Scope> {
    let mut at = current();
    while !at.is_null() {
        // SAFETY: a linked scope is the storage of a block that has not ended.
        let scope = unsafe { at.read() };
        if scope.magic != MAGIC {
            return None;
        }
        if scope.clique == clique {
            return Some(at);
        }
        at = scope.outer;
    }
    None
}

/// The names generated code is compiled against.
///
/// Separate from the functions above for the reason every other exports module in this crate is:
/// these are an ABI and those are Rust.
pub mod exports {
    use core::ffi::c_void;

    use super::Scope;
    use crate::fail::Descriptor;

    /// # Safety
    ///
    /// As [`super::enter`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_restrict_enter(scope: *mut Scope, tag: u32) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::enter(scope, tag) };
    }

    /// # Safety
    ///
    /// As [`super::leave`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_restrict_leave(scope: *mut Scope) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::leave(scope) };
    }

    /// Whether the access wrote, as the integer an `extern "C"` signature can carry.
    ///
    /// A `bool` across this boundary would be one byte whose other two hundred and fifty four
    /// values are undefined behaviour to produce, and generated code is what produces it.
    ///
    /// # Safety
    ///
    /// As [`super::access`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_restrict(
        addr: *const c_void,
        size: usize,
        tag: u32,
        write: u32,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::access(addr, size, tag, write != 0, descriptor) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scope that is linked for as long as it is held, and unlinked however the block ends.
    ///
    /// A guard rather than a pair of calls, because a test that asks for a refusal gets a panic in
    /// the middle of its block and the list has to be put back anyway. That is what generated code
    /// will have to arrange too, since a `restrict` block can be left by a `return` inside it.
    struct Held {
        /// The scope itself, in this structure's storage, which is a local of whoever holds it.
        scope: Scope,
    }

    impl Drop for Held {
        fn drop(&mut self) {
            // SAFETY: this is the scope `scoped` entered, at the address it entered it at, and
            // anything entered inside it has already been dropped.
            unsafe { leave(&raw mut self.scope) };
        }
    }

    /// Runs `body` inside a scope of `clique` over `bases` pointers.
    fn scoped<T>(clique: u16, bases: u16, body: impl FnOnce() -> T) -> T {
        let mut held = Held { scope: Scope::EMPTY };
        // SAFETY: the scope is a field of a local that is not moved again and whose drop leaves
        // it, so it is linked for exactly as long as it is valid.
        unsafe { enter(&raw mut held.scope, tag(clique, bases)) };
        body()
    }

    /// Runs `body` and says whether anything in it was refused.
    ///
    /// The hook is swapped so that a refusal a test is asking for does not print a backtrace and
    /// read as a failure. Every test runs on a thread of its own, so the swap is not racing any
    /// other test's.
    fn refused(body: impl FnOnce()) -> bool {
        let hook = std::panic::take_hook();
        std::panic::set_hook(std::boxed::Box::new(|_| {}));
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
        std::panic::set_hook(hook);
        out.is_err()
    }

    /// One access, with no descriptor, which is what a test has to hand.
    fn touch(clique: u16, base: u16, addr: usize, size: usize, write: bool) {
        // SAFETY: a null descriptor is allowed and reads as one that says nothing, and the address
        // is never read through.
        unsafe { access(addr as *const c_void, size, tag(clique, base), write, core::ptr::null()) };
    }

    #[test]
    fn two_pointers_that_stay_apart_are_not_reported() {
        // Which is `memcpy` doing what it says, and is the case that has to be free of noise or
        // nobody will leave the check on.
        assert!(!refused(|| scoped(11, 2, || {
            for step in 0..16 {
                touch(11, 1, 0x4000 + step * 4, 4, true);
                touch(11, 2, 0x8000 + step * 4, 4, false);
            }
        })));
    }

    #[test]
    fn a_read_and_a_write_of_one_byte_through_two_pointers_is_refused() {
        // The contract itself: the object is modified through one of them, so every access to it
        // had to be through that one.
        assert!(refused(|| scoped(12, 2, || {
            touch(12, 1, 0x4000, 8, true);
            touch(12, 2, 0x4004, 8, false);
        })));

        // And the other way round, since which of the two wrote is not what decides it.
        assert!(refused(|| scoped(12, 2, || {
            touch(12, 1, 0x4000, 8, false);
            touch(12, 2, 0x4004, 8, true);
        })));
    }

    #[test]
    fn two_reads_of_one_byte_are_not_refused() {
        // Nothing was modified, so the contract says nothing. `restrict` on two pointers a function
        // only reads through is a promise about writes that never happen.
        assert!(!refused(|| scoped(13, 2, || {
            touch(13, 1, 0x4000, 8, false);
            touch(13, 2, 0x4004, 8, false);
        })));
    }

    #[test]
    fn two_ranges_that_touch_without_overlapping_are_not_refused() {
        // One past the end is one past the end. An off by one here would report every program that
        // copies a buffer into the space directly after it.
        assert!(!refused(|| scoped(14, 2, || {
            touch(14, 1, 0x4000, 8, true);
            touch(14, 2, 0x4008, 8, true);
            touch(14, 2, 0x3ff8, 8, true);
        })));
    }

    #[test]
    fn the_same_pointer_reaching_the_same_byte_twice_is_what_it_is_for() {
        // A pointer is allowed to reach its own object as often as it likes, through as many
        // derivations as it likes, and a check that said otherwise would refuse every loop.
        assert!(!refused(|| scoped(15, 2, || {
            touch(15, 1, 0x4000, 8, true);
            touch(15, 1, 0x4000, 8, true);
            touch(15, 1, 0x4004, 8, false);
        })));
    }

    #[test]
    fn an_access_the_names_said_nothing_about_is_passed() {
        // Base zero is an access the front end could not trace to a declaration, which is the
        // commonest thing in any program and is not evidence of anything.
        assert!(!refused(|| scoped(16, 2, || {
            touch(16, 1, 0x4000, 8, true);
            touch(16, 0, 0x4000, 8, true);
            touch(0, 2, 0x4000, 8, true);
            touch(16, 2, 0x4000, 0, true);
        })));
    }

    #[test]
    fn a_clique_with_no_scope_around_it_is_passed() {
        // A function some other compiler built, or one this compiler built before the block was
        // reached. There is nothing to compare against, so there is nothing to say.
        assert!(!refused(|| {
            touch(17, 1, 0x4000, 8, true);
            touch(17, 2, 0x4000, 8, true);
            scoped(18, 2, || {
                touch(17, 1, 0x4000, 8, true);
                touch(17, 2, 0x4000, 8, true);
            });
        }));
    }

    #[test]
    fn what_one_block_saw_is_not_what_the_next_one_sees() {
        // The scope is the block's dynamic extent, so a function called twice with different
        // arguments is two promises and not one. Getting this wrong would make the second call of
        // any `memcpy` report against the first.
        assert!(!refused(|| {
            scoped(19, 2, || touch(19, 1, 0x4000, 8, true));
            scoped(19, 2, || touch(19, 2, 0x4000, 8, true));
        }));
    }

    #[test]
    fn the_innermost_scope_of_a_clique_is_the_one_that_answers() {
        // Which is recursion. Every activation of one function has the same clique, and an access
        // inside the inner one is about the pointers the inner one was handed.
        assert!(!refused(|| scoped(20, 2, || {
            touch(20, 1, 0x4000, 8, true);
            scoped(20, 2, || touch(20, 2, 0x4000, 8, true));
        })));
    }

    #[test]
    fn leaving_a_scope_puts_the_one_it_was_inside_back() {
        // Otherwise the outer block would stop being checked from its first call onwards, which is
        // the failure that looks like the check working.
        assert!(refused(|| scoped(21, 2, || {
            touch(21, 1, 0x4000, 8, true);
            scoped(21, 2, || touch(21, 2, 0x9000, 8, true));
            touch(21, 2, 0x4000, 8, false);
        })));
    }

    #[test]
    fn a_scope_of_one_clique_does_not_answer_for_another() {
        // Two functions with `restrict` parameters calling one another. The inner one's accesses
        // are about its own pointers, and the walk has to go past its scope to find the outer
        // one's.
        assert!(refused(|| scoped(22, 2, || {
            touch(22, 1, 0x4000, 8, true);
            scoped(23, 2, || {
                touch(23, 1, 0x4000, 8, true);
                touch(22, 2, 0x4000, 8, false);
            });
        })));
    }

    #[test]
    fn a_base_past_the_fourth_is_not_tracked() {
        // A block with more `restrict` pointers than the scope holds. The extras lose their checks
        // rather than being indexed off the end of the array.
        assert!(!refused(|| scoped(24, 6, || {
            touch(24, 1, 0x4000, 8, true);
            touch(24, 5, 0x4000, 8, true);
            touch(24, 6, 0x4000, 8, true);
        })));
    }

    #[test]
    fn one_threads_scope_is_not_anothers() {
        // Two threads in one `memcpy` are two blocks, each promising about its own arguments, and
        // a scope they shared would have them report each other.
        assert!(!refused(|| scoped(25, 2, || {
            touch(25, 1, 0x4000, 8, true);
            std::thread::spawn(|| {
                assert!(current().is_null(), "nothing of ours is visible over there");
                scoped(25, 2, || touch(25, 2, 0x4000, 8, true));
            })
            .join()
            .expect("the thread ran");
            touch(25, 1, 0x4008, 8, true);
        })));
    }

    #[test]
    fn a_scope_that_has_been_left_is_not_believed_again() {
        // The stale link a `longjmp` leaves. The magic word is what a walk that reaches one has to
        // stop at, because the storage under it belongs to whatever runs next.
        let mut scope = Scope::EMPTY;
        // SAFETY: the scope is this test's local and is left on the line after it.
        unsafe { enter(&raw mut scope, tag(26, 2)) };
        // SAFETY: as above.
        unsafe { leave(&raw mut scope) };
        assert_eq!(scope.magic, 0);
        assert!(current().is_null());

        // SAFETY: the local outlives the walk below, which is the point: the walk has to refuse it
        // for what it says rather than for where it is.
        unsafe { SCOPES.set((&raw mut scope).cast()) };
        assert!(!refused(|| {
            touch(26, 1, 0x4000, 8, true);
            touch(26, 2, 0x4000, 8, true);
        }));
        // SAFETY: null says there is no scope.
        unsafe { SCOPES.set(core::ptr::null_mut()) };
    }

    #[test]
    fn the_scope_is_the_shape_the_other_half_will_be_compiled_against() {
        // An ABI. Generated code reserves this many bytes in its own frame and addresses these
        // fields by constant, so a change here without a change there is two halves that disagree
        // about where the bookkeeping is.
        assert_eq!(size_of::<Seen>(), 24);
        assert_eq!(core::mem::offset_of!(Scope, magic), 0);
        assert_eq!(core::mem::offset_of!(Scope, clique), 4);
        assert_eq!(core::mem::offset_of!(Scope, bases), 6);
        assert_eq!(core::mem::offset_of!(Scope, seen), 8);
        assert_eq!(core::mem::offset_of!(Scope, outer), 8 + 24 * BASES);
        assert_eq!(size_of::<Scope>(), 8 + 24 * BASES + 8);
    }

    #[test]
    fn the_two_numbers_survive_the_round_trip_through_one_word() {
        // The packing is an ABI too, and it is the one the compiler's half will emit as a constant.
        assert_eq!(tag(1, 2), 0x0001_0002);
        assert_eq!(clique_of(tag(u16::MAX, 3)), u16::MAX);
        assert_eq!(base_of(tag(3, u16::MAX)), u16::MAX);
        assert_eq!(tag(0, 0), 0);
    }
}
