//! Where the sysroots and the other generated things are kept.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.2, which names the directory and the
//! variables that move it.
//!
//! This is the one place that answers the question, and it is here rather than in `rucc-sysroot`
//! because answering it means reading the environment. `rucc-sysroot` reads nothing: that is what
//! makes a link line a function of its arguments and what
//! `spec/cross-compile/02-the-goal.md` claim 5 rests on. So the cache directory is resolved once,
//! here, and handed down as a path, and the crate below stays a function of what it is given.
//!
//! Nothing here creates a directory or looks to see whether one is there. The answer is where a
//! sysroot for a target would be, which is a question that has an answer before anything has been
//! downloaded, and saying so is what lets a diagnostic name the directory that is missing.

use std::path::PathBuf;

/// The cache directory, from the environment.
///
/// `RUCC_CACHE_DIR` first, because somebody who set it meant it. Then the platform's own place for
/// a cache that a user can delete without losing anything: `XDG_CACHE_HOME` or `~/.cache` on a
/// Unix, `LOCALAPPDATA` on Windows, which is where a Windows program is expected to put this and
/// not where `XDG_CACHE_HOME` would put it.
///
/// A temporary directory is the last answer rather than a failure. A machine with no home directory
/// is a build container, and a build container that cannot link because nothing set `HOME` is worse
/// than one that downloads a sysroot again on its next run.
#[must_use]
pub fn dir() -> PathBuf {
    resolve(|name| std::env::var(name).ok())
}

/// The same answer from a function that says what the environment holds.
///
/// Split out so that the four cases are testable on one machine. A test that set the real
/// environment would be a test that changed what the rest of the process sees, and these run in
/// threads.
fn resolve(var: impl Fn(&str) -> Option<String>) -> PathBuf {
    // An empty value is a variable nobody set rather than a request to put the cache at the root of
    // the filesystem, which is what `RUCC_CACHE_DIR=` in a makefile would otherwise mean.
    let var = |name: &str| var(name).filter(|value| !value.is_empty());
    if let Some(set) = var("RUCC_CACHE_DIR") {
        return PathBuf::from(set);
    }
    if cfg!(windows) {
        if let Some(local) = var("LOCALAPPDATA") {
            return PathBuf::from(local).join("rucc").join("cache");
        }
    }
    if let Some(xdg) = var("XDG_CACHE_HOME") {
        return PathBuf::from(xdg).join("rucc");
    }
    if let Some(home) = var("HOME") {
        return PathBuf::from(home).join(".cache").join("rucc");
    }
    std::env::temp_dir().join("rucc")
}

#[cfg(test)]
mod tests {
    use super::resolve;
    use std::path::PathBuf;

    /// An environment holding exactly these pairs.
    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| (*value).to_owned())
    }

    #[test]
    fn the_variable_that_names_it_outright_wins() {
        let dir = resolve(env(&[
            ("RUCC_CACHE_DIR", "/build/cache"),
            ("XDG_CACHE_HOME", "/home/a/.cache"),
            ("HOME", "/home/a"),
        ]));
        assert_eq!(dir, PathBuf::from("/build/cache"));
    }

    #[test]
    fn then_the_platforms_own_place_for_a_cache() {
        let dir = resolve(env(&[("XDG_CACHE_HOME", "/home/a/.cache"), ("HOME", "/home/a")]));
        assert_eq!(dir, PathBuf::from("/home/a/.cache/rucc"));
    }

    #[test]
    #[cfg(windows)]
    fn on_windows_it_is_where_a_windows_program_keeps_a_cache() {
        // Not `~/.cache`, which is a Unix convention, and not the roaming profile either, because a
        // cache that is copied between machines by a domain policy is a cache nobody wanted.
        let dir = resolve(env(&[("LOCALAPPDATA", r"C:\Users\a\AppData\Local")]));
        assert_eq!(dir, PathBuf::from(r"C:\Users\a\AppData\Local\rucc\cache"));
    }

    #[test]
    fn then_the_home_directory() {
        let dir = resolve(env(&[("HOME", "/home/a")]));
        assert_eq!(dir, PathBuf::from("/home/a/.cache/rucc"));
    }

    #[test]
    fn and_a_machine_with_nothing_set_still_gets_an_answer() {
        // A build container with no `HOME`. It links, and it downloads again next time, which is
        // the right way round for the two costs.
        let dir = resolve(env(&[]));
        assert!(dir.ends_with("rucc"), "{}", dir.display());
        assert!(dir.is_absolute(), "{}", dir.display());
    }

    #[test]
    fn an_empty_variable_is_not_an_answer() {
        // `RUCC_CACHE_DIR=` is what a makefile that forwards a variable it was not given looks
        // like, and taking it literally would put the cache at the root of the filesystem.
        let dir = resolve(env(&[("RUCC_CACHE_DIR", ""), ("HOME", "/home/a")]));
        assert_eq!(dir, PathBuf::from("/home/a/.cache/rucc"));
    }
}
