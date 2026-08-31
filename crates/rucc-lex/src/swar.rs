//! Word at a time byte scanning.
//!
//! Design: `spec/05-preprocessor.md` section 5.2.
//!
//! Whitespace and comment bodies are where a C compiler spends its time not learning anything.
//! A real header is roughly a third comment and license block by weight, and indentation is
//! most of the rest of the bytes that are not tokens. Walking those one byte at a time through
//! the phase 1 and 2 cursor costs a branch per byte to answer a question that is almost always
//! no, so the two loops that do it read eight bytes at a time instead and fall back to the
//! cursor the moment anything interesting appears.
//!
//! This is SWAR rather than an intrinsic, and deliberately. `u64` arithmetic is the one vector
//! width every target rucc will ever have, it needs no runtime dispatch, no target feature and
//! no unsafe, and it is within a small factor of a hand written SSE loop on the input sizes
//! that occur. A target specific version can go underneath this interface later if a profile
//! ever asks for one.
//!
//! Two directions, and they are not equally forgiving, which is the thing to understand before
//! editing anything here.
//!
//! [`first_of`] answers "where is the next byte I have to look at", and answering too early is
//! harmless: the caller looks at the byte, finds it is not one it cares about, and goes round
//! again. So it uses the cheap equality test, which can claim a match one byte above a real
//! one.
//!
//! [`run_of_blanks`] answers "how many bytes may I skip without looking", and answering too
//! late is a bug, because the caller would skip a byte that means something. So it is built
//! out of [`ge`], which is exact, and never claims a byte is blank when it is not.

/// `0x01` in every byte, which multiplied by `n` gives `n` in every byte.
const LO: u64 = 0x0101_0101_0101_0101;

/// `0x80` in every byte, which is where every one of these masks puts its answers.
const HI: u64 = 0x8080_8080_8080_8080;

/// How many bytes a step covers.
const STEP: usize = 8;

/// `0x80` in each byte position whose byte is at least `n`. Exact for `n` up to `0x80`.
///
/// The trick is that a byte-wise comparison has to stop the subtraction borrowing from one
/// byte into the next. Clearing the high bit of each byte and setting it again gives every
/// byte of the minuend a value of at least `0x80`, which is at least `n`, so no byte can
/// borrow. A byte that had its high bit set was at least `0x80` and so at least `n` already,
/// and is put back in at the end.
#[inline]
const fn ge(w: u64, n: u8) -> u64 {
    let high = w & HI;
    let low = w & !HI;
    let diff = (low | HI).wrapping_sub(LO.wrapping_mul(n as u64));
    (diff & HI) | high
}

/// `0x80` in each byte position whose byte is not `n`. Exact.
#[inline]
const fn ne(w: u64, n: u8) -> u64 {
    // Below `n` or above `n`, which between them are every byte that is not `n`. Both halves
    // are exact, so their union is.
    (!ge(w, n) & HI) | ge(w, n + 1)
}

/// `0x80` in each byte position whose byte is `n`, and possibly in some just above one.
///
/// The classic zero byte test. A byte one above a real match can come out set as well, because
/// the subtraction borrows upwards, so this may only be used to find the *first* match: every
/// position below the lowest set bit is correct, since a false positive needs a real match
/// underneath it to borrow from.
#[inline]
const fn eq_or_just_above(w: u64, n: u8) -> u64 {
    let x = w ^ LO.wrapping_mul(n as u64);
    x.wrapping_sub(LO) & !x & HI
}

/// The index of the lowest byte a mask has an answer in, if it has one.
#[inline]
const fn lowest(mask: u64) -> Option<usize> {
    if mask == 0 { None } else { Some(mask.trailing_zeros() as usize / STEP) }
}

/// Eight bytes at `at`, low byte first, or `None` if there are fewer than eight left.
///
/// Reading little endian rather than native is what makes the byte index of a mask bit the
/// same on every host, so nothing here has to think about endianness again.
#[inline]
fn word(bytes: &[u8], at: usize) -> Option<u64> {
    let chunk: [u8; STEP] = bytes.get(at..at + STEP)?.try_into().ok()?;
    Some(u64::from_le_bytes(chunk))
}

/// The offset of the first byte at or after `at` that is one of `needles`.
///
/// May stop early on a byte that is not in `needles` at all, so the caller has to check the
/// byte it lands on rather than trust it. Returns `bytes.len()` if it ran off the end, which
/// is also what the caller wants, since there is nothing left to look at either way.
///
/// `needles` is expected to be short. Every call site has between two and five.
pub(crate) fn first_of(bytes: &[u8], at: usize, needles: &[u8]) -> usize {
    let mut i = at;
    while let Some(w) = word(bytes, i) {
        let mut found = 0;
        for &n in needles {
            found |= eq_or_just_above(w, n);
        }
        if let Some(k) = lowest(found) {
            return i + k;
        }
        i += STEP;
    }
    while i < bytes.len() {
        if needles.contains(&bytes[i]) {
            return i;
        }
        i += 1;
    }
    bytes.len()
}

/// The offset of the first byte at or after `at` that is neither a space nor a tab.
///
/// Exact, because the caller skips everything before it without looking. Vertical tab and form
/// feed are whitespace too and are left to the caller: they occur in real code about as often
/// as they occur in this sentence, and putting them in here would cost a mask on every word to
/// buy nothing.
pub(crate) fn run_of_blanks(bytes: &[u8], at: usize) -> usize {
    let mut i = at;
    while let Some(w) = word(bytes, i) {
        let neither = ne(w, b' ') & ne(w, b'\t');
        if let Some(k) = lowest(neither) {
            return i + k;
        }
        i += STEP;
    }
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the two scanners are supposed to agree with, written the obvious way.
    fn blanks_scalar(bytes: &[u8], at: usize) -> usize {
        let mut i = at;
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
        i
    }

    #[test]
    fn a_run_of_blanks_ends_where_the_obvious_loop_says_it_does() {
        // Every length either side of the word boundary, and every byte after the run, which
        // between them cover the tail handling and every carry the masks can produce.
        for run in 0..24 {
            for after in 0u16..=255 {
                let after = after as u8;
                if matches!(after, b' ' | b'\t') {
                    continue;
                }
                let mut input = vec![b' '; run];
                input.push(after);
                input.extend_from_slice(b"tail tail tail");
                assert_eq!(
                    run_of_blanks(&input, 0),
                    blanks_scalar(&input, 0),
                    "run of {run} then {after:#04x}"
                );
            }
        }
    }

    #[test]
    fn a_byte_one_above_a_space_is_not_a_space() {
        // `!` is `0x21` and a space is `0x20`, so the cheap equality test claims the `!` is a
        // space too. `run_of_blanks` must not use that test, and this is the input that says
        // whether it does.
        assert_eq!(run_of_blanks(b"        !x", 0), 8);
        assert_eq!(run_of_blanks(b" !", 0), 1);
        assert_eq!(run_of_blanks(b"\t\t\t\t\t\t\t\t\nx", 0), 8);
    }

    #[test]
    fn tabs_and_spaces_mix_freely() {
        assert_eq!(run_of_blanks(b" \t \t \t \t \t x", 0), 11);
        assert_eq!(run_of_blanks(b"x", 0), 0);
        assert_eq!(run_of_blanks(b"", 0), 0);
        // Starting part way in is the same question asked from a different place.
        assert_eq!(run_of_blanks(b"abc   def", 3), 6);
    }

    #[test]
    fn first_of_never_runs_past_a_needle() {
        // The property that matters. It may stop short, but the byte it names must be at or
        // before the first real one, or the caller skips something that meant something.
        for len in 0..40 {
            for at in [0usize, 1, 7, 8, 9] {
                if at > len {
                    continue;
                }
                let mut input = vec![b'x'; len];
                for planted in 0..len {
                    input[planted] = b'*';
                    let got = first_of(&input, at, b"*\n");
                    let want = (at..len).find(|&i| input[i] == b'*').unwrap_or(len);
                    assert!(got <= want, "len {len} at {at} planted {planted}: {got} > {want}");
                    assert!(got <= len);
                    input[planted] = b'x';
                }
                assert_eq!(first_of(&input, at, b"*"), len, "no needle, len {len} at {at}");
            }
        }
    }

    #[test]
    fn first_of_finds_a_needle_in_the_tail_past_the_last_whole_word() {
        // Nine bytes is one word and one byte, so this only passes if the scalar tail runs.
        assert_eq!(first_of(b"xxxxxxxx*", 0, b"*"), 8);
        assert_eq!(first_of(b"xxxxxxx*x", 0, b"*"), 7);
        assert_eq!(first_of(b"*xxxxxxxx", 0, b"*"), 0);
    }

    #[test]
    fn the_comparison_masks_agree_with_arithmetic_on_every_byte() {
        // `ge` is the one everything exact is built out of, so it is worth checking against
        // the definition rather than against a handful of examples.
        for b in 0u16..=255 {
            let b = b as u8;
            let w = u64::from_le_bytes([b; STEP]);
            for n in [1u8, 9, 0x20, 0x2A, 0x5C, 0x7F, 0x80] {
                let want = if b >= n { HI } else { 0 };
                assert_eq!(ge(w, n), want, "byte {b:#04x} against {n:#04x}");
            }
            assert_eq!(ne(w, b' '), if b == b' ' { 0 } else { HI }, "byte {b:#04x}");
        }
    }
}
