//! The vocabulary the interposition table is written in, and the generator that turns a row of it
//! into a wrapper.
//!
//! Design: `spec/safe-memory/10-boundaries.md` sections 10.1 and 10.3.
//!
//! Section 10.1 says there are exactly three things the monitor may do where instrumented code
//! touches memory it does not control, and that it must do one of them explicitly. This module is
//! the machinery for the first of the three, which is to model the boundary: a wrapper performs the
//! judgements the uninstrumented code would have performed, and updates the planes as it would
//! have.
//!
//! The rule from section 10.3 is that an interposed function is one whose memory effects are
//! written down as judgements. Not one we replaced. The wrapper here checks and then calls the C
//! library's own implementation, which is the arrangement that scales: there are several hundred
//! functions to do and rewriting each of them would be several hundred chances to get a `memmove`
//! subtly wrong.
//!
//! # Why a table and a generator rather than several hundred functions
//!
//! Section 10.3 asks for exactly this and says why: writing the wrappers by hand is a large and
//! boring job with a high error rate. A row is the C signature plus an effects clause naming which
//! arguments are read, which are written, and over what extent, in the vocabulary of the
//! `__counted_by` family:
//!
//! ```text
//! memcpy(void * __sized_by(n) dst, const void * __sized_by(n) src, size_t n)
//!     writes(dst, n) reads(src, n)
//! ```
//!
//! [`crate::interpose`] is that line, spelled so a Rust compiler will read it. What it expands to is the
//! wrapper, the `extern "C"` symbol, and an entry in a table of [`Row`] that describes the row as
//! data, so that a mistake in a row is a data fix rather than a code fix and so that
//! `--emit=safety-summary` can count what a build interposed and list what it did not.
//!
//! # What a judgement here can decide, and what it cannot
//!
//! The same as [`crate::check`], because it reads the same plane. Bounds and lifetime, per granule,
//! over the heap this monitor's allocator hands out of. An address that is not the heap's is passed,
//! which is a local, a global or another allocator's memory, and reporting on one would be a false
//! positive against a program doing nothing wrong.
//!
//! [`Kind`] tells a read from a write and nothing yet acts on the difference, since bounds and
//! lifetime do not care which direction the bytes were going. It is in the vocabulary from the
//! start because the init plane of milestone S5 is the thing that cares: a read of a range nobody
//! wrote is document 03's Y6 and a write of one is not a bug at all. Recording the direction now
//! means S5 is a change to what the judgement does rather than a rewrite of every row.
//!
//! # The discovered extent
//!
//! The interesting half of section 10.3. A string function's write length is not known until the
//! source's NUL is found, so the check cannot be a length comparison done up front. [`scan`] walks
//! the string and checks as it walks, which fails at the byte that leaves the object rather than
//! after the fact, and that is a better report than a length check could give: it names the byte
//! the string ran to rather than the call that eventually noticed.

use core::ffi::c_void;

use crate::alloc;
use crate::fail::Judgement;
use crate::plane::{self, GRANULE};

/// Which of section 10.3's four groups a row belongs to.
///
/// The group is what the summary counts by, because "41 movement wrappers and no syscall wrappers"
/// says something about a build's guarantee that a total of 41 does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// Memory movement and the string functions, which section 10.3 calls the highest yield group
    /// by a wide margin because it is where the classic overflow lives.
    Movement,
    /// The allocating functions, whose effects are on the planes rather than on a range.
    Allocation,
    /// The syscall surface of section 10.5, where the kernel writes user memory and does not
    /// consult our planes.
    Syscall,
}

impl Group {
    /// The name the summary prints.
    #[must_use]
    pub const fn what(self) -> &'static str {
        match self {
            Self::Movement => "movement",
            Self::Allocation => "allocation",
            Self::Syscall => "syscall",
        }
    }
}

/// Which way the bytes were going.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The function reads the range.
    Reads,
    /// The function writes it.
    Writes,
}

impl Kind {
    /// The word the row was written with.
    #[must_use]
    pub const fn what(self) -> &'static str {
        match self {
            Self::Reads => "reads",
            Self::Writes => "writes",
        }
    }
}

/// How far an argument reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extent {
    /// `__sized_by(n)`: as many bytes as the row's other argument says, held here as the row spells
    /// it so that a person reading the summary sees the same word they would read in the header.
    SizedBy(&'static str),
    /// Discovered by looking, which is every function whose extent is a NUL.
    Nul,
    /// Discovered by looking, but never further than the row's other argument says.
    ///
    /// The `strn` half of the string functions, which stop at a terminator or at a count, whichever
    /// comes first. Judging one of these as if it were [`Extent::Nul`] would refuse `strncmp(a, b,
    /// 4)` against a four byte buffer holding four bytes, which is a correct call and one of the
    /// commonest things a program does with a fixed width field.
    NulWithin(&'static str),
}

/// What one interposed function does to one of its pointer arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Effect {
    /// The argument, named as the row names it.
    pub arg: &'static str,
    /// Which way the bytes go.
    pub kind: Kind,
    /// How far it reaches.
    pub extent: Extent,
}

/// One row of the interposition table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    /// The name the program calls.
    pub name: &'static str,
    /// The symbol the wrapper is generated under, which is what a call site is redirected to.
    pub wrapper: &'static str,
    /// Which group of section 10.3 it is in.
    pub group: Group,
    /// What it does to its pointer arguments, in the order the row names them.
    pub effects: &'static [Effect],
}

/// Judgement J1 over a whole range, on behalf of an interposed function.
///
/// One judgement for the range rather than one per byte, which is the reason a wrapper is cheaper
/// than the loop it replaces: the whole point of `memcpy` being interposed rather than instrumented
/// is that one comparison covers a copy of any length.
///
/// The first byte and the last byte have to belong to the same live instance. An instance is a run
/// of granules, so two ends agreeing means the whole range is inside it, which is the same argument
/// [`crate::check::bounds`] makes and it holds here for the same reason.
///
/// `site` is what the report says the refusal was about, and the generator builds it out of the
/// row, so it names the function and the argument rather than a line inside this crate.
///
/// # Panics
///
/// When the range is refused, which says what happened and stops the program.
pub fn range(site: &'static str, addr: *const c_void, len: usize) {
    // A call that moves no bytes touches nothing. `memcpy(p, q, 0)` with a null `p` is written by
    // real programs and is not a bug, so checking the first byte of a zero length range would be a
    // false positive on an address the call never reads.
    if len == 0 {
        return;
    }
    let Some(region) = alloc::region() else { return };
    let at = addr as usize;
    if !region.holds(at) {
        return;
    }
    // SAFETY: the address is inside the region the plane was built over, which is what reading the
    // plane asks for, and `holds` above is what says so.
    let instance = unsafe { region.plane.version(at) };
    if !plane::owned(instance) {
        crate::fail::refused_at(Judgement::Access, site, at);
    }
    let last = at.wrapping_add(len - 1);
    // SAFETY: as above, and `holds` is checked before the plane is read.
    if !region.holds(last) || unsafe { region.plane.version(last) } != instance {
        // The last byte rather than the first, because the first one was allowed to be where it is
        // and the last one is where the call went too far.
        crate::fail::refused_at(Judgement::Access, site, last);
    }
}

/// The length of a NUL terminated string, checked as it is discovered.
///
/// Section 10.3's discovered extent. There is no length to compare against up front, so the walk
/// itself is the check: every granule the string reaches has to belong to the instance it started
/// in, and a string with no NUL inside its own object is refused at the byte that leaves it.
///
/// The plane only changes its answer at a granule, so it is asked once per granule rather than once
/// per byte, which makes the check a sixteenth of the cost of the walk it is riding along with.
///
/// A string that is not in the heap this monitor watches is measured and not judged, the same way
/// an ordinary access to one is.
///
/// # Panics
///
/// As [`range`].
///
/// # Safety
///
/// `addr` is a pointer the program passed to a string function. It is read from, one byte at a
/// time, and each byte is inside an instance this monitor owns or outside its heap entirely.
#[must_use]
pub unsafe fn scan(site: &'static str, addr: *const c_void) -> usize {
    // SAFETY: this function's contract is the one below, with no limit on the walk.
    unsafe { scan_within(site, addr, usize::MAX) }
}

/// The same, for a function that stops at a count as well as at a terminator.
///
/// The `strn` half of the string group. `strncmp(a, b, 4)` reads four bytes of a four byte buffer
/// whether or not there is a NUL in them, and that is a correct call: judging it as an unbounded
/// walk would refuse the commonest thing anybody does with a fixed width field. So the walk stops
/// where the call stops, and the bytes past the limit are neither read nor judged.
///
/// # Panics
///
/// As [`range`].
///
/// # Safety
///
/// As [`scan`], except that no more than `limit` bytes are read.
#[must_use]
pub unsafe fn scan_within(site: &'static str, addr: *const c_void, limit: usize) -> usize {
    let start = addr as usize;
    // Outside the heap there is no plane to ask, so the walk is a plain `strlen` and the monitor
    // has nothing to say about it.
    let watched = alloc::region().filter(|region| region.holds(start));
    // The version the string's first byte belongs to. Every later byte has to belong to the same
    // one, and asking once is what makes the rest of the walk a comparison.
    let instance = watched.map(|region| {
        // SAFETY: `holds` in the filter above is what reading the plane asks for.
        unsafe { region.plane.version(start) }
    });
    let mut len = 0;
    loop {
        if len == limit {
            return len;
        }
        let at = start.wrapping_add(len);
        if let (Some(region), Some(instance)) = (watched, instance) {
            // The first byte, and then every byte that starts a granule, which is the only place
            // the plane can have changed its mind.
            if len == 0 || at % GRANULE == 0 {
                let same = plane::owned(instance)
                    && region.holds(at)
                    // SAFETY: `holds` immediately before is what reading the plane asks for.
                    && unsafe { region.plane.version(at) } == instance;
                if !same {
                    crate::fail::refused_at(Judgement::Access, site, at);
                }
            }
        }
        // SAFETY: the byte is inside the instance the string started in, which the check above is
        // what establishes, or outside the heap this monitor watches, where reading it is the
        // program's own business and no worse than the call it asked for.
        if unsafe { (at as *const u8).read() } == 0 {
            return len;
        }
        len += 1;
    }
}

/// Turns rows of the interposition table into wrappers.
///
/// One invocation takes every row of a group and expands to three things: a plain Rust function per
/// row, holding the judgements and the work; an `extern "C"` symbol per row, which is what a call
/// site is redirected to; and a `TABLE` of [`Row`] describing the whole group as data.
///
/// A row is the C signature followed by its effects clause:
///
/// ```text
/// fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void
///     where writes(dst, n), reads(src, n)
/// { ... }
/// ```
///
/// An extent is the name of another argument, which is `__sized_by(n)`, or the word `nul`, which is
/// the extent that is discovered by looking. The body is what the wrapper does once the judgements
/// have been made, and for almost every row it is a call to the C library's own implementation,
/// because section 10.3's rule is that an interposed function is one whose effects are written down
/// rather than one that was rewritten.
///
/// The symbol is `__rucc_wrap_` and the name. It is not the name itself: taking over `memcpy` is a
/// fact about how a loader resolves a symbol, and doing it inside a static archive would have the C
/// library's own internals calling this wrapper as well, which is a different and much larger
/// decision than interposing a program's calls. Redirecting the call site is the compiler's half.
#[macro_export]
macro_rules! interpose {
    (
        group: $group:ident;
        $(
            $(#[$note:meta])*
            fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty
                where $($kind:ident($target:ident, $($len:tt),+)),+
            $body:block
        )+
    ) => {
        $(
            $(#[$note])*
            ///
            /// # Panics
            ///
            /// When one of its arguments is refused, which says what happened and stops the
            /// program.
            ///
            /// # Safety
            ///
            /// The arguments are whatever the program passed, and what this does with them is what
            /// the C library would have done. The judgements happen first, so a range this monitor
            /// owns and the call would have run off is refused rather than performed.
            pub unsafe fn $name($($arg: $ty),*) -> $ret {
                $(
                    $crate::__judge!(
                        $kind,
                        concat!(
                            stringify!($name), ", over its ", stringify!($target), " argument"
                        ),
                        $target,
                        $($len),+
                    );
                )+
                $body
            }
        )+

        /// Every row above, as data.
        ///
        /// What `--emit=safety-summary` counts, and what says which symbol a call site is
        /// redirected to. Generated from the same rows as the wrappers, so the two cannot drift.
        pub static TABLE: &[$crate::effects::Row] = &[
            $(
                $crate::effects::Row {
                    name: stringify!($name),
                    wrapper: concat!("__rucc_wrap_", stringify!($name)),
                    group: $crate::effects::Group::$group,
                    effects: &[
                        $(
                            $crate::effects::Effect {
                                arg: stringify!($target),
                                kind: $crate::__kind!($kind),
                                extent: $crate::__extent!($($len),+),
                            }
                        ),+
                    ],
                }
            ),+
        ];

        /// The symbols a redirected call site is compiled against.
        ///
        /// Separate from the functions above for the reason [`crate::check::exports`] is separate
        /// from its checks: these are an ABI and those are Rust, and a panic may not cross an
        /// `extern "C"` boundary, so a test that called one of these to watch it refuse would abort
        /// the harness rather than see a refusal. The tests call the plain functions.
        ///
        /// Not built under `cargo test`, where this crate is linked into a binary that has a
        /// standard library and these names would be resolved by two definitions.
        #[cfg(not(test))]
        pub mod exports {
            // A row's signature is written where the row is, and a module is a fresh scope, so
            // without this the `c_void` the row spelled would resolve in the file that wrote it and
            // not in here. The glob is shadowed by each definition below, which is how a wrapper
            // named `memcpy` sits beside the plain one it calls.
            use super::*;

            $(
                #[doc = concat!(
                    "`", stringify!($name), "`, with its judgements made first.\n\n",
                    "# Safety\n\nThis is `", stringify!($name), "`."
                )]
                #[unsafe(export_name = concat!("__rucc_wrap_", stringify!($name)))]
                pub unsafe extern "C" fn $name($($arg: $ty),*) -> $ret {
                    // SAFETY: this wrapper's contract is the one it calls, passed straight on.
                    unsafe { super::$name($($arg),*) }
                }
            )+
        }
    };
}

/// One effect of one row, as the judgement it stands for.
///
/// Split out of [`crate::interpose`] because a `macro_rules` arm cannot branch on the value of an `ident`
/// it captured, and matching the word itself is how the vocabulary is turned into code. Which is
/// also what makes an unknown word a compile error naming the row rather than a silently ignored
/// clause.
#[doc(hidden)]
#[macro_export]
macro_rules! __judge {
    (reads, $site:expr, $arg:expr, nul) => {
        // SAFETY: the pointer is one the program passed to a string function, which is what this
        // reads it as.
        let _ = unsafe { $crate::effects::scan($site, $arg.cast()) };
    };
    (reads, $site:expr, $arg:expr, nul, $limit:tt) => {
        // SAFETY: as the unbounded arm, and reading fewer bytes than it would.
        let _ = unsafe { $crate::effects::scan_within($site, $arg.cast(), $limit) };
    };
    (reads, $site:expr, $arg:expr, $len:tt) => {
        $crate::effects::range($site, $arg.cast(), $len);
    };
    (writes, $site:expr, $arg:expr, nul $(, $limit:tt)?) => {
        compile_error!(
            "a written extent cannot be discovered from the destination, since the NUL that would \
             say where it ends is what the call is about to write. Name the source's length."
        );
    };
    (writes, $site:expr, $arg:expr, $len:tt) => {
        $crate::effects::range($site, $arg.cast(), $len);
    };
}

/// The same for the direction, as data.
#[doc(hidden)]
#[macro_export]
macro_rules! __kind {
    (reads) => {
        $crate::effects::Kind::Reads
    };
    (writes) => {
        $crate::effects::Kind::Writes
    };
}

/// The same for the extent, as data.
#[doc(hidden)]
#[macro_export]
macro_rules! __extent {
    (nul) => {
        $crate::effects::Extent::Nul
    };
    (nul, $limit:tt) => {
        $crate::effects::Extent::NulWithin(stringify!($limit))
    };
    ($len:tt) => {
        $crate::effects::Extent::SizedBy(stringify!($len))
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc::{alloc, dealloc};
    use crate::turnstile::turn;

    /// Runs one judgement and says whether it refused, without the panic reaching the harness.
    ///
    /// The same arrangement as `check`'s tests and for the same reason: a refusal a test is asking
    /// for should not print a backtrace and read as a failure.
    fn refused(judgement: impl FnOnce()) -> bool {
        let hook = std::panic::take_hook();
        std::panic::set_hook(std::boxed::Box::new(|_| {}));
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(judgement));
        std::panic::set_hook(hook);
        out.is_err()
    }

    /// The address `offset` bytes into an instance.
    fn at(ptr: *mut c_void, offset: usize) -> *const c_void {
        ptr.cast::<u8>().wrapping_add(offset).cast()
    }

    /// Writes a NUL terminated string into an instance, without its terminator when `nul` is off.
    fn put(ptr: *mut c_void, text: &[u8], nul: bool) {
        for (offset, byte) in text.iter().enumerate() {
            // SAFETY: every caller allocates room for the text first.
            unsafe { ptr.cast::<u8>().add(offset).write(*byte) };
        }
        if nul {
            // SAFETY: as above, and for one byte more.
            unsafe { ptr.cast::<u8>().add(text.len()).write(0) };
        }
    }

    #[test]
    fn a_range_inside_one_live_instance_is_allowed() {
        let _turn = turn();
        // The case that has to be silent, which is every correct call a real program makes.
        let ptr = alloc(64);
        assert!(!refused(|| range("t", at(ptr, 0), 64)));
        assert!(!refused(|| range("t", at(ptr, 32), 32)));
        assert!(!refused(|| range("t", at(ptr, 63), 1)));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_range_that_runs_off_the_end_of_its_instance_is_refused() {
        let _turn = turn();
        // The bug the movement group exists to catch: a length argument larger than the buffer.
        let ptr = alloc(64);
        assert!(refused(|| range("t", at(ptr, 0), 65)));
        assert!(refused(|| range("t", at(ptr, 32), 64)));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_range_into_a_freed_instance_is_refused() {
        let _turn = turn();
        // A `memcpy` into a buffer that was freed while something still held a pointer to it.
        let ptr = alloc(64);
        assert!(!refused(|| range("t", at(ptr, 0), 64)));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
        assert!(refused(|| range("t", at(ptr, 0), 64)));
    }

    #[test]
    fn a_range_of_no_bytes_is_allowed_wherever_it_points() {
        let _turn = turn();
        // `memcpy(p, q, 0)` is written by real programs, usually where a loop happens to have run
        // to zero, and it reads nothing. Judging the first byte of a range the call never touches
        // would be a report about an address that was never accessed.
        let ptr = alloc(64);
        // SAFETY: `ptr` is a live instance, freed once and then only used as an address.
        unsafe { dealloc(ptr) };
        assert!(!refused(|| range("t", at(ptr, 0), 0)));
        assert!(!refused(|| range("t", core::ptr::null(), 0)));
    }

    #[test]
    fn a_range_that_is_not_the_heaps_passes() {
        let _turn = turn();
        // A local, a global, or memory another allocator handed out. This monitor instruments its
        // own heap, and reporting on one of these would be a false positive.
        let mut local = [0_u8; 64];
        let addr: *const c_void = local.as_mut_ptr().cast();
        assert!(!refused(|| range("t", addr, 64)));
        assert!(!refused(|| range("t", addr, 1 << 20)));
    }

    #[test]
    fn a_string_inside_its_instance_is_measured_and_not_judged() {
        let _turn = turn();
        // The silent case, and the one that says the walk gets the length right as well.
        let ptr = alloc(64);
        put(ptr, b"hello", true);
        // SAFETY: the string is inside a live instance and is terminated.
        assert_eq!(unsafe { scan("t", at(ptr, 0)) }, 5);
        put(ptr, b"", true);
        // SAFETY: as above.
        assert_eq!(unsafe { scan("t", at(ptr, 0)) }, 0);
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_string_with_no_terminator_inside_its_instance_is_refused_where_it_leaves_it() {
        let _turn = turn();
        // Document 03's S8, which is the highest yield row in the whole model: a buffer that was
        // filled without room for the NUL, and every `strlen`, `strcpy` and `printf` after it
        // walking into whatever is next.
        let ptr = alloc(64);
        for offset in 0..64 {
            // SAFETY: inside a live instance of sixty four bytes.
            unsafe { ptr.cast::<u8>().add(offset).write(b'a') };
        }
        assert!(refused(|| {
            // SAFETY: the walk is what is being tested, and it stops at the end of the instance.
            let _ = unsafe { scan("t", at(ptr, 0)) };
        }));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_string_in_a_freed_instance_is_refused_at_its_first_byte() {
        let _turn = turn();
        // Use after free through a string function, which is how it usually reaches a log line.
        let ptr = alloc(64);
        put(ptr, b"hello", true);
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
        assert!(refused(|| {
            // SAFETY: the bytes are still mapped, which is what makes this a bug rather than a
            // crash.
            let _ = unsafe { scan("t", at(ptr, 0)) };
        }));
    }

    #[test]
    fn a_string_that_is_not_the_heaps_is_measured_and_not_judged() {
        let _turn = turn();
        // A string literal, which is where most of the strings in a program are.
        let text = b"hello\0";
        // SAFETY: the bytes are a live local and are terminated.
        assert_eq!(unsafe { scan("t", text.as_ptr().cast()) }, 5);
    }
}
