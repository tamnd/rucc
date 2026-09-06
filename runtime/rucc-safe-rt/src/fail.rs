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
}

impl Judgement {
    /// Which judgement a descriptor's byte names, or nothing.
    ///
    /// Nothing is a real answer rather than a defensive one. The byte comes out of an object file
    /// that may have been built by a different version of the compiler, and a report that said J9
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
/// It does not return `!`, even though the only posture implemented today never comes back.
/// Document 06 section 6.5 specifies `-fsafety-on-error=abort|continue|log`, and under `continue`
/// the access is performed as written and the report is deduplicated, which is what a corpus run
/// needs so that one bug does not hide a hundred. Committing the signature to never returning now
/// would mean changing it later, and this signature is ABI.
///
/// # Panics
///
/// Always, which is how the abort posture is spelled.
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
/// Always, which is how the abort posture is spelled.
///
/// # Safety
///
/// As [`__rucc_safety_fail`], except that a null descriptor is allowed and reads as one that says
/// nothing.
pub unsafe fn report(descriptor: *const Descriptor, addr: Option<usize>) -> ! {
    // A null descriptor is not something generated code produces, and reading through it would
    // turn one report into two faults. Everything the address says is still worth saying.
    let row = if descriptor.is_null() {
        Descriptor { judgement: 0, class: 0, size: 0, pc: 0 }
    } else {
        // SAFETY: the caller says this is the address of a descriptor the compiler emitted, which
        // is sixteen bytes of constant data in `.rucc_safety_desc`.
        unsafe { descriptor.read() }
    };
    stop(&row, addr);
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
/// Always, and for the same reason [`__rucc_safety_fail`] does.
pub fn refused(judgement: Judgement) -> ! {
    // The address is not passed. `free` was given one and it is in the caller's hands, and a
    // report that named it would be naming the argument rather than anything the planes know,
    // which is the one thing a reader would take it for. S2's reporter has the stack and can do
    // better than either.
    stop(&Descriptor { judgement: judgement as u8, class: 0, size: 0, pc: 0 }, None);
}

/// Says what happened and does not come back.
///
/// One place decides what stopping means, and it stops through the crate's panic handler rather
/// than open coding an abort, so that the posture is written down once. Returning is not an option
/// in either case: the access the check refused would go ahead.
fn stop(row: &Descriptor, addr: Option<usize>) -> ! {
    let mut text = crate::report::Text::new();
    crate::report::render(&mut text, row, addr);
    crate::report::emit(text.as_str());
    panic!("a memory safety judgement was refused");
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
    }
}
