//! The three checks generated code calls, and what each of them decides.
//!
//! Design: `spec/safe-memory/06-instrumentation.md` sections 6.3 and 6.3.1.
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
/// When the access is refused, which says what happened and stops the program.
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
pub unsafe fn deriv(base: *const c_void, derived: *const c_void, descriptor: *const Descriptor) {
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
    // The derived address rather than the base, because the base is where the pointer was allowed
    // to be and the derived one is where it went.
    // SAFETY: as in `bounds`.
    unsafe { crate::fail::report(descriptor, Some(derived)) }
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

/// The three names generated code is compiled against.
///
/// Separate from the functions above for the reason the allocator's exports are separate from its
/// logic: these are an ABI and those are Rust. The one difference that matters is that a panic may
/// not cross an `extern "C"` boundary, so a test that calls one of these to watch it refuse would
/// abort the harness rather than see a refusal. The tests call the plain functions.
///
/// Every one of them takes the descriptor last, so that the argument registers the address and the
/// size arrive in are the ones they would already be in.
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
    /// As [`__rucc_check_bounds`], for both pointers.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_deriv(
        base: *const c_void,
        derived: *const c_void,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: as above.
        unsafe { super::deriv(base, derived, descriptor) };
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

    /// The derivation check, the same way.
    fn deriv(base: *const c_void, derived: *const c_void) {
        // SAFETY: as above.
        unsafe { super::deriv(base, derived, &raw const ROW) }
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
        // The other end, which the one past the end rule must not accidentally allow: the byte
        // before an instance is its own header and belongs to nobody.
        let ptr = alloc(64);
        let base = at(ptr, 32);
        let under: *const c_void = ptr.cast::<u8>().wrapping_sub(1).cast();
        assert!(!refused(|| deriv(base, at(ptr, 0))));
        assert!(refused(|| deriv(base, under)));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
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
