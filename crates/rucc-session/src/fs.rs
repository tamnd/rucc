//! The file system the compiler reads through, and the include search path.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.4 for the search order, and
//! `spec/05-preprocessor.md` section 5.4 for what `#include_next` means.
//!
//! Nothing below the driver calls `std::fs`. That is what makes the compiler usable as a
//! library, and it is what makes a preprocessor test a value rather than a temporary
//! directory: [`MemoryFileSystem`] is a map from path to bytes, and a test that needs a
//! twelve deep header nest builds one in twelve lines with no clean up to forget.
//!
//! ```
//! use rucc_session::{FileSystem, IncludeForm, MemoryFileSystem, SearchPath};
//!
//! let mut fs = MemoryFileSystem::new();
//! fs.insert("/usr/include/stdio.h", *b"int puts(const char *);\n");
//!
//! let mut search = SearchPath::new();
//! search.push_system("/usr/include");
//!
//! let found = search.resolve(&fs, "stdio.h", IncludeForm::Angled, None, 0).unwrap();
//! assert_eq!(found.name.replace('\\', "/"), "/usr/include/stdio.h");
//! assert!(found.is_system);
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use rucc_diag::SourceBytes;

use crate::runtime;

/// Where the compiler reads source from.
///
/// The one method is deliberate. Everything the preprocessor wants to know about a file, up
/// to and including whether it exists, is answered by trying to read it, and an interface
/// with a separate `exists` invites the race where the answer changes between the two calls.
pub trait FileSystem: fmt::Debug + Send + Sync {
    /// Reads a file.
    ///
    /// # Errors
    ///
    /// Whatever the underlying file system says. [`io::ErrorKind::NotFound`] is the ordinary
    /// case during an include search and is not by itself a problem.
    fn read(&self, path: &Path) -> io::Result<SourceBytes>;
}

/// A file system held in memory, for tests and for embedding the compiler.
#[derive(Debug, Default)]
pub struct MemoryFileSystem {
    files: BTreeMap<PathBuf, SourceBytes>,
}

impl MemoryFileSystem {
    /// An empty file system.
    pub fn new() -> MemoryFileSystem {
        MemoryFileSystem::default()
    }

    /// Adds a file, replacing any file already at that path.
    pub fn insert(
        &mut self,
        path: impl Into<PathBuf>,
        contents: impl AsRef<[u8]> + Send + Sync + 'static,
    ) {
        self.files.insert(path.into(), SourceBytes::new(contents));
    }

    /// How many files it holds.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether it holds nothing.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

impl FileSystem for MemoryFileSystem {
    fn read(&self, path: &Path) -> io::Result<SourceBytes> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))
    }
}

/// Which spelling an `#include` used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IncludeForm {
    /// `#include "local.h"`, which looks next to the including file first.
    Quoted,
    /// `#include <stdio.h>`, which does not.
    Angled,
}

/// One directory on the search path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dir {
    /// The directory, as the user spelled it. Not canonicalised, because a diagnostic that
    /// says `../include/foo.h` is more use than one naming a path the user never typed.
    pub path: PathBuf,
    /// Whether headers found here are system headers, which suppresses warnings in them and
    /// sets the `3` flag on a `-E` line marker.
    pub is_system: bool,
}

/// A header that was found.
#[derive(Debug, Clone)]
pub struct Found {
    /// The path to open, which is the directory joined with the name as written.
    pub path: PathBuf,
    /// That path as a string, for the source map and for diagnostics.
    pub name: String,
    /// Whether it came from a system directory.
    pub is_system: bool,
    /// Where an `#include_next` written in this file should start looking.
    ///
    /// One past the entry this header came from, or zero for a header found next to the file
    /// that included it, because that directory is not on the path and there is nothing to
    /// continue past. Carrying the answer rather than the position is what keeps the two
    /// cases from being confused at the call site.
    pub next: usize,
    /// The contents.
    pub bytes: SourceBytes,
}

/// The directories a header is looked for in, in order.
///
/// GCC's order, because a different one produces header shadowing bugs that are miserable to
/// diagnose: `-iquote` first and only for a quoted include, then `-I`, then `-isystem`, then
/// the configured system directories, then `-idirafter`. The directory of the including file
/// comes before all of it for a quoted include, and it is not part of the numbered list
/// because `#include_next` must not be able to land back on it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SearchPath {
    dirs: Vec<Dir>,
    /// Where the `-I` directories begin, which is where an angled include starts looking.
    quote_end: usize,
    /// Where the `-isystem` and configured system directories begin.
    bracket_end: usize,
    /// Where the `-idirafter` directories begin.
    system_end: usize,
}

impl SearchPath {
    /// An empty search path.
    pub fn new() -> SearchPath {
        SearchPath::default()
    }

    /// Adds a `-iquote` directory, searched only for a quoted include.
    pub fn push_quote(&mut self, dir: impl Into<PathBuf>) {
        let at = self.quote_end;
        self.insert(at, dir.into(), false);
        self.quote_end += 1;
        self.bracket_end += 1;
        self.system_end += 1;
    }

    /// Adds a `-I` directory.
    pub fn push_bracket(&mut self, dir: impl Into<PathBuf>) {
        let at = self.bracket_end;
        self.insert(at, dir.into(), false);
        self.bracket_end += 1;
        self.system_end += 1;
    }

    /// Adds a `-isystem` directory, or one of the target's configured system directories.
    pub fn push_system(&mut self, dir: impl Into<PathBuf>) {
        let at = self.system_end;
        self.insert(at, dir.into(), true);
        self.system_end += 1;
    }

    /// Adds a `-idirafter` directory, which is searched after everything else.
    pub fn push_after(&mut self, dir: impl Into<PathBuf>) {
        let at = self.dirs.len();
        self.insert(at, dir.into(), true);
    }

    fn insert(&mut self, at: usize, path: PathBuf, is_system: bool) {
        self.dirs.insert(at, Dir { path, is_system });
    }

    /// Every directory, in search order.
    pub fn dirs(&self) -> &[Dir] {
        &self.dirs
    }

    /// The first entry an include of this form looks at.
    ///
    /// An angled include skips the `-iquote` directories, which is the only difference
    /// between the two chains once the including file's own directory is out of the way.
    pub fn start(&self, form: IncludeForm) -> usize {
        match form {
            IncludeForm::Quoted => 0,
            IncludeForm::Angled => self.quote_end,
        }
    }

    /// Finds `name`, starting at entry `from` of the search path.
    ///
    /// `relative_to` is the directory of the file doing the including, tried first for a
    /// quoted include and ignored otherwise. Pass `None` for an `#include_next`, which is
    /// defined as continuing past the directory the current file was found in and so must not
    /// look next to it again.
    ///
    /// An absolute name is opened directly and the search path is not consulted, which is
    /// what every C compiler does and what a generated header with an absolute path needs.
    pub fn resolve(
        &self,
        fs: &dyn FileSystem,
        name: &str,
        form: IncludeForm,
        relative_to: Option<&Path>,
        from: usize,
    ) -> Option<Found> {
        let as_path = Path::new(name);
        if is_absolute(as_path) {
            let bytes = open(fs, as_path).ok()?;
            return Some(Found {
                path: as_path.to_path_buf(),
                name: name.to_owned(),
                is_system: false,
                next: 0,
                bytes,
            });
        }
        if form == IncludeForm::Quoted {
            if let Some(dir) = relative_to {
                let path = dir.join(as_path);
                if let Ok(bytes) = open(fs, &path) {
                    return Some(Found {
                        name: display(&path),
                        path,
                        is_system: false,
                        // The including file's own directory is not an entry on the path, so
                        // an `#include_next` from a header found there starts at the top of
                        // the path rather than one past a position that does not exist.
                        next: 0,
                        bytes,
                    });
                }
            }
        }
        for (at, dir) in self.dirs.iter().enumerate().skip(from) {
            let path = dir.path.join(as_path);
            if let Ok(bytes) = open(fs, &path) {
                return Some(Found {
                    name: display(&path),
                    path,
                    is_system: dir.is_system,
                    next: at + 1,
                    bytes,
                });
            }
        }
        None
    }

    /// The directories a failed [`SearchPath::resolve`] with the same arguments looked in.
    ///
    /// `spec/05-preprocessor.md` section 5.7 makes printing this the required behaviour for a
    /// failed include, because "file not found" without the list of places that were tried is
    /// the diagnostic that wastes the most time in this part of the compiler.
    pub fn tried(
        &self,
        name: &str,
        form: IncludeForm,
        relative_to: Option<&Path>,
        from: usize,
    ) -> Vec<PathBuf> {
        if is_absolute(Path::new(name)) {
            return Vec::new();
        }
        let mut list = Vec::new();
        if form == IncludeForm::Quoted {
            if let Some(dir) = relative_to {
                list.push(dir.to_path_buf());
            }
        }
        list.extend(self.dirs.iter().skip(from).map(|d| d.path.clone()));
        list
    }
}

/// Reads a path the search produced, from the shipped headers first and the disk after.
///
/// This is the one place the compiler's own headers are handed out, and it is here rather
/// than in a [`FileSystem`] implementation on purpose. They are not files, they belong to
/// every implementation of the trait equally, and the only way to reach them is through a
/// search path entry spelled [`runtime::DIR`], which no real directory can be spelled as.
fn open(fs: &dyn FileSystem, path: &Path) -> io::Result<SourceBytes> {
    match runtime::read(path) {
        Some(bytes) => Ok(bytes),
        None => fs.read(path),
    }
}

/// A path as a string, lossily, because a diagnostic has to say something.
fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Whether an include name names a file outright rather than one to be searched for.
///
/// `Path::is_absolute` is false for `/usr/include/stdio.h` on Windows, because it has no
/// drive letter. A C file that says `#include "/usr/include/stdio.h"` means a path from the
/// root whatever host is compiling it, so a leading separator counts here as well.
fn is_absolute(path: &Path) -> bool {
    path.is_absolute() || path.has_root()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs_with(files: &[&str]) -> MemoryFileSystem {
        let mut fs = MemoryFileSystem::new();
        for f in files {
            fs.insert(*f, format!("/* {f} */\n").into_bytes());
        }
        fs
    }

    fn text(found: &Found) -> String {
        String::from_utf8_lossy(found.bytes.as_slice()).into_owned()
    }

    /// A path with forward slashes, because `Path::join` uses a backslash on Windows and
    /// these tests are about the search order rather than about separators.
    fn norm(path: &str) -> String {
        path.replace('\\', "/")
    }

    #[test]
    fn a_missing_file_is_not_found_rather_than_an_error() {
        let fs = MemoryFileSystem::new();
        let kind = fs.read(Path::new("/nope.h")).err().map(|e| e.kind());
        assert_eq!(kind, Some(io::ErrorKind::NotFound));
        assert!(fs.is_empty());
    }

    #[test]
    fn quote_directories_are_invisible_to_an_angled_include() {
        let fs = fs_with(&["/q/a.h", "/i/a.h"]);
        let mut search = SearchPath::new();
        search.push_quote("/q");
        search.push_bracket("/i");

        let quoted = search.resolve(&fs, "a.h", IncludeForm::Quoted, None, 0).unwrap();
        assert_eq!(norm(&quoted.name), "/q/a.h");
        let angled = search
            .resolve(&fs, "a.h", IncludeForm::Angled, None, search.start(IncludeForm::Angled))
            .unwrap();
        assert_eq!(norm(&angled.name), "/i/a.h");
    }

    #[test]
    fn the_including_files_own_directory_comes_first_for_a_quoted_include() {
        let fs = fs_with(&["/src/a.h", "/i/a.h"]);
        let mut search = SearchPath::new();
        search.push_bracket("/i");
        let here = Path::new("/src");

        let quoted = search.resolve(&fs, "a.h", IncludeForm::Quoted, Some(here), 0).unwrap();
        assert_eq!(norm(&quoted.name), "/src/a.h");
        // An angled include does not look there, even though it was passed.
        let angled = search.resolve(&fs, "a.h", IncludeForm::Angled, Some(here), 0).unwrap();
        assert_eq!(norm(&angled.name), "/i/a.h");
    }

    #[test]
    fn the_order_is_iquote_then_i_then_isystem_then_idirafter() {
        let fs = fs_with(&["/after/a.h", "/sys/a.h", "/i/a.h", "/q/a.h"]);
        let mut search = SearchPath::new();
        // Pushed in an order that is not the search order, because a driver reads the command
        // line left to right and the groups interleave.
        search.push_after("/after");
        search.push_system("/sys");
        search.push_bracket("/i");
        search.push_quote("/q");
        let order: Vec<_> = search.dirs().iter().map(|d| norm(&d.path.to_string_lossy())).collect();
        assert_eq!(order, ["/q", "/i", "/sys", "/after"]);

        let found = search.resolve(&fs, "a.h", IncludeForm::Quoted, None, 0).unwrap();
        assert_eq!(norm(&found.name), "/q/a.h");
        assert_eq!(found.next, 1);
    }

    #[test]
    fn a_system_directory_marks_what_it_holds_as_a_system_header() {
        let fs = fs_with(&["/i/a.h", "/sys/b.h", "/after/c.h"]);
        let mut search = SearchPath::new();
        search.push_bracket("/i");
        search.push_system("/sys");
        search.push_after("/after");
        let get = |n| search.resolve(&fs, n, IncludeForm::Angled, None, 0).unwrap();
        assert!(!get("a.h").is_system);
        assert!(get("b.h").is_system);
        assert!(get("c.h").is_system);
    }

    #[test]
    fn include_next_continues_past_the_directory_the_current_file_came_from() {
        let fs = fs_with(&["/a/limits.h", "/b/limits.h", "/c/limits.h"]);
        let mut search = SearchPath::new();
        search.push_bracket("/a");
        search.push_bracket("/b");
        search.push_bracket("/c");

        let first = search.resolve(&fs, "limits.h", IncludeForm::Angled, None, 0).unwrap();
        assert_eq!(norm(&first.name), "/a/limits.h");
        let second =
            search.resolve(&fs, "limits.h", IncludeForm::Angled, None, first.next).unwrap();
        assert_eq!(norm(&second.name), "/b/limits.h");
        let third =
            search.resolve(&fs, "limits.h", IncludeForm::Angled, None, second.next).unwrap();
        assert_eq!(norm(&third.name), "/c/limits.h");
        assert!(search.resolve(&fs, "limits.h", IncludeForm::Angled, None, third.next).is_none());
    }

    #[test]
    fn a_name_with_a_directory_in_it_is_joined_onto_each_entry() {
        let fs = fs_with(&["/i/sys/types.h"]);
        let mut search = SearchPath::new();
        search.push_bracket("/i");
        let found = search.resolve(&fs, "sys/types.h", IncludeForm::Angled, None, 0).unwrap();
        assert_eq!(norm(&found.name), "/i/sys/types.h");
        assert_eq!(text(&found), "/* /i/sys/types.h */\n");
    }

    #[test]
    fn an_absolute_name_ignores_the_search_path() {
        let fs = fs_with(&["/gen/config.h", "/i/gen/config.h"]);
        let mut search = SearchPath::new();
        search.push_bracket("/i");
        let found = search.resolve(&fs, "/gen/config.h", IncludeForm::Angled, None, 0).unwrap();
        assert_eq!(norm(&found.name), "/gen/config.h");
        assert!(search.tried("/gen/config.h", IncludeForm::Angled, None, 0).is_empty());
    }

    #[test]
    fn the_list_of_places_tried_is_the_list_that_was_searched() {
        let fs = MemoryFileSystem::new();
        let mut search = SearchPath::new();
        search.push_quote("/q");
        search.push_bracket("/i");
        search.push_system("/sys");
        let here = Path::new("/src");

        assert!(search.resolve(&fs, "a.h", IncludeForm::Quoted, Some(here), 0).is_none());
        let tried = search.tried("a.h", IncludeForm::Quoted, Some(here), 0);
        let tried: Vec<_> = tried.iter().map(|p| norm(&p.to_string_lossy())).collect();
        assert_eq!(tried, ["/src", "/q", "/i", "/sys"]);

        let start = search.start(IncludeForm::Angled);
        let tried = search.tried("a.h", IncludeForm::Angled, Some(here), start);
        let tried: Vec<_> = tried.iter().map(|p| norm(&p.to_string_lossy())).collect();
        assert_eq!(tried, ["/i", "/sys"]);
    }

    #[test]
    fn a_header_found_next_to_its_includer_does_not_skip_the_whole_path_afterwards() {
        // `at` for a file found beside its includer has to leave `at + 1` at the top of the
        // path, because the directory it was found in is not on the path at all.
        let fs = fs_with(&["/src/a.h", "/i/b.h"]);
        let mut search = SearchPath::new();
        search.push_bracket("/i");
        let found =
            search.resolve(&fs, "a.h", IncludeForm::Quoted, Some(Path::new("/src")), 0).unwrap();
        assert_eq!(found.next, 0);
        let next = search.resolve(&fs, "b.h", IncludeForm::Angled, None, found.next).unwrap();
        assert_eq!(norm(&next.name), "/i/b.h");
    }
}
