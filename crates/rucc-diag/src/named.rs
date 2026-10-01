//! The gcc option each warning answers to, and what the command line said about those options.
//!
//! gcc prints the option that controls a warning in brackets after it, as in `[-Wpointer-sign]`,
//! and that name is what `-Wno-pointer-sign`, `-Werror=pointer-sign` and `-Wno-error=pointer-sign`
//! refer to. A build that turns a warning off expects not to hear it, and under `-Werror` a warning
//! it turned off and heard anyway stops the build. The Linux kernel is one such build: it passes
//! `-Wno-pointer-sign` along with `-Werror` and has plenty of code that mixes `char *` with
//! `unsigned char *`.
//!
//! The table goes from this compiler's code to gcc's name rather than the other way, because the
//! code is what a diagnostic carries. A code that is not in it answers to no option, so no `-W`
//! flag touches it, which is also what gcc does with a warning it prints no bracket after.

use std::collections::{BTreeMap, BTreeSet};

use crate::{Diagnostic, Severity};

/// Each warning code and the gcc option that controls it, without the `-W`, sorted by code.
const OPTIONS: &[(&str, &str)] = &[
    ("E0408", "pedantic"),
    ("E0411", "attributes"),
    ("E0412", "old-style-definition"),
    ("E0413", "pedantic"),
    ("E0414", "pedantic"),
    ("E0415", "pedantic"),
    ("E0512", "incompatible-pointer-types"),
    ("E0513", "int-conversion"),
    ("E0514", "discarded-qualifiers"),
    ("E0522", "shift-count-negative"),
    ("E0523", "shift-count-overflow"),
    ("E0524", "overflow"),
    ("E0526", "implicit-int"),
    ("E0567", "pointer-to-int-cast"),
    ("E0568", "int-to-pointer-cast"),
    ("E0572", "pointer-arith"),
    ("E0625", "switch-outside-range"),
    ("E0633", "return-type"),
    ("E0634", "return-type"),
    ("E0664", "varargs"),
    ("E0673", "pointer-sign"),
    ("E0677", "pragmas"),
    ("E0678", "pragmas"),
    ("E0679", "pragmas"),
    ("E0680", "pragmas"),
    ("E0681", "pragmas"),
    ("E0691", "pragmas"),
    ("E0703", "attributes"),
    ("E0707", "attributes"),
    ("E0708", "attributes"),
    ("E0713", "builtin-declaration-mismatch"),
    ("E0745", "unknown-pragmas"),
    ("E0746", "attributes"),
    ("E0747", "format"),
    ("E0750", "attributes"),
    ("E0751", "attributes"),
    ("E0752", "attribute-warning"),
    ("E0768", "compare-distinct-pointer-types"),
    ("E0769", "prio-ctor-dtor"),
    ("E0770", "deprecated-declarations"),
    ("E0771", "unused-result"),
    ("E0784", "attributes"),
    ("E0785", "format"),
    ("E0786", "format-extra-args"),
    ("E0787", "format-contains-nul"),
    ("E0788", "format-zero-length"),
    ("E0789", "format"),
    ("E0790", "designated-init"),
    ("W0331", "cpp"),
    ("W0333", "invalid-memory-model"),
    ("W0334", "expansion-to-defined"),
];

/// The options gcc raises through what it calls a pedwarn, which is the diagnostic the standard
/// requires, and so the only ones `-pedantic-errors` turns into errors. Sorted, and checked
/// against gcc 16.2.0 with `-pedantic-errors` one option at a time.
///
/// Everything else in [`OPTIONS`] is an ordinary warning that happens to be on by default, and
/// gcc leaves it a warning under `-pedantic-errors`: a call to a deprecated function, a result
/// thrown away, a `#warning`, a shift count out of range, a reserved constructor priority. The
/// list is of the ones that are promoted rather than the ones that are not, so that a warning
/// added to the table later stays a warning under `-pedantic-errors` until somebody checks.
const PEDWARNS: &[&str] = &[
    "compare-distinct-pointer-types",
    "discarded-qualifiers",
    "expansion-to-defined",
    "implicit-int",
    "incompatible-pointer-types",
    "int-conversion",
    "pedantic",
    "pointer-arith",
    "pointer-sign",
    "return-type",
];

/// The options gcc leaves off until something asks for them, each with the group that turns it
/// on. Sorted by option.
///
/// The format checks are the ones here. `-Wformat` turns on all four, `-Wall` turns on
/// `-Wformat`, and so does `-Wformat=` with any level but nought, which is gcc 16's reading. A
/// name turned off by itself stays off whatever group is asked for later, which is how gcc reads
/// `-Wno-format-extra-args -Wall`. `-Wall` turns on nothing else yet, which is #485.
const QUIET: &[(&str, &str)] = &[
    ("format", "format"),
    ("format-contains-nul", "format"),
    ("format-extra-args", "format"),
    ("format-zero-length", "format"),
];

/// The gcc option that controls the warning with this code, if it has one.
pub fn option_of(code: &str) -> Option<&'static str> {
    OPTIONS.binary_search_by(|&(known, _)| known.cmp(code)).ok().map(|at| OPTIONS[at].1)
}

/// What the command line said about warnings by name.
///
/// Each flag overrides what an earlier one said about the same name, which is how gcc reads
/// `-Wno-pointer-sign -Wpointer-sign`. `-Werror=` of a name turns the warning on as well as
/// making it an error, and `-Wno-error=` of one leaves it on and only stops `-Werror` from
/// promoting it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Named {
    off: BTreeSet<String>,
    /// The names turned on by a flag, which only matters for the ones in [`QUIET`].
    on: BTreeSet<String>,
    /// Whether `-Wall` was given, which turns on the groups it covers unless they were turned off
    /// by name.
    all: bool,
    errors: BTreeMap<String, bool>,
    pedantic_errors: bool,
}

impl Named {
    /// Reads what follows `-W` in a flag. Anything that is not about one warning by name, such as
    /// `all`, is recorded the same way and simply matches no code.
    pub fn flag(&mut self, name: &str) {
        // `-Wformat=2` is `-Wformat` and more checks this compiler does not make, and
        // `-Wformat=0` is `-Wno-format`.
        let name = match name.split_once('=') {
            Some(("format", "0")) => "no-format",
            Some(("format", _)) => "format",
            _ => name,
        };
        if let Some(name) = name.strip_prefix("no-error=") {
            self.errors.insert(name.to_owned(), false);
        } else if let Some(name) = name.strip_prefix("error=") {
            self.errors.insert(name.to_owned(), true);
            self.off.remove(name);
            self.on.insert(name.to_owned());
        } else if name == "no-all" {
            self.all = false;
        } else if let Some(name) = name.strip_prefix("no-") {
            self.off.insert(name.to_owned());
            self.on.remove(name);
        } else if name == "all" {
            self.all = true;
        } else {
            self.off.remove(name);
            self.on.insert(name.to_owned());
        }
    }

    /// Records `-pedantic-errors`, which promotes the warnings the standard requires and no
    /// others.
    ///
    /// It is not `-Werror` with `-pedantic` on top, which is what it used to be taken for here: a
    /// program calling a function marked `[[deprecated]]` is one gcc builds under
    /// `-std=c23 -pedantic-errors` with a warning, and one that `-Werror` stops.
    pub fn pedantic_errors(&mut self) {
        self.pedantic_errors = true;
    }

    /// Whether this is a warning the command line turned off by name.
    pub fn silenced(&self, diag: &Diagnostic) -> bool {
        diag.severity == Severity::Warning
            && self.name(diag).is_some_and(|name| self.off.contains(name) || !self.asked(name))
    }

    /// Whether a warning that is off until asked for was asked for, which every other warning
    /// is. The name has to be asked for itself or through its group, and the group must not have
    /// been turned off, since `-Wno-format` silences `-Wformat-extra-args` along with the rest.
    fn asked(&self, name: &str) -> bool {
        let Ok(at) = QUIET.binary_search_by(|&(known, _)| known.cmp(name)) else { return true };
        let group = QUIET[at].1;
        if self.off.contains(group) {
            return false;
        }
        self.on.contains(name) || self.on.contains(group) || self.all
    }

    /// Whether this is a warning to report as an error, given whether `-Werror` was passed.
    ///
    /// Under `-pedantic-errors` a warning is promoted when its option is one of the [`PEDWARNS`],
    /// and also when it answers to no option at all, since the warnings with no bracket after
    /// them are the pedantic ones this compiler raises without naming.
    pub fn promoted(&self, diag: &Diagnostic, warnings_are_errors: bool) -> bool {
        if diag.severity != Severity::Warning {
            return false;
        }
        let name = self.name(diag);
        match name.and_then(|name| self.errors.get(name)) {
            Some(&error) => error,
            None => {
                warnings_are_errors
                    || (self.pedantic_errors
                        && name.is_none_or(|name| PEDWARNS.binary_search(&name).is_ok()))
            }
        }
    }

    fn name(&self, diag: &Diagnostic) -> Option<&'static str> {
        diag.code.and_then(option_of)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Span;

    fn warning(code: &'static str) -> Diagnostic {
        Diagnostic::warning("pointer targets differ in signedness".to_owned(), Span::DUMMY)
            .with_code(code)
    }

    /// `-pedantic-errors` stops a build over what the standard requires and nothing else, so a
    /// deprecated call and a thrown-away result stay warnings while a pointer mixed with an
    /// integer does not. `-Werror=` of a name still wins over both.
    #[test]
    fn pedantic_errors_promotes_only_what_gcc_raises_as_a_pedwarn() {
        let mut named = Named::default();
        named.pedantic_errors();
        assert!(named.promoted(&warning("E0513"), false));
        assert!(named.promoted(&warning("E0408"), false));
        assert!(named.promoted(&warning("E0999"), false));
        assert!(!named.promoted(&warning("E0770"), false));
        assert!(!named.promoted(&warning("E0771"), false));
        assert!(!named.promoted(&warning("W0331"), false));
        assert!(named.promoted(&warning("E0770"), true));
        named.flag("error=deprecated-declarations");
        assert!(named.promoted(&warning("E0770"), false));
        named.flag("no-error=int-conversion");
        assert!(!named.promoted(&warning("E0513"), false));
        assert!(PEDWARNS.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(PEDWARNS.iter().all(|name| OPTIONS.iter().any(|&(_, known)| known == *name)));
    }

    #[test]
    fn the_table_is_sorted_so_a_code_can_be_found() {
        assert!(OPTIONS.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(option_of("E0673"), Some("pointer-sign"));
        assert_eq!(option_of("E0001"), None);
    }

    /// A code names one option, so two warnings gcc files under different names cannot share
    /// one. The reserved constructor priority is `prio-ctor-dtor` rather than `attributes`, and
    /// comparing pointers to distinct types is an option of its own where comparing a pointer
    /// with an integer answers to none.
    #[test]
    fn warnings_gcc_files_apart_have_codes_of_their_own() {
        assert_eq!(option_of("E0703"), Some("attributes"));
        assert_eq!(option_of("E0769"), Some("prio-ctor-dtor"));
        assert_eq!(option_of("E0768"), Some("compare-distinct-pointer-types"));
        assert_eq!(option_of("E0517"), None);
    }

    #[test]
    fn the_last_flag_about_a_name_is_the_one_that_counts() {
        let mut named = Named::default();
        named.flag("no-pointer-sign");
        assert!(named.silenced(&warning("E0673")));
        named.flag("pointer-sign");
        assert!(!named.silenced(&warning("E0673")));
        named.flag("no-pointer-sign");
        named.flag("error=pointer-sign");
        assert!(!named.silenced(&warning("E0673")));
        assert!(named.promoted(&warning("E0673"), false));
    }

    #[test]
    fn no_error_of_a_name_keeps_that_warning_a_warning_under_werror() {
        let mut named = Named::default();
        assert!(named.promoted(&warning("E0673"), true));
        named.flag("no-error=pointer-sign");
        assert!(!named.promoted(&warning("E0673"), true));
        assert!(!named.silenced(&warning("E0673")));
        // A warning with no option is not reached by any name.
        named.flag("no-all");
        assert!(named.promoted(&warning("E0001"), true));
        assert!(!named.silenced(&warning("E0001")));
    }

    /// The format checks are quiet by default and heard under `-Wall` or `-Wformat`, as in gcc
    /// 16, and a name turned off stays off whatever group comes after it.
    #[test]
    fn the_format_checks_wait_to_be_asked_for() {
        assert!(QUIET.windows(2).all(|pair| pair[0].0 < pair[1].0));
        let mut named = Named::default();
        assert!(named.silenced(&warning("E0785")));
        assert!(named.silenced(&warning("E0786")));
        assert!(named.silenced(&warning("E0789")));
        assert!(!named.silenced(&warning("E0790")));
        named.flag("all");
        assert!(!named.silenced(&warning("E0785")));
        assert!(!named.silenced(&warning("E0786")));
        named.flag("no-format-extra-args");
        named.flag("all");
        assert!(named.silenced(&warning("E0786")));
        assert!(!named.silenced(&warning("E0785")));
        named.flag("format=0");
        assert!(named.silenced(&warning("E0785")));
        assert!(named.silenced(&warning("E0788")));

        let mut named = Named::default();
        named.flag("format=2");
        assert!(!named.silenced(&warning("E0787")));
        let mut named = Named::default();
        named.flag("error=format");
        assert!(!named.silenced(&warning("E0785")));
        assert!(named.promoted(&warning("E0785"), false));
    }

    #[test]
    fn an_error_is_never_silenced_or_promoted() {
        let mut named = Named::default();
        named.flag("no-pointer-sign");
        let error = Diagnostic::error("x".to_owned(), Span::DUMMY).with_code("E0673");
        assert!(!named.silenced(&error));
        assert!(!named.promoted(&error, true));
    }
}
