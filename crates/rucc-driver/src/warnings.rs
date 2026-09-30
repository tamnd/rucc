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

/// The list itself, kept in `data/gcc-warnings.txt` with the command that made it, so that moving
/// to a newer gcc is running that command again rather than editing this file.
const LIST: &str = include_str!("../data/gcc-warnings.txt");

/// Every name in the list, in its order, which is sorted.
fn names() -> impl Iterator<Item = &'static str> {
    LIST.lines().filter(|line| !line.is_empty() && !line.starts_with('#'))
}

/// Names gcc takes with a number glued on, such as `-Wlarger-than-100`.
const PREFIXES: &[&str] = &["larger-than-"];

/// Whether gcc 16 knows `-W<name>`, where `name` is what follows the `-W`. A value after `=` is not
/// checked, so `-Wformat=9` is known here where gcc refuses the number.
pub(crate) fn known(name: &str) -> bool {
    let name = name.split_once('=').map_or(name, |(name, _)| name);
    names().any(|known| known == name) || PREFIXES.iter().any(|prefix| name.starts_with(prefix))
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
