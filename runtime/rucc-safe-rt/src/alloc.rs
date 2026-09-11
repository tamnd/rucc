//! `malloc` and `free`, and the two others that cannot be left behind.
//!
//! Design: `spec/safe-memory/10-boundaries.md` sections 10.3 and 10.4.
//!
//! An interposed function is one whose memory effects are written down as judgements, and these
//! four are the smallest set that can be interposed at all. The milestone asks for `malloc` and
//! `free`; `calloc` and `realloc` come with them because the family cannot be split. Interpose
//! `malloc` and `free` alone and the C library's `calloc` hands back a pointer our `free` has
//! never seen, which is judgement J6 reported against a program that did nothing wrong, and our
//! `malloc` hands a pointer to the C library's `realloc`, which is worse: it is the monitor
//! corrupting the heap. A boundary is a boundary or it is a bug.
//!
//! Everything past those four is section 10.3's table and milestone S3. `strdup`, `getline`,
//! `asprintf` and the rest of the C library's own allocating functions are not here yet, so a
//! program that calls one and frees the result gets a refusal it did not earn. That is a known
//! hole rather than a surprise, and it is the reason S1's exit criterion is a test suite written
//! against these four rather than a corpus run.
//!
//! # Where the memory comes from
//!
//! One reservation at the first call, through `mmap`, holding the shadow and then the region. The
//! two are one mapping so that the bias between them is fixed by construction rather than by
//! whatever pair of addresses the kernel happened to return.
//!
//! `mmap` is called through the C library rather than as a raw syscall. These wrappers only exist
//! in a hosted program, since they replace functions a hosted program links, and a hosted program
//! has a C library by definition. Tier K has no allocator at all, which document 10 section 10.4
//! already says.
//!
//! # Why this module is Unix only
//!
//! Not because of `mmap`, which has an answer on every platform, but because of the interposition
//! above it. Replacing `malloc` by defining one is a fact about how ELF and Mach-O resolve a symbol
//! at load time, and it is not how Windows works: there the C runtime's heap is reached through an
//! import table, and taking it over means patching that table or shipping a replacement runtime.
//! That is a different design, not a different constant, so it gets its own decision rather than a
//! second arm of a `cfg` here. Everything below this module, which is to say the planes, the layout
//! and the arena, is portable and is compiled everywhere.

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use crate::fail::Judgement;
use crate::heap::Arena;
use crate::init::Init;
use crate::layout::Class;
use crate::plane::{GRANULE, Lifetime, SLOT};
use crate::types::{self, Side, Types};

/// How much address space the heap is given.
///
/// A gibibyte, reserved rather than committed: the mapping is anonymous and pages arrive when
/// they are touched, so a program that allocates a kilobyte pays for a kilobyte. It is a fixed
/// size because growing a region would move the bias its plane is built on, and everything that
/// has ever held a capability would have to be told.
///
/// Running out of one is not the end of the heap. A region is one entry in a table, so more
/// storage means another region beside this one rather than this one moving, and [`REGIONS`] of
/// them is where the heap actually ends.
#[cfg(not(test))]
pub const REGION: usize = 1 << 30;

/// Sixteen mebibytes under `cargo test`.
///
/// A test that wants to watch the heap take a second region has to exhaust the first, and
/// exhausting a gibibyte means asking the machine for half a gibibyte of shadow pages, which is
/// not a thing a unit test should do to whoever runs it. Nothing in this file depends on how large
/// a region is, so shrinking it under test changes how long the arithmetic runs and not what it
/// answers.
#[cfg(test)]
pub const REGION: usize = 1 << 24;

/// How much shadow a region of `len` bytes needs, which is one version per granule.
#[must_use]
pub const fn shadow(len: usize) -> usize {
    len / GRANULE * SLOT
}

/// How much type plane a region of `len` bytes needs, which is one slot per granule.
///
/// A different granule from the one above, eight bytes against sixteen, for the reason
/// `crate::types` gives: the unit of a distinct type on a sixty four bit target is eight bytes, so
/// a sixteen byte granule holds two of them and most of a program's structures disagree in every
/// granule they have. Two granules in one file costs a second shift constant and nothing else.
#[must_use]
pub const fn typing(len: usize) -> usize {
    len / types::GRANULE * types::SLOT
}

/// How much side table a region of `len` bytes gets, for the granules whose bytes disagree.
///
/// One entry for every eight granules. That is a bound on what can be watched rather than a
/// reservation per granule, and the number comes from the census in
/// `spec/safe-memory/05-representation.md` section 5.2.5: 12.6 percent of SQLite's declarations
/// are heterogeneous at this granule, so one in eight is that measurement rounded the safe way.
///
/// It costs half a byte of address space per byte of region, which puts the plane at one byte per
/// byte against section 5.2.3's budget of 1.25. A program that goes past it does not get a wrong
/// answer, it gets granules recorded as untyped, and `crate::types` says why that is the only
/// direction available here.
#[must_use]
pub const fn siding(len: usize) -> usize {
    len / types::GRANULE / 8 * types::ENTRY
}

/// How much init plane a region of `len` bytes needs, which is one bit per byte.
///
/// An eighth, which is the cheapest of the four spans and the only one that is exact. There is no
/// granule to compress here and nothing to round: a byte is the unit C asks the question about, so
/// the plane answers per byte and `crate::init` says why that is affordable.
#[must_use]
pub const fn initing(len: usize) -> usize {
    crate::init::shadow(len)
}

/// What a region's length is rounded up to.
///
/// A page, so that a length is a length the kernel would have rounded to anyway. What the
/// arithmetic actually needs is smaller and is worth writing down: the length has to be a whole
/// number of granules for each shadow to cover it exactly, and the four shadows together have to
/// be a whole number of granules for the region that follows them to be granule aligned, which
/// together is a multiple of a hundred and twenty eight. A machine with larger pages maps a little
/// more than this asks for and nothing reads past what was asked for, so the rounding is a floor
/// rather than an assumption.
const PAGE: usize = 1 << 12;

/// How much region one instance of `n` bytes needs, or nothing if it does not fit in a `usize`.
///
/// A block is a header, an aux twice the size of the payload and the payload, so a request past a
/// quarter of the address space has no block at all. That is the only size this refuses outright,
/// and it is refused here rather than at the reservation so that nothing is mapped for it.
fn needed(n: usize) -> Option<usize> {
    if n > usize::MAX / 4 {
        return None;
    }
    Some(crate::layout::block(Arena::sized(n)))
}

/// Which allocator this is, for judgement J6.
///
/// One, because there is one. The identity matters when a program has several allocators and a
/// pointer from one reaches the other's `free`, which is what document 10 section 10.4's
/// `__rucc_alloc_tag` is for and which is milestone S3.
const IDENTITY: u64 = 1;

/// The one heap, and the lock that keeps two threads out of its free lists.
///
/// A spin lock rather than a futex because this file cannot call into the C library's threading
/// and because the critical section is a few dozen instructions. It is not the answer for a
/// program with real contention, and the allocator underneath it is not either, so replacing both
/// is one job rather than two. What the lock does not cover is the planes, which generated code
/// reads without taking anything: two threads racing on the same address is document 09's
/// problem and milestone S6's.
struct Heap {
    held: AtomicBool,
    arenas: core::cell::UnsafeCell<[Option<Arena>; REGIONS]>,
}

// SAFETY: every path to the cell goes through `locked`, which holds the lock across the whole of
// its access and hands out no reference that outlives it.
unsafe impl Sync for Heap {}

static HEAP: Heap = Heap {
    held: AtomicBool::new(false),
    arenas: core::cell::UnsafeCell::new([const { None }; REGIONS]),
};

impl Heap {
    /// An instance of `n` bytes out of whichever arena has room, or 0.
    ///
    /// Every existing arena is asked before a new region is reserved, and asking is not just a
    /// bump: an arena whose bump is exhausted may still have a block of the right class on a free
    /// list, and that block is better than a fresh region for the same reason reuse is better than
    /// growth everywhere else.
    ///
    /// Zero is a machine with no address space left, a program with more arenas than the table
    /// holds, or a request whose block does not fit in a `usize`. All three are a `malloc`
    /// returning null, which is what every other allocator does about them.
    fn allocate(&self, n: usize) -> usize {
        self.locked(|arenas| {
            // Before anything is asked, because a size whose block does not fit in a `usize` is a
            // size an arena cannot do arithmetic about either, and the arena would be entitled to
            // trap on it rather than answer.
            let Some(need) = needed(n) else { return 0 };
            for arena in arenas.iter_mut().flatten() {
                let payload = arena.begin(n);
                if payload != 0 {
                    return payload;
                }
            }
            let Some(slot) = arenas.iter().position(Option::is_none) else { return 0 };
            // A request larger than a region gets a region of its own rather than a null. What
            // makes a region a gibibyte is that a gibibyte is a reasonable amount of address space
            // to reserve for a heap nobody has measured, and not anything the arithmetic depends
            // on, so a single allocation past that is a reason to reserve more rather than a
            // reason to refuse. SQLite's spellfix tests ask for four hundred megabytes in one
            // call, and a block is a little over three times what was asked for.
            let Some(mut fresh) = reserve(need.max(REGION)) else { return 0 };
            let payload = fresh.begin(n);
            arenas[slot] = Some(fresh);
            payload
        })
    }

    /// Runs `f` against the arena whose region holds `payload`.
    ///
    /// `None` is a pointer no arena of ours handed out, which is judgement J6 and is the caller's
    /// to report. It is also the whole state of a program that frees before it allocates.
    fn owning<T>(&self, payload: usize, f: impl FnOnce(&mut Arena) -> T) -> Option<T> {
        self.locked(|arenas| {
            arenas.iter_mut().flatten().find(|arena| arena.contains(payload)).map(f)
        })
    }

    /// Runs `f` against the arenas with the lock held.
    fn locked<T>(&self, f: impl FnOnce(&mut [Option<Arena>; REGIONS]) -> T) -> T {
        while self
            .held
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        // SAFETY: the lock above is held until it is released below, and no reference derived
        // from the cell escapes this block, so this is the only live reference to it.
        let answer = f(unsafe { &mut *self.arenas.get() });
        self.held.store(false, Ordering::Release);
        answer
    }
}

/// Where the heap is and where its shadow is, for a reader that holds no lock.
///
/// The arena is behind a spin lock because a free list is a mutable structure. The plane is not:
/// a version is one aligned word, generated code only ever reads it, and the allocator only ever
/// writes it while it owns the range. So a check reads this and never touches the arena's lock, which is
/// what stops the monitor serialising every access in a threaded program on a lock that is only
/// there for the free lists.
#[derive(Clone, Copy, Debug)]
pub struct Region {
    /// The lifetime plane over the region.
    pub plane: Lifetime,
    /// The type plane over the same region, with its side table.
    ///
    /// Every watched region has one, including an adopted one, so that a check never has to ask
    /// whether the plane it is about to read is there. A region whose type plane could not be
    /// mapped is not watched at all, which is the state every address outside the heap is in and
    /// is a gap rather than a wrong answer.
    pub types: Types<'static>,
    /// The init plane over the same region, a bit for every byte of it.
    ///
    /// Mapped for every watched region for the reason the type plane is, and it costs less: an
    /// eighth of the region rather than a byte per byte. A region whose init plane could not be
    /// mapped is not watched at all, which keeps every check's question answerable without asking
    /// first whether there is a plane to ask.
    pub init: Init,
    /// The lowest address in the region.
    pub base: usize,
    /// One past the highest.
    pub end: usize,
    /// Document 04's storage class, as the allocator that owns the region described it.
    ///
    /// Held rather than acted on. The plane says who owns a granule and the class says what kind
    /// of storage it is, and nothing this milestone judges asks the second question. Section
    /// 10.2's summary does: a build that watches two heaps and one mapping has a different
    /// guarantee from one that watches three heaps, and the count that says so has to come from
    /// somewhere.
    pub class: u32,
}

impl Region {
    /// Whether `addr` is one this arena is responsible for.
    ///
    /// A pointer to a local, to a global, or to memory some other allocator handed out is not,
    /// and there is no plane covering it to ask. The checks let those through, which is the
    /// honest answer for a milestone that instruments the heap and nothing else.
    #[must_use]
    pub const fn holds(&self, addr: usize) -> bool {
        addr >= self.base && addr < self.end
    }
}

/// How many regions the monitor can watch at once.
///
/// One of them is this allocator's own reservation and the rest are for document 10 section 10.4's
/// adopted arenas, which is jemalloc or tcmalloc or a pool somebody wrote for one program telling
/// the monitor about storage it obtained from the OS itself. A fixed number rather than a growing
/// list because the table is read by every check and written by almost nothing, so the cost that
/// matters is the read, and a fixed array is a bounded walk over memory that is already there.
///
/// Eight is a guess with room in it. A program with more allocators than that is a real thing, and
/// what it gets is the ninth region going unwatched, which is the state every address outside the
/// heap is in already. It is a gap in what the build covers rather than a wrong answer about
/// memory, and section 10.2's summary is where a gap is supposed to be counted.
pub const REGIONS: usize = 8;

/// One region's three numbers, written once and read without a lock.
struct Slot {
    /// The bias its plane's arithmetic is built on.
    origin: AtomicUsize,
    /// The bias the type plane's arithmetic is built on.
    typing: AtomicUsize,
    /// The bias the init plane's arithmetic is built on.
    initing: AtomicUsize,
    /// The lowest address it covers.
    base: AtomicUsize,
    /// One past the highest.
    end: AtomicUsize,
    /// What kind of storage it is.
    class: AtomicU32,
}

impl Slot {
    /// An empty slot, which is what the whole table starts as.
    const fn empty() -> Self {
        Self {
            origin: AtomicUsize::new(0),
            typing: AtomicUsize::new(0),
            initing: AtomicUsize::new(0),
            base: AtomicUsize::new(0),
            end: AtomicUsize::new(0),
            class: AtomicU32::new(0),
        }
    }
}

/// Every region the monitor watches, in the order they were published.
static SPACE: [Slot; REGIONS] = [const { Slot::empty() }; REGIONS];

/// The side table of each region's type plane, in the same order.
///
/// Beside the table rather than in it because a [`Side`] holds a bump counter and is therefore not
/// `Copy`, while a [`Region`] is handed back by value to every check. A region's side table is the
/// one at its own index, so nothing has to be looked up: the index is what [`covering`] is already
/// walking.
static SIDES: [Side; REGIONS] = [const { Side::new() }; REGIONS];

/// How many of the slots have been filled in.
///
/// Release when it grows and acquire when it is read, which is what makes a reader that sees the
/// count see the three numbers that were written before it. Slots are filled in order and never
/// emptied, so a reader that sees a smaller count than the truth reads a prefix of the table and
/// misses a region rather than reading one that is half written.
static FILLED: AtomicUsize = AtomicUsize::new(0);

/// Keeps two threads from claiming the same slot.
///
/// A separate lock from the heap's, because adoption is not an allocation: a thread that is
/// telling the monitor about its own arena has no business waiting behind our free lists, and the
/// heap's lock is taken on a path that publishes a region itself.
static SPACING: AtomicBool = AtomicBool::new(false);

/// Everything the table holds about one region, so that publishing it is one argument.
///
/// A struct rather than eight parameters because five of the eight are addresses of the same type
/// and a call site that swapped two of them would compile and then read the wrong plane.
pub(crate) struct Watch {
    /// The bias the lifetime plane's arithmetic is built on.
    pub origin: usize,
    /// The bias the type plane's arithmetic is built on.
    pub typing: usize,
    /// The bias the init plane's arithmetic is built on.
    pub initing: usize,
    /// Where the type plane's side table starts.
    pub side: usize,
    /// How many entries that table holds.
    pub room: u32,
    /// The lowest address the region covers.
    pub base: usize,
    /// One past the highest.
    pub end: usize,
    /// Document 04's storage class, as the allocator that owns the region described it.
    pub class: u32,
}

/// Adds a region to the table, and says whether there was room.
///
/// False is a program with more than [`REGIONS`] arenas. Nothing here refuses anything over it:
/// the region is simply not watched, which is the same state every non heap address is already in
/// and is not a wrong answer about memory.
///
/// # Safety
///
/// `watch.side` names `watch.room * types::ENTRY` writable bytes that are never handed back, and
/// all three planes' biases name shadow that covers every byte between `watch.base` and
/// `watch.end` and is never handed back either.
pub(crate) unsafe fn publish(watch: Watch) -> bool {
    while SPACING.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err()
    {
        core::hint::spin_loop();
    }
    let at = FILLED.load(Ordering::Relaxed);
    let room = at < REGIONS;
    if room {
        let Watch { origin, typing, initing, side, room: entries, base, end, class } = watch;
        // Before the count grows, like the stores below, and for the same reason: a reader that
        // has acquired the count reads a side table that is already pointed at its mapping.
        //
        // SAFETY: the caller says the mapping is there and outlives the program, and this slot's
        // table has handed out nothing, because a slot is filled once and never emptied.
        unsafe { SIDES[at].map(side, entries) };
        SPACE[at].origin.store(origin, Ordering::Relaxed);
        SPACE[at].typing.store(typing, Ordering::Relaxed);
        SPACE[at].initing.store(initing, Ordering::Relaxed);
        SPACE[at].base.store(base, Ordering::Relaxed);
        SPACE[at].end.store(end, Ordering::Relaxed);
        SPACE[at].class.store(class, Ordering::Relaxed);
        // The release that publishes the stores above, and the reason a reader may load them
        // relaxed once it has acquired this.
        FILLED.store(at + 1, Ordering::Release);
    }
    SPACING.store(false, Ordering::Release);
    room
}

/// The region `addr` is in, or `None` when no watched region holds it.
///
/// `None` is a pointer to a local, to a global, to memory an allocator nobody told us about handed
/// out, or to nothing at all, and it is also the whole state of a program that has not allocated
/// yet. Every check passes in that state, which is not a hole: an address no plane covers is one
/// there is nothing to ask about, and reporting on it would be a false positive against a program
/// doing nothing wrong.
///
/// The walk is the length of the table and the table is nearly always one entry long, which is why
/// this is a loop over an array rather than anything cleverer. It is on the path of every check.
#[must_use]
pub fn covering(addr: usize) -> Option<Region> {
    let filled = FILLED.load(Ordering::Acquire);
    for (at, slot) in SPACE[..filled].iter().enumerate() {
        let base = slot.base.load(Ordering::Relaxed);
        let end = slot.end.load(Ordering::Relaxed);
        if addr >= base && addr < end {
            // SAFETY: a published region is mapped for as long as the program runs, along with the
            // shadow each origin names, so all three planes cover every address between the two
            // above.
            let plane = unsafe { Lifetime::new(slot.origin.load(Ordering::Relaxed)) };
            // SAFETY: as above, and the side table at this index was mapped before the count
            // that got us here grew.
            let types = unsafe { Types::new(slot.typing.load(Ordering::Relaxed), &SIDES[at]) };
            // SAFETY: as above.
            let init = unsafe { Init::new(slot.initing.load(Ordering::Relaxed)) };
            return Some(Region {
                plane,
                types,
                init,
                base,
                end,
                class: slot.class.load(Ordering::Relaxed),
            });
        }
    }
    None
}

/// How many regions are being watched, for the summary and for the tests.
#[must_use]
pub fn watched() -> usize {
    FILLED.load(Ordering::Acquire)
}

/// Whether any watched region has an address in common with `[lo, hi)`.
///
/// What it is for is section 10.4's adoption, where two planes over one address would be a
/// question with two answers and the one that got there first would decide.
pub(crate) fn overlaps(lo: usize, hi: usize) -> bool {
    let filled = FILLED.load(Ordering::Acquire);
    SPACE[..filled]
        .iter()
        .any(|slot| lo < slot.end.load(Ordering::Relaxed) && slot.base.load(Ordering::Relaxed) < hi)
}

/// Maps the shadows and the region as one reservation, and builds the arena over it.
///
/// The shadows come first so that the region's base is the highest of the five spans, which makes
/// each bias `shadow - region / granule * slot` and makes it a subtraction that a reader can
/// check. A bias may still wrap, and [`Lifetime`] says so and does its arithmetic modularly.
///
/// The order is the lifetime plane, the type plane, the type plane's side table, the init plane,
/// and then the region. That is a quarter of a byte per byte for the first, half for each of the
/// next two and an eighth for the last, so the reservation is a little under two and a half times
/// what the program can allocate out of it. It is address space rather than memory: the mapping is
/// anonymous, and a plane a program never touches never costs it a page.
///
/// Called for the first allocation and again whenever every arena is out of room, so a program
/// that needs eight gibibytes gets them a gibibyte at a time and a program that needs a kilobyte
/// never maps the second.
///
/// `want` is the least the region has to be, which is [`REGION`] for ordinary growth and the size
/// of one block for an allocation too large to fit in that. Nothing here depends on the length
/// being the same twice, and a region reserved for one enormous instance is an ordinary arena
/// afterwards that serves ordinary allocations out of what is left.
fn reserve(want: usize) -> Option<Arena> {
    let len = want.checked_next_multiple_of(PAGE)?;
    let under = shadow(len);
    let typed = typing(len);
    let sided = siding(len);
    let inited = initing(len);
    let planes = under.checked_add(typed)?.checked_add(sided)?.checked_add(inited)?;
    let base = map(planes.checked_add(len)?)?;
    let region = base + planes;
    let origin = base.wrapping_sub(region / GRANULE * SLOT);
    let typing = (base + under).wrapping_sub(region / types::GRANULE * types::SLOT);
    let initing = (base + under + typed + sided).wrapping_sub(region / crate::init::SPAN);
    // Published before the arena is handed back, so that the first instance the arena creates is
    // already visible to a check by the time anything could hold a pointer to it.
    //
    // A table with no room is a program that has adopted arenas of its own through section 10.4
    // until there are none left. The mapping is dropped on the floor rather than handed back,
    // because there is no arena to hand back: an arena nothing watches is storage that every check
    // passes, which is worse than the null this returns. Nothing was touched, so what is lost is
    // address space and no pages.
    let watch = Watch {
        origin,
        typing,
        initing,
        side: base + under + typed,
        room: (sided / types::ENTRY) as u32,
        base: region,
        end: region + len,
        class: Class::Allocated as u32,
    };
    // SAFETY: the mapping above is writable and is never handed back, the side table is the span
    // between the type plane and the init plane and holds exactly the entries named here, and all
    // three biases were solved from the same `region` the bounds are written in.
    if !unsafe { publish(watch) } {
        return None;
    }
    // SAFETY: the mapping is readable, writable, private and anonymous, so it is zero filled and
    // owned by this process alone, and it is never unmapped, so it outlives everything built over
    // it. The region is page aligned and therefore granule aligned, the shadow covers exactly the
    // region's granules by the arithmetic above, and `IDENTITY` belongs to this arena alone.
    Some(unsafe { Arena::new(Lifetime::new(origin), region, len, IDENTITY) })
}

/// Asks the operating system for `len` bytes of zeroed, private address space.
///
/// Zero when it refuses, which is what makes an allocation fail rather than what makes the
/// program stop: a `malloc` that cannot get memory returns null, and that is true of this one for
/// the same reason it is true of everyone else's.
pub(crate) fn map(len: usize) -> Option<usize> {
    const READ_WRITE: i32 = 1 | 2;
    // The one number that is not the same everywhere, which is why it is spelled out rather than
    // taken from a header this crate cannot include. Linux is the odd one out: the BSDs and macOS
    // all agree on 0x1000 for the anonymous flag and Linux picked 0x20.
    #[cfg(target_os = "linux")]
    const PRIVATE_ANONYMOUS: i32 = 0x0002 | 0x0020;
    #[cfg(not(target_os = "linux"))]
    const PRIVATE_ANONYMOUS: i32 = 0x0002 | 0x1000;

    unsafe extern "C" {
        fn mmap(
            addr: *mut c_void,
            len: usize,
            prot: i32,
            flags: i32,
            fd: i32,
            off: i64,
        ) -> *mut c_void;
    }
    // SAFETY: a null hint asks the kernel to choose, which is always allowed, and the file
    // descriptor is ignored for an anonymous mapping.
    let at = unsafe { mmap(core::ptr::null_mut(), len, READ_WRITE, PRIVATE_ANONYMOUS, -1, 0) };
    // `MAP_FAILED` is `(void *) -1` rather than null, which is the one place this interface is
    // not the obvious one.
    match at as isize {
        -1 => None,
        _ => Some(at as usize),
    }
}

/// `malloc`: judgement J4, then the address of the payload.
///
/// A request for zero bytes gets a distinct address of a real instance rather than null, which is
/// what C23 permits and what makes the result something `free` accepts. Returning null would mean
/// a program that checks for it treating a successful allocation as a failure.
pub fn alloc(size: usize) -> *mut c_void {
    match HEAP.allocate(size) {
        0 => core::ptr::null_mut(),
        payload => {
            begun(payload, Arena::sized(size));
            payload as *mut c_void
        }
    }
}

/// The other half of judgement J4: a fresh instance has no effective type and nothing has written
/// it.
///
/// Two planes and one region lookup, because the two facts are the same fact said twice. C says
/// allocated storage has no declared type and takes its type from the first store, so the type
/// plane has to forget what the previous occupant of these bytes was: without that a block handed
/// out again would still say what it said last time, and the first honest read of it would be
/// reported as type confusion, which is the false positive that would make the plane unusable. The
/// init plane says the same thing from the other side. The bytes hold whatever the previous
/// occupant left, so a read of one before the program has stored anything there is document 03's
/// Y6, and an instance beginning is the only moment anything knows a range has become storage.
///
/// It is the whole block rather than the request. The bytes between the request and the end of the
/// block are the allocator's rounding, they share granules with the request, and leaving them
/// saying what they said before would turn an overflow inside a block into a type report instead of
/// the bounds report it is.
fn begun(payload: usize, block: usize) {
    let Some(region) = covering(payload) else { return };
    let len = block.min(region.end - payload);
    // SAFETY: the range starts inside the region and is clipped to it, so the type plane covers
    // every granule of it.
    unsafe { region.types.set(payload, len, types::UNTYPED) }
    // SAFETY: the same range, which the init plane covers for the same reason.
    unsafe { region.init.forget(payload, len) }
}

/// What a fill or a copy the allocator itself performed leaves behind: those bytes hold what it
/// wrote.
///
/// The allocator writes the program's storage in two places, and both of them are writes the
/// program is entitled to read back. `calloc` zeroes what was asked for and `realloc` carries the
/// old contents across, and without this the init plane would have just called both of those ranges
/// unwritten and refused the read the program was about to make.
///
/// `src` is where the bytes came from when they came from somewhere, which is `realloc`, and the
/// answer travels with them: a structure whose padding the program never filled is still a
/// structure whose padding nothing filled after it has moved. A source in another region is marked
/// as written rather than looked up, which is the same thinning `crate::check::carry` does for the
/// type plane and is a lost check rather than a wrong answer.
fn wrote(payload: usize, src: Option<usize>, len: usize) {
    let Some(region) = covering(payload) else { return };
    let len = len.min(region.end - payload);
    if let Some(src) = src {
        if region.holds(src) && region.holds(src.wrapping_add(len.saturating_sub(1))) {
            // SAFETY: both ranges are inside the region, whose init plane covers every byte of it.
            unsafe { region.init.copy(payload, src, len) };
            return;
        }
    }
    // SAFETY: the range starts inside the region and is clipped to it.
    unsafe { region.init.set(payload, len) }
}

/// `free`: judgement J6, and then judgement J5.
///
/// Freeing null is a no-op, as it has been since C89.
///
/// A pointer this arena did not hand out is refused rather than passed through to the C library's
/// `free`. Passing it through is the tempting thing, because it would let a program that mixes
/// instrumented and uninstrumented objects work, and it is wrong here: this build has interposed
/// `malloc`, so every pointer that a correct program frees came from this arena, and one that did
/// not is either the bug being looked for or a call the C library made to its own allocator
/// through a name S3 has not interposed yet. Reporting is right for the first and is a known
/// false positive for the second, which is why the milestone's test suite is written against
/// these four functions.
///
/// # Safety
///
/// `ptr` is null or an address the program believes came from this allocator. Nothing else is
/// required: whether it actually did is the judgement, and a pointer to somewhere else entirely
/// is refused by the region check before anything behind it is read.
pub unsafe fn dealloc(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    let payload = ptr as usize;
    let ended = HEAP.owning(payload, |arena| {
        // SAFETY: the address is inside the region, which is what `end` asks for. Everything else
        // about it is the judgement rather than a precondition.
        unsafe { arena.end(payload) }.is_ok()
    });
    // `None` is a free before anything was ever allocated, which is the same judgement: whatever
    // that pointer is, it is not one of ours.
    if ended != Some(true) {
        // Unless somebody said whose it is. An allocator that tags its instances through section
        // 10.4's API turns the vaguest refusal this crate produces into the specific one, which is
        // document 03's free by the wrong deallocator rather than a free of something unknown.
        if let Some(id) = crate::adopt::deallocator(payload) {
            if u64::from(id) != IDENTITY {
                crate::fail::refused_at(
                    Judgement::Free,
                    "free, of a pointer another allocator handed out",
                    payload,
                );
            }
        }
        crate::fail::refused(Judgement::Free);
    }
}

/// `calloc`: `malloc` of the product, and then zeroed.
///
/// The multiplication is checked, which is the point of `calloc` existing at all: `malloc(n * m)`
/// with an overflowing product is one of the oldest heap overflows there is, and the whole reason
/// the two argument form is in the standard.
pub fn alloc_zeroed(count: usize, size: usize) -> *mut c_void {
    let Some(bytes) = count.checked_mul(size) else {
        return core::ptr::null_mut();
    };
    let payload = alloc(bytes);
    if !payload.is_null() {
        // SAFETY: the arena just handed out an instance of at least `bytes` bytes at this address
        // and nothing else has a pointer to it yet.
        unsafe { core::ptr::write_bytes(payload as *mut u8, 0, bytes) };
        // The zeroing is a store, and the whole point of `calloc` is that the program may read it
        // back. The rounding past the request is left as the fresh storage it is.
        wrote(payload as usize, None, bytes);
    }
    payload
}

/// `realloc`: a new instance, the old contents, and the old instance ended.
///
/// Always a copy, never a resize in place, even when the old block is large enough. That is the
/// expensive answer and it is the only correct one here: growing an instance in place would mean
/// the same version covering granules that were somebody else's a moment ago, and shrinking one
/// in place would leave a live capability naming bytes the program has given back. A resize is
/// two instances by definition, and the plane is what says so.
///
/// # Safety
///
/// As [`dealloc`].
pub unsafe fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void {
    if ptr.is_null() {
        return alloc(size);
    }
    if size == 0 {
        // SAFETY: the caller's contract passed straight on.
        unsafe { dealloc(ptr) };
        return core::ptr::null_mut();
    }
    let payload = ptr as usize;
    let old = HEAP.owning(payload, |arena| {
        // SAFETY: the address is inside the region, which is what `extent` asks for.
        unsafe { arena.extent(payload) }
    });
    let Some(Ok(old)) = old else { crate::fail::refused(Judgement::Free) };

    let fresh = alloc(size);
    if fresh.is_null() {
        // The old instance is still live, which is what the standard requires of a `realloc` that
        // could not get memory. Ending it here would turn an allocation failure into a leak of
        // the program's data and a use after free of whatever it does next.
        return fresh;
    }
    // SAFETY: both are live instances of this arena, of at least `old` and `size` bytes, and they
    // do not overlap because the fresh one is not the old one.
    unsafe { core::ptr::copy_nonoverlapping(ptr as *const u8, fresh as *mut u8, old.min(size)) };
    // The copy carries the old instance's answers, which is read before the old instance is ended
    // rather than after, since ending it is what makes those bytes somebody else's to hand out.
    wrote(fresh as usize, Some(payload), old.min(size));
    // SAFETY: as above, and the copy is done with it.
    unsafe { dealloc(ptr) };
    fresh
}

/// `malloc_usable_size`: how much of the instance at `ptr` is the program's to write.
///
/// The class size rather than what was asked for, which is more, and answering with the smaller
/// number would be answering a different question than the one the name asks. A program that
/// rounds a request up to this and then uses all of it is using storage the arena gave it and the
/// plane agrees it owns, so the monitor lets it through, which is the whole point of the call.
///
/// This is not in the standard and it is here because leaving it out is worse than a gap. Without
/// it the call resolves to the C library's, which reads a chunk header this arena never wrote and
/// returns whatever the bytes in front of the payload happen to be, and the caller believes it.
/// SQLite's page cache is one of the callers: it divides the answer by a slot size and carves
/// that many slots out of one allocation, so a number that is too large by any amount is a heap
/// overflow with nothing in the program to blame for it.
///
/// Zero for anything this arena did not hand out, and for an instance that is over. A size is a
/// question rather than an access, so refusing it would report a judgement against a program that
/// has not touched anything yet, and a program that mixes this allocator with another one asks it
/// in good faith. Zero is the answer that makes a caller ask for nothing rather than trust a
/// number the monitor invented, and if it goes on to use the bytes anyway the access is where the
/// judgement belongs.
///
/// # Safety
///
/// As [`dealloc`].
#[must_use]
pub unsafe fn usable(ptr: *mut c_void) -> usize {
    if ptr.is_null() {
        return 0;
    }
    let payload = ptr as usize;
    let size = HEAP.owning(payload, |arena| {
        // SAFETY: the address is inside the region, which is what `extent` asks for.
        unsafe { arena.extent(payload) }
    });
    match size {
        Some(Ok(size)) => size,
        _ => 0,
    }
}

/// The five names a hosted program actually links.
///
/// Only in a real build. Under `cargo test` this crate is linked into a test binary that has a
/// standard library, and a `malloc` defined here would be the one that standard library called,
/// which would make the test harness allocate out of the arena the tests are testing. The logic
/// is in the plain functions above and the tests call those, which is the same arrangement the
/// panic handler in `lib.rs` is already under.
#[cfg(not(test))]
pub mod exports {
    use core::ffi::c_void;

    /// # Safety
    ///
    /// This is `malloc`.
    #[unsafe(no_mangle)]
    pub extern "C" fn malloc(size: usize) -> *mut c_void {
        super::alloc(size)
    }

    /// # Safety
    ///
    /// This is `free`, so `ptr` is null or something the program believes it allocated.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn free(ptr: *mut c_void) {
        // SAFETY: the caller's contract passed straight on.
        unsafe { super::dealloc(ptr) }
    }

    /// # Safety
    ///
    /// This is `calloc`.
    #[unsafe(no_mangle)]
    pub extern "C" fn calloc(count: usize, size: usize) -> *mut c_void {
        super::alloc_zeroed(count, size)
    }

    /// # Safety
    ///
    /// This is `realloc`, so `ptr` is null or something the program believes it allocated.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void {
        // SAFETY: the caller's contract passed straight on.
        unsafe { super::realloc(ptr, size) }
    }

    /// # Safety
    ///
    /// This is `malloc_usable_size`, so `ptr` is null or something the program believes it
    /// allocated.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn malloc_usable_size(ptr: *mut c_void) -> usize {
        // SAFETY: the caller's contract passed straight on.
        unsafe { super::usable(ptr) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plane::DEAD;
    use crate::turnstile::turn;

    /// Reads the byte at `offset` of an instance.
    fn peek(ptr: *mut c_void, offset: usize) -> u8 {
        // SAFETY: every call below is inside an instance the arena handed out.
        unsafe { (ptr as *const u8).add(offset).read() }
    }

    /// Writes `byte` at `offset` of an instance.
    fn poke(ptr: *mut c_void, offset: usize, byte: u8) {
        // SAFETY: as above.
        unsafe { (ptr as *mut u8).add(offset).write(byte) };
    }

    /// What the plane says about an address, which is what a check would compare against.
    fn version(ptr: *mut c_void) -> u64 {
        HEAP.owning(ptr as usize, |arena| {
            // SAFETY: the address came out of this arena, so it is inside its region.
            unsafe { arena.version(ptr as usize) }
        })
        .expect("the address came out of an arena of ours")
    }

    /// Whether the init plane says every byte of a run of an instance has been written.
    fn written(ptr: *mut c_void, offset: usize, len: usize) -> bool {
        let region = covering(ptr as usize).expect("the address came out of a region of ours");
        // SAFETY: the run is inside an instance this arena handed out, so the plane covers it.
        unsafe { region.init.allows(ptr as usize + offset, len) }
    }

    #[test]
    fn what_it_hands_out_is_writable_end_to_end_and_the_neighbours_are_not_disturbed() {
        let _turn = turn();
        // The first test that goes all the way from a reservation the kernel made to a byte the
        // program wrote, which is what makes the rest of this file more than arithmetic.
        let first = alloc(64);
        let second = alloc(64);
        assert!(!first.is_null() && !second.is_null());
        assert_ne!(first, second);

        for offset in 0..64 {
            poke(first, offset, 0x11);
            poke(second, offset, 0x22);
        }
        for offset in 0..64 {
            assert_eq!(peek(first, offset), 0x11);
            assert_eq!(peek(second, offset), 0x22);
        }

        let held = version(second);
        // SAFETY: `first` came out of `alloc` above and has not been freed.
        unsafe { dealloc(first) };
        assert_eq!(version(second), held, "freeing the neighbour ended this one too");
        // SAFETY: as above.
        unsafe { dealloc(second) };
    }

    #[test]
    fn a_pointer_to_a_freed_instance_is_refused_after_the_address_comes_back() {
        let _turn = turn();
        // The property the whole milestone is for, through the C entry points this time.
        let ptr = alloc(128);
        let held = version(ptr);
        // SAFETY: `ptr` came out of `alloc` and has not been freed.
        unsafe { dealloc(ptr) };
        assert_ne!(version(ptr), held);

        let again = alloc(128);
        assert_eq!(again, ptr, "the free list did not hand the address back");
        assert_ne!(version(again), held, "the reused address kept the old version");
        // SAFETY: `again` is live.
        unsafe { dealloc(again) };
    }

    #[test]
    fn freeing_null_is_allowed_and_does_nothing() {
        // C89, and a real program relies on it every time it frees a struct it half built.
        // SAFETY: null is what this function documents as the one thing it always accepts.
        unsafe { dealloc(core::ptr::null_mut()) };
    }

    #[test]
    fn zero_bytes_is_an_instance_rather_than_a_failure() {
        let _turn = turn();
        // `malloc(0)` returning null would have a program that checks the result treat a
        // successful allocation as an out of memory, and would give `free` an address it refuses.
        let ptr = alloc(0);
        assert!(!ptr.is_null());
        assert_ne!(version(ptr), DEAD);
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn calloc_zeroes_storage_that_was_used_before_and_refuses_a_product_that_overflows() {
        let _turn = turn();
        // The zeroing is the part a reused block makes interesting: the memory an instance is
        // given may be an instance that was written all over.
        let dirty = alloc(64);
        for offset in 0..64 {
            poke(dirty, offset, 0xFF);
        }
        // SAFETY: `dirty` is a live instance.
        unsafe { dealloc(dirty) };

        let clean = alloc_zeroed(8, 8);
        assert_eq!(clean, dirty, "the free list did not hand the address back");
        for offset in 0..64 {
            assert_eq!(peek(clean, offset), 0, "calloc handed back what the last owner wrote");
        }
        // SAFETY: `clean` is a live instance.
        unsafe { dealloc(clean) };

        // The overflow is the reason `calloc` takes two arguments rather than one.
        assert!(alloc_zeroed(usize::MAX, 2).is_null());
        assert!(alloc_zeroed(2, usize::MAX).is_null());
    }

    #[test]
    fn realloc_keeps_the_contents_and_ends_the_instance_it_moved_from() {
        let _turn = turn();
        let old = alloc(32);
        for offset in 0..32 {
            poke(old, offset, 0x5A);
        }
        let held = version(old);

        // SAFETY: `old` is a live instance of this allocator.
        let new = unsafe { realloc(old, 256) };
        assert!(!new.is_null());
        assert_ne!(new, old, "a resize is two instances and this one stayed put");
        for offset in 0..32 {
            assert_eq!(peek(new, offset), 0x5A, "the copy lost a byte");
        }
        assert_ne!(version(old), held, "the instance that was moved from is still live");

        // Shrinking keeps as much as still fits, and no more is promised.
        // SAFETY: `new` is a live instance.
        let small = unsafe { realloc(new, 16) };
        for offset in 0..16 {
            assert_eq!(peek(small, offset), 0x5A);
        }
        // SAFETY: `small` is a live instance.
        unsafe { dealloc(small) };
    }

    #[test]
    fn realloc_of_null_is_malloc_and_realloc_to_nothing_is_free() {
        let _turn = turn();
        // Both are in the standard and both are written by real programs, usually inside a
        // grow-this-buffer helper that starts with a null pointer and a length of zero.
        // SAFETY: null is the one argument this function always accepts.
        let ptr = unsafe { realloc(core::ptr::null_mut(), 64) };
        assert!(!ptr.is_null());
        let held = version(ptr);

        // SAFETY: `ptr` is a live instance.
        let gone = unsafe { realloc(ptr, 0) };
        assert!(gone.is_null());
        assert_ne!(version(ptr), held, "the instance is still live after a resize to nothing");
    }

    #[test]
    fn a_fresh_instance_says_nothing_has_written_its_bytes() {
        let _turn = turn();
        // The other half of judgement J4, and the reason the init plane is mapped here rather than
        // left to the compiler. A block handed out again holds what the last owner wrote, so a
        // read of one of those bytes before this owner has stored anything is document 03's Y6,
        // and an instance beginning is the only moment anything knows a range became storage.
        let ptr = alloc(64);
        assert!(!written(ptr, 0, 64));

        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
        let again = alloc(64);
        assert_eq!(again, ptr, "the free list did not hand the address back");
        assert!(!written(again, 0, 64), "a reused block came back saying it had been written");
        // SAFETY: `again` is a live instance.
        unsafe { dealloc(again) };
    }

    #[test]
    fn calloc_says_it_wrote_what_it_zeroed_and_no_more() {
        let _turn = turn();
        // The zeroing is a store the program is entitled to read back, and without the judgement
        // beside it the first read of a `calloc` would be refused on a program doing nothing
        // wrong. The rounding past the request is not part of that bargain: it stays the fresh
        // storage it is, so a write that overruns the request is still something a plane can see.
        let ptr = alloc_zeroed(8, 8);
        assert!(!ptr.is_null());
        assert!(written(ptr, 0, 64));

        // SAFETY: `ptr` came out of this allocator and is live.
        let room = unsafe { usable(ptr) };
        assert!(room >= 64);
        if room > 64 {
            assert!(!written(ptr, 64, room - 64), "the allocator's rounding was called written");
        }
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn realloc_carries_the_answers_of_the_bytes_it_moved() {
        let _turn = turn();
        // A resize is two instances, so the fresh one starts with nothing written and the bytes
        // the copy brought across have to arrive saying what they said where they came from.
        // Otherwise every program that grows a buffer is refused on the first read after it grew.
        let old = alloc_zeroed(8, 8);
        assert!(written(old, 0, 64));

        // SAFETY: `old` is a live instance of this allocator.
        let new = unsafe { realloc(old, 256) };
        assert!(!new.is_null());
        assert!(written(new, 0, 64), "the copy lost what the bytes it read said");
        assert!(!written(new, 64, 192), "the storage the growth added was called written");
        // SAFETY: `new` is a live instance.
        unsafe { dealloc(new) };
    }

    #[test]
    fn the_bias_between_the_region_and_its_shadow_covers_every_granule() {
        let _turn = turn();
        // The one piece of arithmetic in this file that nothing else would catch. If the two were
        // mapped separately the bias would depend on which pair of addresses the kernel returned,
        // which is why they are one mapping.
        let low = alloc(16);
        let high = alloc(1 << 20);
        assert_ne!(version(low), DEAD);
        assert_ne!(version(high), DEAD);
        assert_ne!(version(low), version(high));
        // SAFETY: both are live instances.
        unsafe {
            dealloc(low);
            dealloc(high);
        }
    }

    #[test]
    fn an_address_no_watched_region_holds_is_nobodys() {
        let _turn = turn();
        // The table is what a check walks, and what it says about an address outside every region
        // has to be nothing at all. A local is the ordinary case: instrumented code accesses one
        // on every line, no plane covers it, and an answer other than `None` here would be a
        // report about a program doing nothing wrong.
        let ptr = alloc(16);
        assert!(covering(ptr as usize).is_some());
        assert!(watched() >= 1);

        let local = 0_u64;
        assert!(covering(&raw const local as usize).is_none());
        assert!(covering(0).is_none());

        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn what_a_check_reads_without_the_lock_is_what_the_allocator_wrote_under_it() {
        let _turn = turn();
        // The two paths to the plane have to agree or the monitor reports on the wrong memory.
        // One of them holds the arena's lock and the other holds nothing, which is the whole
        // point, so this is the only place the two answers are put beside each other.
        let ptr = alloc(64);
        let region = covering(ptr as usize).expect("the allocation above is inside a region");
        assert!(!region.holds(region.end));
        // SAFETY: the address is inside the region, so the plane covers it.
        assert_eq!(unsafe { region.plane.version(ptr as usize) }, version(ptr));

        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
        // SAFETY: as above.
        assert_eq!(unsafe { region.plane.version(ptr as usize) }, version(ptr));
    }

    #[test]
    fn a_request_the_first_region_cannot_hold_takes_a_second_one() {
        let _turn = turn();
        // A region is a fixed size and the heap used to have exactly one of them, so a program
        // whose live set passed that size got a null from `malloc` and died with a message about
        // our arena rather than anything about itself. SQLite's in-memory VFS does exactly that:
        // it holds a whole database in one buffer and grows it by reallocating, so the region
        // fills in a staircase that no arrangement of size classes gets around.
        let before = watched();
        // Two of these cannot share a region. A block is the payload, the aux beside it and a
        // header, which is a little over three times what was asked for, so a quarter of a region
        // twice over is more than a whole one.
        let quarter = REGION / 4;
        let first = alloc(quarter);
        let second = alloc(quarter);
        assert!(!first.is_null() && !second.is_null());
        assert!(watched() > before, "the second allocation did not take a new region");

        let here = covering(first as usize).expect("the first came out of a region");
        let there = covering(second as usize).expect("the second came out of a region");
        assert_ne!(here.base, there.base, "two blocks this size came out of one region");

        // The point of a second region is that it is memory, and a plane over it, rather than a
        // row in the table. The last byte because that is the one a short reservation would miss.
        poke(first, quarter - 1, 0x33);
        poke(second, quarter - 1, 0x44);
        assert_eq!(peek(first, quarter - 1), 0x33);
        assert_eq!(peek(second, quarter - 1), 0x44);
        assert_ne!(version(first), DEAD);
        assert_ne!(version(second), DEAD);
        // The two versions are not compared. Each arena numbers its own instances, so the first
        // block out of a fresh region carries the same number as the first block out of the one
        // before it, and that is what `plane::Counter` says it is for. A version only ever means
        // anything against the plane of the region it was written into, and which region that is
        // comes from the address, so two planes agreeing on a number is not an ambiguity.

        // SAFETY: both are live instances.
        unsafe {
            dealloc(first);
            dealloc(second);
        }
    }

    #[test]
    fn one_instance_larger_than_a_region_gets_a_region_of_its_own() {
        let _turn = turn();
        // What makes a region the size it is is that it is a reasonable amount of address space to
        // reserve for a heap nobody has measured, and not anything the arithmetic depends on, so a
        // single allocation past it is a reason to reserve more rather than a reason to refuse.
        // SQLite's spellfix tests ask for four hundred megabytes in one call and a block is a
        // little over three times what was asked for, so under a gibibyte region that request came
        // back null and the test reported an out of memory the program had not caused.
        let before = watched();
        let ptr = alloc(REGION);
        assert!(!ptr.is_null(), "an instance the size of a region was refused");
        assert!(watched() > before, "nothing was reserved for it");
        assert_ne!(version(ptr), DEAD);

        poke(ptr, REGION - 1, 0x55);
        assert_eq!(peek(ptr, REGION - 1), 0x55);
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_request_no_arithmetic_could_serve_is_refused_without_reserving_anything() {
        let _turn = turn();
        // A block is a header, an aux twice the size of the payload and the payload, so a request
        // past a quarter of the address space has no block at all and there is nothing to reserve
        // for it. That is the one size this refuses on sight, and it refuses it before the map so
        // that a request nobody could serve does not cost a region.
        let before = watched();
        let ptr = alloc(usize::MAX / 2);
        assert!(ptr.is_null(), "a request larger than the address space was served");
        assert_eq!(watched(), before, "a refused request reserved a region anyway");
    }

    #[test]
    fn the_usable_size_of_an_instance_is_every_byte_the_class_gave_it() {
        let _turn = turn();
        // A caller of this asks so that it can use the answer, and the answer has to be a number
        // the monitor will then stand behind. So the last byte of it gets written and read back,
        // which is the assertion that matters: a size that reads correctly and refuses at the end
        // would be worse than no answer at all.
        let ptr = alloc(100);
        assert!(!ptr.is_null());
        // SAFETY: `ptr` is a live instance of this arena's.
        let room = unsafe { usable(ptr) };
        assert!(room >= 100, "a hundred bytes were asked for and {room} came back");

        poke(ptr, room - 1, 0x66);
        assert_eq!(peek(ptr, room - 1), 0x66);
        assert_ne!(version(ptr), DEAD);
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn nothing_is_usable_through_a_pointer_this_arena_did_not_hand_out() {
        let _turn = turn();
        // Null because that is what the C library answers, an instance that is over because the
        // bytes behind it are nobody's until they are handed out again, and a local because a
        // pointer from somewhere else entirely has no header of ours in front of it and reading
        // one would be the monitor committing the bug it exists to catch.
        // SAFETY: null is the one argument this always has an answer for.
        assert_eq!(unsafe { usable(core::ptr::null_mut()) }, 0);

        let ptr = alloc(64);
        assert!(!ptr.is_null());
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
        // SAFETY: the pointer is one this arena handed out, which is all this asks.
        assert_eq!(unsafe { usable(ptr) }, 0, "an instance that is over reported room in it");

        let local = 0_u64;
        let outside = core::ptr::addr_of!(local) as *mut c_void;
        // SAFETY: the address is a live local, and the region check is what rules it out before
        // anything in front of it is read.
        assert_eq!(unsafe { usable(outside) }, 0, "a local reported room in the heap");
    }
}
