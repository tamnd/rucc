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

/// The major and minor GCC release of the `libgcov.a` with these bytes, or `None` when no version
/// word is in it.
///
/// The word that comes up most often is the answer. The archive has it several times, and a run of
/// four bytes that only looks like one is not likely to come up as often.
#[must_use]
pub fn release(archive: &[u8]) -> Option<(u32, u32)> {
    let mut seen: Vec<((u32, u32), usize)> = Vec::new();
    for quad in archive.windows(4) {
        let &[b'*', minor @ b'0'..=b'9', units @ b'0'..=b'9', tens @ b'A'..=b'Z'] = quad else {
            continue;
        };
        let major = u32::from(tens - b'A') * 10 + u32::from(units - b'0');
        let release = (major, u32::from(minor - b'0'));
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
        assert_eq!(release(b"\x0f\x1f*33B\x90\x90*33B..*25B"), Some((13, 3)));
        assert_eq!(release(b"cmp *59A"), Some((9, 5)));
        // Lower case and a missing star are not a version word.
        assert_eq!(release(b"*33b 33B* plain text"), None);
        assert_eq!(release(b""), None);
    }
}
