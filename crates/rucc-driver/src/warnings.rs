//! The warning names gcc 16 knows, which is what decides whether a `-W` flag is accepted.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.1.
//!
//! This compiler has no warning groups of its own yet (#485), so a `-W` flag it accepts turns
//! nothing on. Turning a warning off does work for the ones `rucc_diag::option_of` names, as do
//! `-Werror=` and `-Wno-error=`. Either way the flag has to get the answer gcc gives, because
//! configure scripts and meson find out whether a warning exists by passing it and reading the exit
//! status. Accepting every name
//! means a build probed with rucc passes flags that gcc refuses, such as clang's
//! `-Wcast-function-type-strict`, and ends up with a different command line from the gcc build.
//! Refusing a name gcc knows is worse, because it fails a configure script written for gcc.
//!
//! The list is every `-W` name `gcc-16 --help=...` prints across the warning, common, language and
//! undocumented classes, with the value after `=` dropped. It includes the C++, Objective-C and
//! Fortran ones, which gcc accepts on a C compile with a warning and a zero exit status. It lives in
//! `data/gcc-warnings.txt` beside the command that printed it.
//!
//! A build that claims an older gcc with `-fgnuc-version=` gets that gcc's answer instead, so a
//! name gcc 14 does not know is refused under a claim of 14 or older. Those names are in
//! `data/gcc-warnings-after-14.txt`, made the same way with gcc 14.2. The kernel is why: it probes
//! `-Wunterminated-string-initialization` and keeps `-Wno-` of it when the probe passes, so a
//! build claiming gcc 14 that passes the probe has a different command line from the gcc 14 build.

/// The list itself, kept in `data/gcc-warnings.txt` with the command that made it, so that moving
/// to a newer gcc is running that command again rather than editing this file.
const LIST: &str = include_str!("../data/gcc-warnings.txt");

/// The names in the list that gcc 14.2 does not know, kept the same way.
const AFTER_14: &str = include_str!("../data/gcc-warnings-after-14.txt");

/// Every name in a list, in its order, which is sorted.
fn read(list: &'static str) -> impl Iterator<Item = &'static str> {
    list.lines().filter(|line| !line.is_empty() && !line.starts_with('#'))
}

/// Every name gcc 16 knows.
fn names() -> impl Iterator<Item = &'static str> {
    read(LIST)
}

/// Names gcc takes with a number glued on, such as `-Wlarger-than-100`.
const PREFIXES: &[&str] = &["larger-than-"];

/// Whether gcc 16 knows `-W<name>`, where `name` is what follows the `-W`. A value after `=` is not
/// checked, so `-Wformat=9` is known here where gcc refuses the number.
pub(crate) fn known(name: &str) -> bool {
    let name = name.split_once('=').map_or(name, |(name, _)| name);
    names().any(|known| known == name) || PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

/// The first gcc major release that knows `-W<name>`, for a name [`known`] says yes to. It is 15
/// for a name gcc 14.2 does not know, which is the one older release the list was made against,
/// and 0 for the rest.
pub(crate) fn since(name: &str) -> u32 {
    let name = name.split_once('=').map_or(name, |(name, _)| name);
    if read(AFTER_14).any(|newer| newer == name) { 15 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_is_sorted_and_whole() {
        let names: Vec<&str> = names().collect();
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]), "sorted and without repeats");
        assert!(names.len() > 500, "the whole of gcc's list, not part of it");
    }

    #[test]
    fn the_names_gcc_14_lacks_are_sorted_and_all_known_to_gcc_16() {
        let newer: Vec<&str> = read(AFTER_14).collect();
        assert!(newer.windows(2).all(|pair| pair[0] < pair[1]), "sorted and without repeats");
        assert!(newer.iter().all(|name| known(name)), "every one is in the gcc 16 list");
        assert_eq!(since("unterminated-string-initialization"), 15);
        assert_eq!(since("header-guard"), 15);
        assert_eq!(since("all"), 0);
        assert_eq!(since("format=2"), 0);
    }

    #[test]
    fn a_name_gcc_knows_is_known_in_each_of_its_spellings() {
        for name in [
            "all",
            "extra",
            "format=2",
            "cast-align=strict",
            "abi-tag",
            "larger-than-100",
            "no-frame-larger-than",
        ] {
            assert!(known(name), "{name}");
        }
    }

    #[test]
    fn a_name_only_clang_knows_is_not() {
        for name in [
            "cast-function-type-strict",
            "compound-token-split-by-macro",
            "unguarded-availability-new",
            "unused-command-line-argument",
        ] {
            assert!(!known(name), "{name}");
        }
    }
}
