//! The five checks generated code calls, what each of them decides, and the one question it asks.
//!
//! Design: `spec/safe-memory/06-instrumentation.md` sections 6.3 and 6.3.1, and section 7.4 of
//! document 07 for [`extent`], which is not a check and is here because it reads the same plane
//! under the same rules about what the plane can and cannot see.
//!
//! # Why these are calls
//!
//! Section 6.3.1 asks for a compare and a branch in the function being checked, with only the trap
//! out of line. That is not what milestone S1 emits, and the reason is that the inline form needs
//! two things that do not exist yet. It needs the four word capability of
//! `spec/safe-memory/05-representation.md` section 5.2.1 live in registers at the check, and the
//! capability representation is milestone S2. It needs the aux plane to recover a capability for a
//! pointer that went through memory, and the aux plane is milestone S5. Until both are there, the
//! only thing a check can be handed is the address itself, and everything else has to be looked up.
//!
//! So S1 pays a call per check. That is a deliberate trade rather than something nobody noticed.
//! S1's exit criterion is that the checks are correct and that the overhead is written down; S4 is
//! the milestone that is about making the overhead small, and it has nothing to measure against
//! unless S1 produces an honest number. A baseline that flattered itself would be worse than none.
//!
//! # What these can see, and what they cannot
//!
//! All three planes. A version covers one granule of sixteen bytes, so the three checks that read
//! the lifetime plane decide per granule. The type plane's granule is eight bytes and it is per
//! byte inside a granule whose bytes disagree, so [`typed`] decides per byte, which it has to: a
//! structure with a `char` field in it disagrees within a granule and an access to the field beside
//! it must not be refused for that. The init plane has no granule at all and [`filled`] decides per
//! byte everywhere, which is what a question about a structure's padding needs.
//!
//! That is enough for the bugs the plane was built for. A read through a pointer to a freed
//! instance is refused, because the granule the free left behind is marked as given back and stays
//! that way after the address is handed out to somebody else. An access that starts inside an
//! instance and runs past it is refused, and so is a pointer that is walked off the end of the
//! object it came from.
//!
//! There are two things it is not enough for, and both are written down here rather than left for
//! somebody to find in a corpus run.
//!
//! The first is an overflow that stays inside the block the allocator rounded the request up to.
//! `malloc(17)` is served out of a thirty two byte payload, the plane says all thirty two bytes
//! belong to that instance, and a write to byte twenty is not caught. Closing that needs the exact
//! extent, which is in the header the allocator already writes, and reading a header per access is
//! the thing the aux plane exists to avoid.
//!
//! The second is a pointer that has landed in a different live instance before anything is read
//! through it. [`bounds`] asks whether an access straddles out of the instance its first byte is
//! in, and an access wholly inside somebody else's live instance does not straddle anything.
//! Deciding that needs the version the pointer was made with, which is what a capability is.
//!
//! Both are milestone S2 and S5 work, and neither is a surprise: they are the two places where a
//! judgement about a *capability* has been answered with a question about an *address*.
//!
//! # Addresses that are not the heap's
//!
//! Passed. A pointer to a local, to a global, or to memory some other allocator handed out is
//! outside the region, there is no plane covering it, and reporting on it would be a false positive
//! against a program doing nothing wrong. Milestone S1 instruments the heap, which is what its own
//! exit criterion is written against.

use core::ffi::c_void;

use crate::alloc::{self, Region};
use crate::fail::Descriptor;
use crate::plane::{self, Version};
use crate::types::{self, TypeId};

/// Judgement J1, the bounds half: an access of `size` bytes at `addr` stays in one instance, and
/// starts where an access of that alignment is allowed to start.
///
/// The first byte and the last byte have to be owned by the same version. An access that starts
/// inside an instance and ends past it lands in the next block's header, in the neighbour, or in
/// storage nobody owns, and all three read as a different version.
///
/// The alignment is here rather than in a check of its own because document 06 section 6.3 writes
/// it on `check_bounds`, and because a separate call would double the cost of the commonest check
/// in the program to test three bits. `align` is what the access is allowed to assume, so zero and
/// one both mean it assumes nothing and the test is skipped.
///
/// It is answered before anything reads a plane, and for an address no region covers as well as for
/// one the heap owns. Where the storage came from does not enter into it: `addr mod align = 0` is
/// document 04 section 4.4's conjunct and a local read through a pointer that lost three of its low
/// bits is the same S7 bug as a heap one. This is the only thing in this module that decides
/// anything about an address outside the region, and it is decidable there precisely because it
/// needs nothing that was recorded.
///
/// Whether there is an instance at all is not decided here. That is [`live`], and the two are kept
/// apart because the optimizer discharges them at very different rates: the bounds of an access at
/// a constant offset into a known object are usually provable and its liveness usually is not. One
/// fused check would have to survive whenever either half did.
///
/// # Panics
///
/// When the access is refused and [`crate::posture`] says to stop, which is the default. Under the
/// postures that carry on it says what happened and comes back, and the access goes ahead as
/// written, which is the recovery document 06 section 6.5 defines.
///
/// # Safety
///
/// `descriptor` is the address of a descriptor the same build wrote into `.rucc_safety_desc`, or
/// null. It is only read when the check refuses.
pub unsafe fn bounds(
    addr: *const c_void,
    size: usize,
    align: usize,
    descriptor: *const Descriptor,
) {
    let addr = addr as usize;
    // A mask rather than a remainder, because the divisor is a register here and a division per
    // access is not a thing to pay for a test of three bits. It is the right test because an
    // alignment is a power of two, which `rucc_ir::verify` refuses a check for not being.
    if align > 1 && addr & (align - 1) != 0 {
        // SAFETY: as below. The address is the one the access was about.
        unsafe { crate::fail::report(descriptor, Some(addr)) }
        // One report per access. Under the abort posture there is no second one to consider, and
        // under the postures that carry on a report that the same access is also out of bounds
        // would say the same J1 about the same line twice and count twice in the tally.
        return;
    }
    let Some(region) = alloc::covering(addr) else { return };
    // An access of no bytes reads nothing, so the last byte is the first one and the check is
    // trivially satisfied rather than reaching an address one before the pointer.
    let last = addr.wrapping_add(size.saturating_sub(1));
    if !region.holds(last) || owner(&region, addr) != owner(&region, last) {
        // SAFETY: the descriptor is this function's caller's to get right, and it is passed on
        // unchanged. The address is the one the access was about, which is what a report wants.
        unsafe { crate::fail::report(descriptor, Some(addr)) }
    }
}

/// Judgement J1, the lifetime half: something owns `addr` right now.
///
/// Which is weaker than the judgement document 04 section 4.4 states. The judgement is that the
/// capability the access goes through still names the owner, and this only says there is an owner,
/// because S1 has no capability in flight to compare against. What it catches is every access to
/// storage that has been freed and not handed out again, every access to storage that was never
/// allocated, and every access to the allocator's own headers, which between them is use after free
/// and the wilder half of a wild pointer. What it misses is an access through a stale pointer to an
/// address that has since been given to somebody else, and that is caught by S2 the moment a
/// capability carries a version.
///
/// # Panics
///
/// As [`bounds`].
///
/// # Safety
///
/// As [`bounds`].
pub unsafe fn live(addr: *const c_void, descriptor: *const Descriptor) {
    let addr = addr as usize;
    let Some(region) = alloc::covering(addr) else { return };
    if !plane::owned(owner(&region, addr)) {
        // SAFETY: as in `bounds`.
        unsafe { crate::fail::report(descriptor, Some(addr)) }
    }
}

/// Judgement J2: a pointer computed from another pointer did not leave the object it came from.
///
/// Caught where the arithmetic is rather than at whatever line eventually reads through the
/// result, which is what lets a report name the loop that ran too far.
///
/// One past the end is allowed, because C allows it. A program may compute the address just past
/// the last element of an array and compare against it, and it may not read through it. That
/// address is in the next granule and is owned by somebody else, so it has to be spelled out here:
/// a derived pointer is accepted when its own granule belongs to the base's instance, or when the
/// byte before it does. Reading through it is then refused by the access checks, which is exactly
/// the division of labour C describes.
///
/// One element before the start is allowed too, and C does not allow that. Document 03 section 3.1
/// widened judgement J2's window to `[lo - stride, hi]` and says why: `p = &a[-1]` followed by a
/// loop that pre-increments is how a great deal of ordinary C is written, SQLite's bytecode
/// interpreter is one instance of it, and the reverse walk whose final decrement computes the same
/// address is another. Neither reads anything outside the object. So the same trick is played at
/// the other end: a derived pointer below the base is accepted when the byte one stride further on
/// belongs to the base's instance, which is true exactly when the derivation landed within one
/// element of the object's first byte and false when it went further.
///
/// The stride is why this takes four arguments where the other two checks take three. It is the
/// width of one element of whatever is being stepped over, it comes from the derivation rather than
/// from the instance, and without it the difference between `&a[-1]` and `&a[-5]` is not visible
/// here at all.
///
/// A base that owns nothing passes. The pointer being derived from is already dead or was never an
/// instance, and saying so is [`live`]'s job at the access. Reporting it twice would mean one bug
/// producing two reports from two different judgements.
///
/// # Panics
///
/// As [`bounds`].
///
/// # Safety
///
/// As [`bounds`].
pub unsafe fn deriv(
    base: *const c_void,
    derived: *const c_void,
    stride: usize,
    descriptor: *const Descriptor,
) {
    let (base, derived) = (base as usize, derived as usize);
    let Some(region) = alloc::covering(base) else { return };
    let instance = owner(&region, base);
    if !plane::owned(instance) {
        return;
    }
    if region.holds(derived) && owner(&region, derived) == instance {
        return;
    }
    let before = derived.wrapping_sub(1);
    if derived > base && region.holds(before) && owner(&region, before) == instance {
        return;
    }
    let after = derived.wrapping_add(stride);
    if derived < base && region.holds(after) && owner(&region, after) == instance {
        return;
    }
    // The derived address rather than the base, because the base is where the pointer was allowed
    // to be and the derived one is where it went. The base goes along beside it so that the report
    // can name the object the derivation should have stayed in and say how far short of it or past
    // it the result landed.
    // SAFETY: as in `bounds`.
    unsafe { crate::fail::report_from(descriptor, Some(derived), Some(base)) }
}

/// Judgement J3: the bytes this access is about to read agree with the type it is reading them as.
///
/// The effective type rule of C 6.5, asked of the plane `crate::types` holds. What it catches is
/// reading a `struct A` back as a `struct B`, reading a word assembled out of the bytes of two
/// pointers as a pointer, and reading through a pointer the program obtained by casting one
/// unrelated type to another. What it passes is everything C permits, which is a byte nothing has
/// stored through, anything either side of a character type, and the types agreeing.
///
/// An access that runs past the end of the region is asked about only as far as the region goes.
/// The bytes past it are somebody else's or nobody's, there is no plane over them, and the access
/// straddling out at all is already [`bounds`]'s refusal rather than a second one from here.
///
/// # Panics
///
/// As [`bounds`].
///
/// # Safety
///
/// As [`bounds`]. `ty` is a plane vocabulary entry and is not an address.
pub unsafe fn typed(addr: *const c_void, size: usize, ty: TypeId, descriptor: *const Descriptor) {
    let addr = addr as usize;
    let Some(region) = alloc::covering(addr) else { return };
    // SAFETY: the range is clipped to the region, whose type plane covers every granule of it.
    if !unsafe { region.types.allows(addr, clipped(&region, addr, size), ty) } {
        // SAFETY: as in `bounds`.
        unsafe { crate::fail::report(descriptor, Some(addr)) }
    }
}

/// The judgement a store makes: the bytes it wrote were stored through a `ty`.
///
/// Not a check. It refuses nothing and reports nothing, it records the fact [`typed`] later asks
/// about, and a store to an address no plane covers records nothing at all.
///
/// # Safety
///
/// `addr` is whatever the program computed and is never read through. `ty` is a plane vocabulary
/// entry and is not an address.
pub unsafe fn judge(addr: *const c_void, size: usize, ty: TypeId) {
    let addr = addr as usize;
    let Some(region) = alloc::covering(addr) else { return };
    // SAFETY: as in `typed`.
    unsafe { region.types.set(addr, clipped(&region, addr, size), ty) }
}

/// The judgement a copy makes: the bytes at `dst` now say whatever the bytes at `src` say.
///
/// C 6.5 says a copy through `memcpy` or through a character array carries the source's effective
/// type, so this is what a wrapper in [`crate::wrap`] calls once it knows how much was copied, and
/// it is what keeps the punning idiom the standard permits from being reported.
///
/// Two ranges in one region is the case worth having and is what this is written for. A copy whose
/// ends are in different regions, or whose source is outside every region, records the destination
/// as untyped instead of walking a region table per byte. That is the same thinning as running out
/// of side entries and for the same reason: it is a lost check rather than a wrong answer, and the
/// alternative is a lookup per byte on the path every `memcpy` in the program goes down.
///
/// # Safety
///
/// Neither address is read through. They may overlap, and the answer is the same either way.
pub unsafe fn carry(dst: *const c_void, src: *const c_void, len: usize) {
    let (dst, src) = (dst as usize, src as usize);
    let Some(region) = alloc::covering(dst) else { return };
    let len = clipped(&region, dst, len);
    if region.holds(src) && region.holds(src.wrapping_add(len.saturating_sub(1))) {
        // SAFETY: both ranges are inside the region, whose type plane covers every granule of it.
        unsafe { region.types.copy(dst, src, len) }
        return;
    }
    // SAFETY: as above, for the destination alone.
    unsafe { region.types.set(dst, len, types::UNTYPED) }
}

/// Judgement J1, the init half: every byte this access is about to read has been written.
///
/// Document 03's Y6, and the kernel infoleak of CWE-200 with it. What it catches is a read of a
/// member the program never filled, a read of a structure's padding, and a buffer handed to `write`
/// or to a socket with bytes in it nothing ever stored. What it passes is everything a byte nobody
/// has said anything about would be, which is the inversion `crate::init` argues for: an
/// instance beginning is the only thing that makes a byte unwritten, so uninstrumented code writing
/// storage this crate watches loses a check rather than inventing a refusal.
///
/// Clipped to the region for the reason [`typed`] is, and the refusal is the same judgement for the
/// reason [`crate::init::Init::allows`] gives: J1's own wording is about an access the planes did
/// not permit, and this is one of the planes.
///
/// # Panics
///
/// As [`bounds`].
///
/// # Safety
///
/// As [`bounds`].
pub unsafe fn filled(addr: *const c_void, size: usize, descriptor: *const Descriptor) {
    let addr = addr as usize;
    let Some(region) = alloc::covering(addr) else { return };
    // SAFETY: the range is clipped to the region, whose init plane covers every byte of it.
    if !unsafe { region.init.allows(addr, clipped(&region, addr, size)) } {
        // SAFETY: as in `bounds`.
        unsafe { crate::fail::report(descriptor, Some(addr)) }
    }
}

/// The judgement a store makes: the bytes it wrote hold what it wrote.
///
/// Not a check, the same way [`judge`] is not. Which range a store that writes a whole object names
/// is section 9.3's padding rule and is the compiler's decision rather than this crate's, and
/// `crate::init` says why it has to be: a member by member fill leaves the padding alone and a
/// whole object store does not, and the only difference between the two by the time they arrive
/// here is the length.
///
/// # Safety
///
/// `addr` is whatever the program computed and is never read through.
pub unsafe fn wrote(addr: *const c_void, size: usize) {
    let addr = addr as usize;
    let Some(region) = alloc::covering(addr) else { return };
    // SAFETY: as in `filled`.
    unsafe { region.init.set(addr, clipped(&region, addr, size)) }
}

/// The judgement a copy makes: the bytes at `dst` were written wherever the bytes at `src` were.
///
/// Which is what makes an infoleak visible rather than what hides it. A structure filled member by
/// member and then copied whole carries its padding along with it, so the bytes that would leave
/// the program are still the bytes nothing wrote and the read at the boundary is the one refused.
///
/// A copy whose ends are in different regions, or whose source is outside every region, marks the
/// destination as written. That is the permissive direction, which is the one this plane thins in,
/// and it is the same trade [`carry`] makes for the same reason: the alternative is a region lookup
/// per byte on the path every `memcpy` in the program goes down.
///
/// # Safety
///
/// Neither address is read through. They may overlap, and the answer is the same either way.
pub unsafe fn spread(dst: *const c_void, src: *const c_void, len: usize) {
    let (dst, src) = (dst as usize, src as usize);
    let Some(region) = alloc::covering(dst) else { return };
    let len = clipped(&region, dst, len);
    if region.holds(src) && region.holds(src.wrapping_add(len.saturating_sub(1))) {
        // SAFETY: both ranges are inside the region, whose init plane covers every byte of it.
        unsafe { region.init.copy(dst, src, len) }
        return;
    }
    // SAFETY: as above, for the destination alone.
    unsafe { region.init.set(dst, len) }
}

/// The judgement a call out of this build makes: whatever it was handed may now hold something.
///
/// A pointer passed to a function this compiler did not build goes somewhere nothing reports from.
/// The callee may have filled every byte of it, and the plane did not hear about any of it, so the
/// next read here would be refused over storage that was written in front of us. Document 10
/// section 10.1's rule is never to assume, and the only thing this crate knows after such a call is
/// that it no longer knows.
///
/// So the whole instance is marked written, rather than the argument's own range, because the
/// callee was handed a pointer and not a length and there is nothing in the call that says how far
/// it went. That is the permissive direction, which is the one section 9.2 says this plane thins
/// in, and what it costs is every uninitialized read of an instance that was ever handed out.
///
/// Nothing happens for an address outside every region, or one in an arena whose allocator has said
/// nothing, since there is no instance there to say anything about.
///
/// # Safety
///
/// `addr` is whatever the program computed and is never read through.
pub unsafe fn handed(addr: *const c_void) {
    let addr = addr as usize;
    let Some(region) = alloc::covering(addr) else { return };
    let Some((lo, len)) = crate::recover::extent(&region, addr) else { return };
    // SAFETY: the run came out of the plane over this region, so the init plane covers it too.
    unsafe { region.init.set(lo, len) }
}

/// How many of the `size` bytes from `addr` on are inside the region, so a plane walk stays inside
/// the plane.
///
/// `addr` is in the region, which every caller has established, so this is never zero for an
/// access of at least one byte.
const fn clipped(region: &Region, addr: usize, size: usize) -> usize {
    let room = region.end - addr;
    if size < room { size } else { room }
}

/// How many of the `want` bytes from `addr` on belong to whoever owns `addr`.
///
/// Not a judgement. Nothing here refuses anything and nothing here reports anything, because this
/// is a question the compiler asks before a loop runs so that it can leave the checks out of part
/// of it. `spec/safe-memory/07-check-elimination.md` section 7.4 splits a loop at
/// `min(n, extent / sizeof(T))`, runs that part with no checks in it and runs whatever is left with
/// them, and this is where the extent comes from.
///
/// The answer is never more than `want`, and it is allowed to be less than the truth. What is not
/// allowed is an answer larger than the truth, since that is unchecked reads past the end of an
/// object in the half of a split loop that has no checks in it.
///
/// # Why this does not walk
///
/// It used to. The first version read one granule at a time from `addr` until the version changed
/// or `want` was covered, which is an eighth of the work the loop was about to do anyway, and that
/// sounded like a bound. It is not one, because the query is asked in front of the loop and a loop
/// in front of another loop is asked once per iteration of the outer one. `tamnd/rucc#861` is a
/// matrix multiply where the innermost loop's guard asks about three hundred kilobytes forty
/// thousand times, and ninety four percent of the program's instructions were spent in here.
///
/// So the far end is probed first. A version names one instance and an instance is one run of
/// granules with nothing else in the middle of it, which is the invariant `crate::plane` states, so
/// the granule holding the last byte asked about answering with the same version settles every
/// granule between. That is two reads for the common case of a query that fits inside the object.
/// When it does not fit, the boundary is somewhere between a granule that answered and one that did
/// not, and halving finds it in as many reads as the region has bits of address rather than as many
/// as it has granules.
///
/// Two answers are worth spelling out.
///
/// An address no watched region covers gets `want` back. Every check passes for those, so there is
/// nothing a checked half of a loop over one would ever catch, and saying so here is what makes the
/// split collapse to the loop the program wrote. It is the same answer [`bounds`] and [`live`] give
/// and for the same reason: a build that instruments the heap has nothing to say about a local, a
/// global, or storage an allocator nobody told us about handed out.
///
/// An address whose granule is owned by nobody gets zero. It is already dead or was never an
/// instance, the checked half starts at the first iteration, and the check in there is what reports
/// it. Deciding it here would report the loop rather than the access.
#[must_use]
pub fn extent(addr: *const c_void, want: usize) -> usize {
    let addr = addr as usize;
    let Some(region) = alloc::covering(addr) else { return want };
    let instance = owner(&region, addr);
    if !plane::owned(instance) || want == 0 {
        return 0;
    }
    // The last byte asked about, held to the region so that every address probed below is one the
    // plane covers and none of the arithmetic here can wrap. Nothing past the region belongs to the
    // instance anyway, so clamping loses nothing.
    let last = addr.saturating_add(want - 1).min(region.end - 1);
    let start = addr - addr % plane::GRANULE;
    let reached = reach(&region, start, (last - start) / plane::GRANULE, instance, STEP);
    // The rest of the granule the address is in, which is owned by definition since the version
    // that covers the address covers every byte that shares its slot, and then a granule for each
    // one past it that answered. Starting at the next granule would say nothing about an address
    // in the middle of one.
    let covered = reached * plane::GRANULE + (plane::GRANULE - addr % plane::GRANULE);
    covered.min(want)
}

/// How many of the `want` bytes ending at `addr` the instance owning the byte below it covers.
///
/// The mirror of [`extent`], for the loop that walks from high to low. An answer of `n` says that
/// `[addr - n, addr)` belongs to one instance, so a walk whose furthest address is `n` bytes below
/// `addr` stays inside whatever owns it.
///
/// The address is one past what is asked about, which is what makes the question the mirror of the
/// other one rather than an awkward variant of it. The caller has an address the loop reads from and
/// a size it reads, so what it hands over is the end of that first access, and the answer is measured
/// down from there. Ownership is therefore read at `addr - 1`, since `addr` itself may be one past
/// the end of the object and the object is what is being asked about.
///
/// Everything else is [`extent`]'s: never more than `want`, allowed to be less than the truth and
/// never more, `want` back for an address no watched region covers, zero for an address whose
/// granule is owned by nobody, and the far end probed rather than walked to.
#[must_use]
pub fn extent_back(addr: *const c_void, want: usize) -> usize {
    let addr = addr as usize;
    if addr == 0 || want == 0 {
        return 0;
    }
    let last = addr - 1;
    let Some(region) = alloc::covering(last) else { return want };
    let instance = owner(&region, last);
    if !plane::owned(instance) {
        return 0;
    }
    // The lowest byte asked about, held to the region for the reason the forward query gives, and
    // which is also what keeps the count from running below address zero.
    let lowest = last.saturating_sub(want - 1).max(region.base);
    let start = last - last % plane::GRANULE;
    let floor = lowest - lowest % plane::GRANULE;
    let reached = reach(&region, start, (start - floor) / plane::GRANULE, instance, -STEP);
    // The part of the granule the last byte is in that lies below the address, which is owned by
    // definition, and then a granule for each one below it that answered.
    let covered = reached * plane::GRANULE + last % plane::GRANULE + 1;
    covered.min(want)
}

/// One granule on, as the offset [`reach`] steps by.
const STEP: isize = plane::GRANULE as isize;

/// How many granules past the one at `start` still answer with `instance`, out of `span` of them.
///
/// `step` is [`STEP`] to look up the address space and its negation to look down. The answer is the
/// largest `n` no greater than `span` for which every granule from `start` to `start + n * step`
/// belongs to `instance`, and the caller has already established that the granule at `start` does.
///
/// The far end is probed first and the rest is a halving, which is sound because a run of granules
/// carrying one version has nothing else in the middle of it. `crate::plane` states that invariant
/// and says what rests on it, and this is the thing that rests on it: a probe that skipped over a
/// hole would answer for granules nobody looked at.
///
/// Every address reached is inside the region, because both callers work `span` out from the
/// region's own bounds before getting here.
fn reach(region: &Region, start: usize, span: usize, instance: Version, step: isize) -> usize {
    let at = |granules: usize| start.wrapping_add_signed(step * granules as isize);
    if owner(region, at(span)) == instance {
        return span;
    }
    // Halve between a granule that answered and one that did not. The first answered because the
    // caller read it before calling, and the last did not, which is what the probe above found.
    let (mut yes, mut no) = (0, span);
    while no - yes > 1 {
        let mid = yes + (no - yes) / 2;
        if owner(region, at(mid)) == instance {
            yes = mid;
        } else {
            no = mid;
        }
    }
    yes
}

/// The version that owns `addr`.
///
/// A plain function rather than a method because every caller has already established the one
/// thing reading the plane asks for, which is that the address is inside the region.
fn owner(region: &Region, addr: usize) -> Version {
    // SAFETY: `addr` is inside the region the plane was built over, which every caller checks
    // with `holds` before getting here.
    unsafe { region.plane.version(addr) }
}

/// The twelve names generated code is compiled against.
///
/// Separate from the functions above for the reason the allocator's exports are separate from its
/// logic: these are an ABI and those are Rust. The one difference that matters is that a panic may
/// not cross an `extern "C"` boundary, so a test that calls one of these to watch it refuse would
/// abort the harness rather than see a refusal. The tests call the plain functions.
///
/// The checks take the descriptor last, so that the argument registers the address and the size
/// arrive in are the ones they would already be in. Neither extent query has a descriptor, because
/// they decide nothing and so have nothing to report.
pub mod exports {
    use core::ffi::c_void;

    use crate::fail::Descriptor;

    /// # Safety
    ///
    /// Called from generated code with the address of a descriptor the same build wrote into
    /// `.rucc_safety_desc`. `addr` is whatever the program computed and is never read through.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_bounds(
        addr: *const c_void,
        size: usize,
        align: usize,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::bounds(addr, size, align, descriptor) };
    }

    /// # Safety
    ///
    /// As [`__rucc_check_bounds`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_live(addr: *const c_void, descriptor: *const Descriptor) {
        // SAFETY: as above.
        unsafe { super::live(addr, descriptor) };
    }

    /// # Safety
    ///
    /// As [`__rucc_check_bounds`], for both pointers. `stride` is a width and is not read through.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_deriv(
        base: *const c_void,
        derived: *const c_void,
        stride: usize,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: as above.
        unsafe { super::deriv(base, derived, stride, descriptor) };
    }

    /// How many of the `want` bytes from `addr` on the instance owning `addr` covers.
    ///
    /// Safe, unlike the three above, because it takes no descriptor and reads nothing through the
    /// address it is handed. What it asks is the plane about an address, and an address it knows
    /// nothing about is an answer rather than undefined behaviour.
    #[unsafe(no_mangle)]
    pub extern "C" fn __rucc_extent(addr: *const c_void, want: usize) -> usize {
        super::extent(addr, want)
    }

    /// How many of the `want` bytes ending at `addr` the instance owning the byte below it covers.
    ///
    /// Safe for the same reason the one above is, and the address it is handed is one past what it
    /// is asked about, which is what a walk from high to low hands over.
    #[unsafe(no_mangle)]
    pub extern "C" fn __rucc_extent_back(addr: *const c_void, want: usize) -> usize {
        super::extent_back(addr, want)
    }

    /// # Safety
    ///
    /// As [`__rucc_check_bounds`]. `ty` is a plane vocabulary entry and is not an address.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_type(
        addr: *const c_void,
        size: usize,
        ty: u32,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::typed(addr, size, ty, descriptor) };
    }

    /// # Safety
    ///
    /// `addr` is whatever the program computed and is never read through, and `ty` is a plane
    /// vocabulary entry. No descriptor, because a judgement decides nothing and so has nothing to
    /// report.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_meta_type(addr: *const c_void, size: usize, ty: u32) {
        // SAFETY: as above.
        unsafe { super::judge(addr, size, ty) };
    }

    /// # Safety
    ///
    /// As [`__rucc_meta_type`], for both addresses. Neither is read through and they may overlap.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_meta_type_copy(
        dst: *const c_void,
        src: *const c_void,
        len: usize,
    ) {
        // SAFETY: as above.
        unsafe { super::carry(dst, src, len) };
    }

    /// # Safety
    ///
    /// As [`__rucc_check_bounds`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_init(
        addr: *const c_void,
        size: usize,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::filled(addr, size, descriptor) };
    }

    /// # Safety
    ///
    /// `addr` is whatever the program computed and is never read through. No descriptor, for the
    /// reason [`__rucc_meta_type`] has none.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_meta_init(addr: *const c_void, size: usize) {
        // SAFETY: as above.
        unsafe { super::wrote(addr, size) };
    }

    /// # Safety
    ///
    /// As [`__rucc_meta_init`], for both addresses. Neither is read through and they may overlap.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_meta_init_copy(
        dst: *const c_void,
        src: *const c_void,
        len: usize,
    ) {
        // SAFETY: as above.
        unsafe { super::spread(dst, src, len) };
    }

    /// # Safety
    ///
    /// As [`__rucc_meta_init`]. One address, and it is never read through.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_meta_init_handed(addr: *const c_void) {
        // SAFETY: as above.
        unsafe { super::handed(addr) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc::{alloc, dealloc};
    use crate::turnstile::turn;

    /// The descriptor every check in these tests is handed.
    ///
    /// A real one rather than a null, because that is what generated code passes and the reporter
    /// reads it. What is in it does not matter here: these tests are about which accesses are
    /// refused, and what a refusal says is `crate::report`'s tests.
    static ROW: Descriptor = Descriptor { judgement: 1, class: 0, size: 0, pc: 0 };

    /// The bounds check, with the descriptor argument filled in.
    ///
    /// This and the two below shadow the functions they call. What is unsafe about each of those
    /// is the descriptor it is handed, every test here hands it the same real one, and saying so
    /// once rather than at every call site keeps the tests about which accesses are refused.
    fn bounds(addr: *const c_void, size: usize) {
        // SAFETY: the address of a `static`, which is what a descriptor is at run time too.
        unsafe { super::bounds(addr, size, 1, &raw const ROW) }
    }

    /// The bounds check over an access that is allowed to assume an alignment.
    ///
    /// Kept apart from [`bounds`] so that every test above that is about where an access lands
    /// says nothing about alignment, which is what passing one means.
    fn aligned(addr: *const c_void, size: usize, align: usize) {
        // SAFETY: as above.
        unsafe { super::bounds(addr, size, align, &raw const ROW) }
    }

    /// The liveness check, the same way.
    fn live(addr: *const c_void) {
        // SAFETY: as above.
        unsafe { super::live(addr, &raw const ROW) }
    }

    /// The derivation check, the same way, over a stride of one byte.
    ///
    /// One byte because that is what character arithmetic has and it is the narrowest window the
    /// low end of the rule can open. The tests that are about the width pass their own.
    fn deriv(base: *const c_void, derived: *const c_void) {
        // SAFETY: as above.
        unsafe { super::deriv(base, derived, 1, &raw const ROW) }
    }

    /// The derivation check over a stride the caller picks.
    fn stepped(base: *const c_void, derived: *const c_void, stride: usize) {
        // SAFETY: as above.
        unsafe { super::deriv(base, derived, stride, &raw const ROW) }
    }

    /// The type check, the same way.
    fn typed(addr: *const c_void, size: usize, ty: TypeId) {
        // SAFETY: as above.
        unsafe { super::typed(addr, size, ty, &raw const ROW) }
    }

    /// The judgement a store makes, which takes no descriptor and refuses nothing.
    fn judge(addr: *const c_void, size: usize, ty: TypeId) {
        // SAFETY: the address is one an instance in the test owns and is never read through.
        unsafe { super::judge(addr, size, ty) }
    }

    /// The judgement a copy makes.
    fn carry(dst: *const c_void, src: *const c_void, len: usize) {
        // SAFETY: as above, for both.
        unsafe { super::carry(dst, src, len) }
    }

    /// The init check, with the descriptor argument filled in.
    fn filled(addr: *const c_void, size: usize) {
        // SAFETY: as in `bounds`.
        unsafe { super::filled(addr, size, &raw const ROW) }
    }

    /// The judgement a store makes about what it wrote.
    fn wrote(addr: *const c_void, size: usize) {
        // SAFETY: the address is one an instance in the test owns and is never read through.
        unsafe { super::wrote(addr, size) }
    }

    /// The judgement a copy makes about what it moved.
    fn spread(dst: *const c_void, src: *const c_void, len: usize) {
        // SAFETY: as above, for both.
        unsafe { super::spread(dst, src, len) }
    }

    /// The judgement a call out of the build makes about what it was handed.
    fn handed(addr: *const c_void) {
        // SAFETY: as above.
        unsafe { super::handed(addr) }
    }

    /// Two types out of the compiler's universe, in the spelling the plane gives them.
    const A: TypeId = types::interned(0);
    const B: TypeId = types::interned(1);

    /// Runs one check and says whether it refused, without the panic reaching the harness.
    ///
    /// The hook is swapped so that a refusal a test is asking for does not print a backtrace and
    /// read as a failure. Every caller holds the turnstile, so the swap is not racing anything.
    fn refused(check: impl FnOnce()) -> bool {
        let hook = std::panic::take_hook();
        std::panic::set_hook(std::boxed::Box::new(|_| {}));
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(check));
        std::panic::set_hook(hook);
        out.is_err()
    }

    /// The address `offset` bytes into an instance, as the checks take it.
    fn at(ptr: *mut c_void, offset: usize) -> *const c_void {
        ptr.cast::<u8>().wrapping_add(offset).cast()
    }

    #[test]
    fn reading_bytes_back_as_the_type_they_were_stored_through_is_allowed() {
        let _turn = turn();
        // The case that has to be silent, which is nearly every access in a program that is doing
        // nothing wrong, and the one a type checker nobody deploys gets wrong.
        let ptr = alloc(64);
        judge(at(ptr, 0), 32, A);
        assert!(!refused(|| typed(at(ptr, 0), 32, A)));
        assert!(!refused(|| typed(at(ptr, 8), 8, A)));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_block_handed_out_again_says_nothing_about_its_bytes() {
        let _turn = turn();
        // The other half of judgement J4. Allocated storage has no declared type, so what the
        // previous occupant of a block stored through is forgotten when the block is handed out
        // again. Leaving it would report the new owner's first honest read as type confusion,
        // which is the false positive that would make the plane unusable.
        let first = alloc(64);
        judge(at(first, 0), 64, A);
        // SAFETY: `first` is a live instance.
        unsafe { dealloc(first) };

        let second = alloc(64);
        assert_eq!(second, first, "the test is about a block that came back");
        assert!(!refused(|| typed(at(second, 0), 64, B)));
        // SAFETY: as above.
        unsafe { dealloc(second) };
    }

    #[test]
    fn reading_bytes_back_as_a_different_type_is_refused() {
        let _turn = turn();
        // Judgement J3, and the class the plane exists for.
        let ptr = alloc(64);
        judge(at(ptr, 0), 32, A);
        assert!(refused(|| typed(at(ptr, 0), 32, B)));
        // SAFETY: as above.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn bytes_nothing_has_stored_through_read_as_anything() {
        let _turn = turn();
        // A fresh instance says nothing about its bytes, so the first access decides, which is
        // C's rule for storage with no declared type and is what keeps the plane quiet at a
        // boundary with code this compiler did not build.
        let ptr = alloc(64);
        assert!(!refused(|| typed(at(ptr, 0), 8, A)));
        assert!(!refused(|| typed(at(ptr, 0), 8, B)));
        // SAFETY: as above.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_character_access_reads_anything_and_a_character_store_is_read_as_anything() {
        let _turn = turn();
        // Both halves of C 6.5's character rule, which between them are what makes an open coded
        // copy loop legal and therefore what a checker has to not report.
        let ptr = alloc(64);
        judge(at(ptr, 0), 8, A);
        assert!(!refused(|| typed(at(ptr, 0), 8, types::CHARACTER)));
        judge(at(ptr, 3), 1, types::CHARACTER);
        assert!(!refused(|| typed(at(ptr, 0), 8, A)), "one character byte does not refuse it");
        // SAFETY: as above.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_copy_carries_the_types_it_copied_and_the_destination_is_read_as_those() {
        let _turn = turn();
        // The `memcpy` rule, which is what makes punning through a copy legal where C says it is
        // and is also what catches a `struct A` copied over a `struct B` and read back as a `B`.
        let ptr = alloc(128);
        judge(at(ptr, 0), 32, A);
        judge(at(ptr, 64), 32, B);
        assert!(!refused(|| typed(at(ptr, 64), 32, B)));

        carry(at(ptr, 64), at(ptr, 0), 32);

        assert!(refused(|| typed(at(ptr, 64), 32, B)), "the bytes are an A now");
        assert!(!refused(|| typed(at(ptr, 64), 32, A)));
        // SAFETY: as above.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_copy_out_of_somewhere_no_plane_covers_leaves_the_destination_saying_nothing() {
        let _turn = turn();
        // A lost check rather than a wrong answer, which is the direction every thinning in this
        // plane goes. The source here is a local, which no region holds.
        let ptr = alloc(64);
        judge(at(ptr, 0), 32, A);
        let outside = 0u64;

        carry(at(ptr, 0), (&raw const outside).cast(), 8);

        assert!(!refused(|| typed(at(ptr, 0), 8, A)));
        assert!(!refused(|| typed(at(ptr, 0), 8, B)));
        // SAFETY: as above.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn reading_a_byte_of_a_fresh_instance_before_anything_wrote_it_is_refused() {
        let _turn = turn();
        // Document 03's Y6, which is the class this plane exists for. The bytes hold whatever the
        // previous occupant of the block left, and a program that reads one of them is reading a
        // value it never stored.
        let ptr = alloc(64);
        assert!(refused(|| filled(at(ptr, 0), 8)));

        wrote(at(ptr, 0), 8);

        assert!(!refused(|| filled(at(ptr, 0), 8)));
        assert!(refused(|| filled(at(ptr, 8), 8)), "the bytes past the store were not written");
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_read_that_straddles_a_byte_nothing_wrote_is_refused() {
        let _turn = turn();
        // The plane is per byte everywhere, which is what a question about a structure's padding
        // needs: two members filled and the two bytes between them not is the ordinary shape of a
        // member by member fill rather than a corner case.
        let ptr = alloc(64);
        wrote(at(ptr, 0), 2);
        wrote(at(ptr, 4), 4);

        assert!(!refused(|| filled(at(ptr, 0), 2)));
        assert!(!refused(|| filled(at(ptr, 4), 4)));
        assert!(refused(|| filled(at(ptr, 0), 8)), "the padding between them was read");
        // SAFETY: as above.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_block_handed_out_again_has_written_none_of_its_bytes() {
        let _turn = turn();
        // The other half of judgement J4, which is the only thing in the design that makes a byte
        // unwritten. A block that came back still holds the last owner's data, and the plane
        // saying so is what turns the next read of it into the report it should be.
        let first = alloc(64);
        wrote(at(first, 0), 64);
        assert!(!refused(|| filled(at(first, 0), 64)));
        // SAFETY: `first` is a live instance.
        unsafe { dealloc(first) };

        let second = alloc(64);
        assert_eq!(second, first, "the test is about a block that came back");
        assert!(refused(|| filled(at(second, 0), 8)));
        // SAFETY: as above.
        unsafe { dealloc(second) };
    }

    #[test]
    fn a_copy_carries_the_padding_the_source_never_filled() {
        let _turn = turn();
        // The infoleak, written out. A structure filled member by member and copied whole into a
        // buffer that was fully written has to make the destination's padding unreadable again,
        // because those are the bytes that would leave the program.
        let ptr = alloc(128);
        wrote(at(ptr, 0), 2);
        wrote(at(ptr, 4), 4);
        wrote(at(ptr, 64), 8);
        assert!(!refused(|| filled(at(ptr, 64), 8)));

        spread(at(ptr, 64), at(ptr, 0), 8);

        assert!(!refused(|| filled(at(ptr, 64), 2)));
        assert!(refused(|| filled(at(ptr, 64), 8)), "the padding did not come across");
        // SAFETY: as above.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_copy_out_of_somewhere_no_plane_covers_leaves_the_destination_written() {
        let _turn = turn();
        // The permissive direction, which is the one every thinning in this plane goes in. The
        // source is a local, no region holds it, and the bytes it wrote are bytes the program did
        // store even though nothing here watched it happen.
        let ptr = alloc(64);
        let outside = 0u64;

        spread(at(ptr, 0), (&raw const outside).cast(), 8);

        assert!(!refused(|| filled(at(ptr, 0), 8)));
        // SAFETY: as above.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_pointer_handed_out_of_the_build_leaves_its_whole_instance_written() {
        let _turn = turn();
        // Section 10.7's incremental adoption, from this plane's side. The library was handed a
        // pointer and not a length, so there is nothing in the call that says how far it went, and
        // the only honest answer about an instance nothing here watched being written is that the
        // question can no longer be asked about it.
        let ptr = alloc(64);
        assert!(refused(|| filled(at(ptr, 0), 8)));

        handed(at(ptr, 0));

        assert!(!refused(|| filled(at(ptr, 0), 8)));
        assert!(!refused(|| filled(at(ptr, 56), 8)), "the instance runs past where the call began");
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_pointer_handed_out_says_nothing_about_the_instance_beside_it() {
        let _turn = turn();
        // The run stops where the instance does, which is what keeps one call to a library from
        // silencing the whole heap.
        let first = alloc(64);
        let second = alloc(64);
        assert_ne!(first, second, "two instances, so there is a neighbour to be wrong about");

        handed(at(first, 0));

        assert!(!refused(|| filled(at(first, 0), 8)));
        assert!(refused(|| filled(at(second, 0), 8)), "the neighbour was written off too");
        // SAFETY: both are live instances.
        unsafe {
            dealloc(first);
            dealloc(second);
        }
    }

    #[test]
    fn a_pointer_handed_out_that_no_region_covers_records_nothing() {
        let _turn = turn();
        // A local or a global going to a library. There is no instance to say anything about, and
        // walking one out of a region that does not exist would be inventing it.
        let mut local = [0_u8; 64];
        let addr: *const c_void = local.as_mut_ptr().cast();
        handed(addr);
        assert!(!refused(|| filled(addr, 64)));
    }

    #[test]
    fn a_read_of_storage_no_region_covers_is_passed() {
        let _turn = turn();
        // A local is uninitialized in exactly the way this plane is about and there is no plane
        // over it to say so. Refusing here would need a plane over every frame, which is what a
        // later milestone is for, and inventing an answer would be a report about a program this
        // build cannot see.
        let local = 0u64;

        assert!(!refused(|| filled((&raw const local).cast(), 8)));
    }

    #[test]
    fn an_address_no_region_covers_is_passed_and_records_nothing() {
        let _turn = turn();
        // A local, a global, or another allocator's memory. There is no plane over it, so there
        // is nothing to ask and nothing to record, and reporting on it would be a false positive
        // against a program doing nothing wrong.
        let outside = 0u64;
        let addr: *const c_void = (&raw const outside).cast();
        judge(addr, 8, A);
        assert!(!refused(|| typed(addr, 8, B)));
    }

    #[test]
    fn an_access_inside_a_live_instance_is_allowed() {
        let _turn = turn();
        // The case that has to be silent, and there are far more of these in a real program than
        // of anything else in this file.
        let ptr = alloc(64);
        for offset in [0, 1, 32, 60] {
            assert!(!refused(|| bounds(at(ptr, offset), 4)), "offset {offset}");
            assert!(!refused(|| live(at(ptr, offset))), "offset {offset}");
        }
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn an_access_that_starts_where_its_alignment_does_not_allow_is_refused() {
        let _turn = turn();
        // Row S7. The allocator hands out sixteen byte aligned storage, so an int read one byte
        // into it is misaligned and one read four bytes in is not. Nothing about this is out of
        // bounds and the instance is live, which is why it takes its own conjunct to catch.
        let ptr = alloc(64);
        assert!(!refused(|| aligned(at(ptr, 4), 4, 4)));
        assert!(refused(|| aligned(at(ptr, 1), 4, 4)));
        assert!(refused(|| aligned(at(ptr, 2), 4, 4)));
        // The same address under a wider access, which is the wire format case: four bytes in is
        // a fine place for an int and not for a long.
        assert!(refused(|| aligned(at(ptr, 4), 8, 8)));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn an_access_that_assumes_nothing_about_where_it_starts_is_allowed_anywhere() {
        let _turn = turn();
        // A char access, and every access the front end could not work an alignment out for. One
        // and zero both mean the same thing here, which is that there is nothing to test.
        let ptr = alloc(64);
        for offset in [0, 1, 2, 3, 7] {
            assert!(!refused(|| aligned(at(ptr, offset), 1, 1)), "offset {offset}");
            assert!(!refused(|| aligned(at(ptr, offset), 4, 0)), "offset {offset}");
        }
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_misaligned_access_to_storage_no_region_covers_is_refused() {
        let _turn = turn();
        // The one thing this module decides about an address the heap does not own. Where the
        // storage came from does not change whether the address is a multiple of four, and a
        // local read through a pointer that lost its low bits is the same bug as a heap one.
        // A `u64` array rather than a byte one, because a byte array is allowed to start anywhere
        // and the test would then be about where the frame happened to land.
        let stack = [0u64; 2];
        let addr = stack.as_ptr().cast::<c_void>();
        assert!(!refused(|| aligned(addr.wrapping_byte_add(8), 4, 4)));
        assert!(refused(|| aligned(addr.wrapping_byte_add(9), 4, 4)));
    }

    #[test]
    fn a_read_through_a_pointer_to_a_freed_instance_is_refused() {
        let _turn = turn();
        // Use after free, which is the bug the lifetime plane exists for.
        let ptr = alloc(64);
        assert!(!refused(|| live(at(ptr, 0))));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
        assert!(refused(|| live(at(ptr, 0))));
    }

    #[test]
    fn an_access_that_runs_off_the_end_of_an_instance_is_refused() {
        let _turn = turn();
        // A heap overflow, caught because the last byte it touches is owned by somebody else.
        // The instance is a whole number of granules, so the byte after it is the next granule.
        let ptr = alloc(64);
        assert!(!refused(|| bounds(at(ptr, 60), 4)));
        assert!(refused(|| bounds(at(ptr, 60), 8)));
        // A byte wholly past the instance straddles nothing, so the bounds check has no opinion
        // about it and the liveness check is what refuses it: the address is the next block's
        // header, which no instance owns. This is the division of labour the module comment
        // describes and it is why the two checks are emitted as a pair.
        assert!(!refused(|| bounds(at(ptr, 64), 1)));
        assert!(refused(|| live(at(ptr, 64))));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn an_overflow_that_stays_inside_the_rounded_up_block_is_not_caught_yet() {
        let _turn = turn();
        // The hole the module comment describes, written down as a test so that the milestone
        // that closes it has something to turn round. Seventeen bytes are served out of thirty
        // two and the plane says all thirty two belong to the instance.
        let ptr = alloc(17);
        assert!(!refused(|| bounds(at(ptr, 20), 4)));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_pointer_walking_off_its_object_is_refused_where_it_is_computed() {
        let _turn = turn();
        // Judgement J2, and the one past the end that C promises alongside it.
        let ptr = alloc(64);
        let base = at(ptr, 0);
        assert!(!refused(|| deriv(base, at(ptr, 63))));
        assert!(!refused(|| deriv(base, at(ptr, 64))));
        assert!(refused(|| deriv(base, at(ptr, 65))));
        assert!(refused(|| deriv(base, at(ptr, 4096))));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_derivation_that_walks_backwards_out_of_its_object_is_refused() {
        let _turn = turn();
        // The other end. Document 03 section 3.1 opens it by exactly one element, so with a stride
        // of one byte the byte before the instance is allowed and the one before that is not.
        let ptr = alloc(64);
        let base = at(ptr, 32);
        let under = |back: usize| -> *const c_void { ptr.cast::<u8>().wrapping_sub(back).cast() };
        assert!(!refused(|| deriv(base, at(ptr, 0))));
        assert!(!refused(|| deriv(base, under(1))));
        assert!(refused(|| deriv(base, under(2))));
        assert!(refused(|| deriv(base, under(4096))));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn the_element_before_the_first_is_allowed_and_the_one_before_that_is_not() {
        let _turn = turn();
        // Which is the whole reason the check takes a stride. `&a[-1]` on a 24 byte element is 24
        // bytes below the object, and `&a[-2]` is 48, and nothing else about the two derivations
        // tells them apart.
        let stride = 24;
        let ptr = alloc(stride * 4);
        let base = at(ptr, 0);
        let under = |back: usize| -> *const c_void { ptr.cast::<u8>().wrapping_sub(back).cast() };
        assert!(!refused(|| stepped(base, under(stride), stride)));
        assert!(!refused(|| stepped(base, under(stride - 1), stride)));
        assert!(refused(|| stepped(base, under(stride + 1), stride)));
        assert!(refused(|| stepped(base, under(stride * 2), stride)));
        // And the width is the derivation's rather than the object's, so a walk over bytes through
        // the same allocation gets the narrow window and not this one.
        assert!(refused(|| stepped(base, under(stride), 1)));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_step_forward_never_gets_the_low_end_of_the_window() {
        let _turn = turn();
        // The low end is for a derivation that went down. A pointer walked off the top of an
        // object could otherwise land one stride below some other instance and be excused by it.
        let ptr = alloc(64);
        let base = at(ptr, 0);
        assert!(refused(|| stepped(base, at(ptr, 4096), 4096)));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn the_extent_of_a_live_instance_is_measured_from_wherever_it_is_asked_about() {
        let _turn = turn();
        // What section 7.4's split divides by. The answer from the base is the whole instance and
        // the answer from partway in is what is left of it, because a loop that starts in the
        // middle of an array is asking about the rest of the array.
        let ptr = alloc(64);
        assert_eq!(extent(at(ptr, 0), 1024), 64);
        assert_eq!(extent(at(ptr, 32), 1024), 32);
        assert_eq!(extent(at(ptr, 63), 1024), 1);
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn an_address_partway_through_a_granule_still_counts_the_rest_of_it() {
        let _turn = turn();
        // A granule is sixteen bytes and every byte in one has the same version, so an address
        // three bytes into the last granule of an instance has thirteen bytes left. A walk that
        // started at the next granule would say zero and the split would run its fast half not at
        // all, which is sound and is also the answer that makes the whole thing pointless.
        let ptr = alloc(64);
        assert_eq!(extent(at(ptr, 51), 1024), 13);
        assert_eq!(extent(at(ptr, 1), 1024), 63);
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn the_answer_stops_at_the_number_of_bytes_that_were_asked_for() {
        let _turn = turn();
        // Which is what keeps the walk bounded by the work the loop was going to do anyway. A
        // caller that wants ten bytes is told ten and the walk stops after one granule, whatever
        // the instance turns out to be.
        let ptr = alloc(4096);
        assert_eq!(extent(at(ptr, 0), 10), 10);
        assert_eq!(extent(at(ptr, 0), 0), 0);
        assert_eq!(extent(at(ptr, 0), 4096), 4096);
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn the_extent_never_runs_on_into_the_instance_next_door() {
        let _turn = turn();
        // The property the whole thing rests on. An answer larger than the truth would put reads
        // past the end of the object in the half of the loop that has no checks in it, which is
        // the one way this can turn a caught bug into an uncaught one.
        let one = alloc(64);
        let two = alloc(64);
        assert!(extent(at(one, 0), 8192) <= 64, "the first instance stops where it stops");
        assert!(extent(at(two, 0), 8192) <= 64, "and so does the second");
        // SAFETY: both are live instances.
        unsafe {
            dealloc(one);
            dealloc(two);
        }
    }

    #[test]
    fn the_backward_extent_counts_the_bytes_below_the_address_it_is_given() {
        let _turn = turn();
        // The mirror of the forward one, and the address is one past what is asked about, which is
        // why the answer from the end of the instance is the whole of it. A loop that walks from
        // high to low asks from the end of its first access, so it is this end that has to be right.
        let ptr = alloc(64);
        assert_eq!(extent_back(at(ptr, 64), 1024), 64);
        assert_eq!(extent_back(at(ptr, 32), 1024), 32);
        assert_eq!(extent_back(at(ptr, 1), 1024), 1);
        assert_eq!(extent_back(at(ptr, 0), 1024), 0, "nothing below the base belongs to it");
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn the_backward_extent_counts_the_part_of_a_granule_below_the_address() {
        let _turn = turn();
        // The other half of the granule the address is in, for the reason the forward walk gives
        // about the granule it starts in. Three bytes into a granule is three bytes below the
        // address, and stopping at the granule boundary instead would say zero far too often.
        let ptr = alloc(64);
        assert_eq!(extent_back(at(ptr, 3), 1024), 3);
        assert_eq!(extent_back(at(ptr, 63), 1024), 63);
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn the_backward_extent_never_runs_back_into_the_instance_next_door() {
        let _turn = turn();
        // The property the descending half of a split loop rests on, and it is the one above read
        // the other way. An answer larger than the truth would put reads below the start of the
        // object in the half of the loop that has no checks in it.
        let one = alloc(64);
        let two = alloc(64);
        assert!(extent_back(at(one, 64), 8192) <= 64, "the first instance starts where it starts");
        assert!(extent_back(at(two, 64), 8192) <= 64, "and so does the second");
        // SAFETY: both are live instances.
        unsafe {
            dealloc(one);
            dealloc(two);
        }
    }

    #[test]
    fn the_backward_extent_stops_at_what_was_asked_for_and_answers_for_what_is_not_the_heaps() {
        let _turn = turn();
        // The limit and the not the heap's answer, both of them the forward query's and both of
        // them here because a second entry point is a second place to get them wrong.
        let ptr = alloc(4096);
        assert_eq!(extent_back(at(ptr, 4096), 10), 10);
        assert_eq!(extent_back(at(ptr, 4096), 0), 0);
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
        assert_eq!(extent_back(at(ptr, 4096), 1024), 0, "and a freed instance covers nothing");

        let mut local = [0_u8; 64];
        let addr: *mut c_void = local.as_mut_ptr().cast();
        assert_eq!(extent_back(at(addr, 64), 4096), 4096);
    }

    #[test]
    fn the_boundary_is_found_wherever_it_falls() {
        let _turn = turn();
        // The probe and the halving replaced a walk, and a halving that is off by one granule is
        // an extent sixteen bytes longer than the object, which is sixteen bytes of reads with no
        // check on them. So this asks from every offset of an instance several granules long: the
        // answer from `k` bytes in has to be exactly `k` shorter than the answer from the base,
        // whichever side of a probe point the boundary happens to fall.
        let ptr = alloc(200);
        let whole = extent(at(ptr, 0), 1 << 20);
        assert!(whole >= 200, "the instance covers at least what was asked for");
        for k in 0..whole {
            assert_eq!(extent(at(ptr, k), 1 << 20), whole - k, "asked from {k} bytes in");
        }
        // And the same downwards, where the boundary being looked for is the base of the object
        // rather than its end.
        for k in 0..=whole {
            assert_eq!(extent_back(at(ptr, k), 1 << 20), k, "asked from {k} bytes in");
        }
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_query_that_stops_inside_the_object_answers_for_all_of_what_it_asked() {
        let _turn = turn();
        // The case that made this worth changing. The far end of the range asked about is inside
        // the object, so the probe there settles it and no halving happens at all. One short of
        // the boundary, exactly it, and one past it are the three that a wrong comparison here
        // would tell apart, so all three are named.
        let ptr = alloc(200);
        let whole = extent(at(ptr, 0), 1 << 20);
        assert_eq!(extent(at(ptr, 0), whole - 1), whole - 1);
        assert_eq!(extent(at(ptr, 0), whole), whole);
        assert_eq!(extent(at(ptr, 0), whole + 1), whole);
        assert_eq!(extent_back(at(ptr, whole), whole - 1), whole - 1);
        assert_eq!(extent_back(at(ptr, whole), whole), whole);
        assert_eq!(extent_back(at(ptr, whole), whole + 1), whole);
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_query_larger_than_the_whole_region_is_answered_rather_than_walked() {
        let _turn = turn();
        // What a split loop asks when the count it is guarding is not a small number: `want` is
        // the whole sweep, and the sweep can be larger than the heap. Clamping to the region is
        // what keeps the arithmetic from wrapping, and the answer is still the object's.
        let ptr = alloc(64);
        let whole = extent(at(ptr, 0), 1 << 20);
        assert_eq!(extent(at(ptr, 0), usize::MAX), whole);
        assert_eq!(extent_back(at(ptr, whole), usize::MAX), whole);
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_freed_instance_covers_nothing() {
        let _turn = turn();
        // Zero rather than a refusal, because deciding it here would report the loop and not the
        // access. The checked half starts at the first iteration and the check in it says what
        // happened.
        let ptr = alloc(64);
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
        assert_eq!(extent(at(ptr, 0), 1024), 0);
    }

    #[test]
    fn an_address_that_is_not_the_heaps_covers_everything_that_was_asked_for() {
        let _turn = turn();
        // A local, a global, or another allocator's storage. Every check passes for one of those,
        // so a checked half of a loop over one would catch nothing, and the answer that says so is
        // the one that leaves the program with the loop it wrote.
        let mut local = [0_u8; 64];
        let addr: *const c_void = local.as_mut_ptr().cast();
        assert_eq!(extent(addr, 4096), 4096);
    }

    #[test]
    fn an_address_that_is_not_the_heaps_passes_every_check() {
        let _turn = turn();
        // A local, a global and anything another allocator handed out. Reporting on one of these
        // would be a false positive against a program that did nothing wrong, and this milestone
        // instruments the heap.
        let mut local = [0_u8; 64];
        let addr: *const c_void = local.as_mut_ptr().cast();
        let far: *const c_void = addr.cast::<u8>().wrapping_add(1 << 20).cast();
        assert!(!refused(|| bounds(addr, 64)));
        assert!(!refused(|| live(addr)));
        assert!(!refused(|| deriv(addr, far)));
    }
}
