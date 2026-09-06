//! The GNU compatibility surface: features.toml, attributes, builtins, pragmas.
//!
//! Design: `spec/13-gnu-compat.md`. Layer rank 4, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! The matrix is real. `features.toml` next to this file is the source of truth for what the
//! compiler claims to support, `build.rs` turns it into the table below, and the `__has_*`
//! family in the preprocessor answers out of it. The attributes and builtins themselves land
//! with the parser, and every row that says `unimplemented` says so because it is.
//!
//! The rule that makes the table worth having is in section 13.2: answering `__has_builtin`
//! untruthfully is worse than answering no, because a header that gets a yes and then fails
//! to compile is much harder to diagnose than one that takes its fallback path. So only a row
//! marked `implemented` answers yes, and a row marked `implemented` with no test named
//! against it fails the build.
//!
//! ```
//! use rucc_gnu::{Kind, Status};
//!
//! assert_eq!(rucc_gnu::has_feature("__has_include"), 1);
//! assert_eq!(rucc_gnu::has_attribute("cleanup"), 0, "not until the parser lands");
//! assert_eq!(rucc_gnu::has_attribute("no_such_attribute"), 0);
//!
//! // The armoured spelling is the same question.
//! assert_eq!(rucc_gnu::lookup(Kind::Attribute, "__packed__").map(|f| f.name), Some("packed"));
//!
//! // Nested functions are refused rather than pending, and the table says which.
//! let nested = rucc_gnu::lookup(Kind::Extension, "nested_functions").unwrap();
//! assert_eq!(nested.status, Status::Rejected);
//! ```
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-gnu/0.6.3")]

/// What kind of thing a row of the matrix describes.
///
/// The kind is part of the identity of a row, because `deprecated` is both a GNU attribute
/// and a C23 one and the two are answered by different operators with different values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// `__attribute__((x))` and `[[gnu::x]]`, asked about with `__has_attribute`.
    Attribute,
    /// A standard `[[x]]` attribute, asked about with `__has_c_attribute`.
    CAttribute,
    /// `__builtin_x`, asked about with `__has_builtin`.
    Builtin,
    /// A language or preprocessor feature, asked about with `__has_feature`.
    Feature,
    /// A GNU extension to the language, asked about with `__has_extension`.
    Extension,
}

/// How far along a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    /// Recognised and not done. The `__has_*` operators answer no.
    Unimplemented,
    /// Some of it works. The `__has_*` operators still answer no, because a feature that
    /// works most of the time is exactly the case where the fallback path is the safer one.
    Partial,
    /// Done, with a test named against it.
    Implemented,
    /// Will not be done, and the row says why. `nested_functions` is the example.
    Rejected,
}

impl Status {
    /// Whether the `__has_*` family answers yes for a row at this status.
    pub const fn is_available(self) -> bool {
        matches!(self, Status::Implemented)
    }
}

/// What happens when the compiler meets something this row describes and cannot do it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Answer {
    /// Warn and carry on, which is what GCC does for an attribute it does not know. Ignoring
    /// `hot` produces slower code and nothing worse.
    Warn,
    /// Refuse. Ignoring `packed`, `aligned`, `section`, `no_sanitize` or `naked` produces
    /// wrong code rather than slow code, and wrong code that compiles is the worst outcome
    /// available. This is section 13.4's rule.
    Error,
}

/// One row of the matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feature {
    /// The spelling asked about, with no `__` armour on it.
    pub name: &'static str,
    /// Which operator answers for it.
    pub kind: Kind,
    /// The GCC release that introduced it.
    pub gcc_version: &'static str,
    /// How far along it is.
    pub status: Status,
    /// The type a builtin has, written as a C prototype without the name, or empty.
    ///
    /// Empty for everything that is not a builtin, and for a builtin whose type depends on
    /// what it is handed: `__builtin_constant_p` takes anything, `__builtin_add_overflow`
    /// takes three types that have to agree, and the atomics are a family rather than a
    /// function. Those are decided where the arguments are, and a fixed type here would be a
    /// worse answer than none.
    ///
    /// It is a string rather than a structure because `size_t` is a different type on two
    /// targets and this table has no target. The compiler reads it once per builtin it is
    /// asked for. The set of words it may use is fixed and `build.rs` checks it, so a typo
    /// fails this crate's build rather than the compile of whoever first calls the builtin.
    pub signature: &'static str,
    /// The library function this builtin is, for the family where that is the whole answer.
    ///
    /// Empty for everything else. GCC's `__builtin_abort` is a call to `abort`, its
    /// `__builtin_strlen` a call to `strlen`, and the prefix is there so that a program can
    /// reach the function the C library promises even where its own name has been taken by a
    /// macro or by a definition of its own. GCC folds some of these when the arguments allow
    /// it, and folding is an optimization on top: the call is the meaning, and a compiler that
    /// only ever emits the call is right and slow rather than wrong.
    ///
    /// The name is written out rather than worked out by stripping the prefix, because the two
    /// are the same for every row here and need not be for the next one, and a table that says
    /// what it means is worth more than one that saves thirty words.
    pub library: &'static str,
    /// What to do when it is met and is not implemented.
    pub answer: Answer,
    /// What `__has_c_attribute` answers with, which the standard fixes per attribute. One for
    /// every other kind, where the operators answer one or nothing.
    pub value: u32,
    /// Projects known to need it, from the corpus in `spec/15-testing.md`.
    pub used_by: &'static [&'static str],
    /// The tests that prove the status, named as `crate::test` or as a file path.
    pub tests: &'static [&'static str],
    /// Anything a reader needs that the fields above do not say.
    pub notes: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/features.rs"));

/// The whole matrix, sorted by kind and then by name.
pub fn features() -> &'static [Feature] {
    FEATURES
}

/// The row for a name, if the matrix has one.
///
/// The `__x__` spelling is the same question as `x`, because that is how a header writes an
/// attribute name that a macro might otherwise have taken.
pub fn lookup(kind: Kind, name: &str) -> Option<&'static Feature> {
    let bare = unarmour(name);
    let at = FEATURES.binary_search_by(|f| f.kind.cmp(&kind).then_with(|| f.name.cmp(bare)));
    at.ok().map(|at| &FEATURES[at])
}

/// What `__has_attribute(name)` answers.
pub fn has_attribute(name: &str) -> u32 {
    answer(Kind::Attribute, name)
}

/// What `__has_c_attribute(name)` answers, which is the number the standard gives the
/// attribute rather than one.
pub fn has_c_attribute(name: &str) -> u32 {
    answer(Kind::CAttribute, name)
}

/// What `__has_builtin(name)` answers.
pub fn has_builtin(name: &str) -> u32 {
    answer(Kind::Builtin, name)
}

/// What `__has_feature(name)` answers.
pub fn has_feature(name: &str) -> u32 {
    answer(Kind::Feature, name)
}

/// What `__has_extension(name)` answers.
///
/// GCC treats the two as the same question and so do we: a feature that is available is
/// available whether or not the mode it is asked in makes it standard.
pub fn has_extension(name: &str) -> u32 {
    let extension = answer(Kind::Extension, name);
    if extension == 0 { answer(Kind::Feature, name) } else { extension }
}

fn answer(kind: Kind, name: &str) -> u32 {
    match lookup(kind, name) {
        Some(feature) if feature.status.is_available() => feature.value,
        _ => 0,
    }
}

/// `__packed__` and `packed` are the same attribute.
///
/// This is public because [`lookup`] is not the only thing that has to know it. Anything that
/// reads a name out of an attribute list and compares it against a spelling has the same
/// question, and a header writes the armoured form precisely so that a program's own macro
/// called `packed` cannot take the plain one, so a compiler that only knows the plain one reads
/// the wrong layout out of a header that was careful.
#[must_use]
pub fn unarmour(name: &str) -> &str {
    let bare = name.strip_prefix("__").and_then(|n| n.strip_suffix("__"));
    match bare {
        // `__builtin_x` and the atomics keep their prefix, because it is part of the name
        // rather than armour around it.
        Some(bare) if !bare.is_empty() && !name.starts_with("__builtin") => bare,
        _ => name,
    }
}

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_so_the_lookup_can_be_a_search() {
        let keys: Vec<(Kind, &str)> = FEATURES.iter().map(|f| (f.kind, f.name)).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn every_row_is_findable_by_its_own_name() {
        for feature in FEATURES {
            assert_eq!(lookup(feature.kind, feature.name), Some(feature));
        }
    }

    #[test]
    fn a_name_that_is_not_in_the_matrix_answers_no() {
        assert_eq!(has_attribute("nonesuch"), 0);
        assert_eq!(has_builtin("__builtin_nonesuch"), 0);
        assert_eq!(has_feature("nonesuch"), 0);
        assert_eq!(lookup(Kind::Attribute, "nonesuch"), None);
    }

    #[test]
    fn the_armoured_spelling_is_the_same_question() {
        assert_eq!(lookup(Kind::Attribute, "__packed__").map(|f| f.name), Some("packed"));
        assert_eq!(lookup(Kind::Attribute, "packed").map(|f| f.name), Some("packed"));
        assert_eq!(lookup(Kind::Attribute, "__packed"), None, "half the armour is not a name");
    }

    #[test]
    fn a_builtin_keeps_the_prefix_that_is_part_of_its_name() {
        assert!(lookup(Kind::Builtin, "__builtin_expect").is_some());
        assert_eq!(lookup(Kind::Builtin, "expect"), None);
    }

    #[test]
    fn only_an_implemented_row_answers_yes() {
        for feature in FEATURES {
            let answered = answer(feature.kind, feature.name);
            assert_eq!(
                answered != 0,
                feature.status == Status::Implemented,
                "{} answered {answered} at status {:?}",
                feature.name,
                feature.status
            );
        }
    }

    #[test]
    fn an_implemented_row_names_a_test() {
        // build.rs enforces this too. It is here as well because the build script failing is
        // a harder message to read than a failing test.
        for feature in FEATURES {
            if feature.status == Status::Implemented {
                assert!(!feature.tests.is_empty(), "{} claims to be implemented", feature.name);
            }
        }
    }

    #[test]
    fn a_library_builtin_names_the_function_it_is_and_the_type_to_call_it_with() {
        let abort = lookup(Kind::Builtin, "__builtin_abort").expect("in the table");
        assert_eq!(abort.library, "abort");
        assert_eq!(abort.signature, "void(void)");
        for feature in FEATURES {
            if feature.library.is_empty() {
                continue;
            }
            assert_eq!(feature.kind, Kind::Builtin, "{} is not a builtin", feature.name);
            assert!(!feature.signature.is_empty(), "{} has no type to call with", feature.name);
        }
    }

    /// Every one of them so far is the name with the prefix taken off, which is the rule GCC
    /// documents. The field is written out anyway, so this is what checks the two agree.
    #[test]
    fn the_library_function_is_the_name_without_the_prefix() {
        for feature in FEATURES {
            if feature.library.is_empty() {
                continue;
            }
            let bare = feature.name.strip_prefix("__builtin_");
            assert_eq!(bare, Some(feature.library), "{} names something else", feature.name);
        }
    }

    #[test]
    fn a_c_attribute_answers_with_the_number_the_standard_gives_it() {
        let deprecated = lookup(Kind::CAttribute, "deprecated").expect("C23 has it");
        assert_eq!(deprecated.value, 201904);
        // And it is a different row from the GNU attribute of the same name.
        let gnu = lookup(Kind::Attribute, "deprecated").expect("GCC has it too");
        assert_eq!(gnu.value, 1);
    }

    #[test]
    fn ignoring_an_attribute_silently_is_a_decision_the_table_records() {
        let packed = lookup(Kind::Attribute, "packed").expect("in the table");
        assert_eq!(packed.answer, Answer::Error, "ignoring it would produce wrong code");
        let cold = lookup(Kind::Attribute, "cold").expect("in the table");
        assert_eq!(cold.answer, Answer::Warn, "ignoring it would only produce slow code");
    }

    #[test]
    fn nested_functions_are_rejected_rather_than_pending() {
        let nested = lookup(Kind::Extension, "nested_functions").expect("in the table");
        assert_eq!(nested.status, Status::Rejected);
        assert!(!nested.notes.is_empty(), "a rejection has to say why");
    }

    #[test]
    fn milestone_is_recorded() {
        assert!(MILESTONE.starts_with('M'));
    }
}
