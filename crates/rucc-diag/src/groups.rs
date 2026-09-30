//! What the command line said about each gcc warning group.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.1, and #485.
//!
//! gcc puts most of its warnings in a named group, and four spellings act on one: `-Wname` turns
//! it on, `-Wno-name` turns it off, `-Werror=name` turns it on and makes it fatal, and
//! `-Wno-error=name` keeps it a warning under `-Werror`. The last argument about a group wins, for
//! being on and for being fatal separately, so `-Werror=x -Wno-x` is off and
//! `-Werror -Wno-error=x` fails the build on everything except `x`. A build that asks for
//! `-Werror -Wno-deprecated-declarations` is the common case this is for: without it the one
//! warning the project decided to live with is the one that stops the build.
//!
//! Every group a warning in this compiler belongs to is on unless the command line turned it off,
//! which is what the compiler did before it had groups at all. The umbrellas, `-Wall`, `-Wextra`
//! and `-Wpedantic`, only matter for a group that starts off, and there is none yet.

use crate::{Diagnostic, Severity};

/// The state of every group the command line named, in the order it named them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WarningGroups {
    /// Each group named, with whether it was last turned on or off and whether it was last made
    /// fatal or kept a warning. `None` is a side of it nothing said anything about.
    named: Vec<(String, Option<bool>, Option<bool>)>,
}

impl WarningGroups {
    /// Nothing said about any group.
    pub fn new() -> Self {
        Self::default()
    }

    fn entry(&mut self, group: &str) -> &mut (String, Option<bool>, Option<bool>) {
        let at = match self.named.iter().position(|(name, ..)| name == group) {
            Some(at) => at,
            None => {
                self.named.push((group.to_owned(), None, None));
                self.named.len() - 1
            }
        };
        &mut self.named[at]
    }

    /// Reads one argument of the `-W` family, as what follows the `-W`, and says whether it was
    /// about a group. `error` and `no-error` on their own are about every warning at once and are
    /// not read here. A value after `=`, as in `format=2`, is not part of the name.
    pub fn read(&mut self, arg: &str) -> bool {
        let name = |rest: &str| rest.split_once('=').map_or(rest, |(name, _)| name).to_owned();
        if let Some(rest) = arg.strip_prefix("error=") {
            let entry = self.entry(&name(rest));
            entry.1 = Some(true);
            entry.2 = Some(true);
        } else if let Some(rest) = arg.strip_prefix("no-error=") {
            self.entry(&name(rest)).2 = Some(false);
        } else if arg == "error" || arg == "no-error" || arg.is_empty() {
            return false;
        } else if let Some(rest) = arg.strip_prefix("no-") {
            self.entry(&name(rest)).1 = Some(false);
        } else {
            self.entry(&name(arg)).1 = Some(true);
        }
        true
    }

    fn lookup(&self, group: &str) -> Option<&(String, Option<bool>, Option<bool>)> {
        self.named.iter().find(|(name, ..)| name == group)
    }

    /// Whether a warning in that group is to be dropped.
    pub fn is_off(&self, group: &str) -> bool {
        self.lookup(group).is_some_and(|(_, on, _)| *on == Some(false))
    }

    /// Whether a warning in that group fails the build, given whether `-Werror` was on.
    pub fn is_fatal(&self, group: &str, all: bool) -> bool {
        self.lookup(group).and_then(|(.., fatal)| *fatal).unwrap_or(all)
    }
}

/// Whether a warning that was not dropped is to be reported as an error, which is `-Werror` for
/// a warning in no group and what the group's own state says for one in a group.
pub fn promoted(diag: &Diagnostic, groups: &WarningGroups, warnings_are_errors: bool) -> bool {
    if diag.severity != Severity::Warning {
        return false;
    }
    match diag.group {
        Some(group) => groups.is_fatal(group, warnings_are_errors),
        None => warnings_are_errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Span;

    fn groups(args: &[&str]) -> WarningGroups {
        let mut groups = WarningGroups::new();
        for arg in args {
            groups.read(arg);
        }
        groups
    }

    #[test]
    fn the_last_word_about_a_group_wins() {
        assert!(groups(&["no-attributes"]).is_off("attributes"));
        assert!(!groups(&["no-attributes", "attributes"]).is_off("attributes"));
        assert!(groups(&["error=attributes", "no-attributes"]).is_off("attributes"));
        assert!(!groups(&["no-attributes"]).is_off("overflow"));
    }

    #[test]
    fn being_fatal_is_kept_apart_from_being_on() {
        let g = groups(&["no-error=attributes"]);
        assert!(!g.is_fatal("attributes", true));
        assert!(g.is_fatal("overflow", true));
        let g = groups(&["error=attributes"]);
        assert!(g.is_fatal("attributes", false));
        assert!(!g.is_fatal("overflow", false));
        // Turning a fatal group off and on again does not make it a warning.
        assert!(groups(&["error=x", "no-x", "x"]).is_fatal("x", false));
    }

    #[test]
    fn a_value_after_the_name_is_not_part_of_it_and_the_whole_family_is_not_a_group() {
        let mut g = WarningGroups::new();
        assert!(g.read("no-format=2"));
        assert!(g.is_off("format"));
        assert!(!g.read("error"));
        assert!(!g.read("no-error"));
    }

    #[test]
    fn a_warning_in_no_group_is_promoted_by_werror_alone() {
        let g = groups(&["no-error=attributes"]);
        let plain = Diagnostic::warning("w", Span::DUMMY);
        let grouped = Diagnostic::warning("w", Span::DUMMY).in_group("attributes");
        assert!(promoted(&plain, &g, true));
        assert!(!promoted(&grouped, &g, true));
        assert!(!promoted(&Diagnostic::error("e", Span::DUMMY), &g, true));
    }
}
