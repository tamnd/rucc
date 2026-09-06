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
use crate::layout::{Cap, Class, Meta, perm};
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

/// The run of granules around `addr` that `version` owns, as a base and an extent.
///
/// Walked in both directions and stopped at the region's edges, which bounds it at the size of the
/// region in the worst case. That worst case is a single instance filling the whole region, and
/// walking it is linear in an instance's size rather than in the heap's.
fn run(region: &Region, addr: usize, version: Version) -> (usize, usize) {
    let here = addr & !(GRANULE - 1);

    let mut lo = here;
    // SAFETY: the walk stops at the region's base, so every granule it reads is one the plane
    // covers, which is what reading a version asks for.
    while lo > region.base && unsafe { region.plane.version(lo - GRANULE) } == version {
        lo -= GRANULE;
    }

    let mut hi = here + GRANULE;
    // SAFETY: as above, at the other end.
    while hi < region.end && unsafe { region.plane.version(hi) } == version {
        hi += GRANULE;
    }

    (lo, hi - lo)
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

    use super::{Cap, Meta, Origin, counts, recover, witness};
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
        let first = alloc::alloc(48);
        let second = alloc::alloc(48);
        assert!(!second.is_null());

        let cap = recover(first);
        // Sixty four rather than the forty eight that was asked for, because what the walk finds
        // is the storage the instance owns and this allocator rounds a request up to a size class.
        // That over-approximation is the arena's, not recovery's, and it is the same one
        // `an_overflow_that_stays_inside_the_rounded_up_block_is_not_caught_yet` is about.
        assert_eq!(cap.ext, 64);
        assert!(!cap.covers(second as u64, 1), "the walk stopped at the neighbour");

        free(second);
        free(first);
    }
}
