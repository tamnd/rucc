//! The syscall group of the interposition table.
//!
//! Design: `spec/safe-memory/10-boundaries.md` section 10.5.
//!
//! The kernel writes user memory and does not consult our planes. That is the whole reason this
//! group exists: everywhere else the monitor can see the store that goes wrong, and here it cannot,
//! because the store happens on the other side of a trap in code that was never instrumented and
//! never will be. What is left is to judge the buffer before the call, which is document 03's S9.
//!
//! The check costs one bounds comparison against a syscall, which is not a cost anybody can
//! measure. What it buys is the classic form of the bug, which is a size argument larger than the
//! buffer it was meant to describe, caught at the call rather than at whatever the kernel wrote
//! over.
//!
//! # A separate table from the movement group
//!
//! One `interpose!` invocation per file, because the generator writes a `TABLE` and an `exports`
//! module and two of either in one module is two definitions of one name. A file per group also
//! turns out to be the right split for reading: these rows are about a boundary with the kernel and
//! the ones next door are about a boundary with the C library, and the two have different reasons
//! for everything they do.
//!
//! # What is judged and what is not, yet
//!
//! Bounds and lifetime, which is what the planes hold at S1. Section 10.5 asks for two more things
//! and both of them need the init plane, which milestone S5 builds: after a read into a buffer the
//! written range's init plane is set, and before a write out of one the range is checked, because
//! sending uninitialized bytes to a socket is CWE-200 and is the userspace shape of a kernel
//! infoleak. Neither is here. The rows are written so that adding them is a change to what a
//! judgement does rather than a rewrite of the table, which is why [`crate::effects::Kind`] has
//! recorded the direction since the first row.
//!
//! `ioctl` is not here and cannot be until the summary is. Its direction is encoded in the request
//! number for some drivers and in nothing at all for others, so its buffer has genuinely unknown
//! extent, and section 10.5 says what to do with one of those: count it as a J7 transfer rather
//! than pretend. Counting is `--emit=safety-summary`'s job and that is a later box.
//!
//! `recvfrom` and `sendto` are not here either. Their address arguments are a `struct sockaddr` and
//! a length that is read and written through a pointer, which is a shape the vocabulary has no word
//! for yet, and adding one for two rows before the summary can say what it missed is the wrong
//! order to do the work in.
//!
//! # The offset arguments
//!
//! `pread` and `pwrite` take an `off_t`, which these rows spell as a 64 bit integer. That is what
//! it is on every target rucc has, and on a 32 bit target with large file support it would not be,
//! which is a thing to fix when there is a 32 bit target rather than a thing to guess at now.

use core::ffi::{c_int, c_void};

use crate::effects::Iovec;
use crate::interpose;

/// The kernel's own, called once the buffers have been judged.
mod real {
    use core::ffi::{c_int, c_void};

    use crate::effects::Iovec;

    unsafe extern "C" {
        pub(super) fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
        pub(super) fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
        pub(super) fn pread(fd: c_int, buf: *mut c_void, count: usize, at: i64) -> isize;
        pub(super) fn pwrite(fd: c_int, buf: *const c_void, count: usize, at: i64) -> isize;
        pub(super) fn recv(fd: c_int, buf: *mut c_void, count: usize, flags: c_int) -> isize;
        pub(super) fn send(fd: c_int, buf: *const c_void, count: usize, flags: c_int) -> isize;
        pub(super) fn readv(fd: c_int, iov: *const Iovec, count: c_int) -> isize;
        pub(super) fn writev(fd: c_int, iov: *const Iovec, count: c_int) -> isize;
    }
}

interpose! {
    group: Syscall;

    /// `read`, which is the row section 10.5 is about.
    ///
    /// A count larger than the buffer is the bug, and it is a bug the monitor can only catch here:
    /// the store that overruns happens inside the kernel, where there is no instrumentation and no
    /// plane to consult, so a check made after the call would be a check made after the damage.
    ///
    /// The whole buffer is judged rather than the part the call turns out to fill. What the program
    /// is asserting by passing the count is that the buffer is that big, and that assertion is
    /// wrong or right before the kernel has read a byte.
    fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize
        where writes(buf, count)
    {
        // SAFETY: the buffer has been judged, so it is inside one live instance of this monitor's
        // heap or outside its heap entirely, and the kernel is about to write no more of it than
        // the count the judgement used.
        unsafe { real::read(fd, buf, count) }
    }

    /// `write`, which is the same in the other direction.
    ///
    /// The half that leaks rather than corrupts. A count longer than the buffer sends whatever was
    /// next to it, and what is next to it on a heap is usually another request's data.
    fn write(fd: c_int, buf: *const c_void, count: usize) -> isize
        where reads(buf, count)
    {
        // SAFETY: the buffer has been judged, as in `read`.
        unsafe { real::write(fd, buf, count) }
    }

    /// `pread`, which is `read` with the offset given rather than remembered.
    fn pread(fd: c_int, buf: *mut c_void, count: usize, at: i64) -> isize
        where writes(buf, count)
    {
        // SAFETY: the buffer has been judged, as in `read`.
        unsafe { real::pread(fd, buf, count, at) }
    }

    /// `pwrite`, which is `write` with the offset given rather than remembered.
    fn pwrite(fd: c_int, buf: *const c_void, count: usize, at: i64) -> isize
        where reads(buf, count)
    {
        // SAFETY: the buffer has been judged, as in `read`.
        unsafe { real::pwrite(fd, buf, count, at) }
    }

    /// `recv`, which is `read` off a socket.
    ///
    /// Worth its own row rather than being left to the trust set, because a buffer filled from a
    /// socket is filled from somewhere the program does not control, which is where the counts come
    /// from that nobody checked.
    fn recv(fd: c_int, buf: *mut c_void, count: usize, flags: c_int) -> isize
        where writes(buf, count)
    {
        // SAFETY: the buffer has been judged, as in `read`.
        unsafe { real::recv(fd, buf, count, flags) }
    }

    /// `send`, which is `write` down a socket.
    fn send(fd: c_int, buf: *const c_void, count: usize, flags: c_int) -> isize
        where reads(buf, count)
    {
        // SAFETY: the buffer has been judged, as in `read`.
        unsafe { real::send(fd, buf, count, flags) }
    }

    /// `readv`, whose one pointer argument reaches a whole tree of memory.
    ///
    /// An array of descriptors, each naming a buffer of its own, so there are as many ranges to
    /// judge as the count says and one more for the array itself. The array is judged first,
    /// because reading an element out of an array shorter than its count is the bug, and reading it
    /// to find out where the next bug is would be committing it.
    ///
    /// Section 10.5 notes that this means the array's own pointers need capabilities, which they
    /// have when instrumented code stored them. An array built by uninstrumented code is what
    /// boundary capability recovery is for, and that is a later box.
    fn readv(fd: c_int, iov: *const Iovec, count: c_int) -> isize
        where scatters(iov, count)
    {
        // SAFETY: the array and every buffer it names have been judged, which is every range the
        // kernel is about to write.
        unsafe { real::readv(fd, iov, count) }
    }

    /// `writev`, which is the same tree read instead of written.
    fn writev(fd: c_int, iov: *const Iovec, count: c_int) -> isize
        where gathers(iov, count)
    {
        // SAFETY: the array and every buffer it names have been judged, as in `readv`.
        unsafe { real::writev(fd, iov, count) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc::{alloc, dealloc};
    use crate::effects::{Extent, Group, Kind};
    use crate::turnstile::turn;

    /// The file descriptor every test here writes down, which discards what it is given.
    ///
    /// Not standard output, because a test that printed a kilobyte of heap would be a test nobody
    /// wants to run twice. `/dev/null` is opened by the C library rather than by this crate, which
    /// keeps the test about the judgement and not about a file.
    fn sink() -> c_int {
        unsafe extern "C" {
            fn open(path: *const core::ffi::c_char, flags: c_int, ...) -> c_int;
        }
        // SAFETY: the path is a literal and the flag is the one every Unix spells the same way.
        let fd = unsafe { open(c"/dev/null".as_ptr(), 1) };
        assert!(fd >= 0, "the sink could not be opened");
        fd
    }

    /// Runs a wrapper and says whether it refused, without the panic reaching the harness.
    fn refused(call: impl FnOnce()) -> bool {
        let hook = std::panic::take_hook();
        std::panic::set_hook(std::boxed::Box::new(|_| {}));
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(call));
        std::panic::set_hook(hook);
        out.is_err()
    }

    /// The row a name was written as.
    fn row(name: &str) -> &'static crate::effects::Row {
        TABLE.iter().find(|row| row.name == name).expect("the row is in the table")
    }

    #[test]
    fn every_row_is_in_the_syscall_group_and_says_which_way_its_bytes_go() {
        // The group is what the summary counts by, because a build with forty movement wrappers and
        // no syscall wrappers has a different guarantee from one with forty of each.
        for row in TABLE {
            assert_eq!(row.group, Group::Syscall, "{} is in the wrong group", row.name);
            assert!(row.wrapper.starts_with("__rucc_wrap_"));
        }
        assert_eq!(row("read").effects[0].kind, Kind::Writes);
        assert_eq!(row("write").effects[0].kind, Kind::Reads);
        assert_eq!(row("read").effects[0].extent, Extent::SizedBy("count"));
        assert_eq!(row("readv").effects[0].extent, Extent::Vectors("count"));
        assert_eq!(row("writev").effects[0].kind, Kind::Reads);
    }

    #[test]
    fn a_write_of_a_buffer_that_holds_what_it_says_is_allowed() {
        let _turn = turn();
        // The silent case, which is every correct call a program makes.
        let fd = sink();
        let buf = alloc(64);
        // SAFETY: sixty four bytes of a live instance, written out in full.
        assert_eq!(unsafe { write(fd, buf, 64) }, 64);
        // SAFETY: as above, and less of it.
        assert_eq!(unsafe { write(fd, buf, 16) }, 16);
        // SAFETY: `buf` is a live instance.
        unsafe { dealloc(buf) };
    }

    #[test]
    fn a_write_of_more_than_the_buffer_holds_is_refused_before_the_kernel_sees_it() {
        let _turn = turn();
        // The leak. Whatever is next to the buffer goes down the socket, and on a heap what is next
        // to it is usually another request's data. The judgement happens before the trap, so the
        // bytes are still their owner's when the report is printed.
        let fd = sink();
        let buf = alloc(64);
        assert!(refused(|| {
            // SAFETY: the count is longer than the instance, which is the bug.
            let _ = unsafe { write(fd, buf, 1024) };
        }));
        // SAFETY: `buf` is a live instance.
        unsafe { dealloc(buf) };
    }

    #[test]
    fn a_read_into_a_freed_buffer_is_refused() {
        let _turn = turn();
        // Use after free through the kernel, which is the version no sanitizer that watches loads
        // and stores can see, because the store is on the other side of a trap.
        let fd = sink();
        let buf = alloc(64);
        // SAFETY: `buf` is a live instance.
        unsafe { dealloc(buf) };
        assert!(refused(|| {
            // SAFETY: the buffer is judged before the call, which is the point of the row.
            let _ = unsafe { read(fd, buf, 64) };
        }));
    }

    #[test]
    fn a_vector_whose_elements_fit_is_allowed_and_one_whose_element_does_not_is_refused() {
        let _turn = turn();
        // The array is one object and each element names another, so a correct call is a whole
        // tree of correct ranges and a bug in any one of them is the bug.
        let fd = sink();
        let first = alloc(64);
        let second = alloc(64);
        let mut iov = [Iovec { base: first, len: 64 }, Iovec { base: second, len: 64 }];
        // SAFETY: both elements name the whole of a live instance.
        assert_eq!(unsafe { writev(fd, iov.as_ptr(), 2) }, 128);

        iov[1].len = 1024;
        assert!(refused(|| {
            // SAFETY: the second element is what runs off, and the judgement reaches it.
            let _ = unsafe { writev(fd, iov.as_ptr(), 2) };
        }));
        // SAFETY: both are live instances.
        unsafe {
            dealloc(first);
            dealloc(second);
        }
    }

    #[test]
    fn a_vector_longer_than_its_own_array_is_refused_before_an_element_is_read() {
        let _turn = turn();
        // The count is a claim about the array and not only about the buffers. Judging the array
        // first is what keeps the monitor from reading past it to find out whether it should have.
        let fd = sink();
        let buf = alloc(64);
        let iov = [Iovec { base: buf, len: 64 }];
        assert!(refused(|| {
            // SAFETY: the array holds one element and the count says eight, which is the bug.
            let _ = unsafe { writev(fd, iov.as_ptr(), 8) };
        }));
        // SAFETY: `buf` is a live instance.
        unsafe { dealloc(buf) };
    }

    #[test]
    fn a_negative_count_is_left_to_the_kernel() {
        let _turn = turn();
        // `EINVAL` is the right answer and it is the kernel's to give. A monitor that reported a
        // memory safety violation about a call that never touched memory would be wrong twice.
        let fd = sink();
        let buf = alloc(64);
        let iov = [Iovec { base: buf, len: 64 }];
        // SAFETY: the count is refused by the kernel rather than by the judgement.
        assert_eq!(unsafe { writev(fd, iov.as_ptr(), -1) }, -1);
        // SAFETY: `buf` is a live instance.
        unsafe { dealloc(buf) };
    }

    #[test]
    fn a_buffer_that_is_not_the_heaps_is_written_without_a_word() {
        let _turn = turn();
        // A local or a global, which is where a great many syscall buffers live. Reporting on one
        // would be a false positive against a program doing nothing wrong.
        let fd = sink();
        let mut local = [0_u8; 64];
        // SAFETY: a live local of sixty four bytes.
        assert_eq!(unsafe { write(fd, local.as_mut_ptr().cast(), 64) }, 64);
    }
}
