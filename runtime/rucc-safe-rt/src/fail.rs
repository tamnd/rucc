//! What a failed check calls, and what it is told.
//!
//! Design: `spec/safe-memory/06-instrumentation.md` sections 6.3.1 and 6.5.
//!
//! The shape here is the one thing about the runtime that the backend has to agree with, so it is
//! the first thing written. A check that fails branches to a call of [`__rucc_safety_fail`] with the
//! address of a [`Descriptor`] the compiler wrote into the object, and everything the report needs
//! beyond that is looked up rather than passed. That keeps the per-check code in the hot path to a
//! compare and a branch, and lets the cold path be as detailed as document 06 section 6.5 wants,
//! which is the trade that makes good diagnostics affordable rather than a thing we apologise for
//! later.
//!
//! [`crate::report`] is what turns one of these into words.

/// Which judgement of document 04 section 4.4 was violated.
///
/// Numbered as the document numbers them, so that a report and the specification use one
/// vocabulary. The discriminants are ABI: they are compiled into the descriptor table of every
/// object built with `-fsafety` and read back by a runtime that may be a different build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Judgement {
    /// J1: an access that the capability, the planes or the alignment did not permit.
    Access = 1,
    /// J2: a derivation that left the object it was derived from.
    Derive = 2,
    /// J3: an integer turned into a pointer that names no exposed live instance.
    Synthesize = 3,
    /// J4: a storage instance beginning where one already was.
    Begin = 4,
    /// J5: a storage instance ending that was not live.
    End = 5,
    /// J6: a free of something that was not allocated, or not by that allocator.
    Free = 6,
    /// J7: an access to a range whose ownership was transferred away.
    Transfer = 7,
    /// J8: two accesses in one block reaching one byte through two `restrict` pointers of it.
    Restrict = 8,
    /// J9: a word read or written while another thread's write of it was unordered against this
    /// one.
    Race = 9,
}

impl Judgement {
    /// Which judgement a descriptor's byte names, or nothing.
    ///
    /// Nothing is a real answer rather than a defensive one. The byte comes out of an object file
    /// that may have been built by a different version of the compiler, and a report that said J10
    /// with a description of J1 beside it would be worse than one that admits it does not know.
    #[must_use]
    pub const fn of(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Access),
            2 => Some(Self::Derive),
            3 => Some(Self::Synthesize),
            4 => Some(Self::Begin),
            5 => Some(Self::End),
            6 => Some(Self::Free),
            7 => Some(Self::Transfer),
            8 => Some(Self::Restrict),
            9 => Some(Self::Race),
            _ => None,
        }
    }

    /// What it says, in one line, for the report.
    ///
    /// The same wording as document 04 section 4.4, so that somebody holding a report and somebody
    /// holding the specification are reading the same sentence.
    #[must_use]
    pub const fn what(self) -> &'static str {
        match self {
            Self::Access => "an access the capability, the planes or the alignment did not permit",
            Self::Derive => "a pointer derived from another that left the object it came from",
            Self::Synthesize => "an integer turned into a pointer that names no live instance",
            Self::Begin => "a storage instance beginning where one already was",
            Self::End => "a storage instance ending that was not live",
            Self::Free => "a free of something that was not allocated, or not by that allocator",
            Self::Transfer => "an access to a range whose ownership was transferred away",
            Self::Restrict => "one byte reached through two restrict pointers of one block",
            Self::Race => "a word another thread wrote with nothing ordering that against this one",
        }
    }
}

/// What one failing check is, as the object file records it.
///
/// One of these per check that the backend could not discharge, in a `.rucc_safety_desc` section,
/// and what the check passes is its address. `#[repr(C)]` because the reader is not necessarily
/// this build of the runtime and may not be Rust at all.
///
/// An address rather than an index into the section, because an index is an index into one object's
/// descriptors and a link concatenates several objects' worth. `rucc_safety::lower` says the rest.
///
/// The source location is not in here. Document 06 section 6.5 takes it from the DWARF the parent's
/// document 11 already emits, because a compiler that ships line tables twice is a compiler whose
/// two copies eventually disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Descriptor {
    /// Which judgement of section 4.4 the check decides.
    pub judgement: u8,
    /// Which row of document 03 the failure is, as an index into that document's tables.
    pub class: u8,
    /// How many bytes the access covers, saturating, so that a report can say what was attempted.
    pub size: u16,
    /// Where in the program the check is, as the address of the failing branch. Enough to find
    /// the DWARF row without carrying one.
    pub pc: u64,
}

/// What a check calls when it fails.
///
/// This is the only symbol the backend emits a reference to for the whole monitor, and it takes
/// one argument because everything else is either in the descriptor or in the planes.
///
/// It does not return `!`, because under two of the three postures of document 06 section 6.5 it
/// comes back and the access goes ahead as written. [`crate::posture`] says which.
///
/// # Panics
///
/// Under the abort posture, which is the default and is how enforcement is spelled.
///
/// # Safety
///
/// Called from generated code with the address of a descriptor the same build put in
/// `.rucc_safety_desc`. Handing it an address from anywhere else reads sixteen bytes that are not
/// a descriptor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rucc_safety_fail(descriptor: *const Descriptor) {
    // SAFETY: this function's contract is the one below, passed straight on.
    unsafe { report(descriptor, None) }
}

/// What an interposed function calls when it refuses one of its arguments.
///
/// Like [`refused`], and with two things that a `free` cannot supply and a wrapper can. The address
/// is the byte the call would have touched, which for a range that ran too far is the byte it ran
/// to. `site` names the function and the argument, built from the row by
/// [`crate::interpose`], so the report says `memcpy, over its dst argument` rather than a line
/// inside this crate that means nothing to the person whose program stopped.
///
/// # Panics
///
/// Always, as [`refused`] does and for the same reason.
pub fn refused_at(judgement: Judgement, site: &'static str, addr: usize) -> ! {
    judge(
        &Descriptor { judgement: judgement as u8, class: 0, size: 0, pc: 0 },
        &crate::report::Facts { site: Some(site), addr: Some(addr), base: None, witness: None },
        None,
    );
    crate::report::stop()
}

/// The same, for a caller inside this crate, and with the address the check was about.
///
/// [`__rucc_safety_fail`] is an ABI and this is a Rust function, and the difference matters in two
/// places. A panic may not cross an `extern "C"` boundary, so a caller in this crate that goes
/// through the ABI aborts where a caller that goes through this one stops the way the crate's own
/// panic handler says to. And a caller in this crate has the faulting address, where the ABI's
/// signature has no room for one: the inline check of document 06 section 6.3.1 has already
/// compared it and thrown it away by the time it branches.
///
/// # Panics
///
/// Under the abort posture. Under the other two it says what happened and comes back, and the
/// caller performs the access as written.
///
/// # Safety
///
/// As [`__rucc_safety_fail`], except that a null descriptor is allowed and reads as one that says
/// nothing.
pub unsafe fn report(descriptor: *const Descriptor, addr: Option<usize>) {
    // SAFETY: this function's contract is the one below, and a derivation with no base to name is
    // exactly what the one below does with `None`.
    unsafe { report_from(descriptor, addr, None) }
}

/// The same, and says which pointer the refused one was derived from.
///
/// Only judgement J2 has a second address, and only J2 calls this. It is separate from [`report`]
/// rather than a fourth `None` at three call sites, because the base is the thing that turns the
/// refused address from a number into a sentence and a caller that has one should have to say so.
///
/// # Panics
///
/// As [`report`].
///
/// # Safety
///
/// As [`report`]. Neither address is read through.
pub unsafe fn report_from(descriptor: *const Descriptor, addr: Option<usize>, base: Option<usize>) {
    // A null descriptor is not something generated code produces, and reading through it would
    // turn one report into two faults. Everything the address says is still worth saying.
    let row = if descriptor.is_null() {
        Descriptor { judgement: 0, class: 0, size: 0, pc: 0 }
    } else {
        // SAFETY: the caller says this is the address of a descriptor the compiler emitted, which
        // is sixteen bytes of constant data in `.rucc_safety_desc`.
        unsafe { descriptor.read() }
    };
    // The descriptor's address is what section 6.5 means by descriptor id, and it is the identity
    // the `continue` posture deduplicates on. A null one has no identity, so it is never held back.
    let id = (!descriptor.is_null()).then_some(descriptor as usize);
    judge(&row, &crate::report::Facts { site: None, addr, base, witness: None }, id);
}

/// The same, and says which other thread's write the access raced with.
///
/// Only judgement J9 has a second thread, and only J9 calls this. It is separate from [`report`]
/// for the reason [`report_from`] is: a race report's whole content is the pair, an address on its
/// own says which word and not who else touched it, and a caller holding the other thread's stamp
/// should have to say so rather than dropping it.
///
/// # Panics
///
/// As [`report`].
///
/// # Safety
///
/// As [`report`]. The address is not read through and neither stamp is one.
pub unsafe fn report_race(
    descriptor: *const Descriptor,
    addr: usize,
    found: crate::epoch::Stamp,
    mine: crate::epoch::Stamp,
) {
    // A null descriptor reads as one that says nothing, for the reason `report_from` gives.
    let row = if descriptor.is_null() {
        Descriptor { judgement: Judgement::Race as u8, class: 0, size: 0, pc: 0 }
    } else {
        // SAFETY: the caller says this is the address of a descriptor the compiler emitted.
        unsafe { descriptor.read() }
    };
    let id = (!descriptor.is_null()).then_some(descriptor as usize);
    let facts = crate::report::Facts {
        site: None,
        addr: Some(addr),
        base: None,
        witness: Some((found, mine)),
    };
    judge(&row, &facts, id);
}

/// What the runtime calls when it is the one that decided, rather than a compiled check.
///
/// Judgements J4, J5 and J6 are decided inside the allocator, which has no descriptor because
/// there is no check site: the failing code is this crate's, not the program's, and it was reached
/// through a call the program made by name. So the judgement is passed directly and the rest of a
/// report is whatever the reporter can recover from the stack.
///
/// Separate from [`__rucc_safety_fail`] rather than a descriptor of some reserved shape, because
/// there is no descriptor to point at: nothing in the object describes a call to `free`.
///
/// # Panics
///
/// Always, whatever [`crate::posture`] says. The postures that carry on are defined by the access
/// going ahead as written, and there is no access here to let through: the program asked this crate
/// to do something to its own bookkeeping and the answer is that it may not. Carrying on would mean
/// either doing it anyway, which corrupts the thing every later judgement is read out of, or
/// returning a result the caller was not told is a refusal. Both are worse than stopping, and a
/// corpus run that wants past one of these wants the allocator's judgements turned off rather than
/// continued through.
pub fn refused(judgement: Judgement) -> ! {
    // The address is not passed. `free` was given one and it is in the caller's hands, and a
    // report that named it would be naming the argument rather than anything the planes know,
    // which is the one thing a reader would take it for. S2's reporter has the stack and can do
    // better than either.
    judge(
        &Descriptor { judgement: judgement as u8, class: 0, size: 0, pc: 0 },
        &crate::report::Facts::default(),
        None,
    );
    crate::report::stop()
}

/// Says what happened, and stops if the posture says to.
///
/// One place decides both, so that what a refusal means is written down once. Under `abort` it
/// stops through the crate's panic handler rather than open coding an abort. Under the other two it
/// comes back, and the caller's access goes ahead as written, which is what document 06 section 6.5
/// says the recovery is.
///
/// Under `continue` a check site says its piece once. The report is skipped rather than the stop,
/// because the two postures that deduplicate are the two that never stop anyway, so there is no
/// arrangement of the flags where being quiet means letting something through.
fn judge(row: &Descriptor, facts: &crate::report::Facts<'static>, id: Option<usize>) {
    let posture = crate::posture::chosen();
    if posture != crate::posture::Posture::Continue || crate::posture::first_time(id) {
        let mut text = crate::report::Text::new();
        crate::report::render(&mut text, row, facts);
        crate::report::emit(text.as_str());
    }
    if posture == crate::posture::Posture::Abort {
        panic!("a memory safety judgement was refused");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptor_is_the_size_the_backend_will_emit() {
        // The backend writes these bytes and something else reads them, possibly not this build
        // and possibly not in Rust, so the layout is a fact about the format rather than a
        // detail. Growing it is a change to what every object built with -fsafety contains.
        assert_eq!(size_of::<Descriptor>(), 16);
        assert_eq!(align_of::<Descriptor>(), 8);
    }

    #[test]
    fn the_judgements_are_numbered_the_way_the_model_numbers_them() {
        // A report that said "judgement 0" when the specification says J1 would be a report
        // nobody could look up.
        assert_eq!(Judgement::Access as u8, 1);
        assert_eq!(Judgement::Transfer as u8, 7);
        assert_eq!(Judgement::Restrict as u8, 8);
        assert_eq!(Judgement::Race as u8, 9);
        assert_eq!(Judgement::of(9), Some(Judgement::Race));
        assert_eq!(Judgement::of(10), None);
    }
}
