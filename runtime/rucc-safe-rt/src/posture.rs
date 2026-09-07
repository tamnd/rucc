//! What the monitor does after it has said what happened.
//!
//! Design: `spec/safe-memory/06-instrumentation.md` section 6.5.
//!
//! Section 6.5 gives three postures. `abort` stops the program at the operation that violated the
//! model, which is what an enforcement build wants and is the default here. `continue` says what
//! happened, performs the access as written, and carries on, so that one bug does not hide a
//! hundred, and it deduplicates by descriptor id so that a check inside a loop says its piece once
//! rather than a million times. That is unsound as enforcement and is exactly what a corpus run
//! needs, which is the tier distinction doing its job.
//!
//! `log` is the third name and section 6.5 says nothing about what it does differently. The reading
//! taken here is the only one the sentence offers: `continue` is described as deduplicated, so `log`
//! is the same posture without the deduplication, which is what somebody counting occurrences rather
//! than finding distinct bugs wants. That reading is a choice this file made and not something the
//! document says, and it is written here so that the document can overrule it.
//!
//! # Why an environment variable and not the flag
//!
//! Section 6.5 spells this as `-fsafety-on-error=`, and it is not that yet. The runtime is a static
//! archive that a program links against, and a compiler flag would have to travel from the object
//! the compiler wrote to the archive it was linked with. The mechanisms for that are a weak
//! reference the compiled program overrides, which Rust has no stable spelling for, and a
//! constructor the compiler emits, which is a `.init_array` entry the IR cannot express today.
//! Neither is a small piece and each one is its own decision.
//!
//! So the posture is read from `RUCC_SAFETY_ON_ERROR` at the first refusal. That is not the whole
//! of what section 6.5 asks for and it is the half that unblocks the thing the posture exists for,
//! which is running a test suite to the end rather than to the first report. When the flag arrives
//! it sets the default and this stays as the override, which is the shape every other tool of this
//! kind ends up with anyway.

use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

/// What to do after a report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Posture {
    /// Say what happened and stop, which is the default and the enforcement posture.
    Abort,
    /// Say what happened once per check site and carry on.
    Continue,
    /// Say what happened every time and carry on.
    Log,
}

/// The posture as a byte, so that it can be cached in an atomic.
///
/// Zero means nothing has looked yet, which is why the three postures start at one.
const UNREAD: u8 = 0;
const ABORT: u8 = 1;
const CONTINUE: u8 = 2;
const LOG: u8 = 3;

/// What the environment said, once it has been asked.
static CHOSEN: AtomicU8 = AtomicU8::new(UNREAD);

/// What to do after a report, reading the environment the first time it is asked.
///
/// Reading it lazily rather than at startup is deliberate: this crate has no constructor to run at
/// startup, and the first refusal is not on any path where one more `getenv` is worth counting.
/// Two threads that arrive together may both read it, and they read the same string and store the
/// same byte, so the race is one nobody can observe.
#[must_use]
pub fn chosen() -> Posture {
    let cached = CHOSEN.load(Ordering::Relaxed);
    let byte = if cached == UNREAD {
        let byte = read();
        CHOSEN.store(byte, Ordering::Relaxed);
        byte
    } else {
        cached
    };
    match byte {
        CONTINUE => Posture::Continue,
        LOG => Posture::Log,
        _ => Posture::Abort,
    }
}

/// What the environment says, as one of the three bytes.
///
/// A name it does not recognise reads as `abort`. The alternative is refusing to start over a typo
/// in a variable, which would turn a mistake in how a program was run into a report about the
/// monitor rather than about the program.
#[cfg(all(unix, not(test)))]
fn read() -> u8 {
    unsafe extern "C" {
        fn getenv(name: *const core::ffi::c_char) -> *const core::ffi::c_char;
    }
    // SAFETY: the name is a literal with a nul on the end, and `getenv` hands back either null or a
    // pointer to a nul terminated string the process owns for as long as nobody sets it again.
    let value = unsafe { getenv(c"RUCC_SAFETY_ON_ERROR".as_ptr()) };
    if value.is_null() {
        return ABORT;
    }
    // SAFETY: `value` is a nul terminated string from `getenv`, walked to its nul.
    let mut len = 0;
    // SAFETY: as above, and the walk stops at the nul rather than at a length somebody guessed.
    while unsafe { *value.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` bytes from `value` are the string without its nul.
    let bytes = unsafe { core::slice::from_raw_parts(value.cast::<u8>(), len) };
    name(bytes)
}

/// The same on a target with no environment to ask, which is a kernel and a bare machine.
#[cfg(not(all(unix, not(test))))]
fn read() -> u8 {
    ABORT
}

/// Which byte a name is.
fn name(bytes: &[u8]) -> u8 {
    match bytes {
        b"continue" => CONTINUE,
        b"log" => LOG,
        _ => ABORT,
    }
}

/// How many check sites the deduplication remembers.
///
/// A fixed table because there is no allocator here that a failing program can be trusted with, and
/// the program's own is the thing being reported on. Two hundred and fifty six distinct check sites
/// is more than any run has produced, and a run that produced more would start repeating reports
/// rather than losing them, which is the right way round.
const REMEMBERED: usize = 256;

/// How far a lookup walks before it gives up and reports anyway.
const PROBES: usize = 4;

/// The check sites that have already had their say, by descriptor address.
static SEEN: [AtomicUsize; REMEMBERED] = [const { AtomicUsize::new(0) }; REMEMBERED];

/// Whether this check site has not said its piece yet.
///
/// The identity is the descriptor's address, which is what section 6.5 means by descriptor id: one
/// per check the backend could not discharge, in `.rucc_safety_desc`, and the same one every time
/// that check refuses. A caller with no descriptor passes nothing and is never deduplicated, since
/// there is nothing to tell one occurrence from another.
///
/// A full run of probes that finds neither this site nor a free slot answers yes. A duplicate report
/// is noise and a dropped one is a bug nobody hears about, so the table fills up into the first and
/// never into the second.
pub fn first_time(id: Option<usize>) -> bool {
    let Some(id) = id else { return true };
    // The low four bits of a descriptor address are always zero, since a descriptor is sixteen
    // bytes and aligned to its size, so they are shifted out before the address picks a slot.
    let start = (id >> 4) % REMEMBERED;
    for probe in 0..PROBES {
        let slot = &SEEN[(start + probe) % REMEMBERED];
        let held = slot.load(Ordering::Relaxed);
        if held == id {
            return false;
        }
        if held == 0 && slot.compare_exchange(0, id, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
            return true;
        }
        // Somebody else claimed the slot between the load and the exchange. Their address is now
        // in it, so the next turn round the loop reads it and either matches or moves on.
        if slot.load(Ordering::Relaxed) == id {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_the_document_gives_is_the_posture_it_names() {
        assert_eq!(name(b"continue"), CONTINUE);
        assert_eq!(name(b"log"), LOG);
        assert_eq!(name(b"abort"), ABORT);
    }

    #[test]
    fn a_name_nobody_recognises_stops_the_program() {
        // Because the alternative is a report about the monitor rather than about the program,
        // over a typo in how the program was run.
        assert_eq!(name(b"contine"), ABORT);
        assert_eq!(name(b""), ABORT);
    }

    #[test]
    fn nothing_said_is_the_enforcement_posture() {
        // The default has to be the one that stops. A monitor that carried on by default would be
        // a monitor an enforcement build silently did not get.
        assert_eq!(chosen(), Posture::Abort);
    }

    #[test]
    fn a_check_site_says_its_piece_once() {
        let id = 0x1000;
        assert!(first_time(Some(id)));
        assert!(!first_time(Some(id)));
        assert!(!first_time(Some(id)));
    }

    #[test]
    fn two_check_sites_each_get_a_turn() {
        let (first, second) = (0x2000, 0x2010);
        assert!(first_time(Some(first)));
        assert!(first_time(Some(second)));
        assert!(!first_time(Some(first)));
        assert!(!first_time(Some(second)));
    }

    #[test]
    fn a_refusal_with_no_check_site_behind_it_is_never_held_back() {
        // The allocator's judgements have no descriptor, because nothing in the object describes a
        // call to `free`, so there is no identity to tell two of them apart by.
        assert!(first_time(None));
        assert!(first_time(None));
    }
}
