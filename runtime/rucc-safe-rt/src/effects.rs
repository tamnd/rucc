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
//!
//! [`copied`] is the same idea with a second object involved. `strcpy` reads one string and writes
//! another, and neither length is known when it starts, so the two walk together and both are
//! judged at each granule they reach. The refusal lands on the byte that leaves whichever object
//! ran out first, which is what section 10.3 asks for and is the thing a length check cannot do
//! even in principle: there is no length to check until the call is over.

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
    /// As far as the string in the row's other argument reaches, and one byte more for the
    /// terminator.
    ///
    /// The written half of `strcpy` and `strcat`. There is no number here at all: how far the write
    /// goes is a property of a different argument's contents, which is why the judgement has to be
    /// the walk itself rather than a comparison made before it.
    NulOf(&'static str),
    /// The same, but never further than the count the row's other argument names.
    ///
    /// `strncat`, whose write is as long as its source but stops at `n`, and which appends a
    /// terminator either way. The two names are the source argument and the count, in that order.
    NulOfWithin(&'static str, &'static str),
    /// An array of that many `struct iovec`, and the buffer each one of them points at.
    ///
    /// Section 10.5's scatter and gather. The argument is one pointer and what it reaches is a
    /// whole tree: the array itself, which is read either way, and one range per element, which is
    /// where the bytes actually go.
    Vectors(&'static str),
}

/// The `struct iovec` a scatter or gather syscall is handed.
///
/// Declared here rather than taken from a binding crate, because this crate has no dependencies on
/// purpose. Two words in this order is what every Unix means by it and what document 10 section
/// 10.5 assumes when it says the array's own pointers need capabilities.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Iovec {
    /// Where the element's bytes go, or come from.
    pub base: *mut c_void,
    /// How many of them.
    pub len: usize,
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
    let at = addr as usize;
    let Some(region) = alloc::covering(at) else { return };
    // SAFETY: the address is inside the region the plane was built over, which is what reading the
    // plane asks for, and finding that region by the address is what says so.
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
    let watch = Watch::on(start);
    let mut len = 0;
    while len < limit {
        let at = start.wrapping_add(len);
        watch.step(site, at);
        // SAFETY: the byte is inside the instance the string started in, which the step above is
        // what establishes, or outside the heap this monitor watches, where reading it is the
        // program's own business and no worse than the call it asked for.
        if unsafe { (at as *const u8).read() } == 0 {
            return len;
        }
        len += 1;
    }
    len
}

/// Judgement J1 over a write whose length the call discovers as it runs.
///
/// `strcpy` and its relatives. The source is read to its terminator and the destination is written
/// for exactly as far, and neither of those is a number anybody has when the call starts, so the
/// two walk together and each is judged at every granule it reaches. A destination too small is
/// refused at the byte that leaves it rather than after the copy has run, which is the difference
/// between a report that names the overflowing byte and one that names the call.
///
/// `limit` is how far the source may be read, which is `usize::MAX` for the unbounded functions and
/// `n` for `strncat`. The terminator is judged as well, because it is a byte the call writes.
///
/// Returns how many bytes of the source were read, not counting the terminator.
///
/// # Panics
///
/// As [`range`].
///
/// # Safety
///
/// `dst` and `src` are the pointers the program passed to a string function. The source is read one
/// byte at a time and the destination is not read or written at all, only judged.
#[must_use]
pub unsafe fn copied(
    dst_site: &'static str,
    src_site: &'static str,
    dst: *mut c_void,
    src: *const c_void,
    limit: usize,
) -> usize {
    // SAFETY: this function's contract is the one below, with the write starting where the caller
    // said the destination does.
    unsafe { walk(dst_site, src_site, dst as usize, src as usize, limit) }
}

/// The same, for a function that writes after what the destination already holds.
///
/// `strcat`, whose write starts at the destination's own terminator. Finding it is a walk that has
/// to be judged like any other, and it is judged first: appending to a buffer with no terminator in
/// it runs off the end before a single byte of the source has been looked at, and that is the
/// refusal worth reporting rather than one about the source.
///
/// # Panics
///
/// As [`range`].
///
/// # Safety
///
/// As [`copied`], except that the destination is read as far as its own terminator.
#[must_use]
pub unsafe fn appended(
    dst_site: &'static str,
    src_site: &'static str,
    dst: *mut c_void,
    src: *const c_void,
    limit: usize,
) -> usize {
    // SAFETY: the destination is a string, which is what the call was handed.
    let held = unsafe { scan(dst_site, dst.cast_const()) };
    // SAFETY: as `copied`, from the byte the string already there ends at.
    unsafe { walk(dst_site, src_site, (dst as usize).wrapping_add(held), src as usize, limit) }
}

/// The walk both of the discovered writes are.
///
/// # Safety
///
/// As [`copied`].
unsafe fn walk(
    dst_site: &'static str,
    src_site: &'static str,
    dst: usize,
    src: usize,
    limit: usize,
) -> usize {
    let to = Watch::on(dst);
    let from = Watch::on(src);
    let mut len = 0;
    while len < limit {
        let read = src.wrapping_add(len);
        from.step(src_site, read);
        to.step(dst_site, dst.wrapping_add(len));
        // SAFETY: the byte is inside the instance the source started in, which the step above is
        // what establishes, or outside the heap this monitor watches.
        if unsafe { (read as *const u8).read() } == 0 {
            return len;
        }
        len += 1;
    }
    // The terminator, which these functions write whether or not the source reached one. A
    // destination with room for the bytes and not for the NUL is a real overflow of exactly one
    // byte, and it is a common enough one to be worth the extra step.
    to.step(dst_site, dst.wrapping_add(len));
    len
}

/// Judgement J1 over an array of `struct iovec` and over every buffer it names.
///
/// Section 10.5's scatter and gather. The array is one object and each element points at another,
/// so there are `count` plus one ranges to judge, and the array has to be judged first: reading an
/// element out of an array that is shorter than the count says is the bug and reading it to find
/// the next bug would be committing it.
///
/// Returns how many bytes the whole vector describes, which is what the syscall will move at most.
///
/// A negative count is left alone. The kernel answers that with `EINVAL` and a monitor that
/// refused it first would be reporting a memory safety violation about a call that never touched
/// memory.
///
/// # Panics
///
/// As [`range`].
///
/// # Safety
///
/// `addr` is what the program is about to pass to a scatter or gather syscall. Its elements are
/// read, which is what the kernel is about to do with them.
#[must_use]
pub unsafe fn vectors(site: &'static str, addr: *const c_void, count: i32) -> usize {
    let Ok(count) = usize::try_from(count) else { return 0 };
    let each = size_of::<Iovec>();
    range(site, addr, count.saturating_mul(each));

    let mut total = 0_usize;
    for at in 0..count {
        // SAFETY: the array has been judged for the whole of `count`, which is what the read of one
        // element inside it asks for.
        let entry = unsafe { addr.cast::<Iovec>().add(at).read() };
        range(site, entry.base.cast_const(), entry.len);
        total = total.saturating_add(entry.len);
    }
    total
}

/// One argument's instance, remembered so that a walk can ask about each byte cheaply.
///
/// The version the first byte belonged to is read once, and every byte after it is a comparison
/// against that. Comparing against the instance rather than against a rule is what makes a walk
/// into the next block a refusal even when the next block is perfectly live.
#[derive(Clone, Copy)]
struct Watch {
    /// The heap and the version the walk started in, or nothing at all when the address is not the
    /// heap's, where there is no plane to ask and the monitor has nothing to say.
    watched: Option<(alloc::Region, plane::Version)>,
    /// Where the walk started, which is the one byte judged without starting a granule.
    start: usize,
}

impl Watch {
    /// What the plane says about the byte a walk is about to start at.
    fn on(start: usize) -> Self {
        let watched = alloc::covering(start).map(|region| {
            // SAFETY: the region is the one that covers this address, which is what reading the
            // plane asks for.
            (region, unsafe { region.plane.version(start) })
        });
        Self { watched, start }
    }

    /// Judges one byte of the walk, if it is one the plane could have changed its answer at.
    ///
    /// The plane holds one version per granule, so asking about every byte would be fifteen
    /// repeated questions out of every sixteen. The first byte and each granule boundary are the
    /// only places a walk can cross out of the instance it started in.
    ///
    /// # Panics
    ///
    /// When the byte is not in the live instance the walk started in.
    fn step(self, site: &'static str, at: usize) {
        let Some((region, instance)) = self.watched else { return };
        if at != self.start && at % GRANULE != 0 {
            return;
        }
        let same = plane::owned(instance)
            && region.holds(at)
            // SAFETY: `holds` immediately before is what reading the plane asks for.
            && unsafe { region.plane.version(at) } == instance;
        if !same {
            crate::fail::refused_at(Judgement::Access, site, at);
        }
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
/// the extent that is discovered by looking, or both, which is a walk that stops at whichever comes
/// first. The body is what the wrapper does once the judgements have been made, and for almost
/// every row it is a call to the C library's own implementation, because section 10.3's rule is
/// that an interposed function is one whose effects are written down rather than one that was
/// rewritten.
///
/// The whole vocabulary is five words:
///
/// ```text
/// reads(s, n)          n bytes, read
/// reads(s, nul)        as far as the terminator, read
/// reads(s, nul, n)     the terminator or n bytes, whichever comes first
/// writes(d, n)         n bytes, written
/// copies(d, s)         d written as far as s reaches, both judged as the two walk together
/// appends(d, s)        the same, starting at d's own terminator
/// appends(d, s, n)     the same, stopping at n
/// scatters(v, k)       an array of k iovecs, and the buffers they name, written
/// gathers(v, k)        the same, read
/// ```
///
/// A write cannot take a discovered extent of its own, and saying so is a compile error naming the
/// row: the NUL that would say where a written range ends is the byte the call is about to write.
/// `copies` and `appends` exist because that is the shape those functions really have, which is a
/// length that belongs to a different argument than the one being written.
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
                    $crate::__judge!($kind, $name, $target, $($len),+);
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
                    effects: $crate::__effects!(@ [] $($kind($target, $($len),+))+),
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

/// What the report calls one argument of one row.
///
/// The function and the argument as the row spells them, so a refusal names the call the program
/// wrote rather than a line inside this crate.
#[doc(hidden)]
#[macro_export]
macro_rules! __site {
    ($name:ident, $arg:ident) => {
        concat!(stringify!($name), ", over its ", stringify!($arg), " argument")
    };
}

/// One clause of one row, as the judgement it stands for.
///
/// Split out of [`crate::interpose`] because a `macro_rules` arm cannot branch on the value of an `ident`
/// it captured, and matching the word itself is how the vocabulary is turned into code. Which is
/// also what makes an unknown word a compile error naming the row rather than a silently ignored
/// clause.
///
/// The `nul` arms come first. `reads(s, nul)` matches the general arm as well, and the first arm
/// that matches is the one that runs.
#[doc(hidden)]
#[macro_export]
macro_rules! __judge {
    (reads, $name:ident, $arg:ident, nul) => {
        // SAFETY: the pointer is one the program passed to a string function, which is what this
        // reads it as.
        let _ = unsafe { $crate::effects::scan($crate::__site!($name, $arg), $arg.cast()) };
    };
    (reads, $name:ident, $arg:ident, nul, $limit:tt) => {
        // SAFETY: as the unbounded arm, and reading fewer bytes than it would.
        let _ = unsafe {
            $crate::effects::scan_within($crate::__site!($name, $arg), $arg.cast(), $limit)
        };
    };
    (reads, $name:ident, $arg:ident, $len:tt) => {
        $crate::effects::range($crate::__site!($name, $arg), $arg.cast(), $len);
    };
    (writes, $name:ident, $arg:ident, nul $(, $limit:tt)?) => {
        compile_error!(
            "a written extent cannot be discovered from the destination, since the NUL that would \
             say where it ends is what the call is about to write. Use copies or appends."
        );
    };
    (writes, $name:ident, $arg:ident, $len:tt) => {
        $crate::effects::range($crate::__site!($name, $arg), $arg.cast(), $len);
    };
    (copies, $name:ident, $dst:ident, $src:ident) => {
        // SAFETY: both pointers are ones the program passed to a string function, which is what
        // this walks them as, and only the source is read.
        let _ = unsafe {
            $crate::effects::copied(
                $crate::__site!($name, $dst),
                $crate::__site!($name, $src),
                $dst.cast(),
                $src.cast(),
                usize::MAX,
            )
        };
    };
    (appends, $name:ident, $dst:ident, $src:ident) => {
        // SAFETY: as the `copies` arm, and the destination is a string as well.
        let _ = unsafe {
            $crate::effects::appended(
                $crate::__site!($name, $dst),
                $crate::__site!($name, $src),
                $dst.cast(),
                $src.cast(),
                usize::MAX,
            )
        };
    };
    (appends, $name:ident, $dst:ident, $src:ident, $limit:tt) => {
        // SAFETY: as the unbounded arm, and reading fewer bytes of the source than it would.
        let _ = unsafe {
            $crate::effects::appended(
                $crate::__site!($name, $dst),
                $crate::__site!($name, $src),
                $dst.cast(),
                $src.cast(),
                $limit,
            )
        };
    };
    (scatters, $name:ident, $arg:ident, $count:tt) => {
        let site = $crate::__site!($name, $arg);
        // SAFETY: the pointer is the array the program is about to hand a syscall, and reading its
        // elements is what the kernel is about to do.
        let _ = unsafe { $crate::effects::vectors(site, $arg.cast(), $count) };
    };
    (gathers, $name:ident, $arg:ident, $count:tt) => {
        let site = $crate::__site!($name, $arg);
        // SAFETY: as the `scatters` arm, which is the same array read the same way.
        let _ = unsafe { $crate::effects::vectors(site, $arg.cast(), $count) };
    };
}

/// The effects clause of one row, as the data the table holds.
///
/// A muncher rather than the plain repetition the rest of the generator uses, because two of the
/// words describe more than one argument: `copies(dst, src)` is one judgement over a pair and two
/// effects. A macro standing where an array element stands may only expand to one element, so the
/// array is built a clause at a time with what is done so far carried along in the brackets.
///
/// Arm order is the same as [`crate::__judge`]'s and for the same reason.
#[doc(hidden)]
#[macro_export]
macro_rules! __effects {
    (@ [$($done:expr,)*]) => {
        &[$($done),*]
    };
    (@ [$($done:expr,)*] reads($arg:ident, nul) $($rest:tt)*) => {
        $crate::__effects!(@ [
            $($done,)*
            $crate::effects::Effect {
                arg: stringify!($arg),
                kind: $crate::effects::Kind::Reads,
                extent: $crate::effects::Extent::Nul,
            },
        ] $($rest)*)
    };
    (@ [$($done:expr,)*] reads($arg:ident, nul, $limit:tt) $($rest:tt)*) => {
        $crate::__effects!(@ [
            $($done,)*
            $crate::effects::Effect {
                arg: stringify!($arg),
                kind: $crate::effects::Kind::Reads,
                extent: $crate::effects::Extent::NulWithin(stringify!($limit)),
            },
        ] $($rest)*)
    };
    (@ [$($done:expr,)*] copies($dst:ident, $src:ident) $($rest:tt)*) => {
        $crate::__effects!(@ [
            $($done,)*
            $crate::effects::Effect {
                arg: stringify!($dst),
                kind: $crate::effects::Kind::Writes,
                extent: $crate::effects::Extent::NulOf(stringify!($src)),
            },
            $crate::effects::Effect {
                arg: stringify!($src),
                kind: $crate::effects::Kind::Reads,
                extent: $crate::effects::Extent::Nul,
            },
        ] $($rest)*)
    };
    (@ [$($done:expr,)*] appends($dst:ident, $src:ident) $($rest:tt)*) => {
        $crate::__effects!(@ [
            $($done,)*
            // The destination is read as well as written, because its own terminator is what says
            // where the write starts.
            $crate::effects::Effect {
                arg: stringify!($dst),
                kind: $crate::effects::Kind::Reads,
                extent: $crate::effects::Extent::Nul,
            },
            $crate::effects::Effect {
                arg: stringify!($dst),
                kind: $crate::effects::Kind::Writes,
                extent: $crate::effects::Extent::NulOf(stringify!($src)),
            },
            $crate::effects::Effect {
                arg: stringify!($src),
                kind: $crate::effects::Kind::Reads,
                extent: $crate::effects::Extent::Nul,
            },
        ] $($rest)*)
    };
    (@ [$($done:expr,)*] appends($dst:ident, $src:ident, $limit:tt) $($rest:tt)*) => {
        $crate::__effects!(@ [
            $($done,)*
            $crate::effects::Effect {
                arg: stringify!($dst),
                kind: $crate::effects::Kind::Reads,
                extent: $crate::effects::Extent::Nul,
            },
            $crate::effects::Effect {
                arg: stringify!($dst),
                kind: $crate::effects::Kind::Writes,
                extent: $crate::effects::Extent::NulOfWithin(
                    stringify!($src),
                    stringify!($limit),
                ),
            },
            $crate::effects::Effect {
                arg: stringify!($src),
                kind: $crate::effects::Kind::Reads,
                extent: $crate::effects::Extent::NulWithin(stringify!($limit)),
            },
        ] $($rest)*)
    };
    (@ [$($done:expr,)*] scatters($arg:ident, $count:tt) $($rest:tt)*) => {
        $crate::__effects!(@ [
            $($done,)*
            $crate::effects::Effect {
                arg: stringify!($arg),
                kind: $crate::effects::Kind::Writes,
                extent: $crate::effects::Extent::Vectors(stringify!($count)),
            },
        ] $($rest)*)
    };
    (@ [$($done:expr,)*] gathers($arg:ident, $count:tt) $($rest:tt)*) => {
        $crate::__effects!(@ [
            $($done,)*
            $crate::effects::Effect {
                arg: stringify!($arg),
                kind: $crate::effects::Kind::Reads,
                extent: $crate::effects::Extent::Vectors(stringify!($count)),
            },
        ] $($rest)*)
    };
    // Last, because every word above names two arguments and both of them are idents, which is
    // what this arm's extent would happily match.
    (@ [$($done:expr,)*] $kind:ident($arg:ident, $len:tt) $($rest:tt)*) => {
        $crate::__effects!(@ [
            $($done,)*
            $crate::effects::Effect {
                arg: stringify!($arg),
                kind: $crate::__kind!($kind),
                extent: $crate::effects::Extent::SizedBy(stringify!($len)),
            },
        ] $($rest)*)
    };
}

/// The direction of one clause, as data.
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
    fn a_write_whose_length_is_discovered_is_measured_over_the_source() {
        let _turn = turn();
        // What the judgement returns is the length the call is going to move, which is the number
        // the row would have been given if C had written one down.
        let from = alloc(64);
        let to = alloc(64);
        put(from, b"hello", true);
        // SAFETY: both are live instances and the destination has room for the source.
        assert_eq!(unsafe { copied("dst", "src", to, from, usize::MAX) }, 5);
        put(to, b"one", true);
        // SAFETY: as above, from the byte the destination's own string ends at.
        assert_eq!(unsafe { appended("dst", "src", to, from, usize::MAX) }, 5);
        // SAFETY: both are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
        }
    }

    #[test]
    fn a_discovered_write_stops_where_its_limit_says() {
        let _turn = turn();
        // `strncat`'s bound. The source is longer than the count and the walk stops anyway, which
        // is what keeps a bounded append from being judged as an unbounded one.
        let from = alloc(64);
        let to = alloc(64);
        put(from, b"a source that is quite long", true);
        put(to, b"", true);
        // SAFETY: both are live instances and the walk reads four bytes of the source.
        assert_eq!(unsafe { appended("dst", "src", to, from, 4) }, 4);
        // SAFETY: both are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
        }
    }

    #[test]
    fn a_discovered_write_is_refused_when_the_destination_runs_out_first() {
        let _turn = turn();
        // The source is fine and the destination is not, and there is no length anywhere to compare
        // against. The walk is what notices, at the byte that leaves the destination.
        let from = alloc(64);
        let to = alloc(16);
        put(from, b"a string that is longer than sixteen bytes", true);
        assert!(refused(|| {
            // SAFETY: the destination is judged as the source is walked, and it is not written.
            let _ = unsafe { copied("dst", "src", to, from, usize::MAX) };
        }));
        // SAFETY: both are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
        }
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
