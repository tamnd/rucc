//! Turning a refused judgement into words.
//!
//! Design: `spec/safe-memory/06-instrumentation.md` section 6.5.
//!
//! A memory safety report that does not say what the program did is worth very little, and this is
//! the part of ASan that made it succeed. Section 6.5 lists six things a report should carry and
//! this writes the three it can: the judgement in document 04's numbering, the address and the width
//! of the access, and what the lifetime plane says about the range the address is in.
//!
//! The other three are named here rather than left to be noticed missing. The source location comes
//! from DWARF through the `pc` field of the descriptor, and nothing fills that field in yet, because
//! doing so needs a relocation against the enclosing function plus the offset of the call and the IR
//! cannot express one. The allocation and deallocation sites are in the instance header, which has
//! room for them and nothing writing them, since capturing a caller's address at `malloc` is
//! milestone S2's. The type is milestone S5's, along with the plane that would hold it.
//!
//! # Why the text is built in a buffer
//!
//! Because it goes out in one `write`. A report that arrives in pieces can be interleaved with
//! another thread's, and two half reports are worse than one, so the whole thing is assembled first
//! and handed over once. The buffer is fixed and on the stack: there is no allocator here that a
//! failing program can be trusted with, and the failing program's own is the thing being reported
//! on.

use crate::fail::{Descriptor, Judgement};

/// How much room a report is given.
///
/// Generous for what is written today, which is four short lines. A report that outgrew this would
/// be silently cut, so [`Text`] says how much it dropped and a test holds the longest report the
/// renderer can produce against this number.
pub const ROOM: usize = 512;

/// A report being built.
#[derive(Debug)]
pub struct Text {
    /// The bytes so far.
    buf: [u8; ROOM],
    /// How many of them are written.
    len: usize,
    /// How many bytes did not fit, which is zero for every report this renders.
    lost: usize,
}

impl Default for Text {
    fn default() -> Self {
        Self::new()
    }
}

impl Text {
    /// An empty report.
    #[must_use]
    pub const fn new() -> Self {
        Self { buf: [0; ROOM], len: 0, lost: 0 }
    }

    /// What has been written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // SAFETY: every byte in the buffer was put there by `text`, which copies from a `&str`, or
        // by `dec` and `hex`, which write ASCII. A `&str` is only split at a boundary by `text`
        // below, which truncates on a whole append rather than in the middle of one.
        unsafe { core::str::from_utf8_unchecked(&self.buf[..self.len]) }
    }

    /// How many bytes were dropped for want of room.
    #[must_use]
    pub const fn lost(&self) -> usize {
        self.lost
    }

    /// Appends `s`, or counts it as lost if there is no room for the whole of it.
    ///
    /// All of it or none of it, because half of a word is not a shorter report, it is a wrong one.
    pub fn text(&mut self, s: &str) -> &mut Self {
        let bytes = s.as_bytes();
        if bytes.len() > ROOM - self.len {
            self.lost += bytes.len();
            return self;
        }
        self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
        self
    }

    /// Appends `n` in decimal.
    pub fn dec(&mut self, n: u64) -> &mut Self {
        // Twenty digits is what `u64::MAX` takes, and the digits come out backwards.
        let mut digits = [0_u8; 20];
        let mut at = digits.len();
        let mut left = n;
        loop {
            at -= 1;
            digits[at] = b'0' + (left % 10) as u8;
            left /= 10;
            if left == 0 {
                break;
            }
        }
        // SAFETY: every byte written above is an ASCII digit.
        self.text(unsafe { core::str::from_utf8_unchecked(&digits[at..]) })
    }

    /// Appends `n` in hexadecimal, `0x` and every digit, so that two addresses line up when read.
    pub fn hex(&mut self, n: usize) -> &mut Self {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut out = [0_u8; 16];
        let width = out.len();
        for (at, slot) in out.iter_mut().enumerate() {
            *slot = DIGITS[(n >> ((width - 1 - at) * 4)) & 0xf];
        }
        self.text("0x");
        // SAFETY: every byte written above came out of `DIGITS`, which is ASCII.
        self.text(unsafe { core::str::from_utf8_unchecked(&out) })
    }
}

/// What the lifetime plane says about an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// Not in the region the monitor's allocator hands out of, so no plane covers it. A local, a
    /// global, or memory some other allocator gave the program.
    Elsewhere,
    /// In the region and belonging to no instance. The allocator's own headers are here, and so is
    /// everything the bump pointer has not reached yet.
    Nobody,
    /// Owned right now by the instance the counter answered with this number.
    Live(u64),
    /// Owned by that instance until it was freed.
    Freed(u64),
}

/// Asks the plane who owns `addr`.
#[cfg(unix)]
#[must_use]
pub fn owner(addr: usize) -> Owner {
    use crate::plane;

    let Some(region) = crate::alloc::region() else { return Owner::Elsewhere };
    if !region.holds(addr) {
        return Owner::Elsewhere;
    }
    // SAFETY: the address is inside the region the plane was built over, checked just above.
    let slot = unsafe { region.plane.version(addr) };
    // The instance number rather than the slot, because the low bit of a slot is the encoding
    // saying which of the two answers below it is, and a report should say the fact and not the
    // representation.
    match slot {
        plane::DEAD => Owner::Nobody,
        _ if plane::owned(slot) => Owner::Live(slot >> 1),
        _ => Owner::Freed(slot >> 1),
    }
}

/// The same where there is no allocator, which is every target the wrappers are not compiled for.
#[cfg(not(unix))]
#[must_use]
pub fn owner(addr: usize) -> Owner {
    let _ = addr;
    Owner::Elsewhere
}

/// Writes the report for one refused judgement.
///
/// `addr` is what the check was about, and is absent where there is nothing honest to put there,
/// which is the ABI entry point and the allocator's own refusals.
pub fn render(out: &mut Text, row: &Descriptor, addr: Option<usize>) {
    out.text("rucc: memory safety violation\n");

    out.text("  judgement J").dec(u64::from(row.judgement)).text(", ");
    out.text(match Judgement::of(row.judgement) {
        Some(judgement) => judgement.what(),
        None => "which is not a judgement this runtime has heard of",
    });
    out.text("\n");

    // Nothing decides the class yet, so this line is normally absent rather than saying zero.
    // Zero is not a row of document 03's tables and printing it would invite somebody to look one
    // up.
    if row.class != 0 {
        out.text("  class ").dec(u64::from(row.class));
        out.text(" of spec/safe-memory/03-bug-model.md\n");
    }

    let Some(addr) = addr else { return };
    out.text("  ");
    if row.size != 0 {
        out.dec(u64::from(row.size)).text(" bytes at ");
    } else {
        out.text("at ");
    }
    out.hex(addr).text("\n");

    match owner(addr) {
        Owner::Elsewhere => {
            out.text("  which is not in the heap this monitor watches\n");
        }
        Owner::Nobody => {
            out.text("  which no instance owns\n");
        }
        Owner::Live(instance) => {
            out.text("  in instance ").dec(instance).text(", which is live\n");
        }
        Owner::Freed(instance) => {
            out.text("  in instance ").dec(instance).text(", which has been freed\n");
        }
    }
}

/// Puts a finished report where a person will see it.
///
/// Standard error, one `write`, and whatever it returns is not something a program that is about to
/// stop can do anything about.
///
/// Under `cargo test` it writes nothing. What matters is the text, [`render`] is what builds it, and
/// the tests read it from there; a test run that printed a report for every deliberate refusal
/// would bury the failures that are real.
pub fn emit(text: &str) {
    let _ = text;
    #[cfg(all(unix, not(test)))]
    // SAFETY: `write` is the C library's, the pointer and the length are one live `&str`, and file
    // descriptor two is standard error on every Unix.
    unsafe {
        unsafe extern "C" {
            fn write(fd: i32, buf: *const core::ffi::c_void, len: usize) -> isize;
        }
        write(2, text.as_ptr().cast(), text.len());
    }
}

/// Stops the program and does not come back.
///
/// What the panic handler does on a target where there is a C library to ask. `abort` raises
/// `SIGABRT`, which is what a debugger attaches to and what a shell reports as a crash, and both are
/// what somebody running a program under the monitor wants.
#[cfg(unix)]
pub fn stop() -> ! {
    unsafe extern "C" {
        fn abort() -> !;
    }
    // SAFETY: `abort` is the C library's and does not return.
    unsafe { abort() }
}

/// The same where there is no C library to ask, which is a kernel and a bare target.
///
/// A loop rather than an instruction, because which instruction stops a machine is a fact about the
/// machine and this file is not the place that knows it. Tier K replaces this.
#[cfg(not(unix))]
pub fn stop() -> ! {
    loop {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc::{alloc, dealloc};
    use crate::turnstile::turn;

    /// The report for a descriptor and an address, as a `String` a test can read.
    fn rendered(row: &Descriptor, addr: Option<usize>) -> std::string::String {
        let mut text = Text::new();
        render(&mut text, row, addr);
        assert_eq!(text.lost(), 0, "the report did not fit in {ROOM} bytes");
        std::string::String::from(text.as_str())
    }

    /// The descriptor a four byte access carries.
    const ACCESS: Descriptor = Descriptor { judgement: 1, class: 0, size: 4, pc: 0 };

    #[test]
    fn a_report_says_the_judgement_in_the_numbering_the_specification_uses() {
        // Somebody holding a report should be able to find the row it is about by searching the
        // specification for the word in it, which is why the wording is not paraphrased here.
        let text = rendered(&ACCESS, None);
        assert_eq!(
            text,
            "rucc: memory safety violation\n  judgement J1, an access the capability, the planes \
             or the alignment did not permit\n"
        );
    }

    #[test]
    fn a_judgement_number_nothing_knows_is_said_to_be_one_rather_than_described_as_another() {
        // The byte comes out of an object file, so it can be anything. Describing a J9 as a J1
        // would send somebody looking for a bug that is not the one they have.
        let text = rendered(&Descriptor { judgement: 9, ..ACCESS }, None);
        assert!(text.contains("judgement J9,"), "{text}");
        assert!(text.contains("not a judgement this runtime has heard of"), "{text}");
    }

    #[test]
    fn a_report_about_an_access_says_where_and_how_wide() {
        // The two facts a person reading a heap overflow report reaches for first, and the address
        // is padded to the full width so that two of them line up in a terminal.
        let text = rendered(&ACCESS, Some(0x7f_0000_1234));
        assert!(text.contains("  4 bytes at 0x0000007f00001234\n"), "{text}");
    }

    #[test]
    fn a_check_with_no_width_says_the_address_and_does_not_invent_one() {
        // The liveness check carries no size, because whether anybody owns an address is not a
        // question about how many bytes are read through it. Printing "0 bytes" would read as a
        // fact about the access.
        let text = rendered(&Descriptor { size: 0, ..ACCESS }, Some(0x10));
        assert!(text.contains("  at 0x0000000000000010\n"), "{text}");
        assert!(!text.contains("bytes"), "{text}");
    }

    #[test]
    fn the_class_line_is_absent_rather_than_zero_while_nothing_decides_it() {
        // Zero is not a row of document 03's tables. Printing it would invite somebody to look one
        // up and find nothing.
        assert!(!rendered(&ACCESS, None).contains("class"));
        assert!(rendered(&Descriptor { class: 3, ..ACCESS }, None).contains("  class 3 of spec/"));
    }

    #[test]
    fn a_report_says_what_the_plane_knows_about_the_address() {
        let _turn = turn();
        // The line that makes a use after free report a use after free report rather than a crash
        // with an address in it. The instance number is the same one before and after, which is
        // what says the pointer is stale rather than wild.
        let ptr = alloc(64);
        let live = rendered(&ACCESS, Some(ptr as usize));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
        let freed = rendered(&ACCESS, Some(ptr as usize));

        assert!(live.contains(", which is live\n"), "{live}");
        assert!(freed.contains(", which has been freed\n"), "{freed}");
        let instance = |text: &str| {
            let (_, rest) = text.split_once("in instance ").expect("the plane line");
            let (number, _) = rest.split_once(',').expect("the plane line");
            std::string::String::from(number)
        };
        assert_eq!(instance(&live), instance(&freed));
    }

    #[test]
    fn an_address_outside_the_heap_is_said_to_be_outside_it() {
        let _turn = turn();
        // Otherwise a report about a stack object would claim the heap knew something about it.
        let mut local = [0_u8; 16];
        let text = rendered(&ACCESS, Some(local.as_mut_ptr() as usize));
        assert!(text.contains("  which is not in the heap this monitor watches\n"), "{text}");
    }

    #[test]
    fn what_it_writes_is_ascii_and_ends_where_it_says_it_does() {
        // The buffer is read back as UTF-8 without checking, so the writers have to be the only
        // way in and each has to put ASCII in. This is that assumption, tested.
        let mut text = Text::new();
        text.text("x").dec(u64::MAX).hex(usize::MAX).dec(0);
        assert_eq!(text.as_str(), "x184467440737095516150xffffffffffffffff0");
        assert!(text.as_str().is_ascii());
        assert_eq!(text.lost(), 0);
    }

    #[test]
    fn a_report_too_long_for_the_buffer_is_short_rather_than_cut_in_half() {
        // Nothing renders one this long. If something ever does, dropping a whole append keeps the
        // text readable and `lost` is what says the report is incomplete.
        let mut text = Text::new();
        text.text("a");
        let long = std::string::String::from_utf8(std::vec![b'b'; ROOM]).expect("ascii");
        text.text(&long);
        assert_eq!(text.as_str(), "a");
        assert_eq!(text.lost(), ROOM);
    }
}
