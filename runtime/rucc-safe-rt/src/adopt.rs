//! The five functions an allocator we did not write calls to say what it just did.
//!
//! Design: `spec/safe-memory/10-boundaries.md` section 10.4.
//!
//! Every production allocator asks the operating system for a large region and carves it into many
//! objects, which under document 04's model is one storage instance becoming several. A monitor
//! that does not know this has two ways to be wrong and no way to be right. If it treats the region
//! as one instance it catches no overflow inside it at all, because every address in the arena
//! belongs to the same version and running off the end of a 32 byte object lands somewhere that
//! version still owns. If it treats the region as nobody's it reports every access to every object
//! jemalloc ever handed out, which is a monitor nobody will keep switched on for a second run.
//!
//! So the allocator says. `__rucc_alloc_adopt` is the arena arriving, `__rucc_alloc_split` is an
//! object being carved out of it, and `__rucc_alloc_merge` is one going back. Split performs
//! judgement J4 and merge performs J5, which is the whole of temporal safety for that heap: a
//! carved object gets a version nothing has held before, so every pointer to whatever used to live
//! in those bytes fails from then on and keeps failing. That is why this is five functions rather
//! than a framework. The allocator's only job is to say when an instance begins and ends, and the
//! monitor does the rest.
//!
//! # Who is allowed to call these
//!
//! Code in the trust set, which document 14 section 14.8 says is this crate and whatever the build
//! deliberately puts beside it. These functions decide what memory is owned by whom, so an
//! allocator that describes its arena wrongly is not a program with a bug in it, it is a monitor
//! that has been lied to.
//!
//! That is why almost nothing here refuses. A split of storage outside every adopted region, or a
//! merge of an address nothing watches, is quietly ignored: the caller is the allocator rather than
//! the program under test, and turning its mistakes into memory safety reports would put the blame
//! several layers from where it belongs. The two exceptions are the two that are judgements about
//! the heap rather than about the call. A split over storage that is already somebody's instance is
//! J4, and it is the shape of an allocator handing the same block out twice, which is a bug that
//! ends up in the program's hands. A merge of storage nobody owns is J5, and it is a double free
//! arriving through the arena's own path.
//!
//! # What granule resolution costs here
//!
//! The plane holds one version per sixteen bytes, so an allocator whose objects are smaller than
//! that, or not sixteen byte aligned, gets neighbours sharing a version and overflows between them
//! going unreported. Document 04 section 4.3 chose the granule and this is the price it named. Most
//! allocators are already sixteen byte aligned because C requires the alignment for `malloc`
//! anyway, and the ones that pack smaller objects are the ones section 4.3 says to give a finer
//! plane to when there is one.

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::alloc;
use crate::fail::Judgement;
use crate::plane::{self, Counter, GRANULE, SLOT, Version};

/// The versions the instances of every adopted region are numbered from.
///
/// One counter for all of them rather than one each, because a version only ever means anything
/// against the plane of the region it was written into, and a counter per region would be a table
/// to find before an allocation could number itself. Two regions may hold the same number and it is
/// not an ambiguity: nothing ever compares a version from one plane against a version from another.
static VERSIONS: Counter = Counter::new();

/// The lowest granule boundary at or below `addr`.
const fn floor(addr: usize) -> usize {
    addr & !(GRANULE - 1)
}

/// Storage the allocator obtained from the operating system and is about to manage.
///
/// A shadow is mapped for it, and from then on every check that lands in it asks this region's
/// plane instead of falling through as an address nobody watches. The region starts owned by
/// nobody, which is the truth: the allocator has the storage and has carved nothing out of it yet,
/// so the only things in there are its own headers and free lists, and those are its business.
///
/// False when the region could not be taken on, which is one of three things. The range is empty
/// once it has been rounded to granules; it overlaps something already watched, and two planes over
/// one address is a question with two answers; or there was no room in the table or no address
/// space for the shadow. None of the three refuses anything. The storage is simply not watched,
/// which is where it was a moment ago.
///
/// `class` is document 04's storage class, kept per region so that the summary can say what a build
/// was watching rather than only how much.
///
/// # Safety
///
/// `base` names `size` bytes the caller owns and will not hand back to the operating system, since
/// the plane over it lives as long as the program does.
pub unsafe fn adopt(base: *mut c_void, size: usize, class: u32) -> bool {
    let at = base as usize;
    // Rounded inwards rather than outwards. A granule half outside the region is one this monitor
    // would be answering about on behalf of whoever owns the other half of it.
    let lo = at.next_multiple_of(GRANULE);
    let Some(top) = at.checked_add(size) else { return false };
    let hi = floor(top);
    if hi <= lo || alloc::overlaps(lo, hi) {
        return false;
    }
    let Some(shadow) = alloc::map((hi - lo) / GRANULE * SLOT) else { return false };
    alloc::publish(shadow.wrapping_sub(lo / GRANULE * SLOT), lo, hi, class)
}

/// Judgement J4: `[base, base + size)` of an adopted region is a fresh instance.
///
/// The version is one no counter has returned before, so every capability naming the previous
/// occupant of those bytes fails from here on. That is the sentence document 05 section 5.2.2 asks
/// an allocator to be able to say, and saying it is the entire cost of temporal safety for a heap
/// this crate did not write.
///
/// `flags` is the allocator's own, and nothing reads it yet. The plane holds versions and the
/// permissions of document 04 section 4.1 land in the type plane of milestone S5, which is where a
/// read only or a write only carve will have somewhere to be recorded.
///
/// # Panics
///
/// When the range is already somebody's instance, which is an allocator handing the same storage
/// out twice.
///
/// # Safety
///
/// `base` is inside a region this allocator adopted, and `size` bytes from it are too.
pub unsafe fn split(base: *mut c_void, size: usize, flags: u32) {
    let _ = flags;
    let at = base as usize;
    let Some(region) = alloc::covering(at) else { return };
    let lo = floor(at);
    let len = (at - lo + size).next_multiple_of(GRANULE);
    if len == 0 || !region.holds(lo.wrapping_add(len - 1)) {
        return;
    }

    let mut granule = lo;
    while granule < lo + len {
        // SAFETY: the granule is between `lo` and the last address the region holds, both checked
        // above, so the plane covers it.
        if plane::owned(unsafe { region.plane.version(granule) }) {
            crate::fail::refused_at(
                Judgement::Begin,
                "__rucc_alloc_split, over storage that is already an instance",
                granule,
            );
        }
        granule += GRANULE;
    }

    // SAFETY: `lo` is granule aligned by construction, the range is inside the region the plane was
    // built over, and the version is one the counter has not returned before.
    unsafe { region.plane.begin(lo, len, plane::begun(VERSIONS.next())) };
}

/// Judgement J5: the instance at `base` is over and its storage goes back to the region.
///
/// How far it reached is not passed and does not have to be. The instance is a run of granules
/// carrying one version, so the plane already knows where it ends, and asking it is cheaper for the
/// allocator than remembering a length it has usually just read out of its own header anyway.
///
/// # Panics
///
/// When nobody owns the storage at `base`, which is a free of something that was already free.
///
/// # Safety
///
/// `base` is the address a [`split`] of an adopted region returned.
pub unsafe fn merge(base: *mut c_void) {
    let at = base as usize;
    let Some(region) = alloc::covering(at) else { return };
    let lo = floor(at);
    // SAFETY: the region is the one covering this address, so its plane is built over it.
    let version = unsafe { region.plane.version(lo) };
    if !plane::owned(version) {
        crate::fail::refused_at(Judgement::End, "__rucc_alloc_merge, of storage nobody owns", lo);
    }

    let mut end = lo;
    // SAFETY: the walk stops at the region's end, so every granule it reads is one the plane covers.
    while end < region.end && unsafe { region.plane.version(end) } == version {
        end += GRANULE;
    }
    // SAFETY: as above, and the range is exactly the granules that answered with this version.
    unsafe { region.plane.end(lo, end - lo, plane::ended(version)) };
}

/// Every instance within `[base, base + size)` ends at once.
///
/// For the arena and pool allocators that free thousands of objects by moving a pointer back to
/// where it started, which is the pattern that would otherwise leave the plane insisting a region
/// is live long after the program stopped believing it. Tier K wants the same thing for
/// `free_initmem` and for slab page reclamation.
///
/// Nothing is refused. A purge of storage nobody owns is a pool being reset twice, which is what
/// the pattern is for, and the caller here is the allocator rather than the program.
///
/// # Safety
///
/// `base` and `size` name storage inside a region this allocator adopted.
pub unsafe fn purge(base: *mut c_void, size: usize) {
    let at = base as usize;
    let lo = floor(at);
    let hi = (at + size).next_multiple_of(GRANULE);
    let Some(region) = alloc::covering(lo) else { return };

    let mut granule = lo;
    while granule < hi && region.holds(granule) {
        // SAFETY: `holds` in the condition above is what reading the plane asks for.
        let version = unsafe { region.plane.version(granule) };
        if plane::owned(version) {
            // SAFETY: as above, and one granule from a granule aligned address is one slot.
            unsafe { region.plane.end(granule, GRANULE, plane::ended(version)) };
        }
        granule += GRANULE;
    }
}

/// How many identities the table can remember at once.
///
/// A fixed table rather than a field beside each instance, because an adopted region has no room
/// for one. The layout of document 05 section 5.2.2 puts a header in front of a payload and this
/// allocator's payloads are somebody else's, laid out however they like, so there is nowhere in
/// them to write anything.
const TAGS: usize = 256;

/// One remembered identity, and the version it was remembered for.
struct Tag {
    /// The instance's version, or [`plane::DEAD`] for a slot nothing has used.
    version: AtomicU64,
    /// Which deallocator it belongs to.
    id: AtomicU32,
}

impl Tag {
    /// An empty slot, which is what the whole table starts as.
    const fn empty() -> Self {
        Self { version: AtomicU64::new(plane::DEAD), id: AtomicU32::new(0) }
    }
}

/// Which deallocator each tagged instance belongs to, keyed by its version.
static TAGS_BY_VERSION: [Tag; TAGS] = [const { Tag::empty() }; TAGS];

/// The slot a version is remembered in.
///
/// Direct mapped, so a new instance whose version lands here evicts an older one. Losing a tag
/// costs a report its detail and never costs a program a false refusal, because a lookup that
/// misses says it does not know rather than saying the wrong thing.
const fn slot(version: Version) -> usize {
    (version >> 1) as usize % TAGS
}

/// Associates the instance at `base` with a deallocator identity, for judgement J6.
///
/// What it buys is the answer to "this pointer was freed by the wrong allocator", which is a real
/// bug in a program that has more than one and the reason section 10.4 has this function at all. A
/// program with one allocator never needs to call it.
pub fn tag(base: *mut c_void, deallocator: u32) {
    let at = base as usize;
    let Some(region) = alloc::covering(at) else { return };
    // SAFETY: the region is the one covering this address, so its plane is built over it.
    let version = unsafe { region.plane.version(floor(at)) };
    if !plane::owned(version) {
        return;
    }
    let tag = &TAGS_BY_VERSION[slot(version)];
    // The identity first and the version second, with the release on the version, so a reader that
    // matches on the version is reading the identity that was stored for it and not the one the
    // slot held before.
    tag.id.store(deallocator, Ordering::Relaxed);
    tag.version.store(version, Ordering::Release);
}

/// Which deallocator owns the instance at `addr`, when somebody said.
///
/// `None` for an address nobody watches, for storage nobody owns, for an instance nobody tagged,
/// and for one whose tag has been evicted. All four mean the same thing to a caller: there is no
/// identity to compare against, so this is not the judgement that decides anything.
#[must_use]
pub fn deallocator(addr: usize) -> Option<u32> {
    let region = alloc::covering(addr)?;
    // SAFETY: the region is the one covering this address, so its plane is built over it.
    let version = unsafe { region.plane.version(floor(addr)) };
    if !plane::owned(version) {
        return None;
    }
    let tag = &TAGS_BY_VERSION[slot(version)];
    if tag.version.load(Ordering::Acquire) != version {
        return None;
    }
    Some(tag.id.load(Ordering::Relaxed))
}

/// The names an allocator is compiled against.
///
/// Separate from the functions above for the reason the checks' exports are separate from theirs:
/// these are an ABI and those are Rust. The C signatures are the ones section 10.4 writes down, so
/// an allocator that was ported to another monitor and back is calling the same five names with the
/// same five shapes.
pub mod exports {
    use core::ffi::{c_uint, c_void};

    /// # Safety
    ///
    /// As [`super::adopt`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_alloc_adopt(base: *mut c_void, size: usize, class: c_uint) {
        // The answer is dropped because the C signature has nowhere to put it. A region that could
        // not be taken on is one the monitor says nothing about, and section 10.2's summary is
        // where that belongs rather than in the return of a call the allocator makes thousands of.
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        let _ = unsafe { super::adopt(base, size, class) };
    }

    /// # Safety
    ///
    /// As [`super::split`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_alloc_split(base: *mut c_void, size: usize, flags: c_uint) {
        // SAFETY: as above.
        unsafe { super::split(base, size, flags) };
    }

    /// # Safety
    ///
    /// As [`super::merge`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_alloc_merge(base: *mut c_void) {
        // SAFETY: as above.
        unsafe { super::merge(base) };
    }

    /// # Safety
    ///
    /// As [`super::purge`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_alloc_purge(base: *mut c_void, size: usize) {
        // SAFETY: as above.
        unsafe { super::purge(base, size) };
    }

    /// # Safety
    ///
    /// `base` is an address the program may name. Nothing is read through it.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_alloc_tag(base: *mut c_void, deallocator: c_uint) {
        super::tag(base, deallocator);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Class;
    use crate::report::{Owner, owner};
    use crate::turnstile::turn;

    /// How large the arena the tests share is.
    const ARENA: usize = 1 << 16;

    /// The one adopted region these tests carve out of.
    ///
    /// One rather than one each, because the table is eight entries long and a test that took one
    /// of them every run would decide how many other tests could exist. Each test below works at
    /// its own offset, so they do not tread on each other even though they share the storage.
    fn arena() -> usize {
        static ADOPTED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        *ADOPTED.get_or_init(|| {
            let base = alloc::map(ARENA).expect("the tests need one mapping");
            // SAFETY: the mapping is this process's, it is never unmapped, and nothing else has
            // been told about it.
            assert!(unsafe { adopt(base as *mut c_void, ARENA, Class::Allocated as u32) });
            base
        })
    }

    /// The address `offset` bytes into the shared arena.
    fn at(offset: usize) -> *mut c_void {
        (arena() + offset) as *mut c_void
    }

    #[test]
    fn an_adopted_region_is_watched_and_starts_owned_by_nobody() {
        let _turn = turn();
        let base = arena();
        // Watched, which is the difference adopting makes: the same address a moment earlier was
        // one no plane covered and every check let through without looking.
        assert!(alloc::covering(base).is_some());
        assert!(alloc::covering(base + ARENA - 1).is_some());
        // And owned by nobody, which is the truth. The allocator has the storage and has carved
        // nothing out of it, so what is in there is its own headers and free lists.
        assert_eq!(owner(base), Owner::Nobody);
    }

    #[test]
    fn a_carved_instance_is_live_until_it_is_merged() {
        let _turn = turn();
        let object = at(1024);
        // SAFETY: inside the adopted region, and nothing else carves at this offset.
        unsafe { split(object, 64, 0) };
        let Owner::Live(instance) = owner(object as usize) else {
            panic!("the carve was supposed to make an instance");
        };
        // Every granule of it, rather than only the first, since an overflow is caught by the last
        // one belonging to somebody else.
        assert_eq!(owner(object as usize + 48), Owner::Live(instance));
        assert_eq!(owner(object as usize + 64), Owner::Nobody);

        // SAFETY: the instance carved above.
        unsafe { merge(object) };
        // Freed rather than nobody's, which is what makes a stale pointer keep failing instead of
        // starting to work again when the allocator hands these bytes out next.
        assert_eq!(owner(object as usize), Owner::Freed(instance));
        assert_eq!(owner(object as usize + 48), Owner::Freed(instance));
    }

    #[test]
    fn a_carve_after_a_merge_is_a_different_instance() {
        let _turn = turn();
        let object = at(2048);
        // SAFETY: inside the adopted region, and nothing else carves at this offset.
        unsafe { split(object, 32, 0) };
        let first = owner(object as usize);
        // SAFETY: the instance carved above.
        unsafe { merge(object) };
        // SAFETY: the storage is the region's again, which is what merge just said.
        unsafe { split(object, 32, 0) };
        let second = owner(object as usize);

        // The whole of temporal safety for this heap is this line. The same bytes, a version
        // nothing has held before, so a pointer to the first object fails against the second.
        assert_ne!(first, second);
        assert!(matches!(second, Owner::Live(_)));
        // SAFETY: the instance carved above.
        unsafe { merge(object) };
    }

    #[test]
    fn a_purge_ends_everything_it_covers_and_nothing_else() {
        let _turn = turn();
        // Three objects, which is the pattern the call is for: a pool that hands out thousands and
        // gives them all back by moving one pointer.
        for step in 0..3 {
            // SAFETY: inside the adopted region, at offsets nothing else uses.
            unsafe { split(at(4096 + step * 64), 64, 0) };
        }
        let outside = at(4096 + 3 * 64);
        // SAFETY: as above.
        unsafe { split(outside, 64, 0) };

        // SAFETY: the three objects above, and nothing past them.
        unsafe { purge(at(4096), 3 * 64) };
        for step in 0..3 {
            assert!(matches!(owner(at(4096 + step * 64) as usize), Owner::Freed(_)));
        }
        assert!(matches!(owner(outside as usize), Owner::Live(_)));

        // SAFETY: still live, which the assertion above is what says.
        unsafe { merge(outside) };
    }

    #[test]
    fn adopting_storage_that_is_already_watched_is_refused() {
        let _turn = turn();
        // Two planes over one address is a question with two answers, and the second one would be
        // a fresh shadow saying nobody owns storage the first one knows is somebody's.
        let base = arena();
        // SAFETY: the mapping the tests share, offered a second time on purpose.
        assert!(!unsafe { adopt(base as *mut c_void, ARENA, Class::Allocated as u32) });
        // SAFETY: as above, overlapping the tail of it rather than matching it.
        assert!(!unsafe { adopt((base + ARENA / 2) as *mut c_void, ARENA, 0) });
    }

    #[test]
    fn a_region_with_nothing_in_it_is_not_adopted() {
        let _turn = turn();
        // Rounding inwards is what makes this empty, and a region of no granules has no shadow to
        // map and nothing to say about any address.
        // SAFETY: an address in the shared arena, which is mapped, and a size no granule fits in.
        assert!(!unsafe { adopt(at(8192), GRANULE - 1, 0) });
    }

    #[test]
    fn a_tag_says_which_deallocator_an_instance_belongs_to() {
        let _turn = turn();
        let object = at(12288);
        // Untagged is the ordinary case and it answers that it does not know, which is what keeps
        // this out of the way of a program with one allocator.
        assert_eq!(deallocator(object as usize), None);

        // SAFETY: inside the adopted region, at an offset nothing else uses.
        unsafe { split(object, 64, 0) };
        assert_eq!(deallocator(object as usize), None);
        tag(object, 7);
        assert_eq!(deallocator(object as usize), Some(7));
        // Anywhere in the instance, since the tag is remembered against the version rather than
        // against the address, and an interior pointer is what a free of the wrong thing looks like.
        assert_eq!(deallocator(object as usize + 32), Some(7));

        // SAFETY: the instance tagged above.
        unsafe { merge(object) };
        // The instance is over, so there is no identity to give: a version nobody owns is not one
        // anybody can be freeing.
        assert_eq!(deallocator(object as usize), None);
    }
}
