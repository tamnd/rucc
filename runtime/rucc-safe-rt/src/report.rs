//! Turning a refused judgement into words.
//!
//! Design: `spec/safe-memory/06-instrumentation.md` section 6.5.
//!
//! A memory safety report that does not say what the program did is worth very little, and this is
//! the part of ASan that made it succeed. Section 6.5 lists six things a report should carry and
//! this writes the three it can: the judgement in document 04's numbering, the address and the width
//! of the access, and what the lifetime plane says about the range the address is in. A refused
//! derivation gets two lines more, since it is the one judgement with two addresses and the refused
//! one on its own is a number rather than a fact.
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
/// Generous for what is written today, which is six short lines. A report that outgrew this would
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

    let Some(region) = crate::alloc::covering(addr) else { return Owner::Elsewhere };
    // SAFETY: the region is the one that covers this address, so its plane is built over it.
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

/// How far the extent walk will go, in granules.
///
/// The walk below is the only unbounded thing on the failure path, so it is bounded. Sixteen
/// megabytes of object is far more than anything a report has been about so far, and an object
/// larger than this gets no extent line rather than a report that takes a visible moment to
/// arrive. The `log` posture renders one of these per violation rather than one per site, which is
/// the arrangement where an unbounded walk would be felt.
///
/// Gated with the walk it bounds, since where there is no allocator there is no walk.
#[cfg(unix)]
const WALK: usize = 1 << 20;

/// Where the run of granules that owns `addr` begins and ends, as a half open range.
///
/// Read out of the lifetime plane rather than out of the instance header, for two reasons. The
/// plane is what the judgements are decided against, so a report built from it cannot disagree
/// with the refusal it is about: the header holds the size the program asked for and the plane
/// holds the granules the checks permit, and saying seventeen bytes about an object a check will
/// let thirty two through is how somebody concludes the monitor is broken. And document 10 section
/// 10.4 lets a third party allocator set versions in a plane without adopting this crate's block
/// layout, so a report that read a header would work for one arena and lie about the rest.
///
/// Nothing is answered for an address no live instance owns, since there is no object to describe.
#[cfg(unix)]
#[must_use]
pub fn extent(addr: usize) -> Option<(usize, usize)> {
    use crate::plane::GRANULE;

    let region = crate::alloc::covering(addr)?;
    // Every read of the plane in this function goes through here, so what makes one safe is
    // written once. The caller of this closure has established `holds` for the address it passes.
    let owns = |at: usize| {
        // SAFETY: the region is the one covering the address, so its plane is built over it.
        unsafe { region.plane.version(at) }
    };

    let version = owns(addr);
    if !crate::plane::owned(version) {
        return None;
    }

    let here = addr - (addr % GRANULE);
    let mut steps = 0;
    let mut lo = here;
    while lo >= GRANULE && region.holds(lo - GRANULE) && owns(lo - GRANULE) == version {
        lo -= GRANULE;
        steps += 1;
        if steps == WALK {
            return None;
        }
    }

    let mut hi = here + GRANULE;
    while region.holds(hi) && owns(hi) == version {
        hi += GRANULE;
        steps += 1;
        if steps == WALK {
            return None;
        }
    }
    Some((lo, hi))
}

/// The same where there is no allocator.
#[cfg(not(unix))]
#[must_use]
pub fn extent(addr: usize) -> Option<(usize, usize)> {
    let _ = addr;
    None
}

/// Everything a report carries beyond the descriptor.
///
/// A struct rather than four arguments because they are all optional and all of them are absent
/// for at least one caller, and four `None`s in a row at a call site is not something anybody
/// reads correctly twice.
#[derive(Clone, Copy, Debug, Default)]
pub struct Facts<'a> {
    /// Where the judgement was made, for the refusals that happen inside this crate rather than at
    /// a compiled check. An interposed function fills it in with its own name and the argument it
    /// refused, and everything else leaves it out, because a compiled check's location comes from
    /// the DWARF that the descriptor's `pc` points into and saying it twice invites the two to
    /// disagree.
    pub site: Option<&'a str>,
    /// What the check was about, and absent where there is nothing honest to put there, which is
    /// the ABI entry point and the allocator's own refusals.
    pub addr: Option<usize>,
    /// For judgement J2, the pointer the refused one was derived from.
    ///
    /// The only judgement with two addresses, and the reason it is here is that the refused
    /// address on its own says nothing. `0x7fffb7d89460` is a number. The object it should have
    /// stayed in and how far short of it the derivation fell is the sentence somebody can act on,
    /// and until this line existed getting it took a debugger.
    pub base: Option<usize>,
}

/// Writes the report for one refused judgement.
pub fn render(out: &mut Text, row: &Descriptor, facts: &Facts<'_>) {
    let Facts { site, addr, base } = *facts;
    out.text("rucc: memory safety violation\n");

    out.text("  judgement J").dec(u64::from(row.judgement)).text(", ");
    out.text(match Judgement::of(row.judgement) {
        Some(judgement) => judgement.what(),
        None => "which is not a judgement this runtime has heard of",
    });
    out.text("\n");

    if let Some(site) = site {
        out.text("  in ").text(site).text(", which the monitor interposes\n");
    }

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

    let Some(base) = base else { return };
    out.text("  derived from ").hex(base);
    let Some((lo, hi)) = extent(base) else {
        // Either the base owns nothing, which the derivation check would have passed rather than
        // refused, or the object is larger than the walk. Saying where the pointer came from is
        // still worth a line; inventing an extent for it is not.
        out.text("\n");
        return;
    };
    out.text(", in an object running ").hex(lo).text(" to ").hex(hi).text("\n");

    // Which end, and how far. The two numbers a person reading an off by one reaches for, and the
    // reason for the subtraction rather than a signed distance is that "past the end" and "before
    // the start" are different bugs and a minus sign is a poor way to say which.
    out.text("  which is ");
    if addr >= hi {
        out.dec((addr - hi) as u64).text(" bytes past the end of it\n");
    } else if addr < lo {
        out.dec((lo - addr) as u64).text(" bytes before the start of it\n");
    } else {
        out.text("inside it, so the refusal was about the version and not the range\n");
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
    // The allocator is Unix only, because it is the one part of this crate that asks an
    // operating system for memory. The two tests below that put a real address in a report are
    // gated the same way, and everything else here is about rendering and runs everywhere.
    #[cfg(unix)]
    use crate::alloc::{alloc, dealloc};
    #[cfg(unix)]
    use crate::turnstile::turn;

    /// The report for a descriptor and an address, as a `String` a test can read.
    fn rendered(row: &Descriptor, addr: Option<usize>) -> std::string::String {
        from(row, &Facts { site: None, addr, base: None })
    }

    /// The report for a descriptor and whatever else the caller has, the same way.
    fn from(row: &Descriptor, facts: &Facts<'_>) -> std::string::String {
        let mut text = Text::new();
        render(&mut text, row, facts);
        assert_eq!(text.lost(), 0, "the report did not fit in {ROOM} bytes");
        std::string::String::from(text.as_str())
    }

    /// The descriptor a four byte access carries.
    const ACCESS: Descriptor = Descriptor { judgement: 1, class: 0, size: 4, pc: 0 };

    /// The descriptor a refused derivation carries, which has no width because a derivation reads
    /// nothing.
    ///
    /// Gated with the three tests that use it, all of which need a real allocation to derive from
    /// and so are Unix only. Off that, it is a constant nothing reads and the build refuses it.
    #[cfg(unix)]
    const DERIVE: Descriptor = Descriptor { judgement: 2, class: 0, size: 0, pc: 0 };

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

    #[cfg(unix)]
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

    #[cfg(unix)]
    #[test]
    fn an_address_outside_the_heap_is_said_to_be_outside_it() {
        let _turn = turn();
        // Otherwise a report about a stack object would claim the heap knew something about it.
        let mut local = [0_u8; 16];
        let text = rendered(&ACCESS, Some(local.as_mut_ptr() as usize));
        assert!(text.contains("  which is not in the heap this monitor watches\n"), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_derivation_names_the_object_and_says_how_far_out_it_went() {
        let _turn = turn();
        // The line this whole struct exists for. Before it, a J2 report was an address and the
        // word nobody, and turning that into a sentence took a debugger and the allocator's
        // source.
        let ptr = alloc(64);
        let base = ptr as usize;
        let past = from(&DERIVE, &Facts { site: None, addr: Some(base + 96), base: Some(base) });
        let under = from(&DERIVE, &Facts { site: None, addr: Some(base - 96), base: Some(base) });
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };

        let object = std::format!(", in an object running {:#018x} to {:#018x}\n", base, base + 64);
        assert!(past.contains(&object), "{past}");
        assert!(past.contains("  which is 32 bytes past the end of it\n"), "{past}");
        assert!(under.contains(&object), "{under}");
        assert!(under.contains("  which is 96 bytes before the start of it\n"), "{under}");
    }

    #[cfg(unix)]
    #[test]
    fn a_base_that_owns_nothing_is_named_and_given_no_extent() {
        let _turn = turn();
        // A derivation from a pointer the plane knows nothing about is passed rather than refused,
        // so this is a report nothing generates today. It exists because inventing a range for an
        // object that is not there is the one way this line could say something false.
        let mut local = [0_u8; 16];
        let base = local.as_mut_ptr() as usize;
        let text = from(&DERIVE, &Facts { site: None, addr: Some(base + 64), base: Some(base) });
        assert!(text.contains(&std::format!("  derived from {base:#018x}\n")), "{text}");
        assert!(!text.contains("running"), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn the_longest_report_this_renders_fits_in_the_buffer() {
        let _turn = turn();
        // `rendered` asserts nothing was lost, so this is the assertion. Every optional line at
        // once, with the widest numbers each can hold, against the one constant that decides
        // whether a report arrives whole.
        let ptr = alloc(64);
        let base = ptr as usize;
        let row = Descriptor { judgement: 2, class: 255, size: u16::MAX, pc: 0 };
        let facts = Facts {
            site: Some("memcpy, over its dst argument"),
            addr: Some(base - usize::from(u16::MAX)),
            base: Some(base),
        };
        let text = from(&row, &facts);
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
        assert!(text.len() < ROOM, "{} bytes of {ROOM}: {text}", text.len());
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
