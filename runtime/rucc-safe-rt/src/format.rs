//! The judgements the printf family is held to, made at the call site.
//!
//! Design: `spec/safe-memory/10-boundaries.md` section 10.3.
//!
//! The rest of the interposition table wraps a function: a call to `strcpy` goes to a wrapper that
//! judges and then calls the C library's own. That does not work for `printf` and its relatives,
//! because a wrapper for a variadic C function has to be a variadic Rust function or go through
//! `vsnprintf` and a `va_list`, and both are unstable. So nothing here is a wrapper. Each row is a
//! judgement with no work behind it, and `rucc_safety::format` calls them in front of the real
//! call, where the format is a literal the compiler can read and every argument is in hand.
//!
//! Three rows cover what a format does to memory. A `%s` reads its argument to the terminator,
//! which is `printf_string`, or to the precision when one is written, which is
//! `printf_string_within`. `sprintf` and `snprintf` write their output, which is `printf_output`,
//! over as many bytes as the call is about to write: the compiler asks `snprintf` with no
//! destination how long the output is and passes the smaller of that plus the terminator and the
//! size the program gave. A destination one byte short is refused before anything is written.
//!
//! The names are the role and not the function, because one row stands for the whole family. A
//! refusal reads `in printf_output, over its dst argument`, and the line it names is the call the
//! program wrote.
//!
//! A null `%s` argument is not judged. glibc prints `(null)` for one, programs rely on it, and the
//! walk has nowhere to start, which [`crate::effects::scan_within`] says as well.
//!
//! These are not in the compiler's list of interposed names, because nothing a program writes is
//! redirected to them. `rucc_safety::format::JUDGES` is their list, and `cargo xtask interpose`
//! holds the two to each other the way it holds the interposed names to the rest of the table.

use core::ffi::c_char;

use crate::interpose;

interpose! {
    group: Movement;

    /// The output of `sprintf`, `snprintf` and the fortified forms of both.
    ///
    /// `n` is what the call will write, terminator included, which the compiler works out before
    /// the call and not what the program passed as a size. A `snprintf` told it has more room than
    /// it has and given a short string writes the short string, and refusing that would be refusing
    /// a call that does nothing wrong.
    fn printf_output(dst: *mut c_char, n: usize) -> ()
        where writes(dst, n)
    {}

    /// A `%s` with no precision, which reads its argument to the terminator.
    fn printf_string(string: *const c_char) -> ()
        where reads(string, nul)
    {}

    /// A `%s` with a precision written in the format, which stops there if there is no terminator
    /// before it.
    fn printf_string_within(string: *const c_char, n: usize) -> ()
        where reads(string, nul, n)
    {}
}

#[cfg(test)]
mod tests {
    use core::ffi::c_void;

    use super::*;
    use crate::alloc::{alloc, dealloc};
    use crate::turnstile::turn;

    /// Runs a judgement and says whether it refused, without the panic reaching the harness.
    fn refused(call: impl FnOnce()) -> bool {
        let hook = std::panic::take_hook();
        std::panic::set_hook(std::boxed::Box::new(|_| {}));
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(call));
        std::panic::set_hook(hook);
        out.is_err()
    }

    /// A live instance of `n` bytes, every one of them `fill`.
    fn filled(n: usize, fill: u8) -> *mut c_void {
        let ptr = alloc(n);
        // SAFETY: the instance was just handed out with room for `n` bytes.
        unsafe { ptr.cast::<u8>().write_bytes(fill, n) };
        ptr
    }

    #[test]
    fn an_output_that_fits_its_destination_is_allowed() {
        let _turn = turn();
        let dst = filled(8, 0);
        // SAFETY: the instance holds eight bytes, and a judgement writes none of them.
        assert!(!refused(|| unsafe { printf_output(dst.cast(), 8) }));
        // SAFETY: a live instance.
        unsafe { dealloc(dst) };
    }

    #[test]
    fn an_output_one_byte_past_its_destination_is_refused() {
        let _turn = turn();
        let dst = filled(8, 0);
        // SAFETY: the judgement is made before anything is written, which is what this is for.
        assert!(refused(|| unsafe { printf_output(dst.cast(), 9) }));
        // SAFETY: a live instance.
        unsafe { dealloc(dst) };
    }

    #[test]
    fn a_string_with_no_terminator_in_its_instance_is_refused() {
        let _turn = turn();
        let string = filled(8, b'a');
        // SAFETY: the walk is judged at the first byte outside the instance, before it reads it.
        assert!(refused(|| unsafe { printf_string(string.cast()) }));
        // Eight bytes is as far as `%.8s` reads, and all eight are the instance's.
        // SAFETY: as above.
        assert!(!refused(|| unsafe { printf_string_within(string.cast(), 8) }));
        // SAFETY: a live instance.
        unsafe { dealloc(string) };
    }

    #[test]
    fn a_null_string_is_left_to_the_c_library() {
        let _turn = turn();
        // SAFETY: a null pointer is not read.
        assert!(!refused(|| unsafe { printf_string(core::ptr::null()) }));
    }
}
