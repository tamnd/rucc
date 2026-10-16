//! The note a call site leaves when it hands a wrapper the address of an array member.
//!
//! Design: `spec/safe-memory/09-type-init-and-races.md` section 9.4.
//!
//! A capability names a whole object, so `memcpy(s.name, src, sizeof s)` stays inside the one it
//! is in and the frame alone lets it through, even though C says the copy ran off the end of
//! `name`. The compiler knows how long the member is where it took the address, and
//! `rucc_safety::wrap` puts a call to [`exports::__rucc_member`] in front of the call with the
//! address and that length. The wrapper takes the note along with the frame, and an argument at
//! exactly that address is judged against the member rather than the object, which is what
//! `__builtin_object_size(p, 1)` and `_FORTIFY_SOURCE` do where the length is known when compiling.
//!
//! One note, for the first array member a call is handed. Every wrapper takes it whether it reads
//! it or not, so it never outlives the call it was left for, and an argument at any other address
//! leaves it alone.

use core::ffi::c_void;

use crate::tls::Slot;

/// The address the note is about, or null when there is none.
static ADDRESS: Slot = Slot::new(crate::tls::MEMBER);

/// How long the member at that address is, kept as a pointer with no provenance.
static LENGTH: Slot = Slot::new(crate::tls::MEMBER_LENGTH);

/// An array member a call was handed, as the compiler measured it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Note {
    /// The address of the member's first byte.
    pub addr: usize,
    /// How many bytes the member has.
    pub len: usize,
}

/// Leaves a note that the next wrapper's argument at `addr` is a member `len` bytes long.
pub fn leave(addr: *const c_void, len: usize) {
    // SAFETY: neither word is ever read through. One is an address compared with an argument's and
    // the other is a count, and both are only ever read back by this thread.
    unsafe {
        LENGTH.set(core::ptr::without_provenance_mut(len));
        ADDRESS.set(addr.cast_mut());
    }
}

/// The note left for this call, if one was, and clears it either way.
#[must_use]
pub fn take() -> Option<Note> {
    let addr = ADDRESS.get();
    if addr.is_null() {
        return None;
    }
    // SAFETY: null says there is no note, and it is never read through.
    unsafe { ADDRESS.set(core::ptr::null_mut()) };
    Some(Note { addr: addr.addr(), len: LENGTH.get().addr() })
}

/// The names generated code is compiled against.
pub mod exports {
    use core::ffi::c_void;

    /// Says the next wrapper's argument at `addr` is an array member `len` bytes long.
    #[unsafe(no_mangle)]
    pub extern "C" fn __rucc_member(addr: *const c_void, len: usize) {
        super::leave(addr, len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_is_read_back_once() {
        let buf = [0_u8; 32];
        leave(buf.as_ptr().cast(), 16);
        assert_eq!(take(), Some(Note { addr: buf.as_ptr().addr(), len: 16 }));
        assert_eq!(take(), None);
    }

    #[test]
    fn a_thread_that_left_nothing_has_nothing() {
        assert_eq!(take(), None);
    }
}
