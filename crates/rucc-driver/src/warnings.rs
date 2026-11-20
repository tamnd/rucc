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
//!
//! The same goes for the releases the BSDs build with: NetBSD 10 claims gcc 10.5.0 and NetBSD 11
//! claims gcc 12.5.0, and their trees probe and pass warning names by the in-tree gcc's version.
//! `data/gcc-warnings-after-10.txt`, `-after-12.txt` and `-after-13.txt` are the names gcc 10.5,
//! 12.5 and 13.3 do not know, made the same way. Each list holds the next one.

/// The list itself, kept in `data/gcc-warnings.txt` with the command that made it, so that moving
/// to a newer gcc is running that command again rather than editing this file.
const LIST: &str = include_str!("../data/gcc-warnings.txt");

/// The names in the list that gcc 14.2 does not know, kept the same way.
const AFTER_14: &str = include_str!("../data/gcc-warnings-after-14.txt");

/// The names in the list that gcc 13.3 does not know.
const AFTER_13: &str = include_str!("../data/gcc-warnings-after-13.txt");

/// The names in the list that gcc 12.5 does not know.
const AFTER_12: &str = include_str!("../data/gcc-warnings-after-12.txt");

/// The names in the list that gcc 10.5 does not know.
const AFTER_10: &str = include_str!("../data/gcc-warnings-after-10.txt");

/// Each list of names an older release does not know, newest first, with the first release that
/// knows them all. A name that came between two of the releases measured gets the oldest release
/// after the one that lacks it: one gcc 10.5 lacks and gcc 12.5 knows is 11, though it may have
/// come with 12.
const AFTER: &[(&str, u32)] = &[(AFTER_14, 15), (AFTER_13, 14), (AFTER_12, 13), (AFTER_10, 11)];

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

/// The first gcc major release that knows `-W<name>`, for a name [`known`] says yes to: 15 for a
/// name gcc 14.2 does not know, 14 for one gcc 13.3 does not, 13 for one gcc 12.5 does not, 11 for
/// one gcc 10.5 does not, and 0 for the rest.
pub(crate) fn since(name: &str) -> u32 {
    let name = name.split_once('=').map_or(name, |(name, _)| name);
    let lacked = |list: &'static str| read(list).any(|newer| newer == name);
    AFTER.iter().find(|&&(list, _)| lacked(list)).map_or(0, |&(_, since)| since)
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

    /// Each older list is sorted, is in the gcc 16 list, and holds every name of the newer one,
    /// since a name gcc 13 lacks is one gcc 12 and gcc 10 lack too.
    #[test]
    fn each_older_list_holds_the_next_one() {
        for pair in AFTER.windows(2) {
            let (newer, older) = (pair[0].0, pair[1].0);
            let names: Vec<&str> = read(older).collect();
            assert!(names.windows(2).all(|pair| pair[0] < pair[1]), "sorted, {}", pair[1].1);
            assert!(names.iter().all(|name| known(name)), "in the gcc 16 list, {}", pair[1].1);
            assert!(
                read(newer).all(|name| names.contains(&name)),
                "{} holds {}",
                pair[1].1,
                pair[0].1
            );
        }
    }

    /// The release that brought a name, from the lists of the releases the BSDs build with.
    #[test]
    fn a_name_is_dated_by_the_oldest_release_that_knows_it() {
        assert_eq!(since("unterminated-string-initialization"), 15);
        assert_eq!(since("return-mismatch"), 14);
        assert_eq!(since("compare-distinct-pointer-types"), 14);
        assert_eq!(since("xor-used-as-pow"), 13);
        assert_eq!(since("invalid-utf8"), 13);
        assert_eq!(since("use-after-free"), 11);
        assert_eq!(since("dangling-pointer=2"), 11);
        assert_eq!(since("array-parameter"), 11);
        assert_eq!(since("stringop-overflow"), 0);
        assert_eq!(since("implicit-fallthrough=3"), 0);
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
