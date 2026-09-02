//! The headers the compiler ships, and the directory they appear to live in.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.4.
//!
//! A hosted C implementation is two halves. The library ships `<stdio.h>` and everything that
//! declares a function you link against. The compiler ships the handful of headers whose
//! contents are not the library's to know: `<stdarg.h>` is the target's calling convention,
//! `<limits.h>` and `<float.h>` are the target's types, and `<stddef.h>` is the ABI. No
//! library can write those, which is why every compiler carries its own copies and why a
//! compiler that carries none cannot preprocess a program as ordinary as SQLite.
//!
//! They are in the binary rather than on disk. A compiler that has to find its own
//! installation directory before it can preprocess a file is a compiler that stops working
//! when it is copied somewhere else, and a single static binary that works from anywhere is
//! worth more here than the ability to edit a header without rebuilding.
//!
//! Since they are not on disk they need a name, because the search path is a list of
//! directories and a diagnostic has to be able to say where a header came from. That name is
//! [`DIR`], and the angle brackets are the point: no directory a user can create is spelled
//! that way, so nothing on the real file system can shadow these or be shadowed by them.

use std::path::Path;

use rucc_diag::SourceBytes;

/// The directory the shipped headers appear to be in.
///
/// Not a path. It is a name that cannot be one, so that `#include <stdarg.h>` resolving to
/// `<builtin>/stdarg.h` reads as what it is, and so that a real directory can never collide
/// with it.
pub const DIR: &str = "<builtin>";

/// Every shipped header, in the order they are listed here, which is sorted by name.
///
/// The text is in the binary. `include_str!` rather than a build script because the set is
/// small and fixed, and because a build script would put the headers behind a step that has
/// to run before anything can be read.
const HEADERS: &[(&str, &str)] = &[
    ("float.h", include_str!("../runtime/include/float.h")),
    ("iso646.h", include_str!("../runtime/include/iso646.h")),
    ("limits.h", include_str!("../runtime/include/limits.h")),
    ("stdalign.h", include_str!("../runtime/include/stdalign.h")),
    ("stdarg.h", include_str!("../runtime/include/stdarg.h")),
    ("stdbool.h", include_str!("../runtime/include/stdbool.h")),
    ("stddef.h", include_str!("../runtime/include/stddef.h")),
    ("stdint.h", include_str!("../runtime/include/stdint.h")),
    ("stdnoreturn.h", include_str!("../runtime/include/stdnoreturn.h")),
];

/// The names of the shipped headers, sorted.
#[must_use]
pub fn names() -> Vec<&'static str> {
    HEADERS.iter().map(|&(name, _)| name).collect()
}

/// The text of one shipped header, by its name alone.
#[must_use]
pub fn header(name: &str) -> Option<&'static str> {
    HEADERS.iter().find(|&&(have, _)| have == name).map(|&(_, text)| text)
}

/// Reads a path that an include search produced, when it names a shipped header.
///
/// The path is [`DIR`] joined with the header's name, which on Windows means a backslash
/// between them, so the two halves are compared rather than the string.
#[must_use]
pub fn read(path: &Path) -> Option<SourceBytes> {
    if path.parent() != Some(Path::new(DIR)) {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    header(name).map(SourceBytes::new)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn the_shipped_headers_are_read_by_the_name_the_search_path_builds() {
        let path = PathBuf::from(DIR).join("stdarg.h");
        let bytes = read(&path).expect("stdarg.h is shipped");
        let text = String::from_utf8(bytes.as_ref().to_vec()).expect("utf-8");
        assert!(text.contains("__builtin_va_list"));
    }

    #[test]
    fn nothing_outside_the_builtin_directory_is_answered() {
        assert!(read(Path::new("/usr/include/stdarg.h")).is_none());
        assert!(read(Path::new("stdarg.h")).is_none());
        assert!(read(&PathBuf::from(DIR).join("stdio.h")).is_none());
        assert!(read(&PathBuf::from(DIR).join("sys").join("stdarg.h")).is_none());
    }

    #[test]
    fn the_list_is_sorted_so_that_a_new_header_has_one_place_to_go() {
        let mut sorted = names();
        sorted.sort_unstable();
        assert_eq!(names(), sorted);
    }

    /// Every header has to be idempotent and has to name itself in its own guard, because a
    /// program includes `<stddef.h>` forty times and a guard copied from a neighbour is the
    /// way one of them silently stops working.
    #[test]
    fn every_header_guards_itself_under_its_own_name() {
        for &(name, text) in HEADERS {
            let guard = format!("__RUCC_{}", name.trim_end_matches(".h").to_uppercase());
            assert!(text.contains(&guard), "{name} does not mention {guard}");
        }
    }
}
