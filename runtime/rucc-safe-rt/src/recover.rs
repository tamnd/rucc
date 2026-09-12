//! Boundary capability recovery, from document 05 section 5.3 and document 10 section 10.7.
//!
//! A pointer that arrives from code this compiler did not build arrives without a capability.
//! There is no frame beside the call, because the caller did not know to write one, and there is
//! nothing in the pointer, because [`crate::frame`] explains at length why there had better not be.
//! So the capability has to be reconstructed from what the runtime already knows about the address,
//! and this module is where that happens.
//!
//! The rule the whole boundary hangs on is document 10 section 10.1's: never assume. A recovered
//! capability says exactly as much as could be found out and not one byte more, and the amount that
//! could be found out is different in four situations, so recovery answers with which one it was as
//! well as with the capability. Those four counts are what `--emit=safety-summary` reports, and they
//! are the difference between "this binary's guarantee rests on eleven recovered capabilities" and
//! "this binary's guarantee is mostly aspiration".
//!
//! The order the four are tried in is most informative first.
//!
//! [`Origin::Planes`] is the good case. The address is inside a region the runtime watches and some
//! instance owns its granule, so the bounds are that instance's, found by walking the run of equal
//! versions out from the address in both directions, and the version is the one the plane holds. A
//! capability recovered this way is as strong as one that was passed, and is still counted, because
//! the walk found the instance the address is in rather than the instance the pointer was derived
//! from, and for a pointer into the middle of an array of structures those are not the same object.
//!
//! [`Origin::Mapping`] is the weak case that still permits something. The address is inside a
//! watched region, nobody owns the granule, and the region is a mapping rather than a heap, which
//! is what an arena looks like before its allocator has said what it carved. All that is known is
//! the mapping, so the bounds are the mapping's and the capability is marked [`Meta::WIDE`]. It
//! permits running from one object in that arena into the next, which is a real hole and is why it
//! is counted separately.
//!
//! [`Origin::Nobody`] is storage inside a watched heap that no instance owns: a freed block, an
//! allocator header, the gap between two blocks. Nothing can be recovered because there is nothing
//! there, and the answer is the bottom capability. This is the one case where recovery produces
//! something that refuses, and it refuses the same accesses [`crate::check::live`] already refuses.
//!
//! [`Origin::Unwatched`] is an address in no watched region at all: a local, a global, or memory
//! from an allocator nobody told us about. The honest answer is a capability with no bounds, which
//! permits everything and is marked [`Meta::WIDE`]. Refusing here instead would report every
//! program that passes the address of a local across the boundary, and a monitor that reports
//! correct programs is a monitor that gets turned off. This is the count that says how much of a
//! build the boundary is actually covering.

use core::ffi::c_void;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::alloc::{self, Region};
use crate::layout::{self, Cap, Class, Header, Meta, State, perm};
use crate::plane::{self, GRANULE, Version};

/// Where a recovered capability's bounds came from.
///
/// The discriminants are ABI. `--emit=safety-summary` reads these counts out of a running program,
/// and the program and the tool that prints its summary need not be the same build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Origin {
    /// An instance owns the granule, so the bounds are the instance's.
    Planes = 0,
    /// Nobody owns the granule and the region is a mapping, so the bounds are the mapping's.
    Mapping = 1,
    /// Nobody owns the granule and the region is a heap, so there is nothing to recover.
    Nobody = 2,
    /// No watched region holds the address, so nothing at all is known about it.
    Unwatched = 3,
}

/// How many recoveries of each kind this program has done.
///
/// A separate counter per origin rather than one total, because the four mean very different
/// things about a build and a single number would let the harmless one hide the others.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Recoveries that found the instance.
    pub planes: u64,
    /// Recoveries that found only the mapping.
    pub mapping: u64,
    /// Recoveries that found storage nobody owns.
    pub nobody: u64,
    /// Recoveries over an address nothing watches.
    pub unwatched: u64,
}

impl Counts {
    /// Every recovery, however much it found.
    #[must_use]
    pub const fn total(self) -> u64 {
        self.planes + self.mapping + self.nobody + self.unwatched
    }

    /// The recoveries that produced a capability wider than an object.
    ///
    /// The number a reviewer should look at first, because it is how many pointers crossed into
    /// this program carrying permission over storage that was never theirs.
    #[must_use]
    pub const fn wide(self) -> u64 {
        self.mapping + self.unwatched
    }
}

/// The tally itself, in the order [`Origin`] declares.
static TALLY: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// Every recovery this program has done so far.
///
/// Read with `Relaxed`, because the four are read one after another and a summary printed while
/// other threads are still running was never going to be a consistent snapshot of anything. The
/// alternative is a lock on the boundary path, which would cost more than the number is worth.
#[must_use]
pub fn counts() -> Counts {
    Counts {
        planes: TALLY[Origin::Planes as usize].load(Ordering::Relaxed),
        mapping: TALLY[Origin::Mapping as usize].load(Ordering::Relaxed),
        nobody: TALLY[Origin::Nobody as usize].load(Ordering::Relaxed),
        unwatched: TALLY[Origin::Unwatched as usize].load(Ordering::Relaxed),
    }
}

/// Records one recovery and hands back the capability it produced.
fn tally(origin: Origin, cap: Cap) -> Cap {
    bump(origin);
    cap
}

/// Records one recovery and hands back which kind it was.
fn bump(origin: Origin) -> Origin {
    TALLY[origin as usize].fetch_add(1, Ordering::Relaxed);
    origin
}

/// The capability for `addr`, recovered from whatever the runtime knows about it.
///
/// The bounds, the version and the flags are as the module comment describes, and the count for
/// whichever of the four situations this turned out to be goes up by one.
///
/// Permission is read and write in every case that permits anything. The planes do not record
/// which of the two an instance allows, so claiming to know would be the assumption document 10
/// section 10.1 forbids, and refusing a write to storage that permits one would report a correct
/// program. Execute is never granted, because nothing that crosses this boundary as a data pointer
/// is a function.
#[must_use]
pub fn recover(addr: *const c_void) -> Cap {
    let addr = addr as usize;
    let Some(region) = alloc::covering(addr) else {
        return tally(Origin::Unwatched, everything());
    };

    // SAFETY: the region is the one covering this address, so its plane is built over it.
    let version = unsafe { region.plane.version(addr) };
    if plane::owned(version) {
        let (lo, ext) = run(&region, addr, version);
        let meta = word(region.class, Meta::RECOVERED);
        return tally(Origin::Planes, Cap::new(lo as u64, ext as u64, version, meta));
    }

    if region.class == Class::Allocated as u32 {
        return tally(Origin::Nobody, Cap::BOTTOM);
    }

    let meta = word(region.class, Meta::RECOVERED | Meta::WIDE);
    let ext = (region.end - region.base) as u64;
    tally(Origin::Mapping, Cap::new(region.base as u64, ext, plane::FOREIGN, meta))
}

/// Which of the four situations `addr` is in, without working out any bounds.
///
/// [`recover`] with the answer thrown away, which sounds useless and is the only form a build can
/// use today. A capability is four words and there is nowhere to keep one: the aux plane that gives
/// a pointer in memory somewhere to carry its capability is milestone S5, and until it exists a
/// call site that recovers one has to drop it again on the next instruction. Paying for the bounds
/// walk to do that would be an overhead with nothing to show for it, and the walk is linear in the
/// size of the instance the address landed in.
///
/// What is left is the count, and the count is the point. Every crossing this raises is a crossing
/// [`recover`] would have raised for the same address, so the four numbers mean the same thing
/// whichever entry point a build reaches them through, and the day the capability has somewhere to
/// live this becomes a call to [`recover`] rather than a different measurement.
///
/// Nothing is refused here. A crossing is not an access, the judgement belongs at whatever reads
/// through the pointer, and [`crate::check`] is where that happens.
pub fn witness(addr: *const c_void) -> Origin {
    let addr = addr as usize;
    let Some(region) = alloc::covering(addr) else { return bump(Origin::Unwatched) };

    // SAFETY: the region is the one covering this address, so its plane is built over it.
    let version = unsafe { region.plane.version(addr) };
    if plane::owned(version) {
        return bump(Origin::Planes);
    }
    if region.class == Class::Allocated as u32 {
        return bump(Origin::Nobody);
    }
    bump(Origin::Mapping)
}

/// The capability generated code uses for a pointer argument.
///
/// `carried` is what [`crate::frame::Frame::arg`] answered, which is the real capability when an
/// instrumented caller published a frame and the bottom one when nothing did. The bottom one is
/// the signal to recover, so the two halves of the boundary meet here: publishing a frame is the
/// caller's side and this is the callee's, and a callee compiled against this does the right thing
/// either way without knowing which kind of caller it has.
///
/// A caller that genuinely means to pass a null or a dead pointer passes the bottom capability
/// too, and this recovers over it. That is not a hole. Recovery over a dead pointer inside a
/// watched heap answers [`Origin::Nobody`], which is the bottom capability again.
#[must_use]
pub fn argument(carried: Cap, addr: *const c_void) -> Cap {
    if carried.is_bottom() { recover(addr) } else { carried }
}

/// The instance `addr` landed in, as a base and an extent, when the planes know of one.
///
/// [`recover`]'s bounds without the capability around them and without the tally, for a caller that
/// wants to say something about a whole instance rather than about the pointer into it.
/// [`crate::check::handed`] is the one there is, and what it says is that storage handed to code
/// this build did not compile may have been written by it.
///
/// Nothing, rather than the region's own bounds, when the address is in an arena whose allocator
/// said nothing or on storage nobody owns. Those are the two cases where a run is not an instance,
/// and answering with one would be the assumption section 10.1 forbids.
#[must_use]
pub fn extent(region: &Region, addr: usize) -> Option<(usize, usize)> {
    // SAFETY: the caller established that the region covers this address, which is what reading a
    // version asks for.
    let version = unsafe { region.plane.version(addr) };
    plane::owned(version).then(|| run(region, addr, version))
}

/// The run of granules around `addr` that `version` owns, as a base and an extent.
///
/// Searched in both directions and stopped at the region's edges, so the worst case is a single
/// instance filling the whole region and the answer costs a logarithm of that rather than a step
/// per granule. [`edge`] is the search and the note there says why probing is allowed to skip
/// granules nobody looked at.
///
/// Except where the header answers, which is the case document 05 section 5.2.3 built the layout
/// for and is [`stated`]. An address whose own granule is owned and whose neighbour below is not is
/// the first granule of a run, and the thirty two bytes in front of the first granule of a run are
/// that instance's header, which states the extent. That is a load rather than a search, and it is
/// the common case at a boundary: a pointer handed to a library is far more often the address an
/// allocator returned than a pointer into the middle of the object.
fn run(region: &Region, addr: usize, version: Version) -> (usize, usize) {
    let here = addr & !(GRANULE - 1);

    // The same read the search below starts with. Asking it twice is a hit in the first level cache
    // and asking it here is what makes the header safe to read: a granule with the same version
    // underneath it is not the first of its run, so the thirty two bytes in front of it are some
    // other instance's payload, and the program can put anything it likes in those.
    // SAFETY: `here` is above the region's base, so the granule below it is one the plane covers.
    let below = (here > region.base).then(|| unsafe { region.plane.version(here - GRANULE) });
    let first = (below != Some(version)).then(|| stated(region, here, version)).flatten();
    if let Some(ext) = first {
        return (here, ext);
    }

    // How many granules there are to look at on each side, worked out from the region's own bounds
    // so that every address [`edge`] reads is one the plane covers.
    let down = here.saturating_sub(region.base) / GRANULE;
    let up = (region.end - 1 - here) / GRANULE;

    let lo = here - edge(region, here, version, down, -STEP) * GRANULE;
    let hi = here + (edge(region, here, version, up, STEP) + 1) * GRANULE;

    (lo, hi - lo)
}

/// One granule on, as the offset [`edge`] steps by.
const STEP: isize = GRANULE as isize;

/// How many granules past the one at `here` still answer with `version`, out of `span` of them.
///
/// `step` is [`STEP`] to look up the address space and its negation to look down, and the caller
/// has already established that the granule at `here` itself answers. The answer is the largest `n`
/// no greater than `span` for which every granule from `here` to `here + n * step` carries
/// `version`.
///
/// Doubling out from the near end and then halving, rather than stepping one granule at a time.
/// What makes probing sound is that a run of granules carrying one version has nothing else in the
/// middle of it, so a granule that answers puts a floor under the count and one that does not puts
/// a ceiling over it, and the truth is between them. `crate::plane` states that invariant and lists
/// what rests on it. This is [`crate::check`]'s `reach` with the far end unknown: that one is asked
/// about a span the caller named and so can probe the end of it first, and this one is asked how
/// far an instance goes and has only the region's edge to bound it, which is why it doubles rather
/// than starting there. Doubling is what keeps a small object in a large arena cheap, which is the
/// case that matters: an instance of a few granules is found in a few reads whatever the arena
/// around it costs, and the old walk's one read per granule is only ahead of that for an object of
/// one or two granules.
fn edge(region: &Region, here: usize, version: Version, span: usize, step: isize) -> usize {
    if span == 0 {
        return 0;
    }
    let at = |n: usize| here.wrapping_add_signed(step * n as isize);
    // SAFETY: `n` is never more than `span`, which the caller worked out from the region's bounds,
    // so every address reached is one the plane covers.
    let owner = |n: usize| unsafe { region.plane.version(at(n)) };

    let mut yes = 0;
    let mut probe = 1;
    let mut no = loop {
        let at = probe.min(span);
        if owner(at) != version {
            break at;
        }
        if at == span {
            return span;
        }
        yes = at;
        probe *= 2;
    };
    // Halve between a granule that answered and one that did not. The first answered because the
    // caller read it before calling, and the last did not, which is what the doubling above found.
    while no - yes > 1 {
        let mid = yes + (no - yes) / 2;
        if owner(mid) == version {
            yes = mid;
        } else {
            no = mid;
        }
    }
    yes
}

/// How far the instance whose payload begins at `payload` runs, out of its own header.
///
/// Nothing unless the header in front of the address is a live allocated instance's and says the
/// version the plane says. The caller has already established that `payload` is the first granule
/// of a run, which is what makes the address in front of it a header rather than somebody's bytes,
/// and the version compare is what makes a header nobody wrote say nothing: a block the allocator
/// has never used holds whatever the mapping came with, and a block it has finished with holds the
/// ended version, and neither of those is the version the plane is holding for a live instance.
///
/// The other fields are checked because a header that disagrees with itself is a header not to
/// believe. What could produce one is storage adopted from an allocator that lays its own blocks
/// out differently, per `spec/safe-memory/10-interop.md`, which is a region the runtime watches
/// without having carved it.
fn stated(region: &Region, payload: usize, version: Version) -> Option<usize> {
    // Somebody else's arena has no header of ours in front of anything, so there is nothing here
    // to read and the walk is the only answer. See `alloc::Watch::carved`.
    if !region.carved {
        return None;
    }
    // Room for a header between the region's base and the payload, which is the one thing that
    // makes reading the bytes in front of it a read of this region rather than of whatever is
    // mapped below it.
    if payload < region.base + layout::HEADER {
        return None;
    }
    // SAFETY: the address is inside the region, which the caller found it in, and the thirty two
    // bytes in front of it are too by the check above. They are the runtime's own storage rather
    // than the program's, because the caller established that the granule below is not part of
    // this instance and the only thing that sits between two payloads is a header and an aux.
    let header = unsafe { (layout::header_of(payload) as *const Header).read() };
    if header.ver != version {
        return None;
    }

    let ext = header.ext as usize;
    if ext == 0
        || ext % GRANULE != 0
        || header.meta.class() != Class::Allocated as u8
        || header.meta.state() != State::Live as u8
        || payload + ext > region.end
        || layout::block_of(payload, ext) < region.base
    {
        return None;
    }
    Some(ext)
}

/// The metadata word a recovered capability carries.
///
/// The instance identifier is zero, because a recovered capability is not an instance the
/// allocator here handed out and inventing a number for it would put a number in a report that
/// matches nothing.
fn word(class: u32, flags: u8) -> Meta {
    let class = match class {
        c if c == Class::Static as u32 => Class::Static,
        c if c == Class::Automatic as u32 => Class::Automatic,
        c if c == Class::Mapped as u32 => Class::Mapped,
        _ => Class::Allocated,
    };
    Meta::new(class, perm::READ | perm::WRITE, 0).with_flags(flags)
}

/// The capability for an address nothing is known about.
///
/// Bounds over the whole address space, which is the only honest answer, and [`Meta::WIDE`] so
/// that anything asking later can tell it apart from a capability somebody meant.
fn everything() -> Cap {
    let meta = word(Class::Mapped as u32, Meta::RECOVERED | Meta::WIDE);
    Cap::new(0, u64::MAX, plane::FOREIGN, meta)
}

/// The names generated code and the summary are compiled against.
///
/// Separate from the functions above for the reason every other module's exports are: these are an
/// ABI and those are Rust.
pub mod exports {
    use core::ffi::c_void;

    use crate::layout::Cap;

    /// Document 06 section 6.2's `cap_recover`, writing its answer through `out`.
    ///
    /// Through a pointer rather than returned, because a capability is four words and the return
    /// convention for a structure that size is a hidden pointer anyway. Saying so in the signature
    /// means the backend can hand it the stack slot the capability was going into and skip a copy.
    ///
    /// # Safety
    ///
    /// `out` is a writable, aligned [`Cap`] sized slot. `addr` is only ever compared, never read
    /// through, so it may be any value at all including null.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_cap_recover(out: *mut Cap, addr: *const c_void) {
        let cap = super::recover(addr);
        // SAFETY: the caller's slot, which the contract above says is writable and aligned.
        unsafe { out.write(cap) }
    }

    /// Document 10 section 10.2's crossing count, which is what generated code calls.
    ///
    /// One argument and no result, because there is nothing yet for a result to be kept in and a
    /// signature that promised one would have to change the day there is. What it leaves behind is
    /// the count, and the count is what a build's summary is asking for.
    ///
    /// # Safety
    ///
    /// `addr` is a pointer that crossed the boundary. It is only ever compared, never read
    /// through, so it may be any value at all including null.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_cap_witness(addr: *const c_void) {
        let _ = super::witness(addr);
    }

    /// How many capabilities this program has recovered, for the summary to print.
    #[unsafe(no_mangle)]
    pub extern "C" fn __rucc_safety_recovered() -> u64 {
        super::counts().total()
    }

    /// How many of those are wider than an object, which is the number that matters.
    #[unsafe(no_mangle)]
    pub extern "C" fn __rucc_safety_recovered_wide() -> u64 {
        super::counts().wide()
    }
}

#[cfg(test)]
mod tests {
    use core::ffi::c_void;

    use super::{Cap, Header, Meta, Origin, counts, recover, witness};
    use crate::alloc;
    use crate::layout::Class;
    use crate::plane;

    /// The count for one origin, so that a test can say what its call did as well as what it got.
    fn count(origin: Origin) -> u64 {
        let counts = counts();
        match origin {
            Origin::Planes => counts.planes,
            Origin::Mapping => counts.mapping,
            Origin::Nobody => counts.nobody,
            Origin::Unwatched => counts.unwatched,
        }
    }

    /// Gives an instance back, which every test that took one has to do.
    fn free(ptr: *mut c_void) {
        // SAFETY: an instance this file allocated a moment ago and has not freed.
        unsafe { alloc::dealloc(ptr) }
    }

    /// The one mapping these tests recover over.
    ///
    /// Adopted once and never given back, for the reason `adopt`'s tests share theirs: the region
    /// table is eight entries long and a test that took one per run would decide how many other
    /// tests the crate could have. This one is a mapping rather than a heap, which is the whole
    /// point of it, since a heap and a mapping recover differently over storage nobody owns.
    fn arena() -> usize {
        static ADOPTED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        *ADOPTED.get_or_init(|| {
            let base = alloc::map(ARENA).expect("the tests need one mapping");
            // SAFETY: the mapping is this process's, it is never unmapped, and nothing else has
            // been told about it.
            assert!(unsafe {
                crate::adopt::adopt(base as *mut c_void, ARENA, Class::Mapped as u32)
            });
            base
        })
    }

    /// How large that mapping is.
    const ARENA: usize = 1 << 16;

    #[test]
    fn witnessing_a_crossing_counts_it_the_same_way_recovering_one_would() {
        let _turn = crate::turnstile::turn();
        // The two entry points have to agree about what an address is, or the number a summary
        // prints would depend on which one the build happened to call.
        let ptr = alloc::alloc(64);
        assert!(!ptr.is_null());
        let local = 0_u64;

        for (addr, origin) in [
            (ptr.cast::<u8>().wrapping_add(24).cast_const().cast::<c_void>(), Origin::Planes),
            (arena() as *const c_void, Origin::Mapping),
            ((&raw const local).cast::<c_void>(), Origin::Unwatched),
        ] {
            let before = count(origin);
            assert_eq!(witness(addr), origin);
            assert_eq!(count(origin), before + 1, "{origin:?}");
            let _ = recover(addr);
            assert_eq!(count(origin), before + 2, "{origin:?}");
        }

        free(ptr);
        // A freed instance is the fourth, and it has to be freed first to be one.
        let before = count(Origin::Nobody);
        assert_eq!(witness(ptr.cast_const()), Origin::Nobody);
        assert_eq!(count(Origin::Nobody), before + 1);
    }

    #[test]
    fn a_pointer_into_a_live_instance_recovers_that_instances_bounds() {
        let _turn = crate::turnstile::turn();
        let ptr = alloc::alloc(64);
        assert!(!ptr.is_null());

        let before = count(Origin::Planes);
        let cap = recover(ptr.cast::<u8>().wrapping_add(24).cast());
        assert_eq!(count(Origin::Planes), before + 1);

        assert_eq!(cap.lo, ptr as u64);
        assert_eq!(cap.ext, 64);
        assert!(cap.covers(ptr as u64, 64));
        assert!(!cap.covers(ptr as u64, 65));
        assert_eq!(cap.meta.flags(), Meta::RECOVERED);

        free(ptr);
    }

    #[test]
    fn a_pointer_to_a_freed_instance_recovers_nothing() {
        let _turn = crate::turnstile::turn();
        let ptr = alloc::alloc(64);
        free(ptr);

        let before = count(Origin::Nobody);
        let cap = recover(ptr);
        assert_eq!(count(Origin::Nobody), before + 1);
        assert!(cap.is_bottom());
        assert!(!cap.covers(ptr as u64, 1));
    }

    #[test]
    fn an_address_nothing_watches_recovers_a_capability_over_everything() {
        let _turn = crate::turnstile::turn();
        let local = 0u64;
        let addr = core::ptr::addr_of!(local) as usize;

        let before = count(Origin::Unwatched);
        let cap = recover(addr as *const c_void);
        assert_eq!(count(Origin::Unwatched), before + 1);

        assert!(cap.covers(addr as u64, 8));
        assert_eq!(cap.meta.flags(), Meta::RECOVERED | Meta::WIDE);
        assert_eq!(cap.ver, plane::FOREIGN);
    }

    #[test]
    fn an_arena_nobody_has_carved_recovers_the_arena() {
        let _turn = crate::turnstile::turn();
        let arena = arena();

        let before = count(Origin::Mapping);
        let cap = recover((arena + 4096) as *const c_void);
        assert_eq!(count(Origin::Mapping), before + 1);

        assert!(cap.covers(arena as u64, ARENA as u64));
        assert_eq!(cap.meta.flags(), Meta::RECOVERED | Meta::WIDE);
        assert_eq!(cap.meta.class(), Class::Mapped as u8);
    }

    #[test]
    fn a_carried_capability_is_used_as_it_stands() {
        let _turn = crate::turnstile::turn();
        let carried = Cap::new(4096, 16, plane::begun(7), Meta::new(Class::Allocated, 3, 1));

        let before = counts().total();
        let cap = super::argument(carried, 4096 as *const c_void);
        assert_eq!(cap, carried);
        assert_eq!(counts().total(), before, "nothing was recovered");
    }

    #[test]
    fn an_argument_that_arrived_without_a_frame_is_recovered() {
        let _turn = crate::turnstile::turn();
        let ptr = alloc::alloc(32);

        let before = counts().total();
        let cap = super::argument(Cap::BOTTOM, ptr);
        assert_eq!(counts().total(), before + 1);
        assert_eq!(cap.lo, ptr as u64);
        assert_eq!(cap.ext, 32);

        free(ptr);
    }

    #[test]
    fn a_recovered_instance_stops_where_its_neighbour_starts() {
        let _turn = crate::turnstile::turn();
        let first = alloc::alloc(72);
        let second = alloc::alloc(72);
        assert!(!second.is_null());

        let cap = recover(first);
        // Eighty rather than the seventy two that was asked for, because what the walk finds is
        // the storage the instance owns and this allocator rounds a request up to a size class.
        // That over-approximation is the arena's, not recovery's, and it is the same one
        // `an_overflow_that_stays_inside_the_rounded_up_block_is_not_caught_yet` is about. Seventy
        // two rather than a round number precisely so that it is rounded, since a size the arena
        // gives exactly would make this test pass without testing anything.
        assert_eq!(cap.ext, 80);
        assert!(!cap.covers(second as u64, 1), "the walk stopped at the neighbour");

        free(second);
        free(first);
    }

    #[test]
    fn the_header_and_the_walk_say_the_same_thing_about_every_size() {
        let _turn = crate::turnstile::turn();
        // The point of reading the header is that it is the same answer for less work, so the two
        // are compared over a spread of sizes: one granule, a size class boundary, one that gets
        // rounded, and one large enough that the walk it replaces would be thousands of granules.
        for n in [1, 16, 72, 4096, 1 << 17] {
            let ptr = alloc::alloc(n);
            assert!(!ptr.is_null(), "{n}");

            let base = recover(ptr);
            // Halfway in, which for everything above a granule is past the first one, so that
            // address cannot take the header path and has to walk down to the base.
            let inside = recover(ptr.cast::<u8>().wrapping_add(n / 2).cast());
            assert_eq!(base.lo, ptr as u64, "{n}");
            assert_eq!(base.lo, inside.lo, "{n}");
            assert_eq!(base.ext, inside.ext, "{n}");
            assert_eq!(base.ver, inside.ver, "{n}");
            assert_eq!(base.meta, inside.meta, "{n}");

            free(ptr);
        }
    }

    #[test]
    fn every_address_in_an_instance_recovers_the_same_instance() {
        let _turn = crate::turnstile::turn();
        // What the doubling search has to get right. The first granule reads the header and every
        // other granule searches down to the base and up to the end, so an off by one in either
        // direction shows up as one address in the middle disagreeing with the rest. The neighbours
        // are there to give the search something to stop at other than the region's own edge, which
        // is the case a single allocation on its own would not exercise.
        let below = alloc::alloc(48);
        let ptr = alloc::alloc(1000);
        let above = alloc::alloc(48);
        assert!(!below.is_null() && !ptr.is_null() && !above.is_null());

        let whole = recover(ptr);
        assert_eq!(whole.lo, ptr as u64);
        assert!(whole.ext >= 1000, "{}", whole.ext);
        for offset in 0..whole.ext as usize {
            let got = recover(ptr.cast::<u8>().wrapping_add(offset).cast());
            assert_eq!(got.lo, whole.lo, "at {offset}");
            assert_eq!(got.ext, whole.ext, "at {offset}");
            assert_eq!(got.ver, whole.ver, "at {offset}");
        }

        free(below);
        free(ptr);
        free(above);
    }

    #[test]
    fn a_header_shaped_thing_in_somebody_elses_arena_is_not_believed() {
        let _turn = crate::turnstile::turn();
        // The reason `alloc::Watch::carved` exists. An adopted arena's blocks are laid out by
        // whoever adopted them, so the thirty two bytes in front of a payload are that program's
        // own bytes and it may put anything there, including something shaped exactly like one of
        // our headers. Believing it would be answering with an extent a program chose.
        let payload = arena() + 0x2000;
        // SAFETY: the mapping is this process's and these bytes are inside it.
        unsafe { crate::adopt::split(payload as *mut c_void, 64, 0) };
        let region = alloc::covering(payload).expect("the adopted arena is watched");
        // SAFETY: the region covers this address, so its plane is built over it.
        let version = unsafe { region.plane.version(payload) };

        let forged = Header {
            ext: 4096,
            ver: version,
            meta: Meta::new(Class::Allocated, crate::layout::perm::READ, 0),
            allocator: 1,
        };
        // SAFETY: the thirty two bytes in front of the payload are inside the same mapping, which
        // is this process's and is writable.
        unsafe { (crate::layout::header_of(payload) as *mut Header).write(forged) };

        let cap = recover(payload as *const c_void);
        assert_eq!(cap.lo, payload as u64);
        assert_eq!(cap.ext, 64, "the walk answered rather than the forgery");
    }

    #[test]
    fn the_header_of_an_instance_that_has_been_given_back_says_nothing() {
        let _turn = crate::turnstile::turn();
        // Freeing leaves the header in place with the ended version in it, and the plane holds the
        // same ended version, so the two agree. What stops the header being read is that nothing
        // owns the granule, which is decided before any of this: an address in a freed block is
        // `Origin::Nobody` and recovers the bottom capability.
        let ptr = alloc::alloc(64);
        free(ptr);
        assert!(recover(ptr).is_bottom());
    }
}
