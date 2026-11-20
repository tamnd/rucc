//! Which GCC release built a `libgcov.a`.
//!
//! A unit built with `-fprofile-arcs` hands a record to `__gcov_init`, and libgcov writes the
//! counts of that record only when its version word is the one libgcov was built with. GCC builds
//! the two together, so they always agree. This compiler says it is the newest GCC in `__GNUC__`,
//! and the GCC on a machine is often older, so the record has to take its release from the library
//! the link will use.
//!
//! The library says its release nowhere in words. It holds the version word as an operand of the
//! instructions that compare a record against it and write it to the head of a `.gcda` file, so the
//! word is in the bytes of the archive. The word is four characters, such as `B33*` for 13.3, and
//! it is stored low byte first, which is how every target this links for stores a word.
//!
//! The last character is the phase. `gcc/genversion.cc` writes `*` for a release and a prerelease
//! and the first character of `DEV-PHASE` for anything else, which is `e` for a build of the trunk.
//! The snapshot of GCC 16 that Ubuntu packages as `gcc-16` writes a space there. libgcov compares
//! the whole word, so the phase is taken from the library too.

/// The major and minor GCC release of the `libgcov.a` with these bytes and the phase that ends its
/// version word, or `None` when no version word is in it.
///
/// The word that comes up most often is the answer. The archive has it several times, and a run of
/// four bytes that only looks like one is not likely to come up as often.
#[must_use]
pub(crate) fn release(archive: &[u8]) -> Option<(u32, u32, u8)> {
    let mut seen: Vec<((u32, u32, u8), usize)> = Vec::new();
    for quad in archive.windows(4) {
        let &[phase, minor @ b'0'..=b'9', units @ b'0'..=b'9', tens @ b'A'..=b'Z'] = quad else {
            continue;
        };
        if !matches!(phase, b'*' | b' ' | b'a'..=b'z') {
            continue;
        }
        let major = u32::from(tens - b'A') * 10 + u32::from(units - b'0');
        let release = (major, u32::from(minor - b'0'), phase);
        match seen.iter_mut().find(|(known, _)| *known == release) {
            Some((_, count)) => *count += 1,
            None => seen.push((release, 1)),
        }
    }
    seen.into_iter().max_by_key(|&(_, count)| count).map(|(release, _)| release)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_is_the_version_word_the_library_holds_most() {
        // `B33*` low byte first is GCC 13.3, and `A95*` is 9.5.
        assert_eq!(release(b"\x0f\x1f*33B\x90\x90*33B..*25B"), Some((13, 3, b'*')));
        assert_eq!(release(b"cmp *59A"), Some((9, 5, b'*')));
        // A lower case letter and a missing phase are not a version word.
        assert_eq!(release(b"*33b x3B: plain text"), None);
    }

    #[test]
    fn the_phase_of_a_build_that_is_not_a_release_is_kept() {
        // The trunk writes `e`, and Ubuntu's snapshot of GCC 16 writes a space.
        assert_eq!(release(b"\x0fe07B\x90e07B"), Some((17, 0, b'e')));
        assert_eq!(release(b"\x0f 06B\x90 06B\x90 06B"), Some((16, 0, b' ')));
        assert_eq!(release(b""), None);
    }
}
