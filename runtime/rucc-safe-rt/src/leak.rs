//! Row T9 of document 03: what is still allocated when the program exits and nothing points at.
//!
//! Design: `spec/safe-memory/08-temporal-safety.md` section 8.7, and `rucc_safety::leaks` for the
//! two calls the compiler puts in a program built with `-fsafety-leaks`. A leak is not an access
//! that went wrong, so nothing here is a check and nothing here stops the program. It is one walk of
//! the heap at exit, from everything the program can still reach, and a report of what the walk did
//! not get to.
//!
//! # The roots
//!
//! The writable segments of every object the loader has mapped, which is the program's globals and
//! every library's, and the static thread locals of each of them. The C library's count, and have
//! to: `stdout`'s buffer comes out of this heap, since this crate is the program's `malloc`, and
//! the only thing pointing at it is a field of a `FILE` in the C library's own data.
//!
//! The stack only when the program is leaving through `exit`, and then only from the frame that
//! called it upwards. `rucc_safety::leaks` says why: a program that returned from `main` has no
//! frames of its own left, and the bytes they held are still under the C library's, so scanning
//! them would call an allocation reachable because a function that returned long ago once held it.
//! The registers a callee keeps for its caller are saved at the same moment, since a pointer an
//! outer frame holds in one of those is in no frame at all until somebody spills it.
//!
//! What is not a root is any mapping the program made for itself, which is what LeakSanitizer does
//! too, and memory the C library allocated before this heap existed. Neither is the stack of
//! another thread, and a program with more than one still running when it exits is not swept: the
//! stacks are where those threads keep what they are working on, and without them the report would
//! be every allocation a thread was in the middle of using.
//!
//! # The edges
//!
//! Every aligned word of a payload that was reached, read as an address, and an address anywhere
//! inside an instance's request reaches it. That is conservative where section 8.7 asks for
//! precise, and the reason is the aux plane: the slot beside a word would say exactly which words
//! are pointers, but nothing writes a slot yet, which is tamnd/rucc#856. A conservative edge can
//! keep a leak alive, when an integer happens to look like an address inside one, and it can never
//! report an allocation the program could still reach, which is the right way round for a check
//! with no way to be argued with.
//!
//! # What is said
//!
//! A banner of its own, since a leak is not a memory safety violation and a harness that counts
//! those should not count this. The size and address of each instance left, up to a handful, and
//! the totals. Where each was allocated is not said, because the allocator does not keep it: that
//! is a word per instance in the header and an unwind per `malloc`, and it is the next thing this
//! wants rather than something to fake.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// The first line of every leak report, which is what a harness looks for.
pub const BANNER: &str = "rucc: memory leak";

/// How many leaked instances the report lists one by one before it only counts them.
///
/// Not a tuning number: a program that leaks in a loop leaks the same thing thousands of times, and
/// the first few say as much as the rest do.
pub const LISTED: usize = 8;

/// One instance the heap holds, and whether the sweep got to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Held {
    /// Where its payload starts.
    pub lo: usize,
    /// How many bytes the program asked for.
    pub len: usize,
    /// Whether anything the program can reach points into it.
    pub reached: bool,
}

/// The walk: the instances, sorted by where they start, and the ones reached but not yet scanned.
#[derive(Debug)]
pub struct Sweep<'a> {
    held: &'a mut [Held],
    work: &'a mut [usize],
    pending: usize,
}

impl<'a> Sweep<'a> {
    /// A walk over `held`, which is sorted by `lo` and does not overlap, with room in `work` for
    /// one entry per instance, which is the most it can ever hold since nothing is reached twice.
    #[must_use]
    pub fn new(held: &'a mut [Held], work: &'a mut [usize]) -> Self {
        debug_assert!(work.len() >= held.len());
        Self { held, work, pending: 0 }
    }

    /// The instance `addr` points into, if any.
    ///
    /// An instance asked for with no bytes is still an address the program was given and may keep,
    /// so it is reached by a pointer to exactly where it starts.
    fn find(&self, addr: usize) -> Option<usize> {
        let after = self.held.partition_point(|held| held.lo <= addr);
        let at = after.checked_sub(1)?;
        let held = self.held[at];
        (addr - held.lo < held.len.max(1)).then_some(at)
    }

    /// Takes `value` as an address and reaches whatever it points into.
    pub fn mark(&mut self, value: usize) {
        let Some(at) = self.find(value) else { return };
        if self.held[at].reached {
            return;
        }
        self.held[at].reached = true;
        self.work[self.pending] = at;
        self.pending += 1;
    }

    /// Reaches whatever any aligned word of `[lo, hi)` points into.
    ///
    /// # Safety
    ///
    /// Every byte of the range is mapped and readable.
    pub unsafe fn scan(&mut self, lo: usize, hi: usize) {
        const WORD: usize = size_of::<usize>();
        let mut at = lo.next_multiple_of(WORD);
        while at.saturating_add(WORD) <= hi {
            // SAFETY: the caller says the range is readable, and `at` is aligned and the word is
            // inside it. Volatile because what is there may be bytes nothing ever wrote, and the
            // read is about the bits rather than about a value the compiler may reason over.
            let value = unsafe { core::ptr::read_volatile(at as *const usize) };
            self.mark(value);
            at += WORD;
        }
    }

    /// Scans every instance reached so far, and every one those reach, until nothing new is.
    ///
    /// # Safety
    ///
    /// Every instance in the table is mapped and readable over what it asked for.
    pub unsafe fn trace(&mut self) {
        while self.pending > 0 {
            self.pending -= 1;
            let held = self.held[self.work[self.pending]];
            // SAFETY: the caller says every instance is readable over its request.
            unsafe { self.scan(held.lo, held.lo + held.len) };
        }
    }

    /// The instances nothing reached.
    pub fn left(&self) -> impl Iterator<Item = &Held> {
        self.held.iter().filter(|held| !held.reached)
    }
}

/// Writes the report for what the sweep left, or nothing when it left nothing.
pub fn render(sweep: &Sweep<'_>, mut emit: impl FnMut(&str)) {
    let (mut count, mut bytes) = (0_u64, 0_u64);
    for held in sweep.left() {
        count += 1;
        bytes += held.len as u64;
    }
    if count == 0 {
        return;
    }
    let mut out = crate::report::Text::new();
    out.text(BANNER).text("\n");
    out.text("  row T9, allocated, never freed, and nothing the program can reach points at it\n");
    emit(out.as_str());
    for held in sweep.left().take(LISTED) {
        let mut out = crate::report::Text::new();
        out.text("  ").dec(held.len as u64).text(" bytes at ").hex(held.lo).text("\n");
        emit(out.as_str());
    }
    let mut out = crate::report::Text::new();
    if count > LISTED as u64 {
        out.text("  and ").dec(count - LISTED as u64).text(" more\n");
    }
    out.text("  ").dec(bytes).text(" bytes in ").dec(count);
    out.text(if count == 1 { " allocation\n" } else { " allocations\n" });
    emit(out.as_str());
}

/// Whether the sweep has been handed to `atexit` yet.
static ARMED: AtomicBool = AtomicBool::new(false);

/// Where the stack ended when the program said it was leaving through `exit`, or 0 if it has not.
static LEAVING: AtomicUsize = AtomicUsize::new(0);

/// How many registers [`SAVED`] keeps, which is the most any target here has that a callee keeps
/// for its caller: `x19` to `x29` on AArch64.
const REGISTERS: usize = 11;

/// The registers a callee keeps for its caller, as they were when the program said it was leaving.
static SAVED: [AtomicUsize; REGISTERS] = [const { AtomicUsize::new(0) }; REGISTERS];

/// Arms the sweep, once however many units ask.
///
/// What every unit built with `-fsafety-leaks` puts in its constructor section, so it runs before
/// `main`. `atexit` rather than a destructor because the C library runs the handlers it was given
/// in the opposite order, and one registered this early runs after everything the program
/// registered itself, which may free things, and before the loader's destructors, which may too.
#[unsafe(no_mangle)]
pub extern "C" fn __rucc_leaks_watch() {
    if ARMED.swap(true, Ordering::Relaxed) {
        return;
    }
    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    {
        unsafe extern "C" {
            fn atexit(f: extern "C" fn()) -> i32;
        }
        // SAFETY: `atexit` is the C library's and `swept` takes nothing and returns.
        unsafe { atexit(swept) };
    }
}

/// Says the program is leaving through `exit`, so the stack from the caller up is still its own.
///
/// What the compiler puts in front of every call to `exit` under `-fsafety-leaks`. The registers
/// first, before anything in this function has had a reason to use one.
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn __rucc_leaks_exiting() {
    let to = SAVED.as_ptr().cast::<usize>();
    #[cfg(target_arch = "x86_64")]
    // SAFETY: `SAVED` has room for the six words written, the pointer is in a register the asm
    // names so it cannot be one of the six, and nothing is written but those words.
    unsafe {
        core::arch::asm!(
            "mov [rax], rbx",
            "mov [rax + 8], rbp",
            "mov [rax + 16], r12",
            "mov [rax + 24], r13",
            "mov [rax + 32], r14",
            "mov [rax + 40], r15",
            in("rax") to,
            options(nostack, preserves_flags),
        );
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: `SAVED` has room for the eleven words written, the pointer is in a register the asm
    // names so it cannot be one of them, and nothing is written but those words.
    unsafe {
        core::arch::asm!(
            "stp x19, x20, [x9]",
            "stp x21, x22, [x9, #16]",
            "stp x23, x24, [x9, #32]",
            "stp x25, x26, [x9, #48]",
            "stp x27, x28, [x9, #64]",
            "str x29, [x9, #80]",
            in("x9") to,
            options(nostack, preserves_flags),
        );
    }
    let _ = to;
    let here = 0_u8;
    LEAVING.store(core::hint::black_box(&raw const here) as usize, Ordering::Relaxed);
}

/// What `atexit` runs.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
extern "C" fn swept() {
    sweep::run();
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
mod sweep {
    use core::ffi::c_void;
    use core::sync::atomic::Ordering;

    use super::{Held, LEAVING, SAVED, Sweep};

    /// The sweep itself, with everything it needs taken from the operating system.
    pub(super) fn run() {
        if threads() > 1 {
            crate::report::emit("rucc: leak sweep skipped, other threads are still running\n");
            return;
        }
        // First, because asking the C library where the main thread's stack is reads a file, and
        // reading a file allocates, and nothing may allocate once the table below is taken.
        let leaving = LEAVING.load(Ordering::Relaxed);
        let stack = match leaving {
            0 => None,
            lo => crate::stack::bounds().map(|(_, hi)| (lo, hi)),
        };

        let mut count = 0;
        crate::alloc::instances(|_, _| count += 1);
        if count == 0 {
            return;
        }
        let bytes = count * (size_of::<Held>() + size_of::<usize>());
        let Some(at) = crate::alloc::map(bytes) else { return };
        // SAFETY: the mapping is fresh, private, writable and long enough for both arrays, the
        // first at its start and the second right after, and `Held` and `usize` are both aligned
        // to no more than a page.
        let (held, work) = unsafe {
            (
                core::slice::from_raw_parts_mut(at as *mut Held, count),
                core::slice::from_raw_parts_mut(
                    (at + count * size_of::<Held>()) as *mut usize,
                    count,
                ),
            )
        };
        let mut filled = 0;
        crate::alloc::instances(|lo, len| {
            if filled < held.len() {
                held[filled] = Held { lo, len, reached: false };
                filled += 1;
            }
        });
        let held = &mut held[..filled];
        held.sort_unstable_by_key(|held| held.lo);
        let mut sweep = Sweep::new(held, work);

        if let Some((lo, hi)) = stack {
            for saved in &SAVED {
                sweep.mark(saved.load(Ordering::Relaxed));
            }
            // SAFETY: from the frame that called `exit` to the top of this thread's stack, which
            // is all mapped, and the frames in it are the program's and are still there.
            unsafe { sweep.scan(lo, hi) };
        }
        // SAFETY: the callback is handed a pointer to `sweep`, which outlives the call.
        unsafe { dl_iterate_phdr(each, (&raw mut sweep).cast()) };
        // SAFETY: every instance in the table is live, so its payload is mapped.
        unsafe { sweep.trace() };
        super::render(&sweep, crate::report::emit);
        // SAFETY: the mapping is the one made above and nothing refers to it after this.
        unsafe { munmap(at as *mut c_void, bytes) };
    }

    /// `ElfW(Phdr)` on a 64 bit target.
    #[repr(C)]
    struct Phdr {
        kind: u32,
        flags: u32,
        offset: u64,
        vaddr: u64,
        paddr: u64,
        filesz: u64,
        memsz: u64,
        align: u64,
    }

    /// `struct dl_phdr_info`, as far as the thread local fields, which the size argument says are
    /// there or not.
    #[repr(C)]
    struct Info {
        addr: usize,
        name: *const u8,
        phdr: *const Phdr,
        phnum: u16,
        adds: u64,
        subs: u64,
        tls_modid: usize,
        tls_data: *mut c_void,
    }

    const PT_LOAD: u32 = 1;
    const PT_TLS: u32 = 7;
    const PF_W: u32 = 2;

    unsafe extern "C" {
        fn dl_iterate_phdr(
            callback: unsafe extern "C" fn(*mut Info, usize, *mut c_void) -> i32,
            data: *mut c_void,
        ) -> i32;
        fn munmap(addr: *mut c_void, len: usize) -> i32;
        fn open(path: *const core::ffi::c_char, flags: i32, ...) -> i32;
        fn read(fd: i32, buf: *mut c_void, len: usize) -> isize;
        fn close(fd: i32) -> i32;
    }

    /// Scans one loaded object's writable segments and this thread's copy of its thread locals.
    unsafe extern "C" fn each(info: *mut Info, size: usize, data: *mut c_void) -> i32 {
        // SAFETY: the loader hands a valid `Info` for the length of the call, and `data` is the
        // `Sweep` `run` passed in, which nothing else touches while this runs.
        let (info, sweep) = unsafe { (&*info, &mut *data.cast::<Sweep<'_>>()) };
        let tls = if size >= core::mem::offset_of!(Info, tls_data) + size_of::<usize>() {
            info.tls_data as usize
        } else {
            0
        };
        for at in 0..usize::from(info.phnum) {
            // SAFETY: the loader says there are `phnum` headers there.
            let phdr = unsafe { &*info.phdr.add(at) };
            let len = phdr.memsz as usize;
            match phdr.kind {
                PT_LOAD if phdr.flags & PF_W != 0 => {
                    let lo = info.addr.wrapping_add(phdr.vaddr as usize);
                    // SAFETY: a loaded segment is mapped over its whole memory size, and one the
                    // loader mapped writable is readable.
                    unsafe { sweep.scan(lo, lo + len) };
                }
                // SAFETY: the loader's copy of this object's thread locals for this thread, which
                // is the segment's memory size long.
                PT_TLS if tls != 0 => unsafe { sweep.scan(tls, tls + len) },
                _ => {}
            }
        }
        0
    }

    /// How many threads the process has, from the twentieth field of `/proc/self/stat`.
    ///
    /// One when the file cannot be read, which is a process with no `/proc` mounted, since that
    /// is far more often a container than a program with threads still running at exit.
    fn threads() -> usize {
        let mut buf = [0_u8; 512];
        // SAFETY: the path is terminated, and the buffer is ours and as long as it says.
        let len = unsafe {
            let fd = open(c"/proc/self/stat".as_ptr(), 0);
            if fd < 0 {
                return 1;
            }
            let len = read(fd, buf.as_mut_ptr().cast(), buf.len());
            close(fd);
            len
        };
        let Ok(len) = usize::try_from(len) else { return 1 };
        counted(&buf[..len]).unwrap_or(1)
    }

    /// The thread count out of the text of `/proc/self/stat`.
    ///
    /// The name in the second field is in parentheses and may hold spaces and parentheses of its
    /// own, so the count is the eighteenth field after the last closing one.
    pub(super) fn counted(stat: &[u8]) -> Option<usize> {
        let after = stat.iter().rposition(|&b| b == b')')?;
        let rest = core::str::from_utf8(&stat[after + 1..]).ok()?;
        rest.split_ascii_whitespace().nth(17)?.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Some heap-like storage the tests point into: `count` instances of `len` words each, all in
    /// one buffer so that their addresses sort the way they were made.
    fn storage(count: usize, len: usize) -> (std::vec::Vec<usize>, std::vec::Vec<Held>) {
        let words = std::vec![0_usize; count * len];
        let held = (0..count)
            .map(|at| Held {
                lo: words.as_ptr() as usize + at * len * size_of::<usize>(),
                len: len * size_of::<usize>(),
                reached: false,
            })
            .collect();
        (words, held)
    }

    fn left(sweep: &Sweep<'_>) -> std::vec::Vec<usize> {
        sweep.left().map(|held| held.lo).collect()
    }

    #[test]
    fn what_a_root_reaches_is_kept_and_so_is_what_that_reaches() {
        let (mut words, mut held) = storage(4, 4);
        let lo: std::vec::Vec<usize> = held.iter().map(|held| held.lo).collect();
        // Zero points at one, through an interior pointer, and one at two. Three is pointed at only
        // by itself, which is a cycle nothing reaches.
        words[1] = lo[1] + 8;
        words[4 + 3] = lo[2];
        words[12] = lo[3];
        let root = [lo[0] + 31];
        let mut work = std::vec![0; held.len()];
        let mut sweep = Sweep::new(&mut held, &mut work);
        // SAFETY: the root is an array this test owns, and every instance is inside `words`.
        unsafe {
            sweep.scan(root.as_ptr() as usize, root.as_ptr() as usize + size_of_val(&root));
            sweep.trace();
        }
        assert_eq!(left(&sweep), [lo[3]]);
    }

    #[test]
    fn an_empty_instance_is_reached_at_its_start_and_nothing_below_the_first_is_reached() {
        let (_words, mut held) = storage(2, 2);
        let lo: std::vec::Vec<usize> = held.iter().map(|held| held.lo).collect();
        held[1].len = 0;
        let mut work = std::vec![0; held.len()];
        let mut sweep = Sweep::new(&mut held, &mut work);
        // One past the end of the first is the start of the second, so it has to go to the second
        // and not the first.
        sweep.mark(lo[0] + 16);
        assert_eq!(left(&sweep), [lo[0]]);
        sweep.mark(lo[0] - 1);
        assert_eq!(left(&sweep), [lo[0]]);
    }

    #[test]
    fn a_word_that_is_not_aligned_is_not_read() {
        let (_words, mut held) = storage(1, 2);
        let target = held[0].lo;
        let mut work = std::vec![0; held.len()];
        let mut sweep = Sweep::new(&mut held, &mut work);
        let mut bytes = [0_u8; 24];
        let base = (bytes.as_mut_ptr() as usize).next_multiple_of(8);
        let skew = base - bytes.as_ptr() as usize;
        bytes[skew + 4..skew + 12].copy_from_slice(&target.to_ne_bytes());
        // SAFETY: the range is inside `bytes`.
        unsafe { sweep.scan(base, base + 16) };
        assert_eq!(left(&sweep), [target]);
    }

    #[test]
    fn the_report_lists_a_few_and_counts_the_rest() {
        let (_words, mut held) = storage(LISTED + 2, 1);
        held[0].reached = true;
        let mut work = std::vec![0; held.len()];
        let sweep = Sweep::new(&mut held, &mut work);
        let mut text = std::string::String::new();
        render(&sweep, |s| text.push_str(s));
        assert!(text.starts_with(BANNER), "{text}");
        assert_eq!(text.matches(" bytes at 0x").count(), LISTED, "{text}");
        assert!(text.contains("  and 1 more\n"), "{text}");
        assert!(text.ends_with("  72 bytes in 9 allocations\n"), "{text}");
    }

    #[test]
    fn nothing_is_said_when_nothing_was_left() {
        let (_words, mut held) = storage(2, 1);
        for one in &mut held {
            one.reached = true;
        }
        let mut work = std::vec![0; held.len()];
        let sweep = Sweep::new(&mut held, &mut work);
        let mut said = false;
        render(&sweep, |_| said = true);
        assert!(!said);
    }

    #[test]
    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    fn the_thread_count_is_read_past_a_name_with_parentheses_in_it() {
        let stat = b"42 (a (b) c) S 1 42 42 0 -1 4194560 100 0 0 0 0 0 0 0 20 0 3 0 12345 0 0";
        assert_eq!(sweep::counted(stat), Some(3));
        assert_eq!(sweep::counted(b"42 (x) S 1"), None);
    }
}
