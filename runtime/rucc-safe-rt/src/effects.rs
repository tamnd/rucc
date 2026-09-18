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
//! The same as [`crate::check`], because it reads the same planes. Bounds and lifetime, per
//! granule, over the heap this monitor's allocator hands out of. An address that is not the heap's
//! is passed, which is a local, a global or another allocator's memory, and reporting on one would
//! be a false positive against a program doing nothing wrong.
//!
//! Beside the judgement, a wrapper that writes records what it wrote into the init plane and into
//! the type plane. That is not a check and refuses nothing, and it is here for the reason section
//! 10.1 gives for modelling a boundary at all: a plane only some of the writes maintain reports on
//! programs that are correct. A `memset` the monitor did not hear about would leave its destination
//! looking like storage nobody ever wrote, and the next read of it would be refused.
//!
//! Both planes, because for a long time it was only the init one, and the half that was missing is
//! what an instrumented sqlite3 aborted on within a few hundred calls. A wrapper writes bytes, which
//! is a store through a character type, and C 6.5 says storage written that way is storage a later
//! access may give whatever type it likes. The compiler already gets this right for the loop a
//! program writes out by hand, so `for (i = 0; i < n; i++) p[i] = 0;` left the destination readable
//! as anything and `memset(p, 0, n)` left it readable only as whatever was in it before. Any pool
//! allocator that clears a block and hands it back out for a different structure walked into that,
//! which is most of them, and sqlite's lookaside is the one that found it.
//!
//! `memcpy` is the exception and has a word of its own. C names it: a copy carries the source's
//! effective type rather than making the destination bytes, which is what keeps the punning idiom
//! through a character buffer working, so the `moves` clause calls [`crate::check::carry`] and not
//! the character write the rest of them do.
//!
//! It has a third recorded plane for the same reason. A copy moves whatever pointers were in the
//! source along with the bytes, and what says where a pointer points lives in the aux slot beside
//! the word rather than in the word, so a copy that moved the words and left the aux would leave
//! the destination's pointers described by whatever the destination was carrying before. The
//! `moves` clause calls [`crate::check::relocate`] to move the slots across with the bytes.
//!
//! [`Kind`] tells a read from a write, and the two recorded planes are the things that care about
//! the difference: a read of a range nobody wrote is document 03's Y6 and a write of one is not a
//! bug at all. Bounds and lifetime still do not care which direction the bytes were going.
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

/// Which group of interposed function a row belongs to.
///
/// Section 10.3's three that are interposed, and the synchronization primitives, which that section
/// does not name because it is about memory effects and they have none. Document 09 section 9.5 is
/// where they are asked for, and they are here rather than in a file of their own because they are
/// interposed in exactly the same way and for the same reason: the effect a program's call has on
/// what this monitor knows is written down, and the call goes through to the C library's own.
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
    /// The synchronization primitives, whose effect is not on a range at all but on the ordering
    /// between two threads that document 09 section 9.5's clock is made of.
    Ordering,
}

impl Group {
    /// The name the summary prints.
    #[must_use]
    pub const fn what(self) -> &'static str {
        match self {
            Self::Movement => "movement",
            Self::Allocation => "allocation",
            Self::Syscall => "syscall",
            Self::Ordering => "ordering",
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

/// The word the row was written with, which is what decides the planes the wrapper maintains.
///
/// [`Kind`] says which way the bytes go and that is what the summary counts, but it is not enough
/// to know what a wrapper has to do. `moves` and `writes` are both a write of a known length, and
/// they are not the same judgement: a copy hands the destination the source's aux, and a byte-wise
/// write has no source of slots to hand over and has to clear the destination's. A `Kind` cannot
/// tell those apart, so the wiring cannot be checked against it. This can.
///
/// One variant per arm of [`crate::__judge`], which is what makes the check in this module's tests
/// possible: a test whose `match` over this is exhaustive stops compiling when somebody adds an
/// eighth word, and it is the adding of a word that is the risky moment rather than the adding of
/// a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clause {
    /// `reads(arg, ...)`, which judges and touches no plane.
    Reads,
    /// `writes(arg, len)`, a byte-wise write of a known length.
    Writes,
    /// `moves(dst, src, len)`, a copy, which carries the source's slots across.
    Moves,
    /// `copies(dst, src)`, a byte-wise write whose length is a walk of the source.
    Copies,
    /// `appends(dst, src)` and `appends(dst, src, limit)`, the same after a walk of the
    /// destination.
    Appends,
    /// `scatters(arg, count)`, a write into each buffer an iovec array names.
    Scatters,
    /// `gathers(arg, count)`, a read out of each of them.
    Gathers,
}

impl Clause {
    /// The word the row was written with.
    #[must_use]
    pub const fn what(self) -> &'static str {
        match self {
            Self::Reads => "reads",
            Self::Writes => "writes",
            Self::Moves => "moves",
            Self::Copies => "copies",
            Self::Appends => "appends",
            Self::Scatters => "scatters",
            Self::Gathers => "gathers",
        }
    }

    /// Whether the clause puts bytes into the range rather than taking them out of it.
    ///
    /// Not the same question as [`Kind::Writes`] on one effect, because a clause can name two
    /// arguments going opposite ways. This is a property of the word, and it is what the coverage
    /// test uses to decide which clauses have to be exercised.
    #[must_use]
    pub const fn writing(self) -> bool {
        matches!(self, Self::Writes | Self::Moves | Self::Copies | Self::Appends | Self::Scatters)
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
    /// The word the row was written with, which is what says the planes the wrapper maintains.
    pub clause: Clause,
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
/// Returns how far from `dst` the call will have written, not counting the terminator, which is
/// what was already there plus what the source adds. Measured from `dst` rather than from where
/// the write starts, because that is the number a caller recording the write wants and the string
/// already there was written by whatever put it there.
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
    let added =
        unsafe { walk(dst_site, src_site, (dst as usize).wrapping_add(held), src as usize, limit) };
    held.wrapping_add(added)
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
pub unsafe fn vectors(site: &'static str, addr: *const c_void, count: i32, kind: Kind) -> usize {
    let Ok(count) = usize::try_from(count) else { return 0 };
    let each = size_of::<Iovec>();
    range(site, addr, count.saturating_mul(each));

    let mut total = 0_usize;
    for at in 0..count {
        // SAFETY: the array has been judged for the whole of `count`, which is what the read of one
        // element inside it asks for.
        let entry = unsafe { addr.cast::<Iovec>().add(at).read() };
        range(site, entry.base.cast_const(), entry.len);
        if kind == Kind::Writes {
            // SAFETY: the buffer has just been judged, as in the `writes` clause of a wrapper.
            unsafe { crate::check::wrote(entry.base.cast_const(), entry.len) };
            // SAFETY: as above, over the type plane, as in the same clause.
            unsafe {
                crate::check::judge(entry.base.cast_const(), entry.len, crate::types::CHARACTER);
            };
            // SAFETY: as above, over the aux, as in the same clause.
            unsafe { crate::check::erase(entry.base.cast_const(), entry.len) };
        }
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
/// The whole vocabulary is these words:
///
/// ```text
/// reads(s, n)          n bytes, read
/// reads(s, nul)        as far as the terminator, read
/// reads(s, nul, n)     the terminator or n bytes, whichever comes first
/// writes(d, n)         n bytes, written
/// moves(d, s, n)       n bytes read from s and written to d, carrying what the source said
/// copies(d, s)         d written as far as s reaches, both judged as the two walk together
/// appends(d, s)        the same, starting at d's own terminator
/// appends(d, s, n)     the same, stopping at n
/// scatters(v, k)       an array of k iovecs, and the buffers they name, written
/// gathers(v, k)        the same, read
/// acquires(m)          the lock m, taking whatever ordering it was released with
/// releases(m)          the lock m, publishing the clock it is given up at
/// waits(m)             the lock m, given up for the length of the call and taken back after
/// joins(t)             the thread t, taking everything it did before it finished
/// spawns(f)            a thread running f, which the row gives an ordering to itself
/// ```
///
/// The last five are the `Ordering` group and they are the one clause that is not about a range.
/// They have an arm of their own here because a range judgement happens before the call and an
/// ordering edge does not: a release has to be published before the lock is really given up, or a
/// thread that takes the lock next reads a clock from before the work it is being handed, and an
/// acquire has to be taken after the call returns, and only when the call says the lock was taken.
/// Every row in that group returns a `c_int` that is zero when it worked, which is what the arm
/// tests, and a row whose function does not is a row that does not belong in the group.
///
/// `spawns` is the one word that asks the generator for nothing. Its row takes the edge in its own
/// body, because a thread cannot be handed a clock once it has already started and there is nowhere
/// in this grammar to say that one argument is replaced by another function entirely. The word is
/// there so that the row still says in a line what it is for, and so that it lands in the table
/// every other ordering row lands in.
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
        group: Ordering;
        $(
            $(#[$note:meta])*
            fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty
                where $edge:ident($lock:ident)
            $body:block
        )+
    ) => {
        $(
            $(#[$note])*
            ///
            /// # Safety
            ///
            /// The arguments are whatever the program passed, and what this does with them is what
            /// the C library would have done. The lock is never read through here, only its address
            /// is used, so a call this monitor was handed rubbish for is rubbish the C library's own
            /// implementation gets to decide about.
            pub unsafe fn $name($($arg: $ty),*) -> $ret {
                $crate::__edge!($edge, $lock, $body)
            }
        )+

        /// Every row above, as data.
        ///
        /// The effects are empty because these rows have none. What they do is on the clock rather
        /// than on a range, so `--emit=safety-summary` counts them as a group and has nothing to
        /// print per argument.
        pub static TABLE: &[$crate::effects::Row] = &[
            $(
                $crate::effects::Row {
                    name: stringify!($name),
                    wrapper: concat!("__rucc_wrap_", stringify!($name)),
                    group: $crate::effects::Group::Ordering,
                    effects: &[],
                }
            ),+
        ];

        /// The symbols a redirected call site is compiled against.
        ///
        /// Separate from the functions above for the reason the other groups' are.
        #[cfg(not(test))]
        pub mod exports {
            use super::*;

            $(
                #[doc = concat!(
                    "`", stringify!($name), "`, with its ordering edge taken.\n\n",
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
        // SAFETY: the range has just been judged, so it is inside one live instance or outside
        // this monitor's heap, and the plane write passes over an address no region covers.
        unsafe { $crate::check::wrote($arg.cast(), $len) };
        // SAFETY: as above, and the type plane covers the same granules the init plane does.
        unsafe { $crate::check::judge($arg.cast(), $len, $crate::types::CHARACTER) };
        // SAFETY: as above, over the aux, which described pointers these bytes have replaced.
        unsafe { $crate::check::erase($arg.cast(), $len) };
    };
    (moves, $name:ident, $dst:ident, $src:ident, $len:tt) => {
        $crate::effects::range($crate::__site!($name, $dst), $dst.cast(), $len);
        $crate::effects::range($crate::__site!($name, $src), $src.cast(), $len);
        // SAFETY: both ranges have just been judged, as in the `writes` arm.
        unsafe { $crate::check::spread($dst.cast(), $src.cast(), $len) };
        // SAFETY: as above, over the type plane, which is the one C names `memcpy` in.
        unsafe { $crate::check::carry($dst.cast(), $src.cast(), $len) };
        // SAFETY: as above, over the aux, which holds the capability of every pointer being moved.
        unsafe { $crate::check::relocate($dst.cast(), $src.cast(), $len) };
    };
    (copies, $name:ident, $dst:ident, $src:ident) => {
        // SAFETY: both pointers are ones the program passed to a string function, which is what
        // this walks them as, and only the source is read.
        let written = unsafe {
            $crate::effects::copied(
                $crate::__site!($name, $dst),
                $crate::__site!($name, $src),
                $dst.cast(),
                $src.cast(),
                usize::MAX,
            )
        };
        // The terminator as well, which is the byte the walk judged past the length it returned.
        // SAFETY: the destination has just been judged for every byte of that, as in `writes`.
        unsafe { $crate::check::wrote($dst.cast(), written.wrapping_add(1)) };
        // SAFETY: as above, over the type plane, as in `writes`.
        unsafe {
            $crate::check::judge($dst.cast(), written.wrapping_add(1), $crate::types::CHARACTER)
        };
        // SAFETY: as above, over the aux, as in `writes`.
        unsafe { $crate::check::erase($dst.cast(), written.wrapping_add(1)) };
    };
    (appends, $name:ident, $dst:ident, $src:ident) => {
        // SAFETY: as the `copies` arm, and the destination is a string as well.
        let written = unsafe {
            $crate::effects::appended(
                $crate::__site!($name, $dst),
                $crate::__site!($name, $src),
                $dst.cast(),
                $src.cast(),
                usize::MAX,
            )
        };
        // From the destination rather than from where the write starts, because the bytes of the
        // string already there were written by whatever put it there. Saying so again costs a
        // wider plane write and says nothing untrue.
        // SAFETY: as the `copies` arm.
        unsafe { $crate::check::wrote($dst.cast(), written.wrapping_add(1)) };
        // SAFETY: as the `copies` arm.
        unsafe {
            $crate::check::judge($dst.cast(), written.wrapping_add(1), $crate::types::CHARACTER)
        };
        // SAFETY: as the `copies` arm.
        unsafe { $crate::check::erase($dst.cast(), written.wrapping_add(1)) };
    };
    (appends, $name:ident, $dst:ident, $src:ident, $limit:tt) => {
        // SAFETY: as the unbounded arm, and reading fewer bytes of the source than it would.
        let written = unsafe {
            $crate::effects::appended(
                $crate::__site!($name, $dst),
                $crate::__site!($name, $src),
                $dst.cast(),
                $src.cast(),
                $limit,
            )
        };
        // SAFETY: as the unbounded arm.
        unsafe { $crate::check::wrote($dst.cast(), written.wrapping_add(1)) };
        // SAFETY: as the unbounded arm.
        unsafe {
            $crate::check::judge($dst.cast(), written.wrapping_add(1), $crate::types::CHARACTER)
        };
        // SAFETY: as the unbounded arm.
        unsafe { $crate::check::erase($dst.cast(), written.wrapping_add(1)) };
    };
    (scatters, $name:ident, $arg:ident, $count:tt) => {
        let site = $crate::__site!($name, $arg);
        // SAFETY: the pointer is the array the program is about to hand a syscall, and reading its
        // elements is what the kernel is about to do.
        let _ = unsafe {
            $crate::effects::vectors(site, $arg.cast(), $count, $crate::effects::Kind::Writes)
        };
    };
    (gathers, $name:ident, $arg:ident, $count:tt) => {
        let site = $crate::__site!($name, $arg);
        // SAFETY: as the `scatters` arm, which is the same array read the same way.
        let _ = unsafe {
            $crate::effects::vectors(site, $arg.cast(), $count, $crate::effects::Kind::Reads)
        };
    };
}

/// One clause of one `Ordering` row, as the edge it stands for.
///
/// Split out of [`crate::interpose`] for the reason [`crate::__judge`] is, which is that a
/// `macro_rules` arm cannot branch on the value of an `ident` it captured. There are four words and
/// they differ in when the edge is taken as much as in what it does.
///
/// A release publishes before the call, so that the clock is already in the table when the lock is
/// really given up and the thread that takes it next cannot miss it. An acquire takes the edge
/// after, and only when the call says it got the lock: a `pthread_mutex_trylock` that returned
/// `EBUSY` has taken nothing, and syncing to the holder's clock for it would order this thread
/// behind work it was never handed. A join is an acquire against a thread rather than against a
/// lock and it is tested the same way, since a join that failed was told nothing about whether the
/// thread has finished. A spawn is the row's own business and this passes it straight through.
///
/// A wait is both halves in one call and it is the one word that does not test the return value.
/// `pthread_cond_wait` releases the caller's mutex inside itself and holds it again by the time it
/// comes back, however it comes back, so a timed wait that gave up on its deadline is still a
/// thread holding a lock that somebody else gave up in the meantime.
#[doc(hidden)]
#[macro_export]
macro_rules! __edge {
    (acquires, $lock:ident, $body:block) => {{
        let taken = $body;
        if taken == 0 {
            $crate::sync::acquired($lock.cast());
        }
        taken
    }};
    (releases, $lock:ident, $body:block) => {{
        $crate::sync::released($lock.cast());
        $body
    }};
    (joins, $thread:ident, $body:block) => {{
        let waited = $body;
        if waited == 0 {
            $crate::sync::joined($thread.cast());
        }
        waited
    }};
    (waits, $lock:ident, $body:block) => {{
        $crate::sync::released($lock.cast());
        let woke = $body;
        $crate::sync::acquired($lock.cast());
        woke
    }};
    (spawns, $start:ident, $body:block) => {
        $body
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
                clause: $crate::effects::Clause::Reads,
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
                clause: $crate::effects::Clause::Reads,
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
                clause: $crate::effects::Clause::Copies,
                kind: $crate::effects::Kind::Writes,
                extent: $crate::effects::Extent::NulOf(stringify!($src)),
            },
            $crate::effects::Effect {
                arg: stringify!($src),
                clause: $crate::effects::Clause::Copies,
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
                clause: $crate::effects::Clause::Appends,
                kind: $crate::effects::Kind::Reads,
                extent: $crate::effects::Extent::Nul,
            },
            $crate::effects::Effect {
                arg: stringify!($dst),
                clause: $crate::effects::Clause::Appends,
                kind: $crate::effects::Kind::Writes,
                extent: $crate::effects::Extent::NulOf(stringify!($src)),
            },
            $crate::effects::Effect {
                arg: stringify!($src),
                clause: $crate::effects::Clause::Appends,
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
                clause: $crate::effects::Clause::Appends,
                kind: $crate::effects::Kind::Reads,
                extent: $crate::effects::Extent::Nul,
            },
            $crate::effects::Effect {
                arg: stringify!($dst),
                clause: $crate::effects::Clause::Appends,
                kind: $crate::effects::Kind::Writes,
                extent: $crate::effects::Extent::NulOfWithin(
                    stringify!($src),
                    stringify!($limit),
                ),
            },
            $crate::effects::Effect {
                arg: stringify!($src),
                clause: $crate::effects::Clause::Appends,
                kind: $crate::effects::Kind::Reads,
                extent: $crate::effects::Extent::NulWithin(stringify!($limit)),
            },
        ] $($rest)*)
    };
    (@ [$($done:expr,)*] moves($dst:ident, $src:ident, $len:tt) $($rest:tt)*) => {
        $crate::__effects!(@ [
            $($done,)*
            $crate::effects::Effect {
                arg: stringify!($dst),
                clause: $crate::effects::Clause::Moves,
                kind: $crate::effects::Kind::Writes,
                extent: $crate::effects::Extent::SizedBy(stringify!($len)),
            },
            $crate::effects::Effect {
                arg: stringify!($src),
                clause: $crate::effects::Clause::Moves,
                kind: $crate::effects::Kind::Reads,
                extent: $crate::effects::Extent::SizedBy(stringify!($len)),
            },
        ] $($rest)*)
    };
    (@ [$($done:expr,)*] scatters($arg:ident, $count:tt) $($rest:tt)*) => {
        $crate::__effects!(@ [
            $($done,)*
            $crate::effects::Effect {
                arg: stringify!($arg),
                clause: $crate::effects::Clause::Scatters,
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
                clause: $crate::effects::Clause::Gathers,
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
                clause: $crate::__clause!($kind),
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

/// The clause word itself, as data, for the one arm of [`crate::__effects`] that does not know
/// which word it matched.
///
/// The other arms name their word, so they can write the variant down. The trailing arm matches
/// `reads` and `writes` both and has the word in hand as a token, which is what this turns into a
/// value. Two macros rather than one returning a pair, because the arms that do know their word
/// need the [`crate::effects::Kind`] and the clause in two different places in the same literal.
#[doc(hidden)]
#[macro_export]
macro_rules! __clause {
    (reads) => {
        $crate::effects::Clause::Reads
    };
    (writes) => {
        $crate::effects::Clause::Writes
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc::{alloc, dealloc};
    use crate::check;
    use crate::fail::Descriptor;
    use crate::layout::{Cap, Class, Meta, perm};
    use crate::recover;
    use crate::turnstile::turn;
    use crate::types::{self, TypeId};

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
        // What the judgement returns is how far from the destination the call is going to have
        // written, which is the number the row would have been given if C had written one down.
        let from = alloc(64);
        let to = alloc(64);
        put(from, b"hello", true);
        // SAFETY: both are live instances and the destination has room for the source.
        assert_eq!(unsafe { copied("dst", "src", to, from, usize::MAX) }, 5);
        put(to, b"one", true);
        // Three already there and five added, because an append is measured from the destination
        // and not from the byte the write starts at.
        // SAFETY: as above, from the byte the destination's own string ends at.
        assert_eq!(unsafe { appended("dst", "src", to, from, usize::MAX) }, 8);
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
        // Nothing already there, so the answer is the four the walk read.
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

    /// The clauses [`every_writing_clause_maintains_the_planes_a_wrapper_owes`] runs a real row
    /// for, which is every word that puts bytes into a range.
    ///
    /// Written out rather than derived from [`Clause::writing`], because a list derived from the
    /// thing it is checking would agree with it whatever either of them said.
    const EXERCISED: &[Clause] =
        &[Clause::Writes, Clause::Moves, Clause::Copies, Clause::Appends, Clause::Scatters];

    /// A real descriptor, because the plane checks take one and the reporter reads it.
    static ROW: Descriptor = Descriptor { judgement: 9, class: 0, size: 8, pc: 0 };

    /// Two types that are neither untyped nor character, so that the type plane has an answer it
    /// can be wrong about. A plane holding either of these refuses an access through the other.
    const MARK: TypeId = types::interned(7);
    /// The second of them, used where a test needs to ask a question the first would answer yes to.
    const OTHER: TypeId = types::interned(9);

    /// The capability of a whole instance the allocator just handed back, as `cap_of` reads it.
    fn of(ptr: *mut c_void) -> Cap {
        let addr = ptr as usize;
        let region = alloc::covering(addr).expect("the allocator's own storage is watched");
        let (lo, ext) = recover::extent(&region, addr).expect("a live instance owns it");
        // SAFETY: the region is the one covering the address.
        let ver = unsafe { region.plane.version(addr) };
        Cap::new(
            lo as u64,
            ext as u64,
            ver,
            Meta::new(Class::Allocated, perm::READ | perm::WRITE, 0),
        )
    }

    /// Whether the init plane says every byte of the range has been written.
    fn written(ptr: *mut c_void, len: usize) -> bool {
        // SAFETY: the range is inside an instance the caller allocated, and is not read through.
        !refused(|| unsafe { check::filled(at(ptr, 0), len, &ROW) })
    }

    /// Whether the type plane still holds an answer that refuses an access through `ty`.
    fn insists(ptr: *mut c_void, len: usize, ty: TypeId) -> bool {
        // SAFETY: as `written`.
        refused(|| unsafe { check::typed(at(ptr, 0), len, ty, &ROW) })
    }

    /// What the word `offset` bytes in says about a pointer read out of it.
    ///
    /// Bottom is the aux saying nothing, which is what a byte-wise write over the word has to
    /// leave behind. Anything else is a slot, and what it holds is worth looking at rather than
    /// just counting, because a slot is a displacement from the pointer value rather than an
    /// address: asking about the wrong pointee still rebuilds a capability, and it is the base and
    /// the version that say which object it is really about.
    fn recalled(ptr: *mut c_void, offset: usize, pointee: *mut c_void) -> Cap {
        // SAFETY: the word is in an instance the caller allocated, and neither address is read
        // through.
        unsafe { crate::cap::load(of(ptr), at(ptr, offset), pointee) }
    }

    /// An instance with a pointer in the word `offset` bytes in and a type plane that says [`MARK`].
    ///
    /// The three planes in the state a real destination is in before a library function writes over
    /// it: nothing written, a type that is not a character type, and a word describing a pointer.
    fn dressed(size: usize, offset: usize, pointee: *mut c_void) -> *mut c_void {
        let ptr = alloc(size);
        // SAFETY: real capabilities in this test's own storage, neither read through.
        unsafe { crate::cap::store(of(ptr), at(ptr, offset), pointee, of(pointee)) };
        // SAFETY: the range is the whole of an instance this test owns.
        unsafe { check::judge(at(ptr, 0), size, MARK) };
        ptr
    }

    #[test]
    fn every_writing_clause_maintains_the_planes_a_wrapper_owes() {
        let _turn = turn();
        // The invariant tamnd/rucc#1307 is about, in the half a table cannot state. A clause is
        // wired to the planes inside an arm of `__judge`, which is tokens rather than data, so no
        // walk of the table can see whether the wiring is there. What a test can do is run a real
        // row per clause over instrumented memory and look at the planes afterwards, which is what
        // this does, and the walk below then says every row that writes goes through one of these.
        //
        // Three holes of this exact shape have been found by hand: the type plane said nothing at
        // all until tamnd/rucc#1148, `memcpy` carried no aux until the same, and every byte-wise
        // write left the destination's aux describing the previous contents. The last is the worst
        // of the three, because a slot that outlives the pointer it described can permit rather
        // than refuse.
        for clause in EXERCISED {
            exercise(*clause);
        }
    }

    /// Runs one real row of the given clause and asserts the three planes moved.
    ///
    /// The `match` is exhaustive on purpose. Adding an eighth word to the grammar is the moment
    /// this invariant is at risk, and until somebody says here what the new word owes the planes,
    /// this file does not compile.
    fn exercise(clause: Clause) {
        match clause {
            Clause::Reads | Clause::Gathers => {
                panic!("{} does not write, so it owes the planes nothing", clause.what())
            }
            Clause::Writes => writes(),
            Clause::Moves => moves(),
            Clause::Copies => copies(),
            Clause::Appends => appends(),
            Clause::Scatters => scatters(),
        }
    }

    /// `memset`, the plain byte-wise write of a length the row was given.
    fn writes() {
        let pointee = alloc(128);
        let dst = dressed(64, 24, pointee);
        assert!(!written(dst, 64), "a fresh instance has had nothing written to it");
        assert!(insists(dst, 64, OTHER), "and the type plane has an answer");
        assert!(!recalled(dst, 24, pointee).is_bottom(), "and the word describes a pointer");

        // SAFETY: a live instance of sixty four bytes, written for all of it.
        unsafe { crate::wrap::memset(dst, 0, 64) };

        assert!(written(dst, 64), "memset records what it wrote");
        assert!(!insists(dst, 64, OTHER), "and that the bytes are characters now");
        assert!(recalled(dst, 24, pointee).is_bottom(), "and that the word is no longer a pointer");

        // SAFETY: the addresses `alloc` handed back.
        unsafe {
            dealloc(dst);
            dealloc(pointee);
        }
    }

    /// `memcpy`, the one writing clause whose destination takes the source's answers rather than
    /// fresh ones.
    fn moves() {
        let pointee = alloc(128);
        let stale = alloc(128);
        let dst = dressed(64, 24, stale);
        let src = dressed(64, 24, pointee);
        // SAFETY: the whole of an instance this test owns.
        unsafe { check::wrote(at(src, 0), 64) };
        // SAFETY: as above, and the source is what the destination is about to be told.
        unsafe { check::judge(at(src, 0), 64, OTHER) };
        let held = of(pointee);
        assert_eq!(
            recalled(dst, 24, stale).ver,
            of(stale).ver,
            "the destination is carrying somebody else's answer"
        );

        // SAFETY: two live instances of sixty four bytes, copied whole.
        unsafe { crate::wrap::memcpy(dst, src.cast_const(), 64) };

        assert!(written(dst, 64), "a copy carries the source's init answers across");
        assert!(
            !insists(dst, 64, OTHER),
            "and the source's type, rather than recording characters"
        );
        assert!(insists(dst, 64, MARK), "which is what tells a carry from a character write");
        let back = recalled(dst, 24, pointee);
        assert_eq!(back.lo, pointee as u64, "and the source's slots, so the pointer arrives whole");
        assert_eq!(back.ext, 128, "with the extent it was stored with");
        assert_eq!(
            back.ver, held.ver,
            "and the version, which is the half a stale slot gets wrong"
        );

        // SAFETY: the addresses `alloc` handed back.
        unsafe {
            dealloc(dst);
            dealloc(src);
            dealloc(pointee);
            dealloc(stale);
        }
    }

    /// `strcpy`, a byte-wise write whose length is a walk of the source.
    fn copies() {
        let pointee = alloc(128);
        let dst = dressed(64, 0, pointee);
        let src = alloc(64);
        put(src, b"hello", true);

        // SAFETY: two live instances, and the destination has room for the source and its
        // terminator.
        unsafe { crate::wrap::strcpy(dst.cast(), src.cast_const().cast()) };

        assert!(written(dst, 6), "a discovered write records what the walk turned out to reach");
        assert!(!insists(dst, 6, OTHER), "over the type plane as well");
        assert!(recalled(dst, 0, pointee).is_bottom(), "and the word it wrote over says nothing");

        // SAFETY: the addresses `alloc` handed back.
        unsafe {
            dealloc(dst);
            dealloc(src);
            dealloc(pointee);
        }
    }

    /// `strcat`, the same after a walk of the destination.
    fn appends() {
        let pointee = alloc(128);
        let dst = dressed(64, 0, pointee);
        let src = alloc(64);
        put(dst, b"one", true);
        put(src, b"hello", true);

        // SAFETY: two live instances, and the destination has room for both strings.
        unsafe { crate::wrap::strcat(dst.cast(), src.cast_const().cast()) };

        assert!(written(dst, 9), "an append is recorded from the destination, terminator included");
        assert!(!insists(dst, 9, OTHER), "over the type plane as well");
        assert!(recalled(dst, 0, pointee).is_bottom(), "and the word it wrote over says nothing");

        // SAFETY: the addresses `alloc` handed back.
        unsafe {
            dealloc(dst);
            dealloc(src);
            dealloc(pointee);
        }
    }

    /// A scatter, which is a write into each buffer an iovec array names.
    ///
    /// Through [`vectors`] rather than through a row, because every `scatters` row is a syscall and
    /// this is the whole of what they share. A test that opened a pipe to reach `readv` would be
    /// testing the pipe.
    fn scatters() {
        let pointee = alloc(128);
        let buffer = dressed(64, 24, pointee);
        let array = [Iovec { base: buffer, len: 64 }];

        // SAFETY: the array is a live local of one element and the buffer is a live instance.
        let total = unsafe { vectors("t", array.as_ptr().cast(), 1, Kind::Writes) };
        assert_eq!(total, 64);

        assert!(written(buffer, 64), "the kernel's write is recorded as one");
        assert!(!insists(buffer, 64, OTHER), "over the type plane as well");
        assert!(
            recalled(buffer, 24, pointee).is_bottom(),
            "and the word it wrote over says nothing"
        );

        // SAFETY: the addresses `alloc` handed back.
        unsafe {
            dealloc(buffer);
            dealloc(pointee);
        }
    }

    #[test]
    fn every_row_that_writes_goes_through_a_clause_the_behaviour_test_runs() {
        // The other half of the invariant, and the half that is data. The test above says what
        // each word owes the planes; this says that every row writing anything was spelled with
        // one of those words, so a row added tomorrow either lands on wiring that has been checked
        // or fails here.
        let mut seen = 0;
        for table in crate::TABLES {
            for row in *table {
                for effect in row.effects {
                    if effect.kind != Kind::Writes {
                        continue;
                    }
                    seen += 1;
                    assert!(
                        effect.clause.writing(),
                        "{} writes {} through {}, which is not a writing word",
                        row.name,
                        effect.arg,
                        effect.clause.what()
                    );
                    assert!(
                        EXERCISED.contains(&effect.clause),
                        "{} writes {} through {}, which no test runs a row for",
                        row.name,
                        effect.arg,
                        effect.clause.what()
                    );
                }
            }
        }
        assert!(seen > 0, "a walk that found no writing rows is a walk of the wrong thing");
    }

    #[test]
    fn a_clause_and_the_kind_beside_it_agree_about_direction() {
        // A row is spelled once and read as two values, and the generator writes both down in the
        // same literal. Anything a `reads` word produced with `Kind::Writes` beside it, or the
        // other way about, is a typo in an arm of `__effects`.
        for table in crate::TABLES {
            for row in *table {
                for effect in row.effects {
                    let agrees = match effect.clause {
                        Clause::Reads | Clause::Gathers => effect.kind == Kind::Reads,
                        Clause::Writes | Clause::Scatters => effect.kind == Kind::Writes,
                        // The three words that name two arguments, one going each way, so there is
                        // nothing here to disagree with. What holds them to their direction is the
                        // test above rather than this walk.
                        Clause::Moves | Clause::Copies | Clause::Appends => true,
                    };
                    assert!(
                        agrees,
                        "{} spells {} with {} and calls it {}",
                        row.name,
                        effect.arg,
                        effect.clause.what(),
                        effect.kind.what()
                    );
                }
            }
        }
    }
}
