//! The wide string functions of the interposition table, as rows.
//!
//! Design: `spec/safe-memory/10-boundaries.md` section 10.3, and tamnd/rucc#3268.
//!
//! The same shapes as the narrow rows in [`crate::wrap`], one for one: `wcslen` and its relatives
//! only look, `wcscpy`, `wcscat` and their bounded forms write as far as the source reaches, and the
//! `wmem` functions take a count. What is different is the unit. A wide string ends at a whole
//! character of zero bytes, and every count these functions take is a count of characters, so the
//! group says `unit: WideChar` and the generator does the multiplying. A row here is spelled exactly
//! as its narrow twin is, which is what keeps a `wmemcpy` from being judged for a quarter of what it
//! copies.
//!
//! The rows a program meets first are the ones Juliet's CWE135 cases are about: a wide string
//! measured with `strlen`, which stops at the first zero byte of its first character, a buffer
//! sized from that, and a `wcscpy` of the whole string into it.

use crate::interpose;

/// What the C library means by `wchar_t` here, which is four bytes on every Unix this runtime is
/// built for.
///
/// Signed on x86-64 and unsigned on AArch64 Linux, which is the same register and the same four
/// bytes at a call, so the rows are written once with the signed one.
pub type WideChar = i32;

/// The C library's own, called once the judgements have been made, as in [`crate::wrap`].
mod real {
    use super::WideChar;

    unsafe extern "C" {
        pub(super) fn wcslen(s: *const WideChar) -> usize;
        pub(super) fn wcsnlen(s: *const WideChar, n: usize) -> usize;
        pub(super) fn wcscmp(a: *const WideChar, b: *const WideChar) -> i32;
        pub(super) fn wcsncmp(a: *const WideChar, b: *const WideChar, n: usize) -> i32;
        pub(super) fn wcschr(s: *const WideChar, c: WideChar) -> *mut WideChar;
        pub(super) fn wcsrchr(s: *const WideChar, c: WideChar) -> *mut WideChar;
        pub(super) fn wcsstr(haystack: *const WideChar, needle: *const WideChar) -> *mut WideChar;
        pub(super) fn wcscpy(dst: *mut WideChar, src: *const WideChar) -> *mut WideChar;
        pub(super) fn wcpcpy(dst: *mut WideChar, src: *const WideChar) -> *mut WideChar;
        pub(super) fn wcsncpy(dst: *mut WideChar, src: *const WideChar, n: usize) -> *mut WideChar;
        pub(super) fn wcscat(dst: *mut WideChar, src: *const WideChar) -> *mut WideChar;
        pub(super) fn wcsncat(dst: *mut WideChar, src: *const WideChar, n: usize) -> *mut WideChar;
        pub(super) fn wmemcpy(dst: *mut WideChar, src: *const WideChar, n: usize) -> *mut WideChar;
        pub(super) fn wmemmove(dst: *mut WideChar, src: *const WideChar, n: usize)
        -> *mut WideChar;
        pub(super) fn wmemset(dst: *mut WideChar, c: WideChar, n: usize) -> *mut WideChar;
        pub(super) fn wmemcmp(a: *const WideChar, b: *const WideChar, n: usize) -> i32;
        pub(super) fn wmemchr(s: *const WideChar, c: WideChar, n: usize) -> *mut WideChar;
    }
}

interpose! {
    group: Movement;
    unit: WideChar;

    /// `wcslen`, which is `strlen` a character at a time.
    fn wcslen(s: *const WideChar) -> usize
        where reads(s, nul)
    {
        // SAFETY: the string has been judged, so it reaches a terminator inside the instance it
        // started in, or it is not in this monitor's heap and the caller's contract covers it.
        unsafe { real::wcslen(s) }
    }

    /// `wcsnlen`, which is the same walk with somewhere to stop, in characters.
    fn wcsnlen(s: *const WideChar, n: usize) -> usize
        where reads(s, nul, n)
    {
        // SAFETY: the walk has been judged as far as it goes, which is the terminator or `n`.
        unsafe { real::wcsnlen(s, n) }
    }

    /// `wcscmp`, which walks two strings and is judged over both.
    fn wcscmp(a: *const WideChar, b: *const WideChar) -> i32
        where reads(a, nul), reads(b, nul)
    {
        // SAFETY: both strings have been judged, as in `wcslen`.
        unsafe { real::wcscmp(a, b) }
    }

    /// `wcsncmp`, which is the same with somewhere to stop.
    fn wcsncmp(a: *const WideChar, b: *const WideChar, n: usize) -> i32
        where reads(a, nul, n), reads(b, nul, n)
    {
        // SAFETY: both walks have been judged as far as they go, as in `wcsnlen`.
        unsafe { real::wcsncmp(a, b, n) }
    }

    /// `wcschr`, which is a walk that reports where it stopped.
    fn wcschr(s: *const WideChar, c: WideChar) -> *mut WideChar
        where reads(s, nul)
    {
        // SAFETY: the string has been judged, as in `wcslen`.
        unsafe { real::wcschr(s, c) }
    }

    /// `wcsrchr`, which is the same walk and cannot stop early.
    fn wcsrchr(s: *const WideChar, c: WideChar) -> *mut WideChar
        where reads(s, nul)
    {
        // SAFETY: the string has been judged, as in `wcslen`.
        unsafe { real::wcsrchr(s, c) }
    }

    /// `wcsstr`, which walks one string many times and the other once.
    fn wcsstr(haystack: *const WideChar, needle: *const WideChar) -> *mut WideChar
        where reads(haystack, nul), reads(needle, nul)
    {
        // SAFETY: both strings have been judged, as in `wcslen`.
        unsafe { real::wcsstr(haystack, needle) }
    }

    /// `wcscpy`, which is the row this group is here for.
    ///
    /// The source is walked a character at a time and the destination is judged for every byte
    /// the write is going to reach, terminator included, so a buffer sized for the bytes of a wide
    /// string up to its first zero byte is refused at the first byte past it.
    fn wcscpy(dst: *mut WideChar, src: *const WideChar) -> *mut WideChar
        where copies(dst, src)
    {
        // SAFETY: the source reaches a terminator inside its own instance and the destination has
        // room for every character of it, which is what the judgement establishes.
        unsafe { real::wcscpy(dst, src) }
    }

    /// `wcpcpy`, which is `wcscpy` returning where it stopped.
    fn wcpcpy(dst: *mut WideChar, src: *const WideChar) -> *mut WideChar
        where copies(dst, src)
    {
        // SAFETY: both have been judged, as in `wcscpy`.
        unsafe { real::wcpcpy(dst, src) }
    }

    /// `wcsncpy`, which writes exactly `n` characters whatever the source holds.
    fn wcsncpy(dst: *mut WideChar, src: *const WideChar, n: usize) -> *mut WideChar
        where writes(dst, n), reads(src, nul, n)
    {
        // SAFETY: the destination has room for `n` characters and the source has been walked as
        // far as the call will read it, which is the terminator or `n`.
        unsafe { real::wcsncpy(dst, src, n) }
    }

    /// `wcscat`, which discovers where to start as well as how far to go.
    fn wcscat(dst: *mut WideChar, src: *const WideChar) -> *mut WideChar
        where appends(dst, src)
    {
        // SAFETY: the destination is terminated inside its own instance and has room for the source
        // after it, which is what the judgement establishes.
        unsafe { real::wcscat(dst, src) }
    }

    /// `wcsncat`, which stops after `n` characters of the source and terminates anyway.
    fn wcsncat(dst: *mut WideChar, src: *const WideChar, n: usize) -> *mut WideChar
        where appends(dst, src, n)
    {
        // SAFETY: as `wcscat`, reading no more than `n` characters of the source.
        unsafe { real::wcsncat(dst, src, n) }
    }

    /// `wmemcpy`, which is `memcpy` with its count in characters.
    fn wmemcpy(dst: *mut WideChar, src: *const WideChar, n: usize) -> *mut WideChar
        where moves(dst, src, n)
    {
        // SAFETY: both ranges have been judged for `n` characters.
        unsafe { real::wmemcpy(dst, src, n) }
    }

    /// `wmemmove`, which is `wmemcpy` with the overlap allowed.
    fn wmemmove(dst: *mut WideChar, src: *const WideChar, n: usize) -> *mut WideChar
        where moves(dst, src, n)
    {
        // SAFETY: both ranges have been judged, as in `wmemcpy`.
        unsafe { real::wmemmove(dst, src, n) }
    }

    /// `wmemset`, which writes one character `n` times.
    fn wmemset(dst: *mut WideChar, c: WideChar, n: usize) -> *mut WideChar
        where writes(dst, n)
    {
        // SAFETY: the range has been judged, as in `wmemcpy`.
        unsafe { real::wmemset(dst, c, n) }
    }

    /// `wmemcmp`, which reads two ranges and writes neither.
    fn wmemcmp(a: *const WideChar, b: *const WideChar, n: usize) -> i32
        where reads(a, n), reads(b, n)
    {
        // SAFETY: both ranges have been judged, as in `wmemcpy`.
        unsafe { real::wmemcmp(a, b, n) }
    }

    /// `wmemchr`, which is judged for the whole range it may read.
    fn wmemchr(s: *const WideChar, c: WideChar, n: usize) -> *mut WideChar
        where reads(s, n)
    {
        // SAFETY: the range has been judged, as in `wmemcpy`.
        unsafe { real::wmemchr(s, c, n) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ffi::c_void;

    use crate::alloc::{alloc, dealloc};
    use crate::effects::{Clause, Extent, Group};
    use crate::turnstile::turn;

    /// Runs a wrapper and says whether it refused, without the panic reaching the harness.
    fn refused(call: impl FnOnce()) -> bool {
        let hook = std::panic::take_hook();
        std::panic::set_hook(std::boxed::Box::new(|_| {}));
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(call));
        std::panic::set_hook(hook);
        out.is_err()
    }

    /// Writes a wide string into an instance, terminator included.
    fn put(ptr: *mut c_void, text: &str) {
        let wide = ptr.cast::<WideChar>();
        for (at, c) in text.chars().chain(core::iter::once('\0')).enumerate() {
            // SAFETY: every caller has asked for room for the whole string.
            unsafe { wide.add(at).write(c as WideChar) };
        }
    }

    /// The row a name was written as.
    fn row(name: &str) -> &'static crate::effects::Row {
        TABLE.iter().find(|row| row.name == name).expect("the row is in the table")
    }

    #[test]
    fn every_row_counts_in_characters_of_four_bytes() {
        for row in TABLE {
            assert_eq!(row.unit, 4, "{}", row.name);
            assert_eq!(row.group, Group::Movement);
        }
        let copy = row("wcscpy");
        assert_eq!(copy.effects[0].clause, Clause::Copies);
        assert_eq!(copy.effects[0].extent, Extent::NulOf("src"));
        assert_eq!(row("wmemcpy").effects[0].extent, Extent::SizedBy("n"));
        assert!(crate::wrap::TABLE.iter().all(|row| row.unit == 1));
    }

    #[test]
    fn a_wide_copy_into_a_buffer_sized_by_strlen_is_refused() {
        let _turn = turn();
        // Juliet's CWE135. `strlen` of L"AAAAAAAAAA" stops at the second byte, which sizes the
        // buffer for two characters, and the copy writes eleven.
        let from = alloc(64);
        let to = alloc(2 * 4);
        put(from, "AAAAAAAAAA");
        assert!(refused(|| {
            // SAFETY: the destination is judged before anything is written.
            let _ = unsafe { wcscpy(to.cast(), from.cast()) };
        }));
        // SAFETY: both are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
        }
    }

    #[test]
    fn a_wide_copy_with_room_for_its_terminator_is_allowed_and_one_without_is_not() {
        let _turn = turn();
        let from = alloc(64);
        let to = alloc(11 * 4);
        put(from, "AAAAAAAAAA");
        // SAFETY: ten characters and the terminator are the eleven that were asked for.
        assert_eq!(unsafe { wcscpy(to.cast(), from.cast()) }, to.cast());
        // SAFETY: the copy has just been judged and made.
        assert_eq!(unsafe { wcslen(to.cast()) }, 10);
        put(from, "AAAAAAAAAAA");
        assert!(refused(|| {
            // SAFETY: as above, one character longer.
            let _ = unsafe { wcscpy(to.cast(), from.cast()) };
        }));
        // SAFETY: both are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
        }
    }

    #[test]
    fn a_count_is_a_count_of_characters() {
        let _turn = turn();
        // Four characters are sixteen bytes. A row that judged `n` as bytes would let the fifth
        // through, which is the mistake the unit is in the generator to rule out.
        let from = alloc(32);
        let to = alloc(16);
        put(from, "AAAAAA");
        assert!(!refused(|| {
            // SAFETY: four characters of sixteen bytes each way.
            let _ = unsafe { wmemcpy(to.cast(), from.cast(), 4) };
        }));
        assert!(refused(|| {
            // SAFETY: as above, one character more than the destination has.
            let _ = unsafe { wmemcpy(to.cast(), from.cast(), 5) };
        }));
        assert!(refused(|| {
            // SAFETY: as above, over a write of one character.
            let _ = unsafe { wmemset(to.cast(), 0, 5) };
        }));
        // SAFETY: both are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
        }
    }

    #[test]
    fn a_wide_append_past_the_end_is_refused() {
        let _turn = turn();
        let from = alloc(32);
        let to = alloc(8 * 4);
        put(to, "AAAA");
        put(from, "BBB");
        // SAFETY: four and three characters and the terminator are the eight there is room for.
        assert_eq!(unsafe { wcscat(to.cast(), from.cast()) }, to.cast());
        // SAFETY: as above.
        assert_eq!(unsafe { wcslen(to.cast()) }, 7);
        assert!(refused(|| {
            // SAFETY: seven and three do not fit in eight.
            let _ = unsafe { wcscat(to.cast(), from.cast()) };
        }));
        // SAFETY: both are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
        }
    }
}
