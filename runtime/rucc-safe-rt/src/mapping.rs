//! The mapping group of the interposition table.
//!
//! Design: `spec/safe-memory/10-boundaries.md` section 10.4, and row T6 of document 03.
//!
//! A mapping is a storage instance the same way an allocation is. It begins at `mmap`, it ends at
//! `munmap`, and a pointer into it that outlives the `munmap` is the same bug as a pointer into a
//! block that outlives its `free`. What made it a gap is that the kernel chooses where a mapping
//! goes, so the address lands somewhere no plane covers, and the unmapping is a syscall nothing was
//! told about. The read that follows faults if the program is lucky and reads whatever was mapped
//! there since if it is not.
//!
//! # One arena, chosen by us
//!
//! The answer is to choose the address ourselves. The first watched `mmap` reserves one stretch of
//! address space with no access and no backing, and adopts it as a mapped region through
//! [`crate::adopt::adopt`], so that it has the four planes every other watched region has. Every
//! mapping after that is placed inside it with `MAP_FIXED`, keeping the protection, the flags, the
//! file and the offset the program asked for, and begins an instance there with a version no
//! counter has returned before. The bytes are initialized from the start, because a mapping is
//! zeros or the file, never whatever was there before.
//!
//! `munmap` does not hand the range back to the kernel. It maps a fresh reservation over it, which
//! drops the pages exactly as an unmapping would, and ends the instance on the plane, so the next
//! access through an old pointer is refused as J1 by the check that was already going to run. The
//! range stays ours, which is what stops the kernel from putting somebody else's mapping there
//! while old pointers still name it.
//!
//! The arena is handed out next fit, so a range that was just unmapped is the last one to be used
//! again. That is the quarantine document 08 section 8.3 asks for, and it costs nothing to keep:
//! the cursor goes all the way round before it comes back.
//!
//! # What is left alone
//!
//! A request with an address in it is the program saying where, and the kernel's answer to that is
//! the program's business. `MAP_FIXED` and `MAP_FIXED_NOREPLACE` go straight through, and so does a
//! plain hint, since a program that passes one usually wants the address it passed. So does
//! anything that has to live at a particular kind of address: `MAP_32BIT`, `MAP_GROWSDOWN` and
//! `MAP_HUGETLB`. A request larger than the room the arena has left goes through as well. All of
//! these are mappings the monitor does not watch, which is where every mapping was before this.
//!
//! A `MAP_FIXED` that lands inside the arena is the one exception, because it lands on storage the
//! planes describe. Pages nobody owns there become a fresh instance, and pages the program already
//! owns keep their version. The second case is the usual one: a program reserves a large range with
//! no access and then maps pieces of it in as it needs them, and the pointer it holds into the
//! reservation has to go on working.
//!
//! # Partial unmaps and `mremap`
//!
//! Unmapping part of a mapping ends that part and leaves the rest owned by the same version, which
//! is what makes the over-map and trim idiom for aligned mappings work. `mremap` is the third row,
//! because a mapping that grows has to move, and the kernel would move it out of the arena. So a
//! growing `mremap` that may move is given a new place in the arena, the old range becomes a
//! reservation and its instance ends, and the new range is a new instance. A pointer into the old
//! range is then stale, which is what it is.
//!
//! # Adopted mappings
//!
//! An allocator that maps a chunk and adopts it through section 10.4 is handing the monitor a range
//! it already watches. So adoption of a range in the arena ends the mapping's instance there and
//! leaves the planes in place for the allocator to carve, and the arena remembers two things about
//! it: that its pages are the program's even where nobody owns them, and that the mapping's version
//! is what every pointer made from the `mmap` result carries. The second is what keeps the
//! allocator's own pointer into its arena from looking stale against the objects it carved there.
//!
//! # Everywhere else
//!
//! Linux only. The flags are Linux's numbers, `mremap` is Linux's call, and `MAP_NORESERVE` is what
//! keeps a reservation this size from costing anything. On other targets the rows call straight
//! through and watch nothing.

use core::ffi::{c_int, c_void};

use crate::interpose;

/// The C library's own, called once the arena has been arranged.
mod real {
    use core::ffi::{c_int, c_void};

    unsafe extern "C" {
        pub(super) fn mmap(
            at: *mut c_void,
            length: usize,
            protection: c_int,
            flags: c_int,
            fd: c_int,
            offset: i64,
        ) -> *mut c_void;
        pub(super) fn munmap(at: *mut c_void, length: usize) -> c_int;
        /// Variadic in C, with the new address as the one optional argument.
        #[cfg(target_os = "linux")]
        pub(super) fn mremap(
            old: *mut c_void,
            old_size: usize,
            new_size: usize,
            flags: c_int,
            ...
        ) -> *mut c_void;
    }
}

/// `MAP_FAILED`, which is `(void *) -1` rather than null.
const FAILED: *mut c_void = usize::MAX as *mut c_void;

interpose! {
    group: Allocation;

    /// `mmap`, placed in the arena and begun as an instance there.
    fn mmap(
        at: *mut c_void,
        length: usize,
        protection: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: the program's arguments, passed on with only the address chosen.
            unsafe { arena::map(at, length, protection, flags, fd, offset) }
        }
        #[cfg(not(target_os = "linux"))]
        {
            // SAFETY: the program's arguments, passed straight on.
            unsafe { real::mmap(at, length, protection, flags, fd, offset) }
        }
    }

    /// `mmap64`, which is the name the same function is exported under for large offsets.
    ///
    /// The `pread64` story again. glibc's header sends a program built with
    /// `_FILE_OFFSET_BITS=64` here even on a 64 bit target, and SQLite asks for that, so the row
    /// for `mmap` alone left every mapping SQLite made unwatched.
    fn mmap64(
        at: *mut c_void,
        length: usize,
        protection: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void {
        // SAFETY: as in `mmap`, which this is.
        unsafe { mmap(at, length, protection, flags, fd, offset) }
    }

    /// `munmap`, which ends the instance and keeps the range.
    fn munmap(at: *mut c_void, length: usize) -> c_int {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: as in `mmap`.
            unsafe { arena::unmap(at, length) }
        }
        #[cfg(not(target_os = "linux"))]
        {
            // SAFETY: the program's arguments, passed straight on.
            unsafe { real::munmap(at, length) }
        }
    }

    /// `mremap`, which moves a growing mapping somewhere else in the arena rather than out of it.
    ///
    /// The C declaration is variadic and the new address is only passed with `MREMAP_FIXED`. A
    /// call without it leaves the last argument as whatever was in the register, and nothing reads
    /// it unless the flag is set, which is the same rule the C library's own follows.
    fn mremap(
        old: *mut c_void,
        old_size: usize,
        new_size: usize,
        flags: c_int,
        new_address: *mut c_void,
    ) -> *mut c_void {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: as in `mmap`.
            unsafe { arena::remap(old, old_size, new_size, flags, new_address) }
        }
        #[cfg(not(target_os = "linux"))]
        {
            // There is no such call here, so there is no program calling it either.
            let _ = (old, old_size, new_size, flags, new_address);
            FAILED
        }
    }
}

/// Hands part of a mapping to an allocator that is adopting it, if the mapping is one of ours.
///
/// What `crate::adopt::adopt` asks before anything else. A program that maps a chunk and adopts it
/// is the usual way an allocator gets storage, and since the chunk is now inside the arena it is
/// already watched, so adopting it means ending the mapping there and letting the allocator carve.
#[must_use]
pub(crate) fn surrender(lo: usize, hi: usize) -> bool {
    #[cfg(target_os = "linux")]
    {
        arena::surrender(lo, hi)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (lo, hi);
        false
    }
}

/// Whether a capability's version names a mapping an allocator has since adopted.
///
/// Asked by `crate::check` before it calls a capability stale. A pointer made from what `mmap`
/// returned carries the mapping's version, and once the mapping is an allocator's arena that
/// pointer is how the allocator reaches its objects, so it is not a pointer that outlived anything.
/// It is checked the way a pointer the runtime never heard of is checked, which is that the bytes
/// it reaches are somebody's.
#[must_use]
pub(crate) fn surrendered(version: crate::plane::Version) -> bool {
    #[cfg(target_os = "linux")]
    {
        arena::surrendered(version)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = version;
        false
    }
}

/// The arena and the bookkeeping over it.
#[cfg(target_os = "linux")]
mod arena {
    use core::ffi::{c_int, c_void};
    use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    use super::{FAILED, real};
    use crate::alloc::{self, Region};
    use crate::layout::Class;
    use crate::plane::{self, Counter, Version};
    use crate::types;

    const NONE: c_int = 0;
    const PRIVATE: c_int = 0x02;
    const FIXED: c_int = 0x10;
    const ANONYMOUS: c_int = 0x20;
    const GROWSDOWN: c_int = 0x100;
    const NORESERVE: c_int = 0x4000;
    const HUGETLB: c_int = 0x40000;
    const FIXED_NOREPLACE: c_int = 0x10_0000;
    /// Only x86 has it, and elsewhere the same bit means something else.
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    const LOW: c_int = 0x40;
    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
    const LOW: c_int = 0;

    /// The flags that say where a mapping has to be, which the arena cannot promise.
    const PLACED: c_int = FIXED | FIXED_NOREPLACE | LOW | GROWSDOWN | HUGETLB;

    const MAYMOVE: c_int = 1;
    const MOVE_TO: c_int = 2;
    const DONTUNMAP: c_int = 4;

    /// How much address space the arena reserves, which is one region of the heap.
    const SIZE: usize = alloc::REGION;

    /// Held while the arena is being changed.
    static HELD: AtomicBool = AtomicBool::new(false);
    /// Whether the arena has been asked for, so that a failure is not retried on every call.
    static TRIED: AtomicBool = AtomicBool::new(false);
    /// Where the arena starts, or zero when there is none.
    static BASE: AtomicUsize = AtomicUsize::new(0);
    /// Where the next placement starts looking.
    static CURSOR: AtomicUsize = AtomicUsize::new(0);
    /// The page size, read once when the arena is made.
    static PAGE: AtomicUsize = AtomicUsize::new(0);
    /// Where the arena's versions come from.
    static VERSIONS: Counter = Counter::new();

    /// How many adopted mappings the arena can remember at once.
    const ADOPTED: usize = 64;

    /// A mapping the program has since adopted as an allocator's arena.
    ///
    /// Remembered for two reasons. Its storage is owned by nobody between the objects carved out
    /// of it, so without this [`place`] would see free pages there and map over them. And every
    /// pointer the program made from what `mmap` returned carries the mapping's version, which the
    /// plane no longer holds anywhere in the range, so without this every access to an object
    /// through the arena's own pointer would look like a pointer outliving its instance.
    struct Given {
        /// The first byte adopted, granule aligned.
        lo: AtomicUsize,
        /// One past the last, or zero for a slot nothing uses.
        hi: AtomicUsize,
        /// The version the mapping had.
        version: AtomicU64,
    }

    impl Given {
        /// An empty slot.
        const fn empty() -> Self {
            Self { lo: AtomicUsize::new(0), hi: AtomicUsize::new(0), version: AtomicU64::new(0) }
        }

        /// Makes the slot empty again.
        fn forget(&self) {
            self.hi.store(0, Ordering::Relaxed);
            self.version.store(0, Ordering::Relaxed);
            self.lo.store(0, Ordering::Relaxed);
        }
    }

    /// The adopted mappings. Written with the arena held and read without it.
    static GIVEN: [Given; ADOPTED] = [const { Given::empty() }; ADOPTED];

    /// Hands `[lo, hi)` of a mapping to an allocator that is adopting it.
    ///
    /// True when the range is in the arena and is now the allocator's to carve, which is the
    /// answer `crate::adopt::adopt` gives for it. False when it is not in the arena, or is not all
    /// one mapping, or there is no room to remember it, and then adoption goes on as it would
    /// have, which for a range in the arena is a refusal because it overlaps a watched region.
    pub(crate) fn surrender(lo: usize, hi: usize) -> bool {
        let Some(region) = watched() else { return false };
        if lo < region.base || hi > region.end || hi <= lo {
            return false;
        }
        locked(|| {
            // One run and no holes, so the two ends having one owned version says the whole range
            // has it.
            let held = version(&region, lo);
            if !plane::owned(held) || version(&region, hi - plane::GRANULE) != held {
                return false;
            }
            let Some(slot) = GIVEN.iter().find(|given| given.hi.load(Ordering::Relaxed) == 0)
            else {
                return false;
            };
            slot.lo.store(lo, Ordering::Relaxed);
            slot.version.store(held, Ordering::Relaxed);
            slot.hi.store(hi, Ordering::Relaxed);
            // SAFETY: the range is inside the region, granule aligned because adoption rounds it
            // so, and carries this version.
            unsafe { region.plane.end(lo, hi - lo, plane::ended(held)) };
            true
        })
    }

    /// Whether `version` belonged to a mapping an allocator has since adopted.
    pub(crate) fn surrendered(version: Version) -> bool {
        GIVEN.iter().any(|given| {
            given.hi.load(Ordering::Relaxed) != 0
                && given.version.load(Ordering::Relaxed) == version
        })
    }

    /// Runs `f` with the arena held.
    fn locked<T>(f: impl FnOnce() -> T) -> T {
        while HELD.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err()
        {
            core::hint::spin_loop();
        }
        let answer = f();
        HELD.store(false, Ordering::Release);
        answer
    }

    /// The arena's region, made the first time a mapping asks for it.
    ///
    /// Called with the arena held. `None` when it could not be made, which leaves every mapping
    /// unwatched for the rest of the run and is where they all were before this module.
    fn made() -> Option<Region> {
        if !TRIED.swap(true, Ordering::Relaxed) {
            unsafe extern "C" {
                fn getpagesize() -> c_int;
            }
            // SAFETY: no arguments and no preconditions.
            PAGE.store(unsafe { getpagesize() } as usize, Ordering::Relaxed);
            // SAFETY: a null address asks the kernel to choose, and the reservation is anonymous.
            let at = unsafe {
                real::mmap(
                    core::ptr::null_mut(),
                    SIZE,
                    NONE,
                    PRIVATE | ANONYMOUS | NORESERVE,
                    -1,
                    0,
                )
            };
            if at != FAILED {
                // SAFETY: the reservation is this module's and is never handed back.
                if unsafe { crate::adopt::adopt(at, SIZE, Class::Mapped as u32) } {
                    CURSOR.store(at as usize, Ordering::Relaxed);
                    BASE.store(at as usize, Ordering::Release);
                } else {
                    // SAFETY: the reservation was made just above and nothing has seen it.
                    unsafe { real::munmap(at, SIZE) };
                }
            }
        }
        watched()
    }

    /// The arena's region if there is one, without making it.
    fn watched() -> Option<Region> {
        match BASE.load(Ordering::Acquire) {
            0 => None,
            base => alloc::covering(base),
        }
    }

    /// `len` rounded up to whole pages.
    fn pages(len: usize) -> Option<usize> {
        len.checked_next_multiple_of(PAGE.load(Ordering::Relaxed))
    }

    /// The part of `[lo, hi)` the region covers, if any.
    fn inside(region: &Region, lo: usize, hi: usize) -> Option<(usize, usize)> {
        let (lo, hi) = (lo.max(region.base), hi.min(region.end));
        (lo < hi).then_some((lo, hi))
    }

    /// The version at `addr`.
    fn version(region: &Region, addr: usize) -> Version {
        // SAFETY: every caller has `addr` inside the region.
        unsafe { region.plane.version(addr) }
    }

    /// Calls `f` for every run of pages in `[lo, hi)` that carry one version.
    ///
    /// A page at a time, because everything this module begins is whole pages. An adopted mapping
    /// is the exception and [`runs_by`] is how that is walked.
    fn runs(region: &Region, lo: usize, hi: usize, f: impl FnMut(usize, usize, Version)) {
        runs_by(region, lo, hi, PAGE.load(Ordering::Relaxed), f);
    }

    /// [`runs`], a `step` at a time.
    fn runs_by(
        region: &Region,
        lo: usize,
        hi: usize,
        step: usize,
        mut f: impl FnMut(usize, usize, Version),
    ) {
        let mut at = lo;
        while at < hi {
            let held = version(region, at);
            let mut end = at + step;
            while end < hi && version(region, end) == held {
                end += step;
            }
            f(at, end, held);
            at = end;
        }
    }

    /// Maps a fresh reservation over `[lo, hi)`, dropping whatever was there.
    fn reserve(lo: usize, hi: usize) -> bool {
        let flags = PRIVATE | ANONYMOUS | NORESERVE | FIXED;
        // SAFETY: the range is inside the arena, which belongs to this module.
        unsafe { real::mmap(lo as *mut c_void, hi - lo, NONE, flags, -1, 0) != FAILED }
    }

    /// Judgement J4 over `[lo, hi)`: a new instance, with every byte of it initialized.
    fn begin(region: &Region, lo: usize, hi: usize) {
        let len = hi - lo;
        // SAFETY: the range is page aligned and inside the region, and the version is new.
        unsafe {
            region.plane.begin(lo, len, plane::begun(VERSIONS.next()));
            region.types.set(lo, len, types::UNTYPED);
            region.init.set(lo, len);
            region.epochs.clear(lo, len);
        }
    }

    /// Judgement J5 over every instance in `[lo, hi)`, as far as the range reaches.
    fn end(region: &Region, lo: usize, hi: usize) {
        runs(region, lo, hi, |at, stop, held| {
            if plane::owned(held) {
                // SAFETY: the run is inside the region and carries this version.
                unsafe { region.plane.end(at, stop - at, plane::ended(held)) };
            }
        });
    }

    /// Ends `[lo, hi)` and keeps it, which is an unmapping that leaves the range ours.
    fn release(region: &Region, lo: usize, hi: usize) -> bool {
        if !reserve(lo, hi) {
            return false;
        }
        for given in &GIVEN {
            let (from, to) = (given.lo.load(Ordering::Relaxed), given.hi.load(Ordering::Relaxed));
            if to == 0 || from < lo || to > hi {
                continue;
            }
            // An allocator carved this one into objects of its own, which are granules rather than
            // pages, so it is walked a granule at a time before it is forgotten.
            runs_by(region, from, to, plane::GRANULE, |at, stop, held| {
                if plane::owned(held) {
                    // SAFETY: the run is inside the region and carries this version.
                    unsafe { region.plane.end(at, stop - at, plane::ended(held)) };
                }
            });
            given.forget();
        }
        end(region, lo, hi);
        true
    }

    /// Makes an instance of every run nobody owns in `[lo, hi)`, which the program just mapped.
    fn claim(region: &Region, lo: usize, hi: usize) {
        runs(region, lo, hi, |at, stop, held| {
            if !plane::owned(held) {
                begin(region, at, stop);
            }
        });
    }

    /// Where a mapping of `size` bytes, already whole pages, can go.
    ///
    /// Next fit from the cursor, going round once. A gap between two pieces of one instance is
    /// skipped, because a mapping there would sit inside a run of another version and
    /// `crate::plane` says what an extent query takes on trust about runs.
    fn place(region: &Region, size: usize) -> Option<usize> {
        let page = PAGE.load(Ordering::Relaxed);
        if size == 0 || size > region.end - region.base {
            return None;
        }
        let mut at = CURSOR.load(Ordering::Relaxed);
        let mut wrapped = false;
        loop {
            if at + size > region.end {
                if wrapped {
                    return None;
                }
                wrapped = true;
                at = region.base;
                continue;
            }
            // A mapping an allocator adopted is owned by nobody between its objects, and is still
            // the program's.
            let mut busy = GIVEN.iter().find_map(|given| {
                let (from, to) =
                    (given.lo.load(Ordering::Relaxed), given.hi.load(Ordering::Relaxed));
                (to != 0 && from < at + size && at < to).then(|| to.next_multiple_of(page))
            });
            // From the far end, so that one owned page skips everything before it.
            let mut probe = at + size;
            while busy.is_none() && probe > at {
                probe -= page;
                if plane::owned(version(region, probe)) {
                    busy = Some(probe + page);
                    break;
                }
            }
            if busy.is_none() && at > region.base && at + size < region.end {
                let before = version(region, at - page);
                if plane::owned(before) && before == version(region, at + size) {
                    busy = Some(at + page);
                }
            }
            match busy {
                Some(next) => at = next,
                None => {
                    CURSOR.store(at + size, Ordering::Relaxed);
                    return Some(at);
                }
            }
        }
    }

    /// The `mmap` row.
    ///
    /// # Safety
    ///
    /// The program's arguments, which mean what they mean to the C library.
    pub(super) unsafe fn map(
        at: *mut c_void,
        length: usize,
        protection: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void {
        if flags & PLACED != 0 || !at.is_null() || length == 0 {
            // SAFETY: the program's arguments, passed straight on.
            let got = unsafe { real::mmap(at, length, protection, flags, fd, offset) };
            if flags & (FIXED | FIXED_NOREPLACE) != 0 {
                fixed(at as usize, length, got != FAILED);
            }
            return got;
        }
        locked(|| {
            let placed = made().and_then(|region| {
                let size = pages(length)?;
                Some((place(&region, size)?, size, region))
            });
            let Some((lo, size, region)) = placed else {
                // SAFETY: as above.
                return unsafe { real::mmap(at, length, protection, flags, fd, offset) };
            };
            let hi = lo + size;
            // SAFETY: the address is a range of the arena nothing owns, and everything else is the
            // program's.
            let got = unsafe {
                real::mmap(lo as *mut c_void, length, protection, flags | FIXED, fd, offset)
            };
            if got == FAILED {
                // A fixed mapping that fails may already have taken the reservation down.
                reserve(lo, hi);
            } else {
                begin(&region, lo, hi);
            }
            got
        })
    }

    /// A mapping the program placed itself, as far as it landed in the arena.
    fn fixed(at: usize, length: usize, worked: bool) {
        let Some(region) = watched() else { return };
        let Some(hi) = at.checked_add(length).and_then(pages) else { return };
        let Some((lo, hi)) = inside(&region, at, hi) else { return };
        locked(|| {
            if worked {
                claim(&region, lo, hi);
            } else {
                runs(&region, lo, hi, |from, to, held| {
                    if !plane::owned(held) {
                        reserve(from, to);
                    }
                });
            }
        });
    }

    /// The `munmap` row.
    ///
    /// # Safety
    ///
    /// As [`map`].
    pub(super) unsafe fn unmap(at: *mut c_void, length: usize) -> c_int {
        let lo = at as usize;
        let span = watched().and_then(|region| {
            if length == 0 || lo % PAGE.load(Ordering::Relaxed) != 0 {
                return None;
            }
            let hi = lo.checked_add(length).and_then(pages)?;
            Some((inside(&region, lo, hi)?, hi, region))
        });
        let Some(((from, to), hi, region)) = span else {
            // SAFETY: the program's arguments, passed straight on.
            return unsafe { real::munmap(at, length) };
        };
        locked(|| {
            // The parts outside the arena, if the range reached past either end of it.
            // SAFETY: the program's range, cut where the arena starts and stops.
            if lo < from && unsafe { real::munmap(at, from - lo) } != 0 {
                return -1;
            }
            // SAFETY: as above.
            if to < hi && unsafe { real::munmap(to as *mut c_void, hi - to) } != 0 {
                return -1;
            }
            if release(&region, from, to) { 0 } else { -1 }
        })
    }

    /// The `mremap` row.
    ///
    /// # Safety
    ///
    /// As [`map`].
    pub(super) unsafe fn remap(
        old: *mut c_void,
        old_size: usize,
        new_size: usize,
        flags: c_int,
        new_address: *mut c_void,
    ) -> *mut c_void {
        let lo = old as usize;
        let Some(region) = watched() else {
            // SAFETY: the program's arguments, passed straight on.
            return unsafe { real::mremap(old, old_size, new_size, flags, new_address) };
        };
        let page = PAGE.load(Ordering::Relaxed);
        // Whether the old range is an arena mapping this call takes away from, in whole or in
        // part. A duplicate made from a zero length and a move that leaves the old range mapped
        // take nothing away.
        let ours = region.holds(lo)
            && lo % page == 0
            && old_size != 0
            && flags & DONTUNMAP == 0
            && lo.checked_add(old_size).and_then(pages).is_some_and(|hi| hi <= region.end);
        locked(|| {
            let mut flags = flags;
            let mut target = new_address;
            let mut placed = None;
            if ours && new_size > old_size && flags & MAYMOVE != 0 && flags & MOVE_TO == 0 {
                if let Some(size) = pages(new_size) {
                    if let Some(at) = place(&region, size) {
                        flags |= MOVE_TO;
                        target = at as *mut c_void;
                        placed = Some((at, at + size));
                    }
                }
            }
            // SAFETY: the program's arguments, with a destination in the arena when one was found.
            let got = unsafe { real::mremap(old, old_size, new_size, flags, target) };
            if got == FAILED {
                if let Some((from, to)) = placed {
                    reserve(from, to);
                }
                return got;
            }
            if ours {
                let old_hi = pages(lo + old_size).unwrap_or(lo);
                let gone = if got == old {
                    pages(lo.saturating_add(new_size)).unwrap_or(old_hi)
                } else {
                    lo
                };
                if gone < old_hi {
                    release(&region, gone, old_hi);
                }
            }
            if got != old {
                let at = got as usize;
                if let Some((from, to)) = pages(new_size)
                    .and_then(|size| at.checked_add(size))
                    .and_then(|hi| inside(&region, at, hi))
                {
                    claim(&region, from, to);
                }
            }
            got
        })
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::alloc;
    use crate::plane;

    const READ_WRITE: c_int = 1 | 2;
    const PRIVATE_ANONYMOUS: c_int = 0x02 | 0x20;
    const PAGE: usize = 4096;

    /// The version the plane holds at `addr`, or `None` when no region covers it.
    fn held(addr: *mut c_void) -> Option<plane::Version> {
        let region = alloc::covering(addr as usize)?;
        // SAFETY: the region covers the address, so its plane does.
        Some(unsafe { region.plane.version(addr as usize) })
    }

    /// A fresh anonymous mapping through the row.
    fn mapped(length: usize) -> *mut c_void {
        // SAFETY: an anonymous private mapping with no address asked for.
        let at =
            unsafe { mmap(core::ptr::null_mut(), length, READ_WRITE, PRIVATE_ANONYMOUS, -1, 0) };
        assert_ne!(at, FAILED);
        at
    }

    #[test]
    fn a_mapping_is_an_instance_until_it_is_unmapped() {
        let at = mapped(PAGE);
        let version = held(at).expect("the mapping is in the arena");
        assert!(plane::owned(version));
        // SAFETY: the page is mapped readable and writable.
        unsafe { at.cast::<u8>().write(7) };

        // SAFETY: the mapping made above, unmapped once.
        assert_eq!(unsafe { munmap(at, PAGE) }, 0);
        let after = held(at).expect("the range stays in the arena");
        assert!(!plane::owned(after));
        assert_eq!(after, plane::ended(version));
    }

    #[test]
    fn two_mappings_are_two_instances_and_an_unmapped_range_is_not_handed_out_next() {
        let first = mapped(PAGE);
        // SAFETY: as above.
        assert_eq!(unsafe { munmap(first, PAGE) }, 0);
        let second = mapped(PAGE);
        assert_ne!(first, second);
        assert_ne!(held(first), held(second));
        // SAFETY: as above.
        assert_eq!(unsafe { munmap(second, PAGE) }, 0);
    }

    #[test]
    fn trimming_the_ends_of_a_mapping_keeps_the_middle_owned() {
        let at = mapped(4 * PAGE) as usize;
        let version = held(at as *mut c_void).expect("the mapping is in the arena");
        // SAFETY: the first and last pages of the mapping made above.
        unsafe {
            assert_eq!(munmap(at as *mut c_void, PAGE), 0);
            assert_eq!(munmap((at + 3 * PAGE) as *mut c_void, PAGE), 0);
        }
        assert!(!plane::owned(held(at as *mut c_void).unwrap()));
        assert_eq!(held((at + PAGE) as *mut c_void), Some(version));
        assert_eq!(held((at + 2 * PAGE) as *mut c_void), Some(version));
        assert!(!plane::owned(held((at + 3 * PAGE) as *mut c_void).unwrap()));
        // SAFETY: the middle of it, still mapped.
        unsafe {
            (at as *mut u8).add(PAGE).write(1);
            assert_eq!(munmap((at + PAGE) as *mut c_void, 2 * PAGE), 0);
        }
    }

    #[test]
    fn an_adopted_mapping_is_carved_in_place_and_nothing_is_mapped_over_it() {
        let at = mapped(4 * PAGE);
        let version = held(at).expect("the mapping is in the arena");
        // SAFETY: the whole of the mapping made above, adopted once.
        assert!(unsafe {
            crate::adopt::adopt(at, 4 * PAGE, crate::layout::Class::Allocated as u32)
        });
        assert!(surrendered(version));
        assert!(!plane::owned(held(at).unwrap()));

        // SAFETY: the first object of the arena adopted above.
        unsafe { crate::adopt::split(at, 64, 0) };
        let object = held(at).unwrap();
        assert!(plane::owned(object));
        assert_ne!(object, version);

        let other = mapped(PAGE) as usize;
        assert!(other + PAGE <= at as usize || other >= at as usize + 4 * PAGE);

        // SAFETY: both mappings made above, unmapped once each.
        unsafe {
            assert_eq!(munmap(other as *mut c_void, PAGE), 0);
            assert_eq!(munmap(at, 4 * PAGE), 0);
        }
        assert!(!surrendered(version));
        assert_eq!(held(at), Some(plane::ended(object)));
    }

    #[test]
    fn a_mapping_that_grows_moves_within_the_arena_and_the_old_range_ends() {
        const MAYMOVE: c_int = 1;
        let at = mapped(PAGE);
        let version = held(at).expect("the mapping is in the arena");
        // SAFETY: the page is mapped readable and writable.
        unsafe { at.cast::<u8>().write(9) };
        // SAFETY: the mapping made above, grown and allowed to move.
        let grown = unsafe { mremap(at, PAGE, 4 * PAGE, MAYMOVE, core::ptr::null_mut()) };
        assert_ne!(grown, FAILED);
        assert_ne!(grown, at);
        // SAFETY: the mapping the kernel moved, which kept what was written.
        assert_eq!(unsafe { grown.cast::<u8>().read() }, 9);
        let moved = held(grown).expect("the new range is in the arena");
        assert!(plane::owned(moved));
        assert_ne!(moved, version);
        assert_eq!(held(at), Some(plane::ended(version)));
        // SAFETY: as above.
        assert_eq!(unsafe { munmap(grown, 4 * PAGE) }, 0);
    }
}
