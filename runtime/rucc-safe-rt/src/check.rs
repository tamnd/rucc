//! The six checks generated code calls, what each of them decides, and the one question it asks.
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
//! All four planes. A version covers one granule of sixteen bytes, so the three checks that read
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
use crate::aux_slot;
use crate::fail::Descriptor;
use crate::init;
use crate::layout::{AUX_PER_WORD, Cap, Class, Meta, WORD};
use crate::plane::{self, Version};
use crate::recover;
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
/// # The capability may permit and may not refuse
///
/// The capability is asked first and the planes are asked only if it did not permit, which is the
/// whole of box 6 of tamnd/rucc#1241. A capability's two numbers are the instance's base and extent,
/// so a permit is a subtraction and a compare against two words the caller already has in hand, and
/// nothing is read from memory at all. The plane path is a region lookup and two plane loads for the
/// same answer, and it is the path this check has always taken. It costs nothing to ask the
/// capability first, because the lifetime check standing beside this one is loading the same
/// capability for the version compare anyway, which is the sense in which that issue says this is
/// where the version compare pays for itself.
///
/// A permit is safe to believe, because a capability's range was taken off the very thing the plane
/// path reads. [`crate::recover::made`] takes the base and extent out of the instance's own header,
/// and [`crate::recover::recover`] takes them off the run of the lifetime plane that owns the
/// address, which is the same run the two plane loads below compare the ends of. A capability over a
/// mapping covers the region, which is what `region.holds` asks, and the one that covers everything
/// is what an address no region covers already gets.
///
/// A refusal is not, and the reason is worth writing down rather than discovering twice. A
/// capability can be narrower than the access it is standing next to without the access being wrong.
/// It can have been recovered at a boundary, where the run of equal versions the walk found is the
/// storage around the address rather than the object the pointer was made for. It can have been
/// loaded out of an aux slot that was never written, where the same walk answers the same way. In
/// all of those the capability under-describes a correct access, and the planes, which are what this
/// check has always asked, say so. The SQLite amalgamation had one such site, which is the whole of
/// tamnd/rucc#1338: `whereLoopInsert` reads a byte ninety nine into a `WhereLoop` that
/// `whereLoopXfer` had just copied, and the copy moved the word and left the slot beside it, so the
/// capability there was rebuilt from the previous tenant's displacement and covered ninety six bytes
/// starting eight bytes below the pointer. The access is correct and believing the capability aborts
/// a working program on its second statement. [`relocate`] moves the aux with the bytes now, and
/// counting what arrives here in an instrumented build of that amalgamation finds no capability that
/// fails to cover a correct access in fifteen and a half million of them. The restraint stays
/// anyway, because the three reasons above are about what a recovery can know rather than about one
/// bug that has been fixed. So the capability is a fast yes and never a no, and the planes keep the
/// whole of the refusing. What it costs is that a capability narrower than the truth pays for both
/// paths, which is a site that was paying for one of them before.
///
/// What it buys, from the same count: ninety nine and a half percent of the bounds checks that run
/// are answered here and never reach the planes. Three quarters of those are answered by a
/// capability a plane walk recovered, which is the plane's own answer arrived at earlier and is the
/// real saving, and the rest by one that covers everything, which is the cheap answer for an address
/// no region watches. The half percent that falls through is bottom, and a capability the compiler
/// could name an instance for is under one in ten thousand, which is what tamnd/rucc#1241's `cap_of`
/// box is about.
///
/// This is also why a bottom capability and a null one need no case of their own. Neither covers
/// anything, so neither permits anything, and both fall through to the planes the way they always
/// did. That matters most for bottom, which says nobody owns the address: that is [`live`]'s
/// refusal and not this one's, and calling it out of bounds would put the wrong sentence in the
/// report, since storage that has been freed is inside the instance it used to be inside.
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
/// null. It is only read when the check refuses. `capability` is null or the address of a capability
/// the same build reserved a slot for and filled, exactly as [`live`] takes one.
pub unsafe fn bounds(
    addr: *const c_void,
    size: usize,
    align: usize,
    capability: *const Cap,
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
    // SAFETY: this function's own contract about `capability`, passed straight on.
    if unsafe { permits(capability, addr, size) } {
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

/// Judgement J1, the lifetime half: the capability the access goes through still names the
/// instance that owns `addr`.
///
/// Two questions in one, and they fail differently. The first is whether anybody at all owns the
/// address, which catches every access to storage that has been freed and not handed out again,
/// every access to storage that was never allocated, and every access to the allocator's own
/// headers. The second is the lock and key rule of `spec/safe-memory/08-temporal-safety.md` section
/// 8.3: the version the capability was taken at against the version the plane holds now. That is
/// the half that catches an access through a stale pointer to an address the allocator has since
/// given to somebody else, which is the common shape of a use after free in a program that is busy
/// enough to reuse a block, and until the capability reached this function there was nothing here
/// to compare against.
///
/// It is asked only of a capability whose version is about this pointer. A bottom one has none, and
/// neither has a capability recovered from the containing mapping rather than from the planes,
/// whose version is `plane::FOREIGN` and whose bounds are the mapping's. Neither is evidence that
/// anything is stale and both keep the weaker reading, and a null `capability` is the same answer
/// for the same reason. A recovery that did find an instance is not in that group, for the reason
/// `stale` gives: the version it read is the version of the instance the pointer was in when it
/// ran. Neither is a capability an aux slot rebuilt, for the reason `stale` gives about that: the
/// slot travels with the word it describes.
///
/// Generated code fills it in now. `rucc_safety::origin` takes one capability per pointer where the
/// pointer is made, `rucc_safety::lower` hands it to the call, and `rucc_safety::slot` gives it four
/// words of frame, so what arrives here is the capability of the pointer the access went through
/// rather than a null. Where it came from decides how tight the answer is rather than whether the
/// question is asked: a pointer an allocator returned carries the version the allocator wrote, and
/// a pointer nothing in the unit could trace carries the version the plane held where the recovery
/// ran, which is later and so catches less. The remaining boxes of tamnd/rucc#1241 are about moving
/// pointers out of the second group and into the first.
///
/// When the freeing was another thread's and nothing orders it against this access, the report says
/// so and names both of them. That is document 03's C4, the use after free a race produced rather
/// than one thread's own mistake, and the two are worth telling apart: a single threaded use after
/// free is a lifetime the author got wrong, and this one is a lifetime that was right on both
/// threads and wrong between them, which is a different thing to go and fix. The stamp comes from
/// the epoch plane, which `crate::alloc::over` fills with the freeing thread's stamp as the
/// instance ends, and it is read here rather than compared in the allocator because the second half
/// of the pair is the accessing thread and the allocator has not met it.
///
/// A free this thread is ordered against reports the way it always did. That covers the ordinary
/// case, where the same thread freed and then read, and it also covers a free another thread made
/// behind a lock this one then took, which is a program with one bug in it rather than two.
///
/// # Panics
///
/// As [`bounds`].
///
/// # Safety
///
/// As [`bounds`], and `capability` is null or the address of a capability the same build reserved
/// a slot for and filled.
pub unsafe fn live(addr: *const c_void, capability: *const Cap, descriptor: *const Descriptor) {
    let addr = addr as usize;
    let Some(region) = alloc::covering(addr) else { return };
    let holder = owner(&region, addr);
    if plane::owned(holder) {
        // SAFETY: this function's own contract about `capability`, passed straight on.
        if !unsafe { stale(capability, holder) } {
            return;
        }
        // Reported without a witness, unlike the path below, because the epoch plane holds the
        // last thing anybody did to these bytes and on storage that has been handed out again that
        // is the new owner's allocation rather than the free this pointer outlived. Naming the
        // thread that allocated as the thread that freed would be worse than saying nothing.
        //
        // SAFETY: as in `bounds`.
        unsafe { crate::fail::report(descriptor, Some(addr)) };
        return;
    }
    let mine = crate::epoch::here();
    // SAFETY: the address is inside the region, whose epoch plane covers every byte of it.
    let found = unsafe { region.epochs.read(addr) };
    if mine != crate::epoch::NONE && crate::epoch::unordered(found, mine) {
        // SAFETY: as in `bounds`, and neither stamp is an address.
        unsafe {
            crate::fail::report_witness(
                descriptor,
                addr,
                crate::report::Witness::Stranger(found, mine),
            );
        }
        return;
    }
    // SAFETY: as in `bounds`.
    unsafe { crate::fail::report(descriptor, Some(addr)) }
}

/// Judgement J6, the half an allocator cannot decide: the capability being freed still names the
/// instance that owns `addr`.
///
/// `free` takes an address and nothing else, so what the allocator can tell from the header at that
/// address is whether something live begins there. That answers a double free of a block nobody has
/// asked for since, and it gets the other shape wrong: once the allocator has handed the same block
/// back out, a second free of the old pointer finds a live instance and releases somebody else's
/// object. The program then runs on with a hole in it and the report that eventually comes out names
/// an innocent access. tamnd/rucc#492 is that, and the version is what tells the two apart, because
/// the address is the same in both and the instance is not.
///
/// So this asks one question and leaves the rest alone. Nobody owning the address is the allocator's
/// to refuse and it already does, with the header in front of it and more to say than this has:
/// whether the storage was ever allocated, whether it was this allocator that allocated it, and
/// whether the pointer is the base of something or the middle of it. An address outside every region
/// is not ours at all, which is a program mixing allocators and the same answer for the same reason.
/// What is left is the address of a live instance reached through a capability made for a different
/// one, which is the case nothing downstream of here can see.
///
/// It is asked only of a capability whose version is about this pointer, exactly as [`live`] asks
/// it. A bottom one has none, and a capability recovered from a mapping rather than from the planes
/// has none. Neither is evidence that this pointer is the stale one and both let the free through
/// to the allocator to decide the way it always did.
///
/// # Panics
///
/// As [`bounds`].
///
/// # Safety
///
/// As [`live`].
pub unsafe fn freeing(addr: *const c_void, capability: *const Cap, descriptor: *const Descriptor) {
    let addr = addr as usize;
    let Some(region) = alloc::covering(addr) else { return };
    let holder = owner(&region, addr);
    if !plane::owned(holder) {
        return;
    }
    // SAFETY: this function's own contract about `capability`, passed straight on.
    if !unsafe { stale(capability, holder) } {
        return;
    }
    // The address is worth naming and the witness is not, for the reason [`live`] gives about the
    // same pair: the epoch plane holds the last thing anybody did to these bytes, and on a block
    // that has been handed out again that is the new owner's allocation rather than the free this
    // pointer outlived.
    //
    // SAFETY: as in `bounds`.
    unsafe { crate::fail::report(descriptor, Some(addr)) }
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
/// The capability is asked first and, as in [`bounds`], only ever permits. A derivation that lands
/// inside the window it describes is one the planes would permit too, so the answer is the same and
/// it costs a subtraction and a compare over words the caller already has, where the planes cost a
/// region lookup and two reads of the lifetime plane. A null capability, the bottom one and a wide
/// one permit nothing here and go to the planes the way every derivation did before. The wide one
/// is left out where [`bounds`] believes it because its window is a whole mapping, and a derivation
/// is exactly the step from one object in a mapping into the next.
///
/// # Panics
///
/// As [`bounds`].
///
/// # Safety
///
/// As [`live`].
pub unsafe fn deriv(
    base: *const c_void,
    derived: *const c_void,
    stride: usize,
    capability: *const Cap,
    descriptor: *const Descriptor,
) {
    // SAFETY: this function's own contract about `capability`, passed straight on.
    if unsafe { stays(capability, derived as usize, stride) } {
        return;
    }
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
/// type, so this is what the `moves` clause of a wrapper in [`crate::wrap`] calls once it knows how
/// much was copied, and it is what keeps the punning idiom the standard permits from being
/// reported. Every other clause that writes records [`types::CHARACTER`] instead, which is what a
/// wrapper writing bytes really does.
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

/// Both questions a read asks of the planes, in one call: do these bytes agree with this type and
/// has every one of them been written.
///
/// [`typed`] and [`filled`] answer the two separately and are the same function up to which plane
/// they end at. Both take the address, both take the width, both find the region the address is in,
/// and both clip the range to it, and a read that asks one asks the other in the very next
/// instruction. So a read that still needs both pays for the region twice, and the region is the
/// expensive half: [`alloc::covering`] walks the published regions and builds all four planes over
/// the one it finds, where each plane query that follows is a handful of shifts and a load.
///
/// The type plane is asked first, which is the order the two calls were emitted in, so a read that
/// disagrees with both reports the same refusal it reported when they were two calls.
///
/// A read inside one granule is asked of the two slots that answer for it and nothing else, which
/// is [`types::Types::plain`] and [`init::Init::whole`], the same two a strided check asks of each
/// access in its column. Both are a single load, where the general walk is a loop per plane set up
/// for any width, and every scalar read a C program makes is inside one granule unless it is
/// misaligned. Only a yes is taken from them. A no is a granule stored through more than one type
/// or one only partly written, and the walk below is what can tell whether the bytes this read wants
/// are the good ones, so it asks again and its answer is the answer.
///
/// `rucc_safety::lower` is what puts them together, and only when the pair is one read's: same
/// address, same width, nothing between them that writes memory. When `crate::discharge` has taken
/// one of the two out, the other lowers on its own and this is not reached.
///
/// # Panics
///
/// As [`bounds`].
///
/// # Safety
///
/// As [`bounds`]. `ty` is a plane vocabulary entry and is not an address.
pub unsafe fn allowed(addr: *const c_void, size: usize, ty: TypeId, descriptor: *const Descriptor) {
    let addr = addr as usize;
    let Some(region) = alloc::covering(addr) else { return };
    const { assert!(types::GRANULE == init::SPAN) };
    if addr % types::GRANULE + size <= types::GRANULE {
        // SAFETY: the region covers `addr` and is page aligned at both ends, so it covers the whole
        // granule `addr` is in, which is every byte either slot answers for.
        let whole = unsafe {
            (ty == types::CHARACTER || region.types.plain(addr, ty)) && region.init.whole(addr)
        };
        if whole {
            return;
        }
    }
    let size = clipped(&region, addr, size);
    // SAFETY: the range is clipped to the region, whose type plane covers every granule of it and
    // whose init plane covers every byte of it.
    let held = unsafe { region.types.allows(addr, size, ty) && region.init.allows(addr, size) };
    if !held {
        // SAFETY: as in `bounds`.
        unsafe { crate::fail::report(descriptor, Some(addr)) }
    }
}

/// The plane checks over a range, which is what a check taken out of a loop hands over.
///
/// The same judgements as [`typed`], [`filled`] and [`allowed`], in the same order, asked through
/// `sweep` on each plane rather than `allows`, so a long range costs a load per sixty four bytes
/// rather than per eight. `ty` is the type the read wants, or nothing when the type plane is not
/// asked, and `init` says whether the init plane is.
///
/// A function of its own rather than a length test in those, and the compiler is what chooses:
/// `rucc_safety::lower` calls the three `_range` entry points for a check over a computed width
/// or a constant one of sixty four bytes or more, which is the check `rucc_opt::hoist` writes in
/// front of a loop, and the plain ones for every check on an access. A length test at the top of the plain ones was tried and measured. The
/// compiler saved the registers the body needs before it made the test, so each of the many
/// millions of per access checks in `a-binary-tree-walk` paid three instructions more and the
/// program got 0.8 percent slower, which is too much to charge every load for a loop's benefit.
///
/// # Panics
///
/// As [`bounds`].
///
/// # Safety
///
/// As [`bounds`]. `ty` is a plane vocabulary entry and is not an address.
pub unsafe fn swept(
    addr: *const c_void,
    size: usize,
    ty: Option<TypeId>,
    init: bool,
    descriptor: *const Descriptor,
) {
    let addr = addr as usize;
    let Some(region) = alloc::covering(addr) else { return };
    let size = clipped(&region, addr, size);
    // SAFETY: as in `allowed`.
    let held = unsafe {
        ty.is_none_or(|ty| region.types.sweep(addr, size, ty))
            && (!init || region.init.sweep(addr, size))
    };
    if !held {
        // SAFETY: as in `bounds`.
        unsafe { crate::fail::report(descriptor, Some(addr)) }
    }
}

/// The plane checks over a walk that leaves gaps, which is the other check `rucc_opt::hoist` puts in
/// front of a loop.
///
/// The accesses are `width` bytes each, at `addr` and at every `step` along from it that still ends
/// inside `span` bytes of `addr`, and each is asked the question [`allowed`] asks of one access,
/// with the type plane first when it is asked at all. What [`swept`] would do with the same loop is
/// ask about every byte between the first access and the last, and the loop this is for reads eight
/// bytes out of every sixteen hundred, so that is two hundred times the plane it needs to read. Here
/// the region is found once, which is the expensive half of every one of these calls, and each
/// access costs what the plane read for it costs.
///
/// Clipped to the region the way the others are, so an access past its end is not asked about and
/// one that straddles it is asked about as far as it goes. That is [`bounds`]'s refusal, as it is
/// for the dense forms.
///
/// # Panics
///
/// As [`bounds`].
///
/// # Safety
///
/// As [`bounds`]. `ty` is a plane vocabulary entry and is not an address.
pub unsafe fn stepped(
    addr: *const c_void,
    (span, step, width): (usize, usize, usize),
    ty: Option<TypeId>,
    init: bool,
    descriptor: *const Descriptor,
) {
    let addr = addr as usize;
    let Some(region) = alloc::covering(addr) else { return };
    let last = addr.saturating_add(span);
    let ask = |at: usize| {
        // An access inside one granule is one slot of each plane, and a slot that answers for the
        // whole granule answers for every access inside it, which is one load and one compare a
        // plane for the array of one type that a strided walk nearly always is. A granule written
        // in parts, or an access across two, is asked the long way, which gives the same answer.
        let inside = at % types::GRANULE + width <= types::GRANULE
            && at % init::SPAN + width <= init::SPAN
            && width <= region.end - at;
        // SAFETY: as in `allowed`, for the access at `at`, which is inside the region.
        let quick = inside
            && unsafe {
                ty.is_none_or(|ty| region.types.plain(at, ty)) && (!init || region.init.whole(at))
            };
        let size = clipped(&region, at, width);
        // SAFETY: as above.
        let held = quick
            || unsafe {
                ty.is_none_or(|ty| region.types.allows(at, size, ty))
                    && (!init || region.init.allows(at, size))
            };
        if !held {
            // SAFETY: as in `bounds`.
            unsafe { crate::fail::report(descriptor, Some(at)) }
        }
    };
    let mut at = addr;
    // A step of whole granules keeps every access where the first is in its granule, so when the
    // first is inside one granule they all are, and each plane can walk its own slots for the
    // column with the address moving on by the step rather than asking about each access from
    // scratch. The accesses that end inside both the span and the region are walked that way, a
    // plane at a time, and one either plane cannot answer from its slot alone is asked the long
    // way on its own and the walk goes on after it, so the answers and the reports are the ones
    // the loop below would give.
    if step != 0
        && step % types::GRANULE == 0
        && step % init::SPAN == 0
        && addr % types::GRANULE + width <= types::GRANULE
        && addr % init::SPAN + width <= init::SPAN
    {
        let limit = last.min(region.end);
        let count = match limit.checked_sub(addr).and_then(|room| room.checked_sub(width)) {
            Some(room) => room / step + 1,
            None => 0,
        };
        let typed = |k: usize| {
            ty.map_or(count, |ty| {
                // SAFETY: every access before `count` ends inside the region, so its slots are
                // mapped.
                k + unsafe { region.types.column(addr + k * step, step, count - k, ty) }
            })
        };
        let written = |k: usize| {
            if init {
                // SAFETY: as above.
                k + unsafe { region.init.column(addr + k * step, step, count - k) }
            } else {
                count
            }
        };
        let (mut t, mut i) = (typed(0), written(0));
        loop {
            let k = t.min(i);
            if k == count {
                break;
            }
            ask(addr + k * step);
            if t == k {
                t = typed(k + 1);
            }
            if i == k {
                i = written(k + 1);
            }
        }
        at = addr.saturating_add(count.saturating_mul(step));
    }
    while at < region.end && at.saturating_add(width) <= last {
        ask(at);
        // A step of zero is not something the compiler writes, and it would be one access asked
        // about over and over, so it is that one access asked about once.
        if step == 0 {
            break;
        }
        at = at.saturating_add(step);
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
/// The init plane and no other. A copy moves the pointers in a structure as well as its bytes, and
/// the aux that describes them is [`relocate`]'s job rather than this one's.
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

/// The judgement a copy makes about the pointers it moved: the capability goes with the word.
///
/// A structure full of pointers copied through `memcpy` arrives with the pointers in it and without
/// anything that says what they point at, because a capability lives in the aux slot beside the
/// word rather than in the word, and the C library's `memcpy` has never heard of the aux. The word
/// then reads back against whatever slot the destination happened to be carrying, which is the
/// previous tenant's if the storage was used before and is nothing at all if it was not. That is
/// tamnd/rucc#1148, and it is the reason an instrumented SQLite stopped inside `whereLoopInsert`
/// against a `WhereLoop` that `whereLoopXfer` had just copied.
///
/// Copying the slot is exactly right rather than nearly right, and the reason is what a slot holds.
/// [`crate::aux_slot::Slot::of`] writes down a displacement from the pointer value beside it, not
/// an address, and a `memcpy` moves that pointer value across unchanged. So a slot that was correct
/// beside the source word is correct beside the destination word, byte for byte, with no fixing up
/// of anything inside it.
///
/// A destination word the copy only partly covers gets its slot cleared instead. Half a pointer is
/// not a pointer, and leaving the slot would leave a capability describing a word whose bytes have
/// just changed underneath it. The same goes for a whole word whose source has no slot to give: a
/// copy out of a stack object or out of storage this monitor does not watch carries no capability,
/// and the honest record of that is an empty slot and a refusal at the first access through it, not
/// whatever was there before.
///
/// Nothing at all when the destination is not an allocation this runtime laid out, since then there
/// is no aux in front of it to write.
///
/// A destination word whose capability this call changed is stamped as well, which is `restamped`
/// and is the epoch plane's half of a copy. A word whose slot ends up saying the same thing it said
/// before is not, because nothing about that word changed and a stamp is about a change.
///
/// # Safety
///
/// Neither address is read through by this function. They may overlap, and the aux is moved with
/// the same overlap rules the bytes are, so a `memmove` of a structure onto itself is the same
/// answer either way.
pub unsafe fn relocate(dst: *const c_void, src: *const c_void, len: usize) {
    if len < WORD {
        return;
    }
    let (dst, src) = (dst as u64, src as u64);
    let into = recover::recover(dst as *const c_void);
    if into.meta.class() != Class::Allocated as u8 {
        return;
    }
    let from = recover::recover(src as *const c_void);
    let region = alloc::covering(dst as usize);
    let mut taken = None;
    let word = WORD as u64;
    // Every destination word the copy reaches, including the two at the ends it may only reach part
    // of, walked by the destination's word grid because that grid is the one the aux is laid on.
    let mut at = dst & !(word - 1);
    let after = dst.wrapping_add(len as u64).wrapping_add(word - 1) & !(word - 1);
    while at < after {
        if let Some(slot) = aux_slot::address_of(into, at) {
            let whole = at >= dst && at.wrapping_add(word) <= dst.wrapping_add(len as u64);
            let carried =
                if whole { aux_slot::address_of(from, src.wrapping_add(at - dst)) } else { None };
            // SAFETY: the address came from `address_of`, so it is a slot inside the aux of the
            // instance the recovery found and is a pair of words the runtime owns.
            let held = unsafe { core::ptr::read(slot as *const [u64; 2]) };
            match carried {
                // SAFETY: both addresses came from `address_of`, so each is a slot inside the aux
                // of the instance the recovery found, and a slot is `AUX_PER_WORD` bytes the
                // runtime owns. `copy` rather than `copy_nonoverlapping` because the two objects
                // may be one object.
                Some(had) => unsafe {
                    core::ptr::copy(had as *const u8, slot as *mut u8, AUX_PER_WORD);
                },
                // SAFETY: as above, for the destination alone. Zero is `Slot::EMPTY`, which is
                // what a slot nothing ever wrote already reads as.
                None => unsafe { core::ptr::write_bytes(slot as *mut u8, 0, AUX_PER_WORD) },
            }
            // SAFETY: as above, for the slot the branch above has just finished writing.
            let now = unsafe { core::ptr::read(slot as *const [u64; 2]) };
            if held != [0, 0] || now != [0, 0] {
                if let Some(region) = region.as_ref() {
                    let stamp = *taken.get_or_insert_with(crate::epoch::tick);
                    if stamp != crate::epoch::NONE {
                        // SAFETY: the word is inside the region the destination is in, and the
                        // slot is its own, which `restamped` says why is in the same region.
                        unsafe { restamped(region, at, slot, stamp) };
                    }
                }
            }
        }
        at = at.wrapping_add(word);
    }
}

/// Records this thread as the one that changed a pointer word, on the word and on the slot beside
/// it.
///
/// The epoch half of what an interposed write owes, and the half that was missing until
/// tamnd/rucc#1307's audit reached this plane. A `memcpy` that moves a pointer into a shared
/// structure, or a `memset` that takes one out of it, is a store to a pointer word like any other,
/// and generated code stamps those so that another thread loading the word can be told nothing
/// ordered the two. A wrapper that did not stamp left whatever the last instrumented store had
/// written, so the reader compared itself against a stranger that was no longer the writer and the
/// report went missing. That is a lost report rather than a wrong one, which is why it survived
/// three rounds of this audit.
///
/// Both halves with one stamp, the way [`crate::cap::pair`] writes them. A byte-wise write over a
/// pointer word is one store that changed both: it put bytes where the pointer was and it took the
/// capability beside it away. Stamping the word alone would leave the two disagreeing and
/// [`crate::cap::torn`] would then call a correct program's next load of that word a torn store.
///
/// Only a word whose slot this call changed, which is the same granularity generated code has.
/// [`stamped`] is emitted for a store the compiler knows is pointer shaped, because the plane's
/// granule is a pointer wide and so a granule two threads share is one holding no pointer.
/// A wrapper cannot ask the compiler what shape a word is, but it can ask the aux, and a word with
/// no capability on either side of the call is the same kind of word `stamped` declines to watch.
/// That is also what keeps this off the cost of an ordinary `memset`, which walks a buffer with no
/// slots in it and writes no stamps at all.
///
/// # Safety
///
/// `word` is an address inside `region`, and `slot` is the address of that word's aux slot, which
/// is inside the same region for the reason [`crate::cap::pair`] gives.
unsafe fn restamped(region: &Region, word: u64, slot: u64, stamp: crate::epoch::Stamp) {
    // SAFETY: both addresses are inside the region, whose epoch plane covers every byte of it.
    unsafe {
        region.epochs.write(word as usize, stamp);
        region.epochs.write(slot as usize, stamp);
    }
}

/// The judgement a byte-wise write makes about the pointers it wrote over: there are none now.
///
/// [`relocate`] is this for a copy, where the destination gets the source's slots. A `memset`, a
/// `strcpy` or a `read` into a buffer has no source of slots to give, because what it writes is
/// bytes rather than pointers, so every slot the range reaches has to go.
///
/// Leaving them is worse than the stale metadata the other planes are about, because a slot that
/// outlives the word it describes can permit rather than refuse. [`crate::aux_slot::Slot::read`]
/// rebuilds the base by subtracting a displacement from the pointer value beside it, so a word
/// filled with `0xff` by a `memset` and then read back as a pointer answers with a capability whose
/// base is that value minus the old displacement and whose extent is the old object's. The wild
/// address is inside its own bounds by construction, and the version is the old object's and is
/// live while the old object is, so a dereference of it passes both questions. Without the slot the
/// same read recovers nothing, and the access is refused where it should be.
///
/// Every word the range touches, including the two at the ends it may only touch part of, because
/// half a pointer is not a pointer. Nothing at all when the destination is not an allocation this
/// runtime laid out, for the reason [`relocate`] gives.
///
/// The slot is read before it is written, which is not an optimization of the store. It is an
/// optimization of the cache line: a buffer with no pointers in it is nearly every buffer a
/// `memset` touches, its aux is already empty, and writing zeroes over zeroes would dirty two bytes
/// of cache line per byte written and send them to memory.
///
/// A word that really did hold a pointer is stamped as well, which is `restamped` and is the
/// epoch plane's half of a byte-wise write. That read is what makes the stamping cost nothing on
/// the buffers this is mostly called about: a `memset` over a buffer with no slots in it writes no
/// stamps and does not even take one.
///
/// # Safety
///
/// The address is not read through by this function.
pub unsafe fn erase(addr: *const c_void, len: usize) {
    if len == 0 {
        return;
    }
    let addr = addr as u64;
    let into = recover::recover(addr as *const c_void);
    if into.meta.class() != Class::Allocated as u8 {
        return;
    }
    let region = alloc::covering(addr as usize);
    let mut taken = None;
    let word = WORD as u64;
    let mut at = addr & !(word - 1);
    let after = addr.wrapping_add(len as u64).wrapping_add(word - 1) & !(word - 1);
    while at < after {
        if let Some(slot) = aux_slot::address_of(into, at) {
            // SAFETY: the address came from `address_of`, so it is a slot inside the aux of the
            // instance the recovery found, and a slot is `AUX_PER_WORD` bytes the runtime owns and
            // wrote as a pair of words.
            let held = unsafe { core::ptr::read(slot as *const [u64; 2]) };
            if held != [0, 0] {
                // SAFETY: as above.
                unsafe { core::ptr::write_bytes(slot as *mut u8, 0, AUX_PER_WORD) };
                if let Some(region) = region.as_ref() {
                    let stamp = *taken.get_or_insert_with(crate::epoch::tick);
                    if stamp != crate::epoch::NONE {
                        // SAFETY: the word is inside the region the range is in, and the slot is
                        // its own, which `restamped` says why is in the same region.
                        unsafe { restamped(region, at, slot, stamp) };
                    }
                }
            }
        }
        at = at.wrapping_add(word);
    }
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
/// The aux is marked as well, for the same reason and with the same reach. A callee that fills a
/// structure fills the pointers in it too, and it writes them without touching the slots beside
/// them, so the aux of an instance that has been out there says less than the payload does. The
/// bit that records that is [`crate::layout::Meta::HANDED`] in the header, and what it costs is class Y1 over this
/// instance for the rest of its life. `crate::cap::load` is the reader and tamnd/rucc#1081 is why
/// it is one bit per instance rather than something finer.
///
/// Nothing happens for an address outside every region, or one in an arena whose allocator has said
/// nothing, since there is no instance there to say anything about. The aux half needs one thing
/// more than the init half does, which is a header to believe, so an adopted arena's storage is
/// marked written and not marked handed: it has no aux of ours in front of it to be incomplete.
///
/// # Safety
///
/// `addr` is whatever the program computed and is never read through.
pub unsafe fn handed(addr: *const c_void) {
    let addr = addr as usize;
    let Some(region) = alloc::covering(addr) else { return };
    let Some((lo, len)) = recover::extent(&region, addr) else { return };
    // SAFETY: the run came out of the plane over this region, so the init plane covers it too.
    unsafe { region.init.set(lo, len) }
    // SAFETY: the address is inside the region, which is what reading its plane asks for.
    let version = unsafe { region.plane.version(addr) };
    recover::mark_handed(&region, lo, version);
}

/// The judgement a store through a pointer shaped slot makes: this thread wrote these bytes, now.
///
/// The recording half of section 9.5, and the only half there is yet. It takes this thread's next
/// stamp and puts it in every granule the store touched, so that a later reader can ask who wrote
/// the word it is about and whether anything orders that against itself. Nothing reads the plane
/// yet, so what this buys today is the plane being kept rather than anything being reported, which
/// is the state the type plane and the init plane each went through.
///
/// It is emitted for a store the compiler knows is pointer shaped and not for every store, which is
/// what keeps the granularity honest: `crate::epoch::Epochs::fill` stamps a part granule whole, and
/// a granule two threads share is one holding no pointer and so one no judgement will ask about.
///
/// A thread with nowhere to keep a clock stamps nothing. `crate::epoch::tick` answers `NONE` for it,
/// and writing that would erase what another thread had honestly recorded, which is a lost report
/// turned into a wrong one.
///
/// # Safety
///
/// `addr` is whatever the program computed and is never read through.
pub unsafe fn stamped(addr: *const c_void, size: usize) {
    let addr = addr as usize;
    let Some(region) = alloc::covering(addr) else { return };
    let stamp = crate::epoch::tick();
    if stamp == crate::epoch::NONE {
        return;
    }
    // SAFETY: the range is clipped to the region, whose epoch plane covers every byte of it.
    unsafe { region.epochs.fill(addr, clipped(&region, addr, size), stamp) }
}

/// Judgement J9: no other thread wrote these bytes with nothing ordering that against this access.
///
/// The reading half of section 9.5, and document 03's C2 and C3. It asks the epoch plane for a
/// stamp over the range that this thread has not got past, and a stamp it finds is a write by
/// another thread that no synchronization edge puts before this access. Both a load and a store ask
/// it, which is what makes one check cover both of those classes: a load finding a stranger is the
/// pointer word race and a store finding one is two threads writing the same slot with nothing
/// between them, and the comparison is the same from either side.
///
/// The order around a store matters. This runs before [`stamped`] rather than after it, because
/// [`stamped`] overwrites the very stamp this reads, and a check that ran second would be asking
/// about the write it was called for.
///
/// A thread with nowhere to keep a clock asks nothing. `crate::epoch::here` answers `NONE` for it,
/// which stands at thread zero and step zero, and every stamp in the plane is a stranger to that.
/// The thinning is the same one `stamped` makes from the recording side and it goes the same way:
/// such a thread is not watched rather than reported on.
///
/// Storage nobody owns passes, the way a base that owns nothing passes in [`deriv`]. The bytes are
/// freed or were never handed out, [`live`] is the judgement that says so, and since a free now
/// leaves the freeing thread's stamp behind this would otherwise find it and report the same bug a
/// second time under a different number. What [`live`] does with that stamp is document 03's C4.
///
/// # Panics
///
/// As [`bounds`].
///
/// # Safety
///
/// As [`bounds`].
pub unsafe fn raced(addr: *const c_void, size: usize, descriptor: *const Descriptor) {
    let addr = addr as usize;
    let Some(region) = alloc::covering(addr) else { return };
    if !plane::owned(owner(&region, addr)) {
        return;
    }
    let mine = crate::epoch::here();
    if mine == crate::epoch::NONE {
        return;
    }
    // SAFETY: the range is clipped to the region, whose epoch plane covers every byte of it.
    let found = unsafe { region.epochs.stranger(addr, clipped(&region, addr, size), mine) };
    if found != crate::epoch::NONE {
        // SAFETY: as in `bounds`, and neither stamp is an address.
        unsafe {
            crate::fail::report_witness(
                descriptor,
                addr,
                crate::report::Witness::Stranger(found, mine),
            );
        }
    }
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

/// Whether the capability covers the whole of the access on its own.
///
/// Only ever a reason to permit, never a reason to refuse, for the reasons [`bounds`] sets out. A
/// null capability is a check the compiler found none for and covers nothing, and the bottom one
/// names no instance and covers nothing either, so neither needs a case here: both answer no and
/// both send the caller to the planes, which is where they were being sent before.
///
/// A recovered capability answers yes here, which is the one place a recovered capability is
/// believed and is worth saying why. What recovery cannot work out is which instance a pointer was
/// made for, and the range it hands back is the run of the lifetime plane the address is in now,
/// which is what the plane path computes from the same plane. So a yes here is the plane's own yes
/// arrived at earlier. Whether the instance is the right one is [`live`]'s question, it is still
/// asked, and [`stale`] is where the restraint about recovery lives.
///
/// # Safety
///
/// `capability` is null or the address of a filled capability slot.
unsafe fn permits(capability: *const Cap, addr: usize, size: usize) -> bool {
    if capability.is_null() {
        return false;
    }
    // SAFETY: the caller's contract, and a slot is `Cap::BYTES` of storage the frame owns.
    let held = unsafe { core::ptr::read(capability) };
    held.covers(addr as u64, size as u64)
}

/// Whether a pointer derived to `derived` is still inside the window [`deriv`] allows, going by the
/// capability alone.
///
/// The window is the one the planes allow: anywhere in the object, one past its end, and below its
/// start by less than one element, which is where one more step of `stride` lands back inside it.
/// Wrapping arithmetic, so an address below `lo` is a very large distance above it and fails the
/// first test rather than passing it.
///
/// # Safety
///
/// `capability` is null or the address of a filled capability slot.
unsafe fn stays(capability: *const Cap, derived: usize, stride: usize) -> bool {
    if capability.is_null() {
        return false;
    }
    // SAFETY: the caller's contract, and a slot is `Cap::BYTES` of storage the frame owns.
    let held = unsafe { core::ptr::read(capability) };
    if held.is_bottom() || held.meta.flags() & Meta::WIDE != 0 {
        return false;
    }
    let derived = derived as u64;
    derived.wrapping_sub(held.lo) <= held.ext
        || (derived < held.lo
            && derived.wrapping_add(stride as u64).wrapping_sub(held.lo) < held.ext)
}

/// Whether the capability names an instance other than the one that owns the address now.
///
/// False for every capability that names no instance, so a build where the runtime could not work
/// out which object a pointer is in is left with the answer it had before rather than given a
/// refusal it cannot stand behind. [`live`] says why.
///
/// Naming an instance is not the same as having been handed over, and the difference is the whole
/// of what this reads. [`Meta::RECOVERED`] says the capability was worked out at a boundary rather
/// than published by a caller, and that on its own is no reason to keep quiet: a recovery that
/// found an owned granule read the version of the instance that owned the address at the moment the
/// recovery ran, which is the instance the pointer was in then, so a later access finding a
/// different version is a later access to storage that instance no longer holds. That is the lock
/// and key rule with the key taken a little later than it might have been, and taking it later can
/// only miss a use after free that had already happened. It cannot report one that has not.
///
/// [`Meta::WIDE`] is the recovery that found no instance, which is the one this has to stay quiet
/// about. Its bounds are a whole mapping and its version is [`crate::plane::FOREIGN`], which is a
/// number no instance ever carries, so comparing it would refuse every access through a pointer
/// whose object the runtime was never told about. The odd version is the same case reached another
/// way: a slot holding an instance that had already been given back is not evidence about this
/// pointer either.
///
/// [`Meta::REBUILT`] is read and not one of them, and the reason is worth writing down because it
/// was nearly the other way. The flag says the capability came out of an aux slot, which holds a
/// displacement from the pointer it was written beside rather than an address, so it turns back
/// into the right answer only while the word still holds the pointer the slot was written for. A
/// `memcpy` of a structure used to be a way it did not, which is what tamnd/rucc#1148 was about,
/// and while that was true this had to stay quiet about a rebuilt capability or refuse an
/// instrumented SQLite inside `whereLoopInsert`. [`relocate`] moves the aux with the bytes now, so
/// the word and the slot stay together and the version a slot gives is about the pointer that came
/// out of the word beside it.
///
/// # Safety
///
/// `capability` is null or the address of a filled capability slot.
unsafe fn stale(capability: *const Cap, holder: Version) -> bool {
    if capability.is_null() {
        return false;
    }
    // SAFETY: the caller's contract, and a slot is `Cap::BYTES` of storage the frame owns.
    let held = unsafe { core::ptr::read(capability) };
    if held.is_bottom() || held.meta.flags() & Meta::WIDE != 0 {
        return false;
    }
    if !plane::owned(held.ver) {
        return false;
    }
    held.ver != holder
}

/// The fifteen names generated code is compiled against.
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
    use crate::layout::Cap;

    /// # Safety
    ///
    /// Called from generated code with the address of a descriptor the same build wrote into
    /// `.rucc_safety_desc`. `addr` is whatever the program computed and is never read through.
    /// `capability` is null or the address of a capability the same build reserved a slot for and
    /// filled, as [`__rucc_check_live`] takes one.
    ///
    /// The capability comes fourth rather than first, which is the other order from the lifetime
    /// check beside it. The three in front of it are the ones the access itself is about and they
    /// were here first, so leaving them where they are keeps them in the registers the caller would
    /// have put them in anyway, and the descriptor stays last for the reason this module says it
    /// does. The lifetime check has the capability first because it went in with the capability and
    /// has nothing to be moved out of the way.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_bounds(
        addr: *const c_void,
        size: usize,
        align: usize,
        capability: *const Cap,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::bounds(addr, size, align, capability, descriptor) };
    }

    /// # Safety
    ///
    /// As [`__rucc_check_bounds`], and `capability` is null or the address of a capability the same
    /// build reserved a slot for and filled.
    ///
    /// The capability comes first because that is the order tamnd/rucc#1241 wrote the call in, and
    /// it is the one argument here that is not about the access. It arrives filled now:
    /// `rucc_safety::origin` takes a capability where each pointer is made, `rucc_safety::lower`
    /// hands it to this call rather than dropping it, and `rucc_safety::slot` gives it four words of
    /// frame and passes their address. Null is still a thing this takes, since a build whose
    /// producer could not say anything about a pointer gets the bottom capability, and the weaker
    /// question is the answer for one of those.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_live(
        capability: *const Cap,
        addr: *const c_void,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: as above, and a null capability is one of the two this takes.
        unsafe { super::live(addr, capability, descriptor) };
    }

    /// # Safety
    ///
    /// As [`__rucc_check_live`], whose two pointer arguments are the two this has and in the same
    /// order. What differs is where generated code puts the call, which is in front of the free
    /// rather than in front of an access, and what the descriptor says, which is J6 rather than J1.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_free(
        capability: *const Cap,
        addr: *const c_void,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: as above, and a null capability is one of the two this takes.
        unsafe { super::freeing(addr, capability, descriptor) };
    }

    /// # Safety
    ///
    /// As [`__rucc_check_bounds`], for both pointers. `stride` is a width and is not read through,
    /// and `capability` is null or a filled slot, as [`__rucc_check_live`] takes one.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_deriv(
        base: *const c_void,
        derived: *const c_void,
        stride: usize,
        capability: *const Cap,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: as above, and a null capability is one of the two this takes.
        unsafe { super::deriv(base, derived, stride, capability, descriptor) };
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
    /// As [`__rucc_check_type`], whose arguments these are.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_typed_init(
        addr: *const c_void,
        size: usize,
        ty: u32,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::allowed(addr, size, ty, descriptor) };
    }

    /// [`__rucc_check_type`] over a range a loop reads, which is what `rucc_opt::hoist` writes.
    ///
    /// # Safety
    ///
    /// As [`__rucc_check_type`], whose arguments these are.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_type_range(
        addr: *const c_void,
        size: usize,
        ty: u32,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::swept(addr, size, Some(ty), false, descriptor) };
    }

    /// [`__rucc_check_init`] over a range a loop reads, which is what `rucc_opt::hoist` writes.
    ///
    /// # Safety
    ///
    /// As [`__rucc_check_init`], whose arguments these are.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_init_range(
        addr: *const c_void,
        size: usize,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::swept(addr, size, None, true, descriptor) };
    }

    /// [`__rucc_check_typed_init`] over a range a loop reads, which is what `rucc_opt::hoist`
    /// writes.
    ///
    /// # Safety
    ///
    /// As [`__rucc_check_type`], whose arguments these are.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_typed_init_range(
        addr: *const c_void,
        size: usize,
        ty: u32,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::swept(addr, size, Some(ty), true, descriptor) };
    }

    /// [`__rucc_check_type`] over the accesses of a walk that leaves gaps, which is what
    /// `rucc_opt::hoist` writes for one: `width` bytes at `addr` and at every `step` along from it
    /// that still ends inside `span` bytes of `addr`.
    ///
    /// # Safety
    ///
    /// As [`__rucc_check_type`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_type_strided(
        addr: *const c_void,
        span: usize,
        step: usize,
        width: usize,
        ty: u32,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::stepped(addr, (span, step, width), Some(ty), false, descriptor) };
    }

    /// [`__rucc_check_init`] over the accesses of a walk that leaves gaps, as
    /// [`__rucc_check_type_strided`] has them.
    ///
    /// # Safety
    ///
    /// As [`__rucc_check_init`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_init_strided(
        addr: *const c_void,
        span: usize,
        step: usize,
        width: usize,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::stepped(addr, (span, step, width), None, true, descriptor) };
    }

    /// [`__rucc_check_typed_init`] over the accesses of a walk that leaves gaps, as
    /// [`__rucc_check_type_strided`] has them.
    ///
    /// # Safety
    ///
    /// As [`__rucc_check_type`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_typed_init_strided(
        addr: *const c_void,
        span: usize,
        step: usize,
        width: usize,
        ty: u32,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::stepped(addr, (span, step, width), Some(ty), true, descriptor) };
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

    /// The third of the three a copy makes, and the one about the aux rather than about a plane.
    ///
    /// `__rucc_wrap_memcpy` has always called [`super::relocate`] beside the other two. This is the
    /// same call under a name generated code can reach, so that a copy the compiler wrote for an
    /// assignment of a whole structure carries the capability of every pointer in it the way a call
    /// to `memcpy` does.
    ///
    /// # Safety
    ///
    /// As [`__rucc_meta_init_copy`]. Neither address is read through and they may overlap.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_cap_copy(dst: *const c_void, src: *const c_void, len: usize) {
        // SAFETY: as above.
        unsafe { super::relocate(dst, src, len) };
    }

    /// # Safety
    ///
    /// As [`__rucc_meta_init`]. One address, and it is never read through.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_meta_init_handed(addr: *const c_void) {
        // SAFETY: as above.
        unsafe { super::handed(addr) };
    }

    /// # Safety
    ///
    /// As [`__rucc_meta_init`]. One address, and it is never read through.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_meta_epoch(addr: *const c_void, size: usize) {
        // SAFETY: as above.
        unsafe { super::stamped(addr, size) };
    }

    /// # Safety
    ///
    /// As [`__rucc_check_bounds`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_race(
        addr: *const c_void,
        size: usize,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::raced(addr, size, descriptor) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc::{alloc, dealloc};
    use crate::layout::perm;
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
        unsafe { super::bounds(addr, size, 1, core::ptr::null(), &raw const ROW) }
    }

    /// The bounds check over an access that is allowed to assume an alignment.
    ///
    /// Kept apart from [`bounds`] so that every test above that is about where an access lands
    /// says nothing about alignment, which is what passing one means.
    fn aligned(addr: *const c_void, size: usize, align: usize) {
        // SAFETY: as above.
        unsafe { super::bounds(addr, size, align, core::ptr::null(), &raw const ROW) }
    }

    /// The bounds check over an access that goes through a capability.
    ///
    /// Which is the path generated code takes wherever the capability survived to the access, and
    /// [`bounds`] is the one it takes where it did not. The alignment is one byte so that these say
    /// nothing about alignment either.
    fn within(addr: *const c_void, size: usize, capability: &Cap) {
        // SAFETY: as above, and the capability outlives the call.
        unsafe { super::bounds(addr, size, 1, capability, &raw const ROW) }
    }

    /// The liveness check, the same way, with no capability to compare against.
    ///
    /// Which is the weaker of the two questions [`super::live`] asks and is every test written
    /// before the capability reached it. [`held`] is the other one.
    fn live(addr: *const c_void) {
        // SAFETY: as above, and a null capability is one of the two this takes.
        unsafe { super::live(addr, core::ptr::null(), &raw const ROW) }
    }

    /// The liveness check over an access that goes through a capability.
    fn held(addr: *const c_void, capability: &Cap) {
        // SAFETY: as above, and the capability outlives the call.
        unsafe { super::live(addr, capability, &raw const ROW) }
    }

    /// The derivation check, the same way, over a stride of one byte.
    ///
    /// One byte because that is what character arithmetic has and it is the narrowest window the
    /// low end of the rule can open. The tests that are about the width pass their own.
    fn deriv(base: *const c_void, derived: *const c_void) {
        // SAFETY: as above.
        unsafe { super::deriv(base, derived, 1, core::ptr::null(), &raw const ROW) }
    }

    /// The derivation check over a stride the caller picks.
    fn stepped(base: *const c_void, derived: *const c_void, stride: usize) {
        // SAFETY: as above.
        unsafe { super::deriv(base, derived, stride, core::ptr::null(), &raw const ROW) }
    }

    /// The derivation check over a stride the caller picks, through a capability.
    fn carried(base: *const c_void, derived: *const c_void, stride: usize, capability: &Cap) {
        // SAFETY: as above, and the capability outlives the call.
        unsafe { super::deriv(base, derived, stride, capability, &raw const ROW) }
    }

    /// The type check and the init check as the one call a read that needs both makes.
    fn fused(addr: *const c_void, size: usize, ty: TypeId) {
        // SAFETY: as above.
        unsafe { allowed(addr, size, ty, &raw const ROW) }
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

    /// The judgement a store through a pointer shaped slot makes about who wrote it.
    fn stamped(addr: *const c_void, size: usize) {
        // SAFETY: as above.
        unsafe { super::stamped(addr, size) }
    }

    /// What the epoch plane holds for the granule at `addr`.
    fn stamp_at(addr: *const c_void) -> crate::epoch::Stamp {
        let addr = addr as usize;
        let region = alloc::covering(addr).expect("the instance is in a watched region");
        // SAFETY: the address is one an instance in the test owns, so the plane covers it.
        unsafe { region.epochs.read(addr) }
    }

    /// Puts a stamp in the plane as though some other thread had written the granule at `addr`.
    ///
    /// Really starting a thread and having it store would work and would test less. What the race
    /// check compares is two stamps, and a thread this one spawns and then joins is a thread the
    /// join has ordered, so the interesting stamp is one that has to be placed rather than earned.
    fn written_by(addr: *const c_void, stamp: crate::epoch::Stamp) {
        let addr = addr as usize;
        let region = alloc::covering(addr).expect("the instance is in a watched region");
        // SAFETY: as above.
        unsafe { region.epochs.write(addr, stamp) }
    }

    /// A thread number this thread does not have.
    fn somebody_else() -> u64 {
        crate::epoch::thread(crate::epoch::here()) + 1
    }

    /// The race check, with the descriptor argument filled in.
    fn raced(addr: *const c_void, size: usize) {
        // SAFETY: as in `bounds`.
        unsafe { super::raced(addr, size, &raw const ROW) }
    }

    /// The strided plane checks, asking both planes, with the descriptor argument filled in.
    fn strided(addr: *const c_void, span: usize, step: usize, width: usize, ty: TypeId) {
        // SAFETY: as in `bounds`.
        unsafe { super::stepped(addr, (span, step, width), Some(ty), true, &raw const ROW) }
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

    /// The capability of a whole instance the allocator just handed back.
    ///
    /// What `cap_of` reads out of the header in front of the payload, which is what generated code
    /// has in hand at a store into the object and is what the aux calls a container.
    fn whole(ptr: *mut c_void) -> Cap {
        let addr = ptr as usize;
        let region = alloc::covering(addr).expect("the allocator's own storage is watched");
        let (lo, ext) = recover::extent(&region, addr).expect("a live instance owns it");
        let meta = Meta::new(Class::Allocated, perm::READ | perm::WRITE, 0);
        Cap::new(lo as u64, ext as u64, owner(&region, addr), meta)
    }

    /// Writes the capability of the pointer `value` into the slot beside the word at `addr`.
    fn put(container: Cap, addr: *const c_void, value: *const c_void, cap: Cap) -> bool {
        // SAFETY: the container is a live instance this runtime's allocator laid out, so the aux
        // in front of it is the runtime's own storage, and neither address is read through.
        unsafe { crate::cap::store(container, addr, value, cap) }
    }

    /// What the slot beside the word at `addr` says about the pointer `value` that came out of it.
    fn took(container: Cap, addr: *const c_void, value: *const c_void) -> Cap {
        // SAFETY: as [`put`].
        unsafe { crate::cap::load(container, addr, value) }
    }

    /// The judgement a copy makes about the pointers it moved.
    fn relocate(dst: *const c_void, src: *const c_void, len: usize) {
        // SAFETY: as [`put`], for both ends of the copy.
        unsafe { super::relocate(dst, src, len) }
    }

    /// The judgement a byte-wise write makes about the pointers it wrote over.
    fn erase(addr: *const c_void, len: usize) {
        // SAFETY: as [`put`], over a range inside one live instance.
        unsafe { super::erase(addr, len) }
    }

    /// What the plane says about the aux slot beside the word at `addr`, which is the other half of
    /// what `crate::cap::torn` compares.
    fn slot_stamp(container: Cap, addr: *const c_void) -> crate::epoch::Stamp {
        let slot = aux_slot::address_of(container, addr as u64).expect("the word has a slot");
        let region = alloc::covering(addr as usize).expect("the instance is in a watched region");
        // SAFETY: the slot is in the aux of the block the word is in, which is inside the same
        // region, for the reason `crate::cap::pair` gives.
        unsafe { region.epochs.read(slot as usize) }
    }

    /// Every granule stamp over an instance, so a test can say nothing was written rather than
    /// saying one address was not.
    fn stamps(ptr: *mut c_void, size: usize) -> std::vec::Vec<crate::epoch::Stamp> {
        (0..size).step_by(WORD).map(|offset| stamp_at(at(ptr, offset))).collect()
    }

    #[test]
    fn a_byte_wise_write_over_a_pointer_word_records_who_wrote_over_it() {
        let _turn = turn();
        // The epoch half of tamnd/rucc#1307's audit. A wrapper taking a pointer out of a shared
        // structure is a store to a pointer word like any other, and a thread that loads the word
        // afterwards has to be able to find out that nothing ordered the two. Until this the
        // wrapper left whatever the last instrumented store had written, so the reader compared
        // itself against a stranger who was no longer the writer and the report went missing.
        let pointee = alloc(128);
        let holder = alloc(64);
        let word = at(holder, 24);
        assert!(put(whole(holder), word, pointee, whole(pointee)));
        let before = stamp_at(word);

        erase(word, 8);

        let after = stamp_at(word);
        assert_ne!(after, crate::epoch::NONE, "somebody wrote over the pointer");
        assert_ne!(after, before, "and it was this call rather than the store before it");
        assert_eq!(
            slot_stamp(whole(holder), word),
            after,
            "both halves wear one stamp, since one call changed both"
        );
        assert!(
            !crate::epoch::torn(after, slot_stamp(whole(holder), word)),
            "so the next load of that word is not called a torn store"
        );

        // SAFETY: the addresses `alloc` handed back.
        unsafe {
            dealloc(holder);
            dealloc(pointee);
        }
    }

    #[test]
    fn a_byte_wise_write_over_a_buffer_with_no_pointers_in_it_records_nothing() {
        let _turn = turn();
        // Which is nearly every buffer a `memset` is called about, and is why this costs what it
        // costs. The plane's granule is a pointer wide, so a granule two threads share is one
        // holding no pointer, and a word with no capability on either side of the call is the same
        // kind of word generated code's own stamping declines to watch.
        let ptr = alloc(64);
        assert!(stamps(ptr, 64).iter().all(|&s| s == crate::epoch::NONE));

        erase(at(ptr, 0), 64);

        assert!(
            stamps(ptr, 64).iter().all(|&s| s == crate::epoch::NONE),
            "a walk that found no slots wrote no stamps"
        );

        // SAFETY: the address `alloc` handed back.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_copy_that_moves_a_pointer_records_who_moved_it() {
        let _turn = turn();
        // The same sentence from the other side. A `memcpy` that puts a pointer into a structure
        // another thread can reach is a store to a pointer word, and the plane has to say so or a
        // reader over there has nothing to compare itself against.
        let pointee = alloc(128);
        let source = alloc(64);
        let target = alloc(64);
        assert!(put(whole(source), at(source, 24), pointee, whole(pointee)));
        // SAFETY: two live instances of sixty four bytes, copied whole.
        unsafe { core::ptr::copy_nonoverlapping(source.cast::<u8>(), target.cast::<u8>(), 64) };

        relocate(at(target, 0), at(source, 0), 64);

        let stamp = stamp_at(at(target, 24));
        assert_ne!(stamp, crate::epoch::NONE, "the copy wrote a pointer word");
        assert_eq!(slot_stamp(whole(target), at(target, 24)), stamp, "and both halves agree");
        assert_eq!(
            stamp_at(at(target, 0)),
            crate::epoch::NONE,
            "and said nothing about the words it moved that held no pointer"
        );

        // SAFETY: the addresses `alloc` handed back.
        unsafe {
            dealloc(source);
            dealloc(target);
            dealloc(pointee);
        }
    }

    #[test]
    fn a_store_records_which_thread_wrote_it_and_a_fresh_instance_remembers_nobody() {
        let _turn = turn();
        // The recording half on its own: a store lands in the plane, the granules the store did
        // not touch stay empty, and an instance beginning forgets whoever wrote these bytes when
        // they were somebody else's, which is the report the next occupant would otherwise be in.
        let ptr = alloc(64);
        assert_eq!(stamp_at(at(ptr, 0)), crate::epoch::NONE, "nobody has written it");

        stamped(at(ptr, 0), 8);
        let first = stamp_at(at(ptr, 0));
        assert_ne!(first, crate::epoch::NONE, "and now this thread has");
        assert_eq!(crate::epoch::thread(first), crate::epoch::thread(crate::epoch::here()));
        assert_eq!(stamp_at(at(ptr, 8)), crate::epoch::NONE, "the word beside it is untouched");

        stamped(at(ptr, 0), 8);
        assert!(
            crate::epoch::clock(stamp_at(at(ptr, 0))) > crate::epoch::clock(first),
            "a second store counts as a second store"
        );

        // SAFETY: the address `alloc` handed back, which is what `free` takes.
        unsafe { dealloc(ptr) };
        let again = alloc(64);
        assert_eq!(stamp_at(at(again, 0)), crate::epoch::NONE, "and the storage came back clean");
        // SAFETY: as above.
        unsafe { dealloc(again) };
    }

    #[test]
    fn a_word_another_thread_wrote_with_nothing_ordering_it_is_refused() {
        let _turn = turn();
        // Judgement J9, which is document 03's C2 and C3. The other thread's step is not one this
        // thread has got past, so nothing it has done orders the write before this access, and a
        // Lamport clock that is not behind is the whole of the evidence there is.
        let ptr = alloc(64);
        let ahead = crate::epoch::clock(crate::epoch::here()) + 1;
        written_by(at(ptr, 0), crate::epoch::stamp(somebody_else(), ahead));

        assert!(refused(|| raced(at(ptr, 0), 8)));
        assert!(refused(|| raced(at(ptr, 4), 4)), "any byte of the granule asks about the word");
        assert!(!refused(|| raced(at(ptr, 8), 8)), "and the word beside it is nobody's");
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_word_this_thread_wrote_and_one_it_has_got_past_are_both_allowed() {
        let _turn = turn();
        // The two ways an access is ordered against what it finds, which between them are nearly
        // every access in a program that locks correctly. Reporting either would be a report about
        // a program doing nothing wrong, and that is the failure this detector is not allowed.
        let ptr = alloc(64);
        stamped(at(ptr, 0), 8);
        assert!(!refused(|| raced(at(ptr, 0), 8)), "this thread wrote it");

        written_by(at(ptr, 0), crate::epoch::stamp(somebody_else(), 1));
        assert!(!refused(|| raced(at(ptr, 0), 8)), "and this thread is past where that one was");
        // SAFETY: as above.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn an_instance_beginning_forgets_the_thread_that_wrote_its_bytes_last_time() {
        let _turn = turn();
        // The same thing judgement J4 does to the other three planes, and here it is what keeps a
        // block that came back from being a race between its last owner's writer and this one.
        let first = alloc(64);
        let ahead = crate::epoch::clock(crate::epoch::here()) + 1;
        written_by(at(first, 0), crate::epoch::stamp(somebody_else(), ahead));
        assert!(refused(|| raced(at(first, 0), 8)));
        // SAFETY: `first` is a live instance.
        unsafe { dealloc(first) };

        let second = alloc(64);
        assert_eq!(second, first, "the test is about a block that came back");
        assert!(!refused(|| raced(at(second, 0), 8)));
        // SAFETY: as above.
        unsafe { dealloc(second) };
    }

    #[test]
    fn a_free_leaves_the_freeing_thread_over_the_bytes_it_ended() {
        let _turn = turn();
        // The recording half of document 03's C4. The lifetime plane says the storage is over and
        // cannot say whose doing that was, so the epoch plane carries the answer, and it is what a
        // later access from another thread is compared against.
        let ptr = alloc(64);
        let before = crate::epoch::clock(crate::epoch::here());
        // SAFETY: the address `alloc` handed back, which is what `free` takes.
        unsafe { dealloc(ptr) };

        let left = stamp_at(at(ptr, 0));
        assert_eq!(crate::epoch::thread(left), crate::epoch::thread(crate::epoch::here()));
        assert!(crate::epoch::clock(left) > before, "a free counts a step like any other store");
        assert_eq!(stamp_at(at(ptr, 56)), left, "the whole block, not the first word of it");
    }

    #[test]
    fn storage_nobody_owns_is_a_lifetime_question_rather_than_a_race_one() {
        let _turn = turn();
        // One bug gets one report. A free now leaves a stamp behind, so a stranger's stamp over
        // dead storage is something the race check would find, and it steps aside because the
        // liveness check is already going to say the same thing under the number it belongs to.
        let ptr = alloc(64);
        // SAFETY: as above.
        unsafe { dealloc(ptr) };
        let ahead = crate::epoch::clock(crate::epoch::here()) + 1;
        written_by(at(ptr, 0), crate::epoch::stamp(somebody_else(), ahead));

        assert!(!refused(|| raced(at(ptr, 0), 8)), "not this judgement's to report");
        assert!(refused(|| live(at(ptr, 0))), "and the one it is left to still reports it");
    }

    #[test]
    fn a_word_no_region_covers_is_never_a_race() {
        let _turn = turn();
        // A local or a global. There is no plane over it, so there is nothing that says who wrote
        // it, and a monitor that reported on what it did not watch would be reporting on programs
        // that are correct.
        let outside = 0u64;
        assert!(!refused(|| raced((&raw const outside).cast(), 8)));
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
    fn a_read_inside_one_granule_gets_the_answer_the_whole_walk_gives() {
        let _turn = turn();
        // The single granule answer takes only a yes from the two slots, so a granule that is
        // mixed or partly written has to come out the same as it did when every read walked.
        let ptr = alloc(64);
        judge(at(ptr, 0), 8, A);
        wrote(at(ptr, 0), 8);
        assert!(!refused(|| fused(at(ptr, 0), 8, A)));
        assert!(!refused(|| fused(at(ptr, 4), 4, types::CHARACTER)));
        assert!(refused(|| fused(at(ptr, 0), 8, B)));

        judge(at(ptr, 8), 4, A);
        judge(at(ptr, 12), 4, B);
        wrote(at(ptr, 8), 4);
        assert!(!refused(|| fused(at(ptr, 8), 4, A)));
        assert!(refused(|| fused(at(ptr, 8), 4, B)));
        assert!(refused(|| fused(at(ptr, 12), 4, B)), "the bytes it wants were never written");

        // Two granules, which the walk answers as it always did.
        wrote(at(ptr, 12), 4);
        assert!(refused(|| fused(at(ptr, 4), 12, A)));
        assert!(!refused(|| fused(at(ptr, 4), 12, types::CHARACTER)));
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
    fn a_strided_check_asks_about_the_accesses_and_not_the_bytes_between_them() {
        let _turn = turn();
        // A column of four byte elements sixteen bytes apart, which is `b[k * N + j]` in small. The
        // twelve bytes after each element are never written and never read, and the check over the
        // column is about the elements alone, so a check over the whole span would refuse where
        // this passes.
        let ptr = alloc(256);
        for k in 0..16 {
            judge(at(ptr, k * 16), 4, A);
            wrote(at(ptr, k * 16), 4);
        }
        assert!(!refused(|| strided(at(ptr, 0), 15 * 16 + 4, 16, 4, A)));
        assert!(refused(|| filled(at(ptr, 0), 15 * 16 + 4)), "the dense form reads the gaps");
        // Eight bytes at each element is four more than anything wrote.
        assert!(refused(|| strided(at(ptr, 0), 15 * 16 + 8, 16, 8, A)));
        // An element stored as another type is refused, and so is one that was never written, and
        // both only when it is one the walk reaches.
        judge(at(ptr, 7 * 16), 4, B);
        assert!(refused(|| strided(at(ptr, 0), 15 * 16 + 4, 16, 4, A)));
        assert!(!refused(|| strided(at(ptr, 0), 6 * 16 + 4, 16, 4, A)));
        assert!(!refused(|| strided(at(ptr, 8 * 16), 7 * 16 + 4, 16, 4, A)));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };

        let ptr = alloc(256);
        for k in (0..16).filter(|&k| k != 9) {
            judge(at(ptr, k * 16), 4, A);
            wrote(at(ptr, k * 16), 4);
        }
        judge(at(ptr, 9 * 16), 4, A);
        assert!(refused(|| strided(at(ptr, 0), 15 * 16 + 4, 16, 4, A)), "nine is unwritten");
        assert!(!refused(|| strided(at(ptr, 0), 15 * 16 + 4, 32, 4, A)), "and every other one");
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };

        // Eight byte elements over storage written as one type from end to end, where one slot of
        // each plane answers for each element, and the same refusals from there.
        let ptr = alloc(256);
        judge(at(ptr, 0), 256, A);
        wrote(at(ptr, 0), 256);
        assert!(!refused(|| strided(at(ptr, 0), 15 * 16 + 8, 16, 8, A)));
        judge(at(ptr, 5 * 16), 8, B);
        assert!(refused(|| strided(at(ptr, 0), 15 * 16 + 8, 16, 8, A)), "five is another type");
        assert!(!refused(|| strided(at(ptr, 8), 15 * 16, 16, 8, A)), "and in no other column");
        // SAFETY: `ptr` is a live instance.
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
    fn a_copy_carries_the_capability_beside_the_word_it_moved() {
        let _turn = turn();
        // tamnd/rucc#1148, and the shape tamnd/rucc#1338 turned out to be. A slot holds a
        // displacement from the pointer beside it rather than an address, so a copy that moves the
        // word and leaves the slot alone leaves the destination reading the previous tenant's
        // displacement against a pointer that has just arrived, which describes a span that
        // pointer is not inside.
        let src = alloc(64);
        let dst = alloc(64);
        let target = alloc(64);
        let older = alloc(64);

        // What the destination is carrying from whoever had the storage before it, which is a
        // pointer twenty four bytes into an object of thirty two.
        assert!(put(whole(dst), at(dst, 0), at(older, 24), whole(older).narrowed(0, 32)));
        // What the source is carrying, which is the base of its own object.
        assert!(put(whole(src), at(src, 0), at(target, 0), whole(target)));
        // The copy itself, which is what the `memcpy` a wrapper interposes does to the bytes and
        // is the whole of what it used to do.
        // SAFETY: one word inside two live instances of sixty four bytes each.
        unsafe { core::ptr::copy_nonoverlapping(src.cast::<u8>(), dst.cast::<u8>(), WORD) };

        let stale = took(whole(dst), at(dst, 0), at(target, 0));
        assert!(!stale.covers(at(target, 40) as u64, 8), "the previous tenant's displacement");

        relocate(at(dst, 0), at(src, 0), WORD);

        let moved = took(whole(dst), at(dst, 0), at(target, 0));
        assert_eq!(moved, took(whole(src), at(src, 0), at(target, 0)), "the slot came across");
        assert!(moved.covers(at(target, 40) as u64, 8), "and describes the object it points at");

        // SAFETY: four live instances, each the address its `alloc` handed back.
        unsafe {
            dealloc(src);
            dealloc(dst);
            dealloc(target);
            dealloc(older);
        }
    }

    #[test]
    fn a_copy_that_reaches_part_of_a_word_leaves_no_capability_beside_it() {
        let _turn = turn();
        // A slot is about the whole word, so a copy that moves half of one has not moved the
        // pointer the slot would be describing. Carrying the source's answer would describe an
        // object the destination word does not point at, and leaving the previous tenant's
        // standing is the fault above, so the only answer left is that the word holds no pointer.
        let dst = alloc(64);
        let src = alloc(64);
        let target = alloc(64);
        let older = alloc(64);

        assert!(put(whole(dst), at(dst, 8), at(older, 24), whole(older).narrowed(0, 32)));
        assert!(put(whole(src), at(src, 8), at(target, 0), whole(target)));
        // One word starting four bytes in, so it reaches part of two words and the whole of
        // neither.
        relocate(at(dst, 12), at(src, 12), WORD);

        assert!(took(whole(dst), at(dst, 8), at(target, 0)).is_bottom(), "cleared, not carried");
        assert!(took(whole(dst), at(dst, 16), at(target, 0)).is_bottom(), "and so is the next one");

        // SAFETY: as above.
        unsafe {
            dealloc(dst);
            dealloc(src);
            dealloc(target);
            dealloc(older);
        }
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
    fn a_read_through_a_stale_pointer_into_a_reused_block_is_refused() {
        let _turn = turn();
        // The half of use after free an address on its own cannot answer, which is what the
        // version compare is here for. The block goes back and comes out again, so the plane says
        // somebody owns it, and the only thing that says it is not the somebody this pointer was
        // made for is the version the capability was taken at.
        let ptr = alloc(64);
        let held_for = recover::made(ptr);
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
        let again = alloc(64);
        assert_eq!(again, ptr, "the allocator is expected to hand the same block back");
        assert!(refused(|| held(at(ptr, 0), &held_for)));
        // The pointer the second allocation gave out reads the same bytes and is not refused,
        // which is what says this is about the pointer rather than about the address.
        assert!(!refused(|| held(at(again, 0), &recover::made(again))));
        // SAFETY: `again` is a live instance.
        unsafe { dealloc(again) };
    }

    #[test]
    fn a_capability_that_says_nothing_leaves_the_answer_about_the_address_alone() {
        let _turn = turn();
        // Bottom names no instance and neither does a capability recovered from the mapping rather
        // than from the planes, whose version is a number no instance carries. Neither is evidence
        // that a pointer is stale, so the same read the test above refuses goes through under
        // both, which is the conservative direction.
        let ptr = alloc(64);
        let made = recover::made(ptr);
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
        let again = alloc(64);
        assert_eq!(again, ptr, "the allocator is expected to hand the same block back");
        assert!(!refused(|| held(at(ptr, 0), &Cap::BOTTOM)));
        let flags = Meta::RECOVERED | Meta::WIDE;
        let mapped = Cap::new(made.lo, made.ext, plane::FOREIGN, made.meta.with_flags(flags));
        assert!(!refused(|| held(at(ptr, 0), &mapped)));
        // A capability an aux slot rebuilt is not in this group, which is the whole of what
        // carrying the aux across a copy bought. It names an instance and the slot travelled with
        // the word, so the same read is refused under it.
        let rebuilt = made.meta.with_flags(made.meta.flags() | Meta::REBUILT);
        assert!(refused(|| held(at(ptr, 0), &Cap::new(made.lo, made.ext, made.ver, rebuilt))));
        // SAFETY: `again` is a live instance.
        unsafe { dealloc(again) };
    }

    #[test]
    fn a_capability_recovered_from_the_planes_still_answers_the_version_question() {
        let _turn = turn();
        // Where the pointer's provenance came from decides how much the answer catches, not
        // whether it is asked. A recovery that found an owned granule read the version of the
        // instance that owned the address when it ran, so a later access finding a different
        // version is an access to storage that instance no longer holds. Taking the key later than
        // the allocator would have can only miss a use after free that had already happened, and
        // this is the case that says it does not miss the ordinary one.
        let ptr = alloc(64);
        let walked = recover::recover(at(ptr, 8));
        assert!(walked.meta.flags() & Meta::RECOVERED != 0, "a walk of the planes is a recovery");
        assert!(walked.meta.flags() & Meta::WIDE == 0, "and it found the instance");
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
        let again = alloc(64);
        assert_eq!(again, ptr, "the allocator is expected to hand the same block back");
        assert!(refused(|| held(at(ptr, 8), &walked)));
        // SAFETY: `again` is a live instance.
        unsafe { dealloc(again) };
    }

    #[test]
    fn a_capability_for_a_block_that_has_not_been_freed_refuses_nothing() {
        let _turn = turn();
        // The compare has to be silent on every ordinary access or it would refuse the whole
        // program, so it is worth a test of its own rather than only the negative halves above.
        let ptr = alloc(64);
        let held_for = recover::made(ptr);
        for offset in [0, 8, 32, 63] {
            assert!(!refused(|| held(at(ptr, offset), &held_for)), "offset {offset}");
        }
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_capability_that_covers_the_access_permits_it_on_its_own() {
        let _turn = turn();
        // The fast path. The two numbers the capability carries are the instance's base and
        // extent, so every access inside the block is permitted by a subtraction and a compare
        // with no plane read at all, and the answers are the ones the plane would have given,
        // which is what makes this a shortcut rather than a second check.
        let ptr = alloc(64);
        let made = recover::made(ptr);
        for offset in [0, 8, 32, 60] {
            assert!(!refused(|| within(at(ptr, offset), 4, &made)), "offset {offset}");
        }
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_capability_narrower_than_the_access_leaves_the_planes_to_answer() {
        let _turn = turn();
        // The restraint the whole design rests on. A capability that does not cover the access is
        // not evidence that the access is wrong: it can be narrower than the object, or recovered
        // from a plane walk that found the storage around the pointer rather than the object the
        // pointer was made for. So it sends the question to the planes rather than refusing, and
        // the planes permit a read that is inside the block whatever the capability said.
        let ptr = alloc(64);
        let member = recover::made(ptr).narrowed(8, 16);
        assert!(!refused(|| within(at(ptr, 8), 16, &member)));
        assert!(!refused(|| within(at(ptr, 40), 4, &member)));
        // What the planes refuse they still refuse, which is the access that leaves the block.
        assert!(refused(|| within(at(ptr, 60), 8, &member)));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_capability_that_covers_nothing_leaves_the_planes_to_answer_too() {
        let _turn = turn();
        // Bottom and null cover nothing, so they permit nothing and need no case of their own.
        // Bottom matters most: it says nobody owns the address, which is the lifetime check's
        // refusal and not this one's, and refusing here would put the wrong sentence in the report.
        let ptr = alloc(64);
        assert!(!refused(|| within(at(ptr, 60), 4, &Cap::BOTTOM)));
        assert!(refused(|| within(at(ptr, 60), 8, &Cap::BOTTOM)));
        assert!(!refused(|| bounds(at(ptr, 60), 4)));
        assert!(refused(|| bounds(at(ptr, 60), 8)));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
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
    fn a_derivation_through_its_capability_is_answered_over_the_same_window() {
        let _turn = turn();
        // The capability answers the ones inside and the planes still refuse the ones outside, so
        // the window is the one the two tests either side of this pin down, one past the end at
        // the top and less than one element below the start at the bottom.
        let ptr = alloc(64);
        let cap = whole(ptr);
        let base = at(ptr, 32);
        let under = |back: usize| -> *const c_void { ptr.cast::<u8>().wrapping_sub(back).cast() };
        assert!(!refused(|| carried(base, at(ptr, 0), 1, &cap)));
        assert!(!refused(|| carried(base, at(ptr, 64), 1, &cap)));
        assert!(refused(|| carried(base, at(ptr, 65), 1, &cap)));
        assert!(!refused(|| carried(base, under(4), 4, &cap)));
        assert!(refused(|| carried(base, under(8), 4, &cap)));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_capability_too_narrow_for_a_derivation_leaves_it_to_the_planes() {
        let _turn = turn();
        // Only ever a reason to permit. Eight bytes of a sixty four byte instance says nothing
        // about the other fifty six, and the planes say they are the instance's.
        let ptr = alloc(64);
        let wide = whole(ptr);
        let cap = Cap::new(wide.lo, 8, wide.ver, wide.meta);
        assert!(!refused(|| carried(at(ptr, 0), at(ptr, 40), 1, &cap)));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_wide_capability_does_not_let_a_derivation_into_the_next_object() {
        let _turn = turn();
        // A recovery that found the mapping and not the instance covers every object in it, and
        // stepping from one of those into the next is the derivation this check is for.
        let ptr = alloc(64);
        let own = whole(ptr);
        let flags = Meta::RECOVERED | Meta::WIDE;
        let meta = Meta::new(Class::Mapped, perm::READ | perm::WRITE, 0).with_flags(flags);
        let cap = Cap::new(own.lo, 1 << 20, own.ver, meta);
        assert!(refused(|| carried(at(ptr, 0), at(ptr, 4096), 1, &cap)));
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
