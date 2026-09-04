//! The four block routines the backend calls when a copy is too big to open up.
//!
//! Design: `spec/12-abi-and-runtime.md` section 12.8. `rucc-codegen` unrolls a small structure
//! copy or a small fill into moves and calls the library for the rest, so on a hosted target
//! these names come from the C library and are never reached. On a freestanding target there is
//! no C library and this is what the call finds.
//!
//! The names are the C ones because that is what the backend emits and what a GCC-built object
//! linked beside ours refers to. The return values are the C ones too, even though nothing we
//! emit reads them, because a C program is allowed to.
//!
//! # Why the loops are shaped this way
//!
//! A byte at a time is correct everywhere and slow everywhere. A word at a time is what makes
//! these worth having, and it needs both pointers to be aligned, because an unaligned load is
//! merely slow on x86-64 and is a fault on targets further down the ladder. So each routine
//! walks bytes until the destination is aligned, then walks words while both stay aligned, then
//! walks the bytes that are left. When the two pointers are out of phase with each other the
//! word loop cannot run at all and the whole thing is the byte loop, which is the honest answer
//! rather than a fast wrong one.

// Only the C entry points below name it, and they are not compiled under `cargo test`.
#[cfg(not(test))]
use core::ffi::c_void;

/// How many bytes a word is on this target, which is what the aligned loops step by.
const WORD: usize = size_of::<usize>();

/// Copies `n` bytes from `src` to `dest`, lowest address first.
///
/// # Safety
///
/// Both ranges must be readable and writable for `n` bytes and must not overlap.
#[inline]
pub unsafe fn copy_forward(dest: *mut u8, src: *const u8, n: usize) {
    let mut at = 0;
    // Bytes up to the first aligned destination, and no further than the copy itself.
    let head = dest.align_offset(WORD).min(n);
    while at < head {
        // SAFETY: `at` is below `n` and the caller says `n` bytes of each are usable.
        unsafe { dest.add(at).write(src.add(at).read()) };
        at += 1;
    }
    // Only when the source landed aligned too. Out of phase, this loop never runs and the tail
    // below copies everything.
    if src.wrapping_add(at).align_offset(WORD) == 0 {
        while at + WORD <= n {
            // SAFETY: both pointers are word aligned here and `at + WORD` is within `n`.
            unsafe {
                dest.add(at).cast::<usize>().write(src.add(at).cast::<usize>().read());
            }
            at += WORD;
        }
    }
    while at < n {
        // SAFETY: `at` is below `n` and the caller says `n` bytes of each are usable.
        unsafe { dest.add(at).write(src.add(at).read()) };
        at += 1;
    }
}

/// Copies `n` bytes from `src` to `dest`, highest address first.
///
/// This is the direction that is correct when the destination is above the source and the two
/// overlap, since a forward copy would then read bytes it had already written over.
///
/// # Safety
///
/// Both ranges must be readable and writable for `n` bytes.
#[inline]
pub unsafe fn copy_backward(dest: *mut u8, src: *const u8, n: usize) {
    let mut at = n;
    // Bytes down to the last aligned destination, going the other way for the same reason.
    let tail = dest.wrapping_add(n).align_offset(WORD);
    let tail = if tail == 0 { 0 } else { (WORD - tail).min(n) };
    while at > n - tail {
        at -= 1;
        // SAFETY: `at` is below `n` and the caller says `n` bytes of each are usable.
        unsafe { dest.add(at).write(src.add(at).read()) };
    }
    if src.wrapping_add(at).align_offset(WORD) == 0 {
        while at >= WORD {
            at -= WORD;
            // SAFETY: both pointers are word aligned here and `at + WORD` is within `n`.
            unsafe {
                dest.add(at).cast::<usize>().write(src.add(at).cast::<usize>().read());
            }
        }
    }
    while at > 0 {
        at -= 1;
        // SAFETY: `at` is below `n` and the caller says `n` bytes of each are usable.
        unsafe { dest.add(at).write(src.add(at).read()) };
    }
}

/// Writes `byte` into `n` bytes at `dest`.
///
/// # Safety
///
/// The range must be writable for `n` bytes.
#[inline]
pub unsafe fn fill(dest: *mut u8, byte: u8, n: usize) {
    let mut at = 0;
    let head = dest.align_offset(WORD).min(n);
    while at < head {
        // SAFETY: `at` is below `n` and the caller says `n` bytes are writable.
        unsafe { dest.add(at).write(byte) };
        at += 1;
    }
    // The same byte in every lane of a word, so the aligned loop writes one word per step.
    let word = usize::from_ne_bytes([byte; size_of::<usize>()]);
    while at + WORD <= n {
        // SAFETY: the pointer is word aligned here and `at + WORD` is within `n`.
        unsafe { dest.add(at).cast::<usize>().write(word) };
        at += WORD;
    }
    while at < n {
        // SAFETY: `at` is below `n` and the caller says `n` bytes are writable.
        unsafe { dest.add(at).write(byte) };
        at += 1;
    }
}

/// Compares `n` bytes at `a` and `b`, and reports the first place they differ.
///
/// Negative when `a` is the smaller, positive when `b` is, zero when the two runs are equal.
/// The comparison is on unsigned bytes, which is what C says and is not what a Rust `i8` would
/// give.
///
/// # Safety
///
/// Both ranges must be readable for `n` bytes.
#[inline]
pub unsafe fn compare(a: *const u8, b: *const u8, n: usize) -> i32 {
    let mut at = 0;
    // A word at a time only while both are aligned, and only to find the word that differs. The
    // byte loop below then finds which byte inside it, because the answer is about byte order in
    // memory and a word comparison would be about this target's byte order instead.
    if a.align_offset(WORD) == 0 && b.align_offset(WORD) == 0 {
        while at + WORD <= n {
            // SAFETY: both pointers are word aligned here and `at + WORD` is within `n`.
            let (left, right) =
                unsafe { (a.add(at).cast::<usize>().read(), b.add(at).cast::<usize>().read()) };
            if left != right {
                break;
            }
            at += WORD;
        }
    }
    while at < n {
        // SAFETY: `at` is below `n` and the caller says `n` bytes of each are readable.
        let (left, right) = unsafe { (a.add(at).read(), b.add(at).read()) };
        if left != right {
            return i32::from(left) - i32::from(right);
        }
        at += 1;
    }
    0
}

// The C names. Not compiled under `cargo test`, where the host already has these symbols and a
// second definition of them is a duplicate symbol rather than a runtime library.

/// `void *memcpy(void *dest, const void *src, size_t n)`.
///
/// # Safety
///
/// The C contract: both ranges usable for `n` bytes, and not overlapping.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    // SAFETY: the caller promises the C contract, which is what `copy_forward` asks for.
    unsafe { copy_forward(dest.cast(), src.cast(), n) };
    dest
}

/// `void *memmove(void *dest, const void *src, size_t n)`.
///
/// # Safety
///
/// The C contract: both ranges usable for `n` bytes. Overlap is allowed, which is the whole
/// difference between this and `memcpy`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memmove(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    // Backward only when the destination is inside the source, because that is the one case a
    // forward copy would read a byte it had already written over. Compared as addresses, since
    // C only defines the relation for pointers into one object and this is exactly the case
    // where they are.
    if (dest as usize).wrapping_sub(src as usize) < n {
        // SAFETY: the caller promises `n` usable bytes on both sides.
        unsafe { copy_backward(dest.cast(), src.cast(), n) };
    } else {
        // SAFETY: as above, and the ranges do not overlap in the direction that would matter.
        unsafe { copy_forward(dest.cast(), src.cast(), n) };
    }
    dest
}

/// `void *memset(void *dest, int c, size_t n)`.
///
/// The value is an `int` in C and only its low byte is used, which is why this takes one and
/// narrows it rather than taking a `u8`.
///
/// # Safety
///
/// The C contract: the range is writable for `n` bytes.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset(dest: *mut c_void, c: i32, n: usize) -> *mut c_void {
    // SAFETY: the caller promises `n` writable bytes.
    unsafe { fill(dest.cast(), c as u8, n) };
    dest
}

/// `int memcmp(const void *a, const void *b, size_t n)`.
///
/// # Safety
///
/// The C contract: both ranges are readable for `n` bytes.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> i32 {
    // SAFETY: the caller promises `n` readable bytes on both sides.
    unsafe { compare(a.cast(), b.cast(), n) }
}

#[cfg(test)]
mod tests {
    use std::vec;
    use std::vec::Vec;

    use super::*;

    /// Every starting offset in a word, so the head loop, the word loop and the tail loop each
    /// get to be the only one that runs and each get to run beside the others.
    const PHASES: [usize; 9] = [0, 1, 2, 3, 4, 5, 6, 7, 8];

    /// A run of bytes that is different at every position, so a copy that drops or repeats one
    /// is visible rather than lucky.
    fn pattern(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn a_copy_of_every_length_at_every_alignment_is_the_bytes_that_were_there() {
        for n in 0..80 {
            for from in PHASES {
                for to in PHASES {
                    let src = pattern(n + from + 8);
                    let mut dest = vec![0xAA; n + to + 8];
                    // SAFETY: both slices are longer than the offset plus `n`.
                    unsafe { copy_forward(dest[to..].as_mut_ptr(), src[from..].as_ptr(), n) };
                    assert_eq!(&dest[to..to + n], &src[from..from + n], "n {n} {from} to {to}");
                    assert!(dest[..to].iter().all(|&b| b == 0xAA), "wrote before the start");
                    assert!(dest[to + n..].iter().all(|&b| b == 0xAA), "wrote past the end");
                }
            }
        }
    }

    #[test]
    fn a_backward_copy_moves_the_same_bytes_a_forward_one_does() {
        for n in 0..80 {
            for from in PHASES {
                for to in PHASES {
                    let src = pattern(n + from + 8);
                    let mut dest = vec![0xAA; n + to + 8];
                    // SAFETY: both slices are longer than the offset plus `n`.
                    unsafe { copy_backward(dest[to..].as_mut_ptr(), src[from..].as_ptr(), n) };
                    assert_eq!(&dest[to..to + n], &src[from..from + n], "n {n} {from} to {to}");
                    assert!(dest[..to].iter().all(|&b| b == 0xAA), "wrote before the start");
                    assert!(dest[to + n..].iter().all(|&b| b == 0xAA), "wrote past the end");
                }
            }
        }
    }

    #[test]
    fn a_fill_of_every_length_at_every_alignment_touches_exactly_its_range() {
        for n in 0..80 {
            for to in PHASES {
                let mut dest = vec![0xAA; n + to + 8];
                // SAFETY: the slice is longer than the offset plus `n`.
                unsafe { fill(dest[to..].as_mut_ptr(), 0x5C, n) };
                assert!(dest[to..to + n].iter().all(|&b| b == 0x5C), "n {n} at {to}");
                assert!(dest[..to].iter().all(|&b| b == 0xAA), "wrote before the start");
                assert!(dest[to + n..].iter().all(|&b| b == 0xAA), "wrote past the end");
            }
        }
    }

    #[test]
    fn a_comparison_finds_the_first_byte_that_differs_and_not_a_later_one() {
        for n in 1..40 {
            for at in 0..n {
                let a = pattern(n);
                let mut b = a.clone();
                b[at] = b[at].wrapping_add(1);
                // SAFETY: both are `n` bytes long.
                let answer = unsafe { compare(a.as_ptr(), b.as_ptr(), n) };
                assert!(answer < 0, "n {n} differing at {at} gave {answer}");
                // SAFETY: as above, the other way round.
                assert!(unsafe { compare(b.as_ptr(), a.as_ptr(), n) } > 0);
            }
        }
    }

    #[test]
    fn equal_runs_compare_equal_at_every_length_and_alignment() {
        for n in 0..80 {
            for at in PHASES {
                let a = pattern(n + at);
                let b = a.clone();
                // SAFETY: both slices are longer than the offset plus `n`.
                let answer = unsafe { compare(a[at..].as_ptr(), b[at..].as_ptr(), n) };
                assert_eq!(answer, 0, "n {n} at {at}");
            }
        }
    }

    #[test]
    fn a_comparison_is_on_unsigned_bytes_the_way_c_says_and_not_on_signed_ones() {
        // 0x80 is the smaller as an `i8` and the larger as a `u8`, which is the whole point.
        let (a, b) = ([0x80_u8], [0x01_u8]);
        // SAFETY: both are one byte long.
        let answer = unsafe { compare(a.as_ptr(), b.as_ptr(), 1) };
        assert!(answer > 0, "0x80 is above 0x01 as C compares them, got {answer}");
    }

    #[test]
    fn a_move_up_into_itself_does_not_read_what_it_already_wrote() {
        for n in 1..40 {
            for shift in 1..9 {
                let mut buf = pattern(n + shift);
                let want = buf[..n].to_vec();
                let base = buf.as_mut_ptr();
                // SAFETY: the destination starts `shift` in and both ends stay inside `buf`.
                unsafe { copy_backward(base.add(shift), base, n) };
                assert_eq!(&buf[shift..shift + n], &want[..], "n {n} shifted {shift}");
            }
        }
    }

    #[test]
    fn a_move_down_into_itself_is_the_forward_direction_and_is_also_right() {
        for n in 1..40 {
            for shift in 1..9 {
                let mut buf = pattern(n + shift);
                let want = buf[shift..shift + n].to_vec();
                let base = buf.as_mut_ptr();
                // SAFETY: the source starts `shift` in and both ends stay inside `buf`.
                unsafe { copy_forward(base, base.add(shift), n) };
                assert_eq!(&buf[..n], &want[..], "n {n} shifted {shift}");
            }
        }
    }

    #[test]
    fn nothing_at_all_is_not_a_special_case_anywhere() {
        let mut dest = [0xAA_u8; 4];
        let src = [0x11_u8; 4];
        // SAFETY: a length of zero touches nothing, which is what is being checked.
        unsafe {
            copy_forward(dest.as_mut_ptr(), src.as_ptr(), 0);
            copy_backward(dest.as_mut_ptr(), src.as_ptr(), 0);
            fill(dest.as_mut_ptr(), 0x5C, 0);
            assert_eq!(compare(dest.as_ptr(), src.as_ptr(), 0), 0);
        }
        assert_eq!(dest, [0xAA; 4]);
    }
}
