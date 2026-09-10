//! The three checks generated code calls, what each of them decides, and the one question it asks.
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
//! The lifetime plane, and nothing else. A version covers one granule of sixteen bytes, so what
//! these decide is decided per granule.
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

/// Judgement J1, the bounds half: an access of `size` bytes at `addr` stays in one instance.
///
/// The first byte and the last byte have to be owned by the same version. An access that starts
/// inside an instance and ends past it lands in the next block's header, in the neighbour, or in
/// storage nobody owns, and all three read as a different version.
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
pub unsafe fn bounds(addr: *const c_void, size: usize, descriptor: *const Descriptor) {
    let addr = addr as usize;
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

/// How many of the `want` bytes from `addr` on belong to whoever owns `addr`.
///
/// Not a judgement. Nothing here refuses anything and nothing here reports anything, because this
/// is a question the compiler asks before a loop runs so that it can leave the checks out of part
/// of it. `spec/safe-memory/07-check-elimination.md` section 7.4 splits a loop at
/// `min(n, extent / sizeof(T))`, runs that part with no checks in it and runs whatever is left with
/// them, and this is where the extent comes from.
///
/// The answer is never more than `want`, and it is allowed to be less than the truth. Under this
/// milestone answering means walking the plane, so a walk that stops once it has covered the bytes
/// the loop was going to read is bounded by an eighth of the work the loop is already doing, and a
/// short answer costs iterations in the checked half rather than being wrong. What is not allowed
/// is an answer larger than the truth, which is why the walk stops at the first granule that reads
/// as somebody else's.
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
    if !plane::owned(instance) {
        return 0;
    }
    // The rest of the granule the address is in, which is owned by definition, since the version
    // that covers the address covers every byte that shares its slot. A walk that started at the
    // next granule would say nothing about an address in the middle of one.
    let mut covered = plane::GRANULE - addr % plane::GRANULE;
    let mut next = addr.wrapping_add(covered);
    while covered < want && region.holds(next) && owner(&region, next) == instance {
        covered += plane::GRANULE;
        next = next.wrapping_add(plane::GRANULE);
    }
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
/// never more, `want` back for an address no watched region covers, and zero for an address whose
/// granule is owned by nobody.
#[must_use]
pub fn extent_back(addr: *const c_void, want: usize) -> usize {
    let addr = addr as usize;
    if addr == 0 {
        return 0;
    }
    let last = addr - 1;
    let Some(region) = alloc::covering(last) else { return want };
    let instance = owner(&region, last);
    if !plane::owned(instance) {
        return 0;
    }
    // The part of the granule the last byte is in that lies below the address, which is owned by
    // definition, for the reason the forward walk gives about the granule it starts in.
    let mut covered = last % plane::GRANULE + 1;
    let mut next = last.wrapping_sub(covered);
    while covered < want && covered < addr && region.holds(next) && owner(&region, next) == instance
    {
        covered += plane::GRANULE;
        next = next.wrapping_sub(plane::GRANULE);
    }
    covered.min(want)
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

/// The five names generated code is compiled against.
///
/// Separate from the functions above for the reason the allocator's exports are separate from its
/// logic: these are an ABI and those are Rust. The one difference that matters is that a panic may
/// not cross an `extern "C"` boundary, so a test that calls one of these to watch it refuse would
/// abort the harness rather than see a refusal. The tests call the plain functions.
///
/// The three checks take the descriptor last, so that the argument registers the address and the
/// size arrive in are the ones they would already be in. Neither extent query has a descriptor,
/// because they decide nothing and so have nothing to report.
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
        descriptor: *const Descriptor,
    ) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::bounds(addr, size, descriptor) };
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
        unsafe { super::bounds(addr, size, &raw const ROW) }
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
