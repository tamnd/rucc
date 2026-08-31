//! Getting the bytes of a source file, by mapping it when that is cheaper than copying it.
//!
//! Design: `spec/05-preprocessor.md` section 5.1, which asks that a file be memory mapped
//! rather than read into a buffer, so that a large translation unit is not copied before it is
//! read. The saving is real but it is not free, and which way round it comes out depends
//! entirely on the size of the file.
//!
//! Reading costs one system call and a copy the kernel does with wide stores out of a page
//! cache it already has warm. Mapping costs two system calls, a page table walk, and a minor
//! fault every time the scanner crosses into a page it has not touched yet. On a small file the
//! faults dominate and reading wins; on a large one the copy dominates and mapping wins. So
//! this does both, and [`MAP_THRESHOLD`] is where the two curves cross on the machines this was
//! measured on.
//!
//! The rest of the compiler cannot tell the difference. [`SourceBytes`] holds anything that is
//! a slice of bytes, so a mapping goes into the source map exactly where a `Vec<u8>` used to,
//! and a span is the same span either way.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use memmap2::Mmap;
use rucc_diag::SourceBytes;

/// The size at which mapping starts to beat reading.
///
/// Measured rather than guessed, by getting the bytes of a file both ways and then touching
/// every one of them, interleaved, taking the best of seven rounds. Two megabytes is where the
/// two curves cross on the slower of the two machines. At that size Linux has them within one
/// percent of each other and macOS already prefers mapping by a factor of one and three
/// quarters. Above it mapping only gets further ahead: at thirty two megabytes it is four times
/// faster on Linux and twice as fast on macOS, because a copy that size stops fitting in any
/// cache and the allocation behind it stops being free.
///
/// Below it, reading wins or ties, so this leaves every header on the reading path where it
/// belongs. Almost nothing a compiler reads is this big. A system header is a few kilobytes,
/// a kernel source file is tens; the amalgamated SQLite is nine megabytes and generated or
/// preprocessed input runs to hundreds, and those are the ones this is for.
pub(crate) const MAP_THRESHOLD: u64 = 2 * 1024 * 1024;

/// The bytes of `path`.
///
/// # Errors
///
/// Whatever opening or reading the file says. A mapping that fails for its own reasons is not
/// an error: it falls back to reading, because a file the kernel will not map is still a file
/// the compiler can compile.
pub(crate) fn read(path: &Path) -> io::Result<SourceBytes> {
    let file = File::open(path)?;
    let meta = file.metadata()?;

    // `is_file` matters as much as the size does. A pipe or a character device reports a length
    // of zero whatever it is about to produce, and mapping one either fails or gives an empty
    // file, so anything that is not an ordinary file goes down the reading path where the
    // length is discovered by reading it.
    if meta.is_file() && meta.len() >= MAP_THRESHOLD {
        // SAFETY: mapping a file is unsafe because another process can change it underneath
        // the mapping, which turns a read of the mapped memory into a bus error rather than
        // into a short read. There is no way to prevent that on any system this compiler
        // targets, and GCC and Clang both map their input and both have the same hazard.
        // Editing a source file while the compiler is reading it produces an undefined result
        // whichever way the bytes arrive, so this trades a garbled diagnostic for a signal on
        // an input that was already meaningless.
        if let Ok(map) = unsafe { Mmap::map(&file) } {
            return Ok(SourceBytes::new(map));
        }
    }

    slurp(&file, meta.len())
}

/// Reads the whole file into one allocation.
///
/// The length from the metadata is a hint for the allocation and nothing more. A file can grow
/// between the two calls, and `read_to_end` handles that by growing the vector, which is why the
/// result is the vector's length rather than the metadata's.
fn slurp(mut file: &File, hint: u64) -> io::Result<SourceBytes> {
    let hint = usize::try_from(hint).unwrap_or(0);
    let mut bytes = Vec::with_capacity(hint);
    file.read_to_end(&mut bytes)?;
    Ok(SourceBytes::new(bytes))
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::PathBuf;

    use super::*;

    /// A file in the temporary directory, deleted when the test finishes with it.
    struct TempFile(PathBuf);

    impl TempFile {
        fn new(name: &str, contents: &[u8]) -> TempFile {
            let path = std::env::temp_dir().join(format!("rucc-map-{}-{name}", std::process::id()));
            let mut file = File::create(&path).expect("temporary directory should be writable");
            file.write_all(contents).expect("writing a temporary file should work");
            TempFile(path)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn a_small_file_arrives_whole() {
        let file = TempFile::new("small", b"int main(void) { return 0; }\n");
        let bytes = read(&file.0).expect("should read");
        assert_eq!(bytes.as_slice(), b"int main(void) { return 0; }\n");
    }

    #[test]
    fn an_empty_file_is_empty_rather_than_an_error() {
        // Worth its own test because mapping a zero length file fails on every system, so this
        // only passes if the threshold keeps it off the mapping path.
        let file = TempFile::new("empty", b"");
        let bytes = read(&file.0).expect("should read");
        assert!(bytes.as_slice().is_empty());
    }

    #[test]
    fn a_file_over_the_threshold_arrives_whole_too() {
        // The point of the test is that the two paths agree. A mapped file that came back one
        // page short, or with a trailing zero page, would show up here and nowhere else.
        let big: Vec<u8> = (0..MAP_THRESHOLD as usize + 4321).map(|i| (i % 251) as u8).collect();
        let file = TempFile::new("big", &big);
        let bytes = read(&file.0).expect("should read");
        assert_eq!(bytes.as_slice().len(), big.len());
        assert_eq!(bytes.as_slice(), &big[..]);
    }

    #[test]
    fn a_file_that_is_not_there_says_so() {
        let path = std::env::temp_dir().join("rucc-map-no-such-file-at-all");
        let error = read(&path).expect_err("should not be there");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
