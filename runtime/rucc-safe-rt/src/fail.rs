//! What a failed check calls, and what it is told.
//!
//! Design: `spec/safe-memory/06-instrumentation.md` sections 6.3.1 and 6.5.
//!
//! The shape here is the one thing about the runtime that the backend has to agree with, so it is
//! the first thing written. A check that fails branches to a call of [`__rucc_safety_fail`] with
//! one number, and everything the report needs beyond that number is looked up rather than passed.
//! That keeps the per-check code in the hot path to a compare and a branch, and lets the cold path
//! be as detailed as document 06 section 6.5 wants, which is the trade that makes good diagnostics
//! affordable rather than a thing we apologise for later.

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

/// What one failing check is, as the object file records it.
///
/// One of these per check that the backend could not discharge, in a `.rucc_safety_desc` section,
/// and the number the check passes is an index into that section. `#[repr(C)]` because the reader
/// is not necessarily this build of the runtime and may not be Rust at all.
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

/// The number a check hands the runtime, which is an index into the descriptor section.
pub type DescriptorId = u32;

/// What a check calls when it fails.
///
/// This is the only symbol the backend emits a reference to for the whole monitor, and it takes
/// one argument because everything else is either in the descriptor table or in the planes.
///
/// It does not return `!`, even though the only posture implemented today never comes back.
/// Document 06 section 6.5 specifies `-fsafety-on-error=abort|continue|log`, and under `continue`
/// the access is performed as written and the report is deduplicated, which is what a corpus run
/// needs so that one bug does not hide a hundred. Committing the signature to never returning now
/// would mean changing it later, and this signature is ABI.
///
/// # Panics
///
/// Always, for now, which is how the abort posture is spelled until S2 writes the reporter.
///
/// # Safety
///
/// Called from generated code with an id the same build put in `.rucc_safety_desc`. Handing it a
/// number from anywhere else reads a descriptor that is not there.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __rucc_safety_fail(_descriptor: DescriptorId) {
    // The reporter is S2. Until there is one the posture is abort, and abort goes through the
    // crate's panic handler rather than being open coded here, so that there is one place that
    // decides what stopping means. Returning is not an option: the access the check refused
    // would go ahead.
    panic!("a safety check failed and the reporter is not written yet");
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
