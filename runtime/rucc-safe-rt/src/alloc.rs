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
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::fail::Judgement;
use crate::heap::Arena;
use crate::plane::{GRANULE, Lifetime, SLOT};

/// How much address space the heap is given.
///
/// A gibibyte, reserved rather than committed: the mapping is anonymous and pages arrive when
/// they are touched, so a program that allocates a kilobyte pays for a kilobyte. It is a fixed
/// size because growing the region would move the bias the plane is built on, and everything
/// that has ever held a capability would have to be told. Running out of it is not the end of the
/// heap either: a region is one entry in a table now, so more storage means another region beside
/// this one rather than this one moving.
pub const REGION: usize = 1 << 30;

/// How much shadow the region needs, which is one version per granule.
pub const SHADOW: usize = REGION / GRANULE * SLOT;

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
    arena: core::cell::UnsafeCell<Option<Arena>>,
}

// SAFETY: every path to the cell goes through `with`, which holds the lock across the whole of
// its access and hands out no reference that outlives it.
unsafe impl Sync for Heap {}

static HEAP: Heap = Heap { held: AtomicBool::new(false), arena: core::cell::UnsafeCell::new(None) };

impl Heap {
    /// Runs `f` against the one arena, making it on the first call.
    ///
    /// `None` when there is no arena and the reservation could not be made, which is a machine
    /// with no address space left and is the only reason this can fail.
    fn with<T>(&self, f: impl FnOnce(&mut Arena) -> T) -> Option<T> {
        while self
            .held
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        // SAFETY: the lock above is held until it is released below, and no reference derived
        // from the cell escapes this block, so this is the only live reference to it.
        let slot = unsafe { &mut *self.arena.get() };
        if slot.is_none() {
            *slot = reserve();
        }
        let answer = slot.as_mut().map(f);
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
    /// The lowest address in the region.
    pub base: usize,
    /// One past the highest.
    pub end: usize,
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
    /// The lowest address it covers.
    base: AtomicUsize,
    /// One past the highest.
    end: AtomicUsize,
}

impl Slot {
    /// An empty slot, which is what the whole table starts as.
    const fn empty() -> Self {
        Self { origin: AtomicUsize::new(0), base: AtomicUsize::new(0), end: AtomicUsize::new(0) }
    }
}

/// Every region the monitor watches, in the order they were published.
static SPACE: [Slot; REGIONS] = [const { Slot::empty() }; REGIONS];

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

/// Adds a region to the table, and says whether there was room.
///
/// False is a program with more than [`REGIONS`] arenas. Nothing here refuses anything over it:
/// the region is simply not watched, which is the same state every non heap address is already in
/// and is not a wrong answer about memory.
fn publish(origin: usize, base: usize, end: usize) -> bool {
    while SPACING.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err()
    {
        core::hint::spin_loop();
    }
    let at = FILLED.load(Ordering::Relaxed);
    let room = at < REGIONS;
    if room {
        SPACE[at].origin.store(origin, Ordering::Relaxed);
        SPACE[at].base.store(base, Ordering::Relaxed);
        SPACE[at].end.store(end, Ordering::Relaxed);
        // The release that publishes the three stores above, and the reason a reader may load them
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
    for slot in &SPACE[..filled] {
        let base = slot.base.load(Ordering::Relaxed);
        let end = slot.end.load(Ordering::Relaxed);
        if addr >= base && addr < end {
            // SAFETY: a published region is mapped for as long as the program runs, along with the
            // shadow the origin names, so the plane covers every address between the two above.
            let plane = unsafe { Lifetime::new(slot.origin.load(Ordering::Relaxed)) };
            return Some(Region { plane, base, end });
        }
    }
    None
}

/// How many regions are being watched, for the summary and for the tests.
#[must_use]
pub fn watched() -> usize {
    FILLED.load(Ordering::Acquire)
}

/// Maps the shadow and the region as one reservation, and builds the arena over it.
///
/// The shadow comes first so that the region's base is the higher of the two, which makes the
/// bias `shadow - region / GRANULE * SLOT` and makes it a subtraction that a reader can check.
/// The bias may still wrap, and [`Lifetime`] says so and does its arithmetic modularly.
fn reserve() -> Option<Arena> {
    let shadow = map(SHADOW + REGION)?;
    let region = shadow + SHADOW;
    let origin = shadow.wrapping_sub(region / GRANULE * SLOT);
    // Published before the arena is handed back, so that the first instance the arena creates is
    // already visible to a check by the time anything could hold a pointer to it. This is the first
    // call into an empty table, so there is room by construction.
    publish(origin, region, region + REGION);
    // SAFETY: the mapping is readable, writable, private and anonymous, so it is zero filled and
    // owned by this process alone, and it is never unmapped, so it outlives everything built over
    // it. The region is page aligned and therefore granule aligned, the shadow covers exactly the
    // region's granules by the arithmetic above, and `IDENTITY` belongs to this arena alone.
    Some(unsafe { Arena::new(Lifetime::new(origin), region, REGION, IDENTITY) })
}

/// Asks the operating system for `len` bytes of zeroed, private address space.
///
/// Zero when it refuses, which is what makes an allocation fail rather than what makes the
/// program stop: a `malloc` that cannot get memory returns null, and that is true of this one for
/// the same reason it is true of everyone else's.
fn map(len: usize) -> Option<usize> {
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
    match HEAP.with(|arena| arena.begin(size)) {
        Some(0) | None => core::ptr::null_mut(),
        Some(payload) => payload as *mut c_void,
    }
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
    let ended = HEAP.with(|arena| {
        arena.contains(payload)
            // SAFETY: the address is inside the region, which is what `end` asks for. Everything
            // else about it is the judgement rather than a precondition.
            && unsafe { arena.end(payload) }.is_ok()
    });
    // `None` is a free before anything was ever allocated, which is the same judgement: whatever
    // that pointer is, it is not one of ours.
    if ended != Some(true) {
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
    let old = HEAP.with(|arena| {
        arena.contains(payload).then(|| {
            // SAFETY: the address is inside the region, which is what `extent` asks for.
            unsafe { arena.extent(payload) }
        })
    });
    let Some(Some(Ok(old))) = old else { crate::fail::refused(Judgement::Free) };

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
    // SAFETY: as above, and the copy is done with it.
    unsafe { dealloc(ptr) };
    fresh
}

/// The four names a hosted program actually links.
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
        HEAP.with(|arena| {
            // SAFETY: the address came out of this arena, so it is inside its region.
            unsafe { arena.version(ptr as usize) }
        })
        .expect("the heap exists by now")
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
}
